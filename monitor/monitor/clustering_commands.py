"""Command handlers for task and smart clustering IPC requests."""

from __future__ import annotations

import logging
from typing import Any, Callable, Dict, Optional

logger = logging.getLogger(__name__)

HANDLED_CLUSTERING_COMMANDS = {
    "run_clustering",
    "get_clustering_status",
    "set_clustering_interval",
    "get_tasks",
    "start_task_vectors_export",
    "get_task_vectors_export_status",
    "export_task_vectors_page",
    "finish_task_vectors_export",
    "upsert_task_vectors",
    "get_task_vectors_count",
}


def _requires_service(scheduler=None, manager=None) -> Optional[Dict[str, str]]:
    if scheduler is None and manager is None:
        return {"error": "Clustering service not initialised"}
    return None


def _requires_auth(auth_gate: Callable[..., bool]) -> Optional[Dict[str, str]]:
    if not auth_gate(force=True):
        return {"error": "AUTH_REQUIRED: clustering requires unlocked session"}
    return None


def handle_clustering_command(
    req: Dict[str, Any],
    scheduler,
    manager,
    auth_gate: Callable[..., bool],
) -> Optional[Dict[str, Any]]:
    """Handle clustering-related commands.

    Returns None when the command is not owned by this module.
    """
    cmd = req.get("command")
    if cmd not in HANDLED_CLUSTERING_COMMANDS:
        return None

    if cmd == "run_clustering":
        service_error = _requires_service(scheduler=scheduler)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        start_time = req.get("start_time")
        end_time = req.get("end_time")
        clustering_mode = req.get("clustering_mode", "auto")
        manual = bool(req.get("manual", False))
        try:
            if start_time is not None:
                start_time = float(start_time)
            if end_time is not None:
                end_time = float(end_time)
            result = scheduler.run_now(
                start_time=start_time,
                end_time=end_time,
                clustering_mode=clustering_mode,
                manual=manual,
            )
            return {"status": "success", **result}
        except Exception as e:
            return {"error": str(e)}

    if cmd == "get_clustering_status":
        service_error = _requires_service(scheduler=scheduler)
        if service_error:
            return service_error
        sched_config = scheduler.get_config()
        last = scheduler.get_last_result()
        return {
            "status": "success",
            "config": sched_config,
            "last_result": {
                k: v for k, v in (last or {}).items()
                if k != "clusters"
            } if last else None,
        }

    if cmd == "get_task_vectors_count":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        try:
            count = int(manager.hot_collection.count())
            return {"status": "success", "count": count}
        except Exception as e:
            logger.warning("get_task_vectors_count failed: %s", e)
            return {"error": str(e)}

    if cmd == "set_clustering_interval":
        service_error = _requires_service(scheduler=scheduler)
        if service_error:
            return service_error
        interval = req.get("interval", "1w")
        try:
            scheduler.set_interval(interval)
            return {"status": "success", "interval": interval}
        except ValueError as e:
            return {"error": str(e)}

    if cmd == "get_tasks":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        try:
            last = scheduler.get_last_result() if scheduler else None
            hot_clusters = last.get("clusters", []) if last else []
            cold_clusters = manager.get_cold_clusters()
            return {
                "status": "success",
                "hot_clusters": hot_clusters,
                "cold_clusters": cold_clusters,
            }
        except Exception as e:
            return {"error": str(e)}

    if cmd == "start_task_vectors_export":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        try:
            result = manager.start_task_vectors_export(
                export_id=req.get("export_id", ""),
            )
            return {"status": "success", **result}
        except Exception as e:
            logger.exception("start_task_vectors_export failed")
            return {"error": str(e)}

    if cmd == "get_task_vectors_export_status":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        try:
            result = manager.get_task_vectors_export_status(req.get("export_id", ""))
            return {"status": "success", **result}
        except Exception as e:
            logger.exception("get_task_vectors_export_status failed")
            return {"error": str(e)}

    if cmd == "export_task_vectors_page":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        try:
            result = manager.export_task_vectors_page(
                export_id=req.get("export_id", ""),
                cursor=req.get("cursor", 0),
                limit=req.get("limit", 128),
            )
            return {"status": "success", **result}
        except Exception as e:
            logger.exception("export_task_vectors_page failed")
            return {"error": str(e)}

    if cmd == "finish_task_vectors_export":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        released = manager.finish_task_vectors_export(req.get("export_id", ""))
        return {"status": "success", "released": released}

    if cmd == "upsert_task_vectors":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        try:
            count = manager.upsert_task_vectors(req.get("records", []))
            return {"status": "success", "upserted": count}
        except Exception as e:
            logger.exception("upsert_task_vectors failed")
            return {"error": str(e)}

    return None
