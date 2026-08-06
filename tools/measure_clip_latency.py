"""Measure CPU latency of the Rust Chinese-CLIP encoders.

M2.5 steps 8 and 9 pick two constants from numbers that did not exist yet: the
capture indexer's chunk size (one image, because a request in flight cannot be
interrupted and the chunk therefore *is* the worst-case foreground wait) and the
query encode's budget. This probe produces those numbers on the machine it runs
on, the same way `docs/python-removal-roadmap.md` records the step-6
cross-encoder figures.

It reuses the worker harness from `validate_rust_semantic.py` rather than
speaking the protocol again, so a change to the framing breaks one file.

    python tools/measure_clip_latency.py
"""

from __future__ import annotations

import argparse
import statistics
import time
from pathlib import Path
from typing import List

import numpy as np
from PIL import Image

from validate_rust_semantic import (
    DEFAULT_APPDATA_ROOT,
    REQUEST_TIMEOUT_MS,
    SemanticWorker,
    _default_ort_dylib,
    _default_worker,
)

#: Sizes that bracket a real capture. 1920x1080 is the common case; the 3840
#: row is there because CLIP resizes to 224 square regardless, so the cost of a
#: larger screenshot is decode and resize rather than inference — which is worth
#: seeing separately.
IMAGE_SIZES = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)]

QUERIES = [
    "一段蓝色渐变的界面",
    "VS Code 里的 Rust 代码",
    "a spreadsheet with a bar chart",
]


def _screenshot(width: int, height: int) -> Image.Image:
    """A deterministic image with enough structure to be worth encoding."""
    x = np.linspace(0, 255, width, dtype=np.uint8)
    y = np.linspace(0, 255, height, dtype=np.uint8)
    red = np.tile(x, (height, 1))
    green = np.tile(y.reshape(-1, 1), (1, width))
    blue = (red // 2 + green // 2).astype(np.uint8)
    return Image.fromarray(np.dstack([red, green, blue]), mode="RGB")


def _embed_image(worker: SemanticWorker, images: List[Image.Image]) -> float:
    body = bytearray()
    inputs = []
    for image in images:
        raw = image.tobytes()
        inputs.append(
            {
                "width": image.width,
                "height": image.height,
                "stride": image.width * 3,
                "offset": len(body),
                "body_len": len(raw),
            }
        )
        body.extend(raw)
    started = time.perf_counter()
    response = worker.request(
        "embed_image",
        model="chinese_clip",
        images=inputs,
        body_len=len(body),
        timeout_ms=REQUEST_TIMEOUT_MS,
        body=bytes(body),
    )
    elapsed = time.perf_counter() - started
    if response.get("status") != "embedding_complete":
        raise RuntimeError(f"unexpected image embedding response: {response!r}")
    return elapsed


def _embed_text(worker: SemanticWorker, texts: List[str]) -> float:
    started = time.perf_counter()
    response = worker.request(
        "embed_text",
        model="chinese_clip",
        texts=texts,
        timeout_ms=REQUEST_TIMEOUT_MS,
    )
    elapsed = time.perf_counter() - started
    if response.get("status") != "embedding_complete":
        raise RuntimeError(f"unexpected text embedding response: {response!r}")
    return elapsed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument(
        "--models-root", type=Path, default=DEFAULT_APPDATA_ROOT / "models"
    )
    args = parser.parse_args()

    worker_path = _default_worker()
    ort = _default_ort_dylib()
    print(f"Worker: {worker_path}")
    print(f"ONNX Runtime: {ort}")

    worker = SemanticWorker(
        worker_path,
        ort,
        models_root=args.models_root,
        onnx_models_root=args.models_root,
        provider="cpu",
        dml_device_id=0,
    )
    try:
        ready = worker.read_response()
        print(f"Ready: worker={ready.get('worker_version')}, provider={ready.get('provider')}")

        # The first request pays the model load; report it on its own rather
        # than letting it inflate the steady-state figures.
        cold = _embed_image(worker, [_screenshot(1920, 1080)])
        print(f"\nCold first image (includes the 177 MB model load): {cold:.3f} s")

        print("\nSteady-state single image, by capture size:")
        for width, height in IMAGE_SIZES:
            image = _screenshot(width, height)
            samples = [_embed_image(worker, [image]) for _ in range(args.repeats)]
            print(
                f"  {width}x{height}: median {statistics.median(samples):.3f} s "
                f"(min {min(samples):.3f}, max {max(samples):.3f})"
            )

        print("\nPer-image cost by batch size (1920x1080):")
        image = _screenshot(1920, 1080)
        for batch in (1, 2, 4, 8):
            samples = [_embed_image(worker, [image] * batch) for _ in range(args.repeats)]
            median = statistics.median(samples)
            print(f"  batch {batch}: {median:.3f} s total, {median / batch:.3f} s per image")

        print("\nQuery text encode (this is what a search waits for):")
        # Warm, because a search that lands after any indexing has run finds the
        # model resident.
        _embed_text(worker, QUERIES[:1])
        for text in QUERIES:
            samples = [_embed_text(worker, [text]) for _ in range(args.repeats)]
            print(f"  {text[:28]!r}: median {statistics.median(samples) * 1000:.1f} ms")
        samples = [_embed_text(worker, QUERIES) for _ in range(args.repeats)]
        print(f"  all {len(QUERIES)} in one request: median {statistics.median(samples) * 1000:.1f} ms")
    finally:
        worker.close()


if __name__ == "__main__":
    main()
