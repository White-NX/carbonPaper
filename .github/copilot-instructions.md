# Copilot Instructions for CarbonPaper

## Architecture Overview

CarbonPaper is a Windows desktop application for text-searchable screenshot history.

- **Frontend**: React + Vite (UI) in `src/`. Communicates with Rust via Tauri `invoke()` commands.
- **Backend**: Rust (Tauri v2) in `src-tauri/src/`. Owns the application lifecycle, screenshot capture (WGC), the encrypted SQLite database, Windows Hello authentication, and the worker processes:
    -   `carbonpaper-ml`: RapidOCR and the CLIP ANN index builder.
    -   `carbonpaper-semantic-worker`: Chinese-CLIP, MiniLM, BGE and reranker inference.
    -   `carbonpaper-office`: Office document observation.
    -   `carbonpaper-nmh`: browser extension native messaging.

There is no Python component. The former Python monitor was removed; see `docs/python-removal-roadmap.md`.

### Data Flow
1.  **React** calls `invoke('command_name')`.
2.  **Rust** handles the command directly, or forwards a bounded request to one of its worker processes.

## Critical Developer Workflows

- **Full App**: `npm run debug` starts Vite, builds the worker binaries and launches the app.
- **Rust checks**: run `cargo check --manifest-path src-tauri/Cargo.toml` after editing `.rs` files.
- **Tests**: `npm run test:frontend`, `npm run test:rust`, `npm run test:security`.

### Lifecycle Commands
- `start_monitor`, `stop_monitor`, `pause_monitor`, `resume_monitor`: capture lifecycle control.
- `get_monitor_status`: returns `{ paused: bool, stopped: bool }`.

## Coding Conventions

- **Frontend**:
    -   Use `@tauri-apps/api/core` for `invoke`.
    -   Do not use raw `fetch` for backend logic; route through Rust `invoke`.
    -   Wrap authenticated calls with `withAuth()` from `src/lib/auth_api.js`.
- **Rust**:
    -   Commands return `Result<T, String>`.
    -   Sensitive commands call `check_auth_required()` before processing.
- **Error Handling**:
    -   Rust `invoke` throws errors to JS. Handle them in `try/catch` blocks in React components.
