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
    "nl_cluster_query",
    "nl_cluster_reranker_status",
    "smart_cluster_drain_now",
    "smart_cluster_stop_drain",
    "smart_cluster_worker_status",
    "smart_cluster_calibrate_preview",
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

    if cmd == "nl_cluster_query":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        query = req.get("query", "")
        n_results = req.get("n_results", 30)
        enable_rerank = bool(req.get("enable_rerank", False))
        rerank_variant = req.get("rerank_variant") or "uint8"
        try:
            from task_clustering import ModelNotAvailableError
            from reranker import RerankerNotAvailableError
            try:
                results = manager.query_by_text(
                    query,
                    n_results=int(n_results),
                    enable_rerank=enable_rerank,
                    rerank_variant=rerank_variant,
                )
            except ModelNotAvailableError:
                return {"error": "MiniLM model not downloaded — run clustering setup first"}
            except RerankerNotAvailableError as e:
                return {"error": f"RERANKER_UNAVAILABLE: {e}"}
            return {
                "status": "success",
                "results": results,
                "reranked": enable_rerank,
                "rerank_variant": rerank_variant if enable_rerank else None,
            }
        except Exception as e:
            logger.exception("nl_cluster_query failed")
            return {"error": str(e)}

    if cmd == "nl_cluster_reranker_status":
        try:
            from reranker import Reranker, _resolve_model_path, list_available_variants
            r = Reranker()
            return {
                "status": "success",
                "available": Reranker.is_model_available(),
                "loaded": r.is_loaded(),
                "loaded_variant": r.loaded_variant,
                "provider": r.provider,
                "available_variants": list_available_variants(),
                "model_path": _resolve_model_path(),
            }
        except Exception as e:
            return {"error": str(e)}

    if cmd == "smart_cluster_drain_now":
        try:
            from smart_cluster_worker import SmartClusterWorker
            SmartClusterWorker().request_drain_now()
            return {"status": "success"}
        except Exception as e:
            return {"error": str(e)}

    if cmd == "smart_cluster_stop_drain":
        try:
            from smart_cluster_worker import SmartClusterWorker
            SmartClusterWorker().request_stop_drain()
            return {"status": "success"}
        except Exception as e:
            return {"error": str(e)}

    if cmd == "smart_cluster_worker_status":
        try:
            from smart_cluster_worker import SmartClusterWorker
            worker = SmartClusterWorker()
            sc = worker.storage_client
            pending_count = sc.smart_cluster_count_pending() if sc else 0
            return {
                "status": "success",
                "is_running": worker.is_running(),
                "is_force_running": worker.is_force_running(),
                "pending_count": pending_count,
            }
        except Exception as e:
            return {"error": str(e)}

    if cmd == "smart_cluster_calibrate_preview":
        service_error = _requires_service(manager=manager)
        if service_error:
            return service_error
        auth_error = _requires_auth(auth_gate)
        if auth_error:
            return auth_error
        query = req.get("query", "")
        n_results = int(req.get("n_results", 30))
        try:
            from task_clustering import ModelNotAvailableError
            from reranker import RerankerNotAvailableError
            try:
                results = manager.query_by_text(
                    query,
                    n_results=n_results,
                    enable_rerank=True,
                )
            except ModelNotAvailableError:
                return {"error": "MiniLM model not downloaded — run clustering setup first"}
            except RerankerNotAvailableError as e:
                return {"error": f"RERANKER_UNAVAILABLE: {e}"}
            return {"status": "success", "results": results}
        except Exception as e:
            logger.exception("smart_cluster_calibrate_preview failed")
            return {"error": str(e)}

    return None
