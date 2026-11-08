//! Query text only. Requests can never select EmbedImage or an input file.
use crate::ml_protocol::{
    MlRequest, MlResponse, MlSemanticModel, MAX_ML_HEADER_BYTES, ML_PROTOCOL_VERSION,
};
use std::{path::PathBuf, process::Stdio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
pub struct QueryModels {
    pub runtime_root: PathBuf,
    pub models_root: PathBuf,
    pub onnx_models_root: PathBuf,
    pub available: bool,
}
impl QueryModels {
    pub async fn embed_clip(&self, text: String) -> Result<(String, String, Vec<f32>), String> {
        self.embed(MlSemanticModel::ChineseClip, text).await
    }
    pub async fn embed_minilm(&self, text: String) -> Result<(String, String, Vec<f32>), String> {
        let (id, _, vector) = self.embed(MlSemanticModel::MinilmL12, text).await?;
        Ok((
            id,
            crate::semantic_models::MINILM_VECTOR_SPACE_REVISION.into(),
            vector,
        ))
    }
    async fn embed(
        &self,
        model: MlSemanticModel,
        text: String,
    ) -> Result<(String, String, Vec<f32>), String> {
        if !self.available {
            return Err("AGENT_INDEX_UNAVAILABLE: Query runtime is unavailable".into());
        }
        let mut command =
            tokio::process::Command::new(self.runtime_root.join("carbonpaper-semantic-worker.exe"));
        command
            .args(["--models-root"])
            .arg(&self.models_root)
            .arg("--onnx-models-root")
            .arg(&self.onnx_models_root)
            .arg("--ort-dylib")
            .arg(self.runtime_root.join("onnxruntime/1.24.2/onnxruntime.dll"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|_| "AGENT_INDEX_UNAVAILABLE: Query runtime is unavailable")?;
        let mut input = child.stdin.take().ok_or("AGENT_INDEX_UNAVAILABLE")?;
        let mut output = child.stdout.take().ok_or("AGENT_INDEX_UNAVAILABLE")?;
        let work = async {
            match read(&mut output).await? {
                MlResponse::SemanticReady {
                    protocol_version, ..
                } if protocol_version == ML_PROTOCOL_VERSION => {}
                _ => return Err("AGENT_INDEX_UNAVAILABLE: Query runtime version mismatch".into()),
            }
            let request = MlRequest::EmbedText {
                request_id: 1,
                timeout_ms: 60000,
                model,
                texts: vec![text],
            };
            let mut bytes = Vec::new();
            crate::ml_protocol::write_request(&mut bytes, &request, &[])
                .map_err(|_| "AGENT_INDEX_UNAVAILABLE")?;
            input
                .write_all(&bytes)
                .await
                .map_err(|_| "AGENT_INDEX_UNAVAILABLE")?;
            match read(&mut output).await? {
                MlResponse::EmbeddingComplete {
                    model: returned_model,
                    mut vectors,
                    ..
                } if returned_model == model && vectors.len() == 1 => {
                    let descriptor = crate::semantic_models::descriptor(model);
                    Ok((
                        descriptor.model_id.into(),
                        descriptor.revision.into(),
                        vectors.remove(0),
                    ))
                }
                _ => Err("AGENT_INDEX_UNAVAILABLE: Query model is unavailable".into()),
            }
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(75), work)
            .await
            .map_err(|_| "AGENT_INDEX_UNAVAILABLE: Query timed out")?;
        let _ = child.kill().await;
        let _ = child.wait().await;
        result
    }
}
async fn read(output: &mut tokio::process::ChildStdout) -> Result<MlResponse, String> {
    let len = output
        .read_u32_le()
        .await
        .map_err(|_| "AGENT_INDEX_UNAVAILABLE")? as usize;
    if len == 0 || len > MAX_ML_HEADER_BYTES {
        return Err("AGENT_INDEX_UNAVAILABLE".into());
    }
    let mut bytes = vec![0; len];
    output
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "AGENT_INDEX_UNAVAILABLE")?;
    serde_json::from_slice(&bytes).map_err(|_| "AGENT_INDEX_UNAVAILABLE".into())
}
