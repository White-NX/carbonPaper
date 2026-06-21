"""Shared watchdog/supervisor for restartable Python worker processes."""

from __future__ import annotations

import logging
import multiprocessing
import threading
import time
from typing import Any, Callable, Dict, Optional, Sequence


logger = logging.getLogger(__name__)


class WorkerProtocolError(RuntimeError):
    """Raised when a worker response cannot be matched to the request."""


class WorkerBackoffError(RuntimeError):
    """Raised when a worker is temporarily held down after restart churn."""


def attach_response_metadata(request: Dict[str, Any], response: Any) -> Dict[str, Any]:
    """Attach request correlation fields to a worker response."""
    if isinstance(response, dict):
        result = dict(response)
    else:
        result = {"status": "error", "error": f"Worker returned non-object response: {type(response).__name__}"}
    request_id = request.get("_request_id")
    if request_id is not None:
        result["_request_id"] = request_id
    command = request.get("command")
    if command is not None:
        result["_command"] = command
    return result


class WorkerSupervisor:
    """Restartable child-process supervisor with request/response correlation.

    The supervisor owns a single multiprocessing child and pipe. Every command
    is sent with a monotonically increasing ``_request_id`` and the worker must
    echo it back. Mismatched or missing ids mean the pipe is contaminated, so the
    child is restarted before later requests can use it.
    """

    def __init__(
        self,
        *,
        name: str,
        target: Callable[..., Any],
        args: Sequence[Any] = (),
        ready_timeout: float = 30.0,
        stop_timeout: float = 2.0,
        kill_timeout: float = 5.0,
        restart_limit: int = 3,
        restart_window_secs: float = 60.0,
        restart_cooldown_secs: float = 60.0,
        slow_request_secs: float = 3.0,
        slow_start_secs: float = 5.0,
        log: Optional[logging.Logger] = None,
    ):
        self.name = name
        self.target = target
        self.args = tuple(args)
        self.ready_timeout = float(ready_timeout)
        self.stop_timeout = float(stop_timeout)
        self.kill_timeout = float(kill_timeout)
        self.restart_limit = max(1, int(restart_limit))
        self.restart_window_secs = max(1.0, float(restart_window_secs))
        self.restart_cooldown_secs = max(1.0, float(restart_cooldown_secs))
        self.slow_request_secs = max(0.0, float(slow_request_secs))
        self.slow_start_secs = max(0.0, float(slow_start_secs))
        self.log = log or logger

        self._ctx = multiprocessing.get_context("spawn")
        self._conn = None
        self._proc = None
        self._lock = threading.RLock()
        self._request_seq = 0
        self._restart_events = []
        self._restart_count = 0
        self._failure_count = 0
        self._last_error = None
        self._last_restart_reason = None
        self._last_restart_at = None
        self._started_at = None
        self._current_command = None
        self._state = "stopped"
        self._degraded_until = 0.0

    def start(self, timeout: Optional[float] = None):
        with self._lock:
            if self._proc is not None and self._proc.is_alive() and self._conn is not None:
                return

            now = time.monotonic()
            if self._degraded_until and now < self._degraded_until:
                remaining = self._degraded_until - now
                self._state = "degraded"
                raise WorkerBackoffError(
                    f"{self.name} restart rate limit active for {remaining:.1f}s"
                )

            started = time.perf_counter()
            parent_conn, child_conn = self._ctx.Pipe()
            proc = self._ctx.Process(
                target=self.target,
                args=(child_conn, *self.args),
                name=self.name,
                daemon=True,
            )
            self._state = "starting"
            proc.start()
            self._conn = parent_conn
            self._proc = proc
            self._started_at = time.time()

            ready_timeout = float(timeout if timeout is not None else self.ready_timeout)
            if not parent_conn.poll(ready_timeout):
                self.restart(reason="startup_timeout", error=f"ready timed out after {ready_timeout}s")
                raise TimeoutError(f"{self.name} cold start timed out after {ready_timeout}s")

            ready = parent_conn.recv()
            if not isinstance(ready, dict) or ready.get("status") != "ready":
                error = ready.get("error") if isinstance(ready, dict) else str(ready)
                self.restart(reason="startup_error", error=error)
                raise RuntimeError(error or f"{self.name} failed to start")

            self._state = "ready"
            self._last_error = None
            elapsed = time.perf_counter() - started
            if elapsed >= self.slow_start_secs:
                self.log.warning(
                    "%s started slowly elapsed=%.3fs pid=%s",
                    self.name,
                    elapsed,
                    proc.pid,
                )
            else:
                self.log.info("%s started elapsed=%.3fs pid=%s", self.name, elapsed, proc.pid)

    def request(
        self,
        command: str,
        payload: Optional[Dict[str, Any]] = None,
        timeout: float = 120.0,
        start_timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        with self._lock:
            self.start(timeout=start_timeout)
            assert self._conn is not None

            request_id = self._next_request_id_locked()
            message = {"command": command, "_request_id": request_id}
            if payload:
                message.update(payload)

            self._current_command = command
            self._state = "busy"
            request_started = time.perf_counter()
            try:
                self._conn.send(message)
                if not self._conn.poll(timeout):
                    self._failure_count += 1
                    self.restart(
                        reason="request_timeout",
                        error=f"{command} timed out after {timeout}s",
                    )
                    raise TimeoutError(f"{self.name} command {command} timed out after {timeout}s")

                result = self._conn.recv()
            except (EOFError, OSError) as exc:
                if isinstance(exc, TimeoutError):
                    raise
                self._failure_count += 1
                self.restart(reason="ipc_error", error=str(exc))
                raise RuntimeError(f"{self.name} IPC failed during {command}: {exc}") from exc
            finally:
                self._current_command = None

            if not isinstance(result, dict):
                self._failure_count += 1
                self.log.error(
                    "%s protocol desync command=%s request_id=%s response_type=%s",
                    self.name,
                    command,
                    request_id,
                    type(result).__name__,
                )
                self.restart(reason="protocol_desync", error=f"non-object response: {type(result).__name__}")
                raise WorkerProtocolError(f"{self.name} returned non-object response for {command}")

            response_id = result.get("_request_id")
            response_command = result.get("_command")
            if response_id != request_id or response_command != command:
                self._failure_count += 1
                self.log.error(
                    "%s protocol desync expected_command=%s expected_request_id=%s "
                    "actual_command=%s actual_request_id=%s result_keys=%s",
                    self.name,
                    command,
                    request_id,
                    response_command,
                    response_id,
                    sorted(result.keys()),
                )
                self.restart(
                    reason="protocol_desync",
                    error=(
                        f"expected {command}#{request_id}, "
                        f"got {response_command}#{response_id}"
                    ),
                )
                raise WorkerProtocolError(
                    f"{self.name} response mismatch for {command}: "
                    f"expected request_id={request_id}, got request_id={response_id}, "
                    f"command={response_command}"
                )

            if result.get("error") or result.get("status") == "error":
                self._failure_count += 1
                self._last_error = result.get("error")
            else:
                self._last_error = None
            self._state = "ready"
            elapsed = time.perf_counter() - request_started
            if elapsed >= self.slow_request_secs:
                self.log.warning(
                    "%s command slow command=%s request_id=%s elapsed=%.3fs timeout=%.3fs result_keys=%s",
                    self.name,
                    command,
                    request_id,
                    elapsed,
                    timeout,
                    sorted(result.keys()),
                )
            else:
                self.log.debug(
                    "%s command done command=%s request_id=%s elapsed=%.3fs",
                    self.name,
                    command,
                    request_id,
                    elapsed,
                )
            return result

    def restart(self, reason: str = "manual", error: Optional[str] = None, count: bool = True):
        with self._lock:
            if count:
                self._note_restart_locked(reason, error)
            else:
                self._last_restart_reason = reason
                self._last_restart_at = time.time()
                self._last_error = error
            self._state = "restarting"
            try:
                if self._proc is not None and self._proc.is_alive():
                    self._proc.terminate()
                    self._proc.join(timeout=self.kill_timeout)
                    if self._proc.is_alive():
                        self._proc.kill()
            finally:
                self._proc = None
                self._conn = None
                if self._degraded_until and time.monotonic() < self._degraded_until:
                    self._state = "degraded"
                else:
                    self._state = "stopped"

    def stop(self):
        with self._lock:
            try:
                if self._conn and self._proc and self._proc.is_alive():
                    request_id = self._next_request_id_locked()
                    self._conn.send({"command": "stop", "_request_id": request_id})
                    if self._conn.poll(self.stop_timeout):
                        self._conn.recv()
            except Exception:
                pass
            self.log.info("%s stopping", self.name)
            self.restart(reason="stop", count=False)

    def status_snapshot(self) -> Dict[str, Any]:
        acquired = self._lock.acquire(blocking=False)
        if acquired:
            try:
                return self._status_snapshot_unlocked(lock_contended=False)
            finally:
                self._lock.release()
        return self._status_snapshot_unlocked(lock_contended=True)

    def _status_snapshot_unlocked(self, *, lock_contended: bool) -> Dict[str, Any]:
        proc = self._proc
        alive = bool(proc is not None and proc.is_alive())
        return {
            "name": self.name,
            "state": self._state,
            "alive": alive,
            "pid": proc.pid if proc is not None else None,
            "current_command": self._current_command,
            "restart_count": self._restart_count,
            "failure_count": self._failure_count,
            "last_error": self._last_error,
            "last_restart_reason": self._last_restart_reason,
            "last_restart_at": self._last_restart_at,
            "degraded_until": self._degraded_until or None,
            "started_at": self._started_at,
            "lock_contended": lock_contended,
        }

    def _next_request_id_locked(self) -> int:
        self._request_seq += 1
        return self._request_seq

    def _note_restart_locked(self, reason: str, error: Optional[str]):
        now = time.monotonic()
        self._restart_count += 1
        self._last_restart_reason = reason
        self._last_restart_at = time.time()
        self._last_error = error

        cutoff = now - self.restart_window_secs
        self._restart_events = [ts for ts in self._restart_events if ts >= cutoff]
        self._restart_events.append(now)
        if len(self._restart_events) > self.restart_limit:
            self._degraded_until = now + self.restart_cooldown_secs
            self.log.error(
                "%s entered degraded state after %s restarts in %.1fs; cooldown %.1fs",
                self.name,
                len(self._restart_events),
                self.restart_window_secs,
                self.restart_cooldown_secs,
            )
        else:
            self.log.warning("%s restarting reason=%s error=%s", self.name, reason, error)
