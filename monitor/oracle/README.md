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

`golden-v1.json` records exact token tensors, preprocessing tensors, normalized
embeddings, raw reranker logits, search filtering/pagination output, model file
fingerprints, and the tolerances that future Rust parity tests must enforce.
Numeric records carry explicit `cpu` and `directml` tolerance profiles: callers
must select the profile matching the Rust execution provider instead of
weakening the canonical CPU gate. The live CLIP `search_nl` threshold is also
recorded from the explicit production request, not inferred from a mock default.
Normal CI validates the fixture/golden structure and the model-free contracts;
it does not load or download the large models.
