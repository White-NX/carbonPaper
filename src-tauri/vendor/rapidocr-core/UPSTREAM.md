# RapidOCR core compatibility copy

This directory contains the published `rapidocr-core` 0.2.2 crate from
[White-NX/rapidocr-rs](https://github.com/White-NX/rapidocr-rs), based on commit
`bc4afd4a3fc5cb65f0358c902241d547e4775274`. The upstream source is licensed under
Apache-2.0; see `LICENSE`.

CarbonPaper uses this copy to upgrade to `ort` 2.0.0-rc.15 and Rust 1.100.0
before a compatible RapidOCR release is available. The compatibility changes
pin the ORT dependency and share an explicit ONNX environment between pipeline
stages. Model verification, OCR processing and cancellation retain the
published implementation.

`README.md` and the examples describe the upstream API. `RapidOcr::new` continues
to initialize its own environment; `RapidOcr::new_with_environment` accepts an
environment supplied by its caller. The compatibility copy can be replaced by
a published upstream crate once that release supports the same ORT API.
