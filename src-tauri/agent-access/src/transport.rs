use crate::{
    dispatcher, embeddings::QueryModels, mcp_contract, repository::AgentRepository,
    sensitive_filter::SensitiveFilterState,
};
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use carbonpaper_app_bound::protocol::{
    AgentRequestContext, AgentViewBinding, BrokerError, Request as BrokerRequest,
    Response as BrokerResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderConfiguration {
    pub generation: String,
    pub port: u16,
    pub models_root: PathBuf,
    pub onnx_models_root: PathBuf,
    pub app_version: String,
}

struct ReaderState {
    binding: AgentViewBinding,
    key: Zeroizing<Vec<u8>>,
    filters: SensitiveFilterState,
    models: QueryModels,
    reads: Arc<tokio::sync::Semaphore>,
    _runtime_locks: Vec<std::fs::File>,
    app_version: String,
}
async fn broker(request: BrokerRequest) -> Result<BrokerResponse, BrokerError> {
    tokio::task::spawn_blocking(move || carbonpaper_app_bound::windows::transport::call(request))
        .await
        .map_err(|_| BrokerError::Unavailable)?
}

async fn authorize(
    State(_state): State<Arc<ReaderState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(secret) = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| s.len() <= 256)
    else {
        return (StatusCode::UNAUTHORIZED, "Bearer credential required").into_response();
    };
    let token_hash = hex::encode(Sha256::digest(secret.as_bytes()));
    match broker(BrokerRequest::AgentAuthorize{token_hash}).await {
        Ok(BrokerResponse::AgentContext(context))=>{request.extensions_mut().insert(context);next.run(request).await},
        Err(BrokerError::Busy)=>(StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":{"code":"AGENT_SYNC_INCOMPLETE","message":"CarbonPaper text preparation or synchronization is incomplete"}}))).into_response(),
        Err(BrokerError::Unavailable)=>(StatusCode::SERVICE_UNAVAILABLE,"Agent authorization service unavailable").into_response(),
        _=>(StatusCode::UNAUTHORIZED,"Credential is invalid, expired or revoked").into_response(),
    }
}

#[derive(Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}
fn rpc(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn rpc_error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
async fn handle(
    State(state): State<Arc<ReaderState>>,
    Extension(context): Extension<AgentRequestContext>,
    Json(request): Json<RpcRequest>,
) -> Response {
    let id = request.id;
    if request.jsonrpc != "2.0" {
        return Json(rpc_error(id, -32600, "Invalid JSON-RPC version")).into_response();
    }
    let result = match request.method.as_str() {
        "initialize" => rpc(
            id.clone(),
            json!({"protocolVersion":mcp_contract::MCP_PROTOCOL_VERSION,"capabilities":{"tools":{}},
            "serverInfo":{"name":"CarbonPaper","version":state.app_version},
            "instructions":"Unattended, read-only text access. Tool schema v3 uses screenshot_id, captured_at_ms and half-open Unix millisecond time ranges. Images, files and write tools are unavailable."}),
        ),
        "ping" | "notifications/initialized" => rpc(id.clone(), json!({})),
        "tools/list" => rpc(id.clone(), dispatcher::tools_list()),
        "tools/call" => {
            let params = request.params.unwrap_or(Value::Null);
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let outcome = call(state.clone(), name, args).await;
            let failed = outcome.is_err();
            let output = crate::page::fit(
                outcome.unwrap_or_else(|e| mcp_contract::error_payload(&e)),
                100_000,
            );
            rpc(
                id.clone(),
                json!({"content":[{"type":"text","text":output.to_string()}],"isError":failed}),
            )
        }
        _ => rpc_error(id.clone(), -32601, "Method not found"),
    };
    // No serialized history leaves this process after a grant, dataset, source
    // or privacy revision changes, including an already admitted request.
    match broker(BrokerRequest::AgentCheck { context }).await {
        Ok(BrokerResponse::Ok) => Json(result).into_response(),
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(rpc_error(
                id,
                -32001,
                "Credential or text view changed during request",
            )),
        )
            .into_response(),
    }
}
async fn call(state: Arc<ReaderState>, name: String, args: Value) -> Result<Value, String> {
    if matches!(
        name.as_str(),
        "upsert_smart_cluster_summary" | "delete_smart_cluster_summary"
    ) {
        return Err("READ_ONLY_GRANT: Unattended credentials authorize text reads only".into());
    }
    mcp_contract::validate_arguments(&name, &args)?;
    let _permit = state
        .reads
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "AGENT_UNAVAILABLE")?;
    let query = if name == "search_nl" {
        Some(
            state
                .models
                .embed_clip(args["query"].as_str().unwrap().to_string())
                .await?,
        )
    } else if name == "search_recaps" && args["mode"].as_str() != Some("keyword") {
        state
            .models
            .embed_minilm(args["query"].as_str().unwrap().to_string())
            .await
            .ok()
    } else {
        None
    };
    tokio::task::spawn_blocking(move || {
        let repo = AgentRepository::open(
            std::path::Path::new(&state.binding.view_path),
            &state.key,
            &state.binding,
            true,
        )?;
        let config = repo
            .get("privacy", "settings")?
            .ok_or("AGENT_PRIVACY_FILTER_UNAVAILABLE")?;
        state.filters.update_config(config);
        let vector = query
            .as_ref()
            .map(|(id, rev, v)| (id.as_str(), rev.as_str(), v.as_slice()));
        dispatcher::call(&repo, &state.filters, &name, args, vector)
    })
    .await
    .map_err(|_| "AGENT_READER_FAILED".to_string())?
}

