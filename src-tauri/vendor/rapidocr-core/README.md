# rapidocr-core

Rust OCR core for RapidOCR-style ONNX pipelines.

The crate provides:

- `RapidOcr` library API for `det -> optional cls -> rec`.
- `RapidOcrConfig` TOML-compatible configuration.
- `InferenceOptions` limits ONNX Runtime intra-op/inter-op threads and parallel execution.
- `OcrCancellationToken` cooperatively cancels active ONNX runs and later pipeline stages.
- The optional `tokio` feature provides a dedicated-worker `TokioRapidOcr` service and cancel-on-drop tasks.
- Registered ONNX model sets through `model_set_by_name` and `available_model_sets`.
- Explicit model cache/download handling through `ModelCache` and `ModelDownloadMode`.

Models are not bundled in the crate package. Applications should either let `ModelCache` download registered model assets, pre-populate a model directory during deployment, or provide explicit model paths in configuration.

The default `model-download` feature enables the blocking HTTP downloader. Disable default
features when models are provisioned separately to avoid depending on `reqwest`.

The optional Windows-only `directml` feature makes the DirectML execution provider available.
Select `ExecutionProvider::DirectMl` in `InferenceOptions` to use it. DirectML requires a DirectX
12-capable device and serial session execution.

The optional `tokio` feature adds a bounded, single-worker asynchronous adapter. Its timeout API
uses ONNX Runtime termination and waits for worker cleanup, but termination remains cooperative
rather than hard-real-time. Call `shutdown().await` for deterministic cleanup; dropping the adapter
requests cancellation but does not join its worker thread.

See the workspace `README.md` for parity, benchmark, and CLI workflows.
