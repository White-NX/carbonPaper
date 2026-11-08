//! Lifecycle of the protected, text-only listener. The existing MCP lifecycle
//! lock serializes both listener owners; no interactive token is upgraded.
use crate::{mcp_server::McpRuntimeState, storage::StorageState};
use carbonpaper_agent_access::transport::ReaderConfiguration;
use carbonpaper_app_bound::{manifest, windows::identity};
use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

static STARTED: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(false);

pub(crate) fn available() -> bool {
    identity::active_runtime()
        .ok()
        .flatten()
        .is_some_and(|dir| {
            manifest::read_manifest(&dir)
                .ok()
                .is_some_and(|(m, _)| m.files.contains_key("carbonpaper-mcp-reader.exe"))
        })
}
pub(crate) fn spawn(app: tauri::AppHandle) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    storage.start_agent_exporter();
    tauri::async_runtime::spawn(async move {
        while !STOPPED.load(Ordering::Acquire) {
            if storage.agent_access_enabled() && storage.agent_preparation().phase == "ready" {
                let state = app.state::<McpRuntimeState>();
                let _guard = state.lock_lifecycle().await;
                if !state.is_running() || state.mode() != "unattended" {
                    if storage
                        .load_policy()
                        .ok()
                        .and_then(|p| p["mcp_enabled"].as_bool())
                        .unwrap_or(false)
                    {
                        if let Err(error) = start_reader(app.clone(), &storage, &state).await {
                            state.set_last_error(error.clone());
                            let _=app.emit("mcp-status-changed",serde_json::json!({"state":"error","mode":"unattended","error":error}));
                        }
                    }
                }
            } else if !storage.agent_access_enabled() {
                let state = app.state::<McpRuntimeState>();
                let _guard = state.lock_lifecycle().await;
                if state.mode() == "unattended" {
                    crate::mcp_server::stop_server(&state).await;
                    let credentials =
                        app.state::<Arc<crate::credential_manager::CredentialManagerState>>();
                    if credentials.is_session_valid()
                        && storage
                            .load_policy()
                            .ok()
                            .and_then(|p| p["mcp_enabled"].as_bool())
                            .unwrap_or(false)
                    {
                        let _ = crate::mcp_server::auto_start_under_lifecycle(
                            app.clone(),
                            &credentials,
                            &storage,
                            &state,
                        )
                        .await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}
pub(crate) fn stop() {
    STOPPED.store(true, Ordering::Release);
}

struct ReaderJob(windows::Win32::Foundation::HANDLE);
// SAFETY: this uniquely owned kernel handle is only closed on drop; Windows
// allows job handles to move across threads and no borrowed pointers are kept.
unsafe impl Send for ReaderJob {}
impl Drop for ReaderJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
fn assign_job(child: &tokio::process::Child) -> Result<ReaderJob, String> {
    use windows::Win32::System::JobObjects::*;
    // SAFETY: initialized, correctly sized job information is copied by the
    // OS. The child process handle remains owned and live through assignment.
    unsafe {
        let job = ReaderJob(CreateJobObjectW(None, None).map_err(|e| e.to_string())?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&info) as u32,
        )
        .map_err(|e| e.to_string())?;
        AssignProcessToJobObject(
            job.0,
            windows::Win32::Foundation::HANDLE(child.raw_handle().ok_or("AGENT_READER_EXITED")?),
        )
        .map_err(|e| e.to_string())?;
        Ok(job)
    }
}

pub(crate) async fn start_reader(
    app: tauri::AppHandle,
    storage: &StorageState,
    state: &McpRuntimeState,
) -> Result<(), String> {
    if storage.agent_preparation().phase != "ready" {
        return Err("AGENT_SYNC_INCOMPLETE".into());
    }
    let binding = storage.agent_binding().ok_or("AGENT_ACCESS_DISABLED")?;
    let root = identity::active_runtime()
        .map_err(|e| e.to_string())?
        .ok_or("AGENT_PROTECTED_RUNTIME_REQUIRED")?;
    let (manifest, _) = manifest::read_manifest(&root).map_err(|e| e.to_string())?;
    let executable = root.join("carbonpaper-mcp-reader.exe");
    identity::assert_protected(&executable).map_err(|e| e.to_string())?;
    let mut file = identity::locked_file(&executable).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut file,
        manifest
            .files
            .get("carbonpaper-mcp-reader.exe")
            .ok_or("AGENT_PROTECTED_RUNTIME_REQUIRED")?,
    )
    .map_err(|e| e.to_string())?;
    let appdata =
        crate::resource_utils::file_in_local_appdata().ok_or("AGENT_MODELS_UNAVAILABLE")?;
    let port = crate::mcp_server::port_from_policy(&storage.load_policy()?);
    let configuration = ReaderConfiguration {
        generation: binding.generation,
        port,
        models_root: appdata.join("models"),
        onnx_models_root: appdata.join("models-onnx"),
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    crate::mcp_server::stop_server(state).await;
    let mut command = tokio::process::Command::new(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|e| format!("AGENT_READER_START_FAILED: {e}"))?;
    let job = assign_job(&child)?;
    let mut input = child.stdin.take().ok_or("AGENT_READER_START_FAILED")?;
    input
        .write_all(&serde_json::to_vec(&configuration).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;
    input.shutdown().await.map_err(|e| e.to_string())?;
    drop(input);
    let mut output =
        tokio::io::BufReader::new(child.stdout.take().ok_or("AGENT_READER_START_FAILED")?);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        output.read_line(&mut line),
    )
    .await
    .map_err(|_| "AGENT_READER_START_TIMEOUT")?
    .map_err(|e| e.to_string())?;
    let ready: serde_json::Value =
        serde_json::from_str(&line).map_err(|_| "AGENT_READER_START_FAILED")?;
    if ready["state"] != "ready" || ready["port"].as_u64() != Some(u64::from(port)) {
        return Err("AGENT_READER_START_FAILED".into());
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let listener_app = app.clone();
    let handle = tokio::spawn(async move {
        let _job = job;
        let _image_lock = file;
        let _output = output;
        tokio::select! {
            _=shutdown_rx=>{let _=child.kill().await;let _=child.wait().await;}
            _=child.wait()=>{
                let _=listener_app.emit("mcp-status-changed",serde_json::json!({"state":"stopped","mode":"unattended"}));
            }
        }
    });
    state.install_reader(handle, shutdown_tx, port);
    let _=app.emit("mcp-status-changed",serde_json::json!({"state":"running","mode":"unattended","active_port":port,"runtime_generation":state.generation()}));
    Ok(())
}