pub async fn serve(configuration: ReaderConfiguration) -> Result<(), String> {
    if configuration.port == 0 {
        return Err("AGENT_INVALID_PORT".into());
    }
    let identity = carbonpaper_app_bound::windows::identity::inspect_process(std::process::id())
        .map_err(|e| e.to_string())?;
    let verified = carbonpaper_app_bound::windows::identity::verify_main(identity)
        .map_err(|e| e.to_string())?;
    if verified.principal.role != carbonpaper_app_bound::protocol::RuntimeRole::McpReader {
        return Err("AGENT_READER_IDENTITY_DENIED".into());
    }
    let runtime_root = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("AGENT_RUNTIME_MISSING")?
        .to_path_buf();
    let (manifest, _) =
        carbonpaper_app_bound::manifest::read_manifest(&runtime_root).map_err(|e| e.to_string())?;
    // Pin all executable, model runtime and dictionary bytes before accepting
    // traffic. User-downloadable model weights remain data, never executable code.
    let mut runtime_locks = Vec::new();
    for name in [
        "carbonpaper-semantic-worker.exe",
        "onnxruntime/1.24.2/onnxruntime.dll",
        "onnxruntime/1.24.2/onnxruntime_providers_shared.dll",
        "onnxruntime/1.24.2/DirectML.dll",
        "compliance_process/dicts/dict_01.dict.enc",
        "compliance_process/dicts/dict_02.dict.enc",
        "compliance_process/dicts/dict_03.dict.enc",
        "compliance_process/dicts/dict_04.dict.enc",
        "compliance_process/dicts/dict_05.dict.enc",
    ] {
        if let Some(expected) = manifest.files.get(name) {
            let mut file =
                carbonpaper_app_bound::windows::identity::locked_file(&runtime_root.join(name))
                    .map_err(|e| e.to_string())?;
            carbonpaper_app_bound::windows::identity::verify_file(&mut file, expected)
                .map_err(|e| e.to_string())?;
            runtime_locks.push(file);
        } else if name.contains("dict_") {
            return Err("AGENT_PRIVACY_FILTER_UNAVAILABLE".into());
        }
    }
    let BrokerResponse::AgentKeys(keys) = broker(BrokerRequest::AgentReaderKeys {
        generation: configuration.generation,
    })
    .await
    .map_err(|e| e.to_string())?
    else {
        return Err("AGENT_KEYS_UNAVAILABLE".into());
    };
    let key = Zeroizing::new(keys.content_key.0.clone());
    let binding = keys.binding;
    AgentRepository::open(
        std::path::Path::new(&binding.view_path),
        &key,
        &binding,
        true,
    )?;
    let filters = SensitiveFilterState::default();
    filters.load_dicts(&runtime_root)?;
    // The desktop synchronizes privacy configuration as encrypted typed data.
    let repo = AgentRepository::open(
        std::path::Path::new(&binding.view_path),
        &key,
        &binding,
        true,
    )?;
    if let Some(config) = repo.get("privacy", "settings")? {
        filters.update_config(config);
    } else {
        return Err("AGENT_PRIVACY_FILTER_UNAVAILABLE".into());
    }
    drop(repo);
    let state = Arc::new(ReaderState {
        binding,
        key,
        filters,
        models: QueryModels {
            runtime_root,
            models_root: configuration.models_root,
            onnx_models_root: configuration.onnx_models_root,
            available: manifest
                .files
                .contains_key("carbonpaper-semantic-worker.exe")
                && manifest
                    .files
                    .contains_key("onnxruntime/1.24.2/onnxruntime.dll"),
        },
        reads: Arc::new(tokio::sync::Semaphore::new(4)),
        _runtime_locks: runtime_locks,
        app_version: configuration.app_version,
    });
    let router = Router::new()
        .route("/mcp", post(handle))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state);
    let socket = tokio::net::TcpSocket::new_v4().map_err(|e| e.to_string())?;
    socket
        .bind(([127, 0, 0, 1], configuration.port).into())
        .map_err(|e| format!("AGENT_PORT_UNAVAILABLE: {e}"))?;
    let listener = socket.listen(128).map_err(|e| e.to_string())?;
    println!("{}", json!({"state":"ready","port":configuration.port}));
    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}
