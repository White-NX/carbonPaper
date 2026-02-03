# Copilot Instructions for CarbonPaper

## Architecture Overview

CarbonPaper is a hybrid desktop application combining text-searchable screenshot history and activity monitoring.

- **Frontend**: React + Vite (UI). Communicates with Rust via Tauri commands.
- **Backend (Host)**: Rust (Tauri). Manages the application lifecycle and acts as a supervisor for the Python subsystem.
- **Backend (Intelligence)**: Python (`monitor/`). Runs as a child process. Handles:
    -   Screen capture (`capture.py`)
    -   OCR (PaddleOCR) and Vector Embedding (Chinese-CLIP)
    -   Storage (ChromaDB)
    -   IPC Server (Windows Named Pipes)

### Data Flow
1.  **React** calls `invoke('command_name')`.
2.  **Rust** receives command, creates/connects to Named Pipe `\\.\pipe\carbon_monitor_secure`.
3.  **Python** receives JSON payload, executes task (e.g., `search`, `status`), and returns JSON response.

## Critical Developer Workflows

### 1. Build & Run
- **Full App**: `npm run tauri dev`
    -   Starts Vite server.
    -   Compiles Rust.
    -   Rust binary launches `monitor/main.py` automatically using found `.venv`.
- **Python Only (Debugging)**:
    -   Activate venv.
    -   Run `python monitor/main.py`.
    -   It will print a random pipe name (if not running under Tauri) or listen on the secure pipe.

### 2. Python Environment
- The Rust backend specifically looks for `.venv/Scripts/python.exe` (Windows) relative to the `monitor/` directory.
- **CRITICAL**: In `monitor/main.py`, `import torch` **MUST** happen before other heavy imports (like PaddleOCR/cv2) to avoid DLL loading errors (WinError 127).

### 3. IPC & Commands
- **Rust -> Python**: JSON over Named Pipe.
- **Existing Commands**:
    -   `start_monitor`, `stop_monitor`, `pause_monitor`, `resume_monitor`: Lifecycle control.
    -   `get_monitor_status`: Returns `{ paused: bool, stopped: bool, interval: number }`.
    -   **Available but Unlinked**: `search` (text), `search_nl` (natural language).

## Project Structure Highlights

- `monitor/`: Python subsystem.
    -   `main.py`: Entry point.
    -   `vector_store.py`: Handles ChromaDB and Chinese-CLIP interactions.
    -   `screenshot_worker.py`: Threads for performing OCR and Embedding on queue.
- `src-tauri/src/monitor.rs`: Rust logic for process management and IPC forwarding.
- `src/components/Timeline.jsx`: **Currently uses Mock Data**. Needs to be connected to backend `search` commands.
- `src/hooks/useComfyAPI.js`: **Mocked/Deprecated**. Remnants of ComfyUI integration.

## Coding Conventions

- **Frontend**:
    -   Use `@tauri-apps/api/core` for `invoke`.
    -   Do not use raw `fetch` for backend logic; route through Rust `invoke` if talking to Python.
- **Python**:
    -   Use `monitor` package structure.
    -   Ensure threaded workers (`capture.py`, `screenshot_worker.py`) handle graceful shutdowns via `stop_event`.
- **Error Handling**:
    -   Rust `invoke` throws errors to JS. Handle them in `try/catch` blocks in React components.

## Implementation Status
- **Active**: Screenshot capture, OCR, Vector DB storage, Monitor Start/Stop.
- **Pending Integration**: The Frontend `Timeline` does not yet query the Python backend for real data.

## Additional Notes
- **Packages**: Python sub-services using paddlepaddle==3.2.0 and paddleocr==3.3.0 for OCR tasks, APIs of these packages are very likely to be different from those of version 2.x. This needs to be noted.
- **Rust Code**: When editing the .rs file, a tool is needed to check whether the written code has any issues.