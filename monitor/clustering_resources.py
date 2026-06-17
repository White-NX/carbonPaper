"""Resource policy helpers for task clustering."""

from __future__ import annotations

import os
from typing import Any, Dict

EMBEDDING_DIM = 384
PACMAP_N_COMPONENTS = 15
LOW_MEMORY_CLUSTERING_THRESHOLD = 20_000
MANUAL_CLUSTERING_PROMPT_THRESHOLD = 20_000
CLUSTERING_SAMPLE_SIZE = int(os.environ.get("CARBONPAPER_CLUSTERING_SAMPLE_SIZE", "5000") or "5000")
CLUSTERING_ASSIGNMENT_BATCH_SIZE = int(os.environ.get("CARBONPAPER_CLUSTERING_ASSIGNMENT_BATCH_SIZE", "2000") or "2000")


def estimate_clustering_peak_bytes(n_vectors: int, dim: int = EMBEDDING_DIM) -> int:
    """Return a conservative peak estimate for full PaCMAP + HDBSCAN."""
    n = max(0, int(n_vectors))
    if n <= 0:
        return 0
    raw = n * dim * 4
    reduced = n * PACMAP_N_COMPONENTS * 8
    graphish = n * 128 * 16
    pairwise_cap = min(n * n * 4, 8 * 1024 * 1024 * 1024)
    interpreter_overhead = n * 1024
    return int((raw * 6) + (reduced * 4) + graphish + pairwise_cap + interpreter_overhead)


def memory_status_for_clustering(n_vectors: int) -> Dict[str, Any]:
    """Estimate whether full clustering has enough effective memory.

    Virtual memory/pagefile can keep a run alive but may thrash heavily, so only
    a bounded portion is counted as useful headroom.
    """
    estimated = estimate_clustering_peak_bytes(n_vectors)
    status: Dict[str, Any] = {
        "estimated_peak_bytes": estimated,
        "available_physical_bytes": None,
        "available_swap_bytes": None,
        "effective_available_bytes": None,
        "low_memory": False,
        "physical_pressure": False,
        "source": "unknown",
    }
    try:
        import psutil

        vm = psutil.virtual_memory()
        swap = psutil.swap_memory()
        available_physical = int(getattr(vm, "available", 0) or 0)
        available_swap = int(getattr(swap, "free", 0) or 0)
        total_physical = int(getattr(vm, "total", 0) or 0)
        swap_credit = min(available_swap, max(total_physical // 2, 0))
        effective_available = available_physical + swap_credit

        status.update({
            "available_physical_bytes": available_physical,
            "available_swap_bytes": available_swap,
            "effective_available_bytes": effective_available,
            "physical_pressure": bool(estimated and available_physical and estimated > available_physical * 0.85),
            "low_memory": bool(estimated and effective_available and estimated > effective_available * 0.75),
            "source": "psutil",
        })
    except Exception as e:
        status["error"] = str(e)
    return status
