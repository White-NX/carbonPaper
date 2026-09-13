//! Optional local-model check for the isolated development service probe.
//! Set CARBONPAPER_SMART_CLUSTER_SMOKE_ROOT to the checkout and
//! CARBONPAPER_SMART_CLUSTER_SMOKE_MODELS to the installed models directory.
//! Only the probe's fixed synthetic input is ever sent to the local worker.
use super::ml_protocol::{self, MlRequest, MlResponse, MlSemanticModel};
use carbonpaper_app_bound::protocol::Consumer;
use std::{
    io::BufReader,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub struct SemanticSmoke {
    child: Child,
    input: ChildStdin,
    responses: Receiver<Result<MlResponse, String>>,
    vectors: Option<Vec<Vec<f32>>>,
    scored: bool,
}

impl SemanticSmoke {
    pub fn from_environment() -> Result<Option<Self>, String> {
        let Some(root) = std::env::var_os("CARBONPAPER_SMART_CLUSTER_SMOKE_ROOT") else {
            return Ok(None);
        };
        let root = PathBuf::from(root);
        let models = std::env::var_os("CARBONPAPER_SMART_CLUSTER_SMOKE_MODELS")
            .ok_or("Set CARBONPAPER_SMART_CLUSTER_SMOKE_MODELS to installed local models")?;
        let mut child =
            Command::new(root.join("src-tauri/pre-bundle/carbonpaper-semantic-worker.exe"))
                .arg("--models-root")
                .arg(&models)
                .arg("--onnx-models-root")
                .arg(&models)
                .arg("--ort-dylib")
                .arg(root.join("src-tauri/pre-bundle/onnxruntime/1.24.2/onnxruntime.dll"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|e| e.to_string())?;
        let input = child.stdin.take().ok_or("Worker stdin missing")?;
        let output = child.stdout.take().ok_or("Worker stdout missing")?;
        let (send, responses) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let response = ml_protocol::read_response(&mut reader);
                let failed = response.is_err();
                if send.send(response).is_err() || failed {
                    break;
                }
            }
        });
        let smoke = Self {
            child,
            input,
            responses,
            vectors: None,
            scored: false,
        };
        match smoke.receive()? {
            MlResponse::SemanticReady {
                protocol_version, ..
            } if protocol_version == ml_protocol::ML_PROTOCOL_VERSION => Ok(Some(smoke)),
            _ => Err("Unexpected semantic worker handshake".into()),
        }
    }

    fn receive(&self) -> Result<MlResponse, String> {
        let response = self
            .responses
            .recv_timeout(Duration::from_secs(300))
            .map_err(|e| format!("Semantic smoke response timed out or closed: {e}"))??;
        if let MlResponse::Error { kind, message, .. } = response {
            return Err(format!("Semantic smoke failed: {kind}: {message}"));
        }
        Ok(response)
    }

    fn request(&mut self, request: MlRequest) -> Result<MlResponse, String> {
        ml_protocol::write_request(&mut self.input, &request, &[])?;
        self.receive()
    }

    pub fn consume(&mut self, consumer: Consumer, input: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(input)
            .map_err(|e| e.to_string())?
            .to_string();
        match consumer {
            Consumer::MiniLm => {
                let response = self.request(MlRequest::EmbedText {
                    request_id: 1,
                    timeout_ms: 300_000,
                    model: MlSemanticModel::MinilmL12,
                    texts: vec![text.clone(), text],
                })?;
                let MlResponse::EmbeddingComplete {
                    dimensions: 384,
                    vectors,
                    ..
                } = response
                else {
                    return Err("Expected MiniLM vectors".into());
                };
                if vectors.len() != 2
                    || vectors
                        .iter()
                        .any(|v| v.len() != 384 || v.iter().any(|x| !x.is_finite()))
                {
                    return Err("Invalid MiniLM vector batch".into());
                }
                self.vectors = Some(vectors);
            }
            Consumer::SmartCluster => {
                // MiniLM has finished its broker consumer already. Reuse its
                // vectors and the text obtained through this new independent lease.
                let vectors = self
                    .vectors
                    .take()
                    .ok_or("MiniLM did not run before SmartCluster")?;
                let cosine: f32 = vectors[0].iter().zip(&vectors[1]).map(|(a, b)| a * b).sum();
                if cosine < 0.40 {
                    return Err("Synthetic pair failed the MiniLM prefilter".into());
                }
                let response = self.request(MlRequest::Rerank {
                    request_id: 2,
                    timeout_ms: 300_000,
                    model: MlSemanticModel::BgeRerankerV2M3,
                    query: text.clone(),
                    documents: vec![text],
                })?;
                let MlResponse::RerankComplete { scores, .. } = response else {
                    return Err("Expected rerank scores".into());
                };
                if scores.len() != 1 || !scores[0].is_finite() {
                    return Err("Invalid rerank score batch".into());
                }
                self.scored = true;
                println!("[app-bound dev] Synthetic staged MiniLM + SmartCluster passed: cosine={cosine:.4}, score={:.4}", scores[0]);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), String> {
        if !self.scored {
            return Err("SmartCluster inference did not run".into());
        }
        match self.request(MlRequest::Shutdown { request_id: 3 })? {
            MlResponse::ShuttingDown { request_id: 3 } => Ok(()),
            _ => Err("Unexpected semantic shutdown response".into()),
        }
    }
}

impl Drop for SemanticSmoke {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
