"""Restartable Presidio worker process.

Presidio/spaCy initialization and analysis can run longer than the Rust-side
MCP timeout. Keeping it in a child process lets the monitor terminate in-flight
work after timeout instead of letting model loading continue in an IPC handler
thread.
"""

from __future__ import annotations

import traceback
from typing import Any, Dict, List, Optional

from .worker_supervisor import WorkerSupervisor, attach_response_metadata


def _worker_main(conn):
    try:
        from .presidio_service import PresidioService
        conn.send({"status": "ready"})
    except Exception as exc:
        conn.send({"status": "error", "error": str(exc), "traceback": traceback.format_exc()})
        return

    svc = PresidioService.get_instance()
    while True:
        try:
            msg = conn.recv()
        except EOFError:
            return

        command = msg.get("command")

        def send_response(response: Dict[str, Any]):
            conn.send(attach_response_metadata(msg, response))

        try:
            if command == "stop":
                send_response({"status": "success"})
                return
            if command == "analyze":
                language = msg.get("language", "zh-CN")
                if not svc._initialized:
                    svc.initialize(language)
                elif language:
                    # set_language is cheap when unchanged and reloads when needed.
                    svc.switch_language(language)
                    if not svc._initialized:
                        svc.initialize(language)
                results = svc.analyze(msg.get("texts", []), msg.get("entity_types"))
                send_response({
                    "status": "success",
                    "results": [{"entities": list(ents)} for ents in results],
                })
            elif command == "set_language":
                svc.switch_language(msg.get("language", "zh-CN"))
                send_response({"status": "success"})
            elif command == "status":
                send_response({
                    "status": "success",
                    "initialized": bool(svc._initialized),
                    "language": svc._current_lang,
                    "model": svc._current_model,
                })
            elif command == "unload":
                svc.unload()
                send_response({"status": "success"})
            elif command == "check_idle":
                unloaded = svc.check_idle_and_unload()
                send_response({"status": "success", "unloaded": bool(unloaded)})
            else:
                send_response({"status": "error", "error": f"Unknown Presidio worker command: {command}"})
        except Exception as exc:
            send_response({"status": "error", "error": str(exc), "traceback": traceback.format_exc()})


class PresidioWorker(WorkerSupervisor):
    def __init__(self):
        super().__init__(
            name="CarbonPresidioWorker",
            target=_worker_main,
            ready_timeout=30.0,
            stop_timeout=1.0,
            kill_timeout=3.0,
        )

    def request(self, payload: Dict[str, Any], timeout: float = 14.0) -> Dict[str, Any]:
        command = payload.get("command")
        if not command:
            raise ValueError("Presidio worker payload requires command")
        payload = dict(payload)
        payload.pop("command", None)
        return super().request(
            command,
            payload,
            timeout=timeout,
            start_timeout=min(max(timeout, 1.0), 30.0),
        )

    def analyze(
        self,
        texts: List[str],
        language: str,
        entity_types: Optional[List[str]] = None,
        timeout: float = 14.0,
    ) -> List[Dict[str, Any]]:
        result = self.request(
            {
                "command": "analyze",
                "texts": texts,
                "language": language,
                "entity_types": entity_types,
            },
            timeout=timeout,
        )
        if result.get("status") != "success":
            raise RuntimeError(result.get("error", "Presidio analyze failed"))
        return result.get("results", [])

    def status(self) -> Dict[str, Any]:
        with self._lock:
            if self._proc is None or not self._proc.is_alive() or self._conn is None:
                return {"status": "success", "initialized": False, "language": None, "model": None}
        return self.request({"command": "status"}, timeout=2.0)

    def unload(self) -> Dict[str, Any]:
        with self._lock:
            if self._proc is None or not self._proc.is_alive() or self._conn is None:
                return {"status": "success", "unloaded": False}
        return self.request({"command": "unload"}, timeout=5.0)

    def check_idle(self) -> Dict[str, Any]:
        with self._lock:
            if self._proc is None or not self._proc.is_alive() or self._conn is None:
                return {"status": "success", "unloaded": False}
        return self.request({"command": "check_idle"}, timeout=5.0)


_presidio_worker = PresidioWorker()


def get_presidio_worker() -> PresidioWorker:
    return _presidio_worker
