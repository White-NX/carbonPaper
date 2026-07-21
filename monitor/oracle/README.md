# M2 Python behavior oracle

This directory freezes the Python behavior that the v0.8.4 Beta Rust semantic
runtime must match before any production cutover.

The fixtures contain only synthetic text and programmatically generated images.
The oracle runs locally, disables telemetry and DirectML by default, and never
downloads models. It fails if the already-installed pinned model files are
missing.

Generate the canonical CPU oracle with the CarbonPaper virtual environment:

```powershell
& "$env:LOCALAPPDATA\carbonpaper\.venv\Scripts\python.exe" `
  tools\migration_oracle.py generate
```

Validate the current Python implementation against the committed oracle:

```powershell
& "$env:LOCALAPPDATA\carbonpaper\.venv\Scripts\python.exe" `
  tools\migration_oracle.py validate
```

Build the isolated Rust semantic worker, then validate its CPU output against
the same committed oracle:

```powershell
npm run build:semantic-ml
& "$env:LOCALAPPDATA\carbonpaper\.venv\Scripts\python.exe" `
  tools\validate_rust_semantic.py --provider cpu
```

Use `--provider directml` for the explicit DirectML gate. Chinese-CLIP and BGE
pass that gate. MiniLM and the uint8 reranker currently exceed the reviewed
DirectML numeric tolerance, so the worker does not advertise them for that
provider and the desktop supervisor retries them on CPU. The validator uses
only installed model files and the pinned ONNX Runtime DLL, disables network
model access and telemetry, and prints exact-token or numeric metrics for each
model contract.

`golden-v1.json` records exact token tensors, preprocessing tensors, normalized
embeddings, raw reranker logits, search filtering/pagination output, model file
fingerprints, and the tolerances that future Rust parity tests must enforce.
Numeric records carry explicit `cpu` and `directml` tolerance profiles: callers
must select the profile matching the Rust execution provider instead of
weakening the canonical CPU gate. The live CLIP `search_nl` threshold is also
recorded from the explicit production request, not inferred from a mock default.
Normal CI validates the fixture/golden structure and the model-free contracts;
it does not load or download the large models.
