//! Isolated Rust semantic ONNX worker.

#[allow(dead_code)]
#[path = "../../src/ml_protocol.rs"]
mod ml_protocol;
#[path = "../../src/semantic_engine.rs"]
mod semantic_engine;
#[allow(dead_code)]
#[path = "../../src/semantic_models.rs"]
mod semantic_models;

use ml_protocol::{
    read_request, write_response, MlProvider, MlRequest, MlResponse, MlSemanticTimings,
    ML_PROTOCOL_VERSION,
};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let result = std::thread::Builder::new()
        .name("carbonpaper-semantic-main".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(run)
        .map_err(|error| format!("failed to start semantic worker thread: {error}"))
        .and_then(|thread| {
            thread
                .join()
                .map_err(|_| "semantic worker thread panicked".to_string())?
        });
    if let Err(error) = result {
        eprintln!("[ML:SEMANTIC] fatal: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    let committed = ort::init_from(&args.ort_dylib)
        .map_err(|error| {
            format!(
                "provider_unavailable: failed to open ONNX Runtime {}: {error}",
                args.ort_dylib.display()
            )
        })?
        .with_name("carbonpaper-semantic")
        .commit();
    if !committed {
        return Err("provider_unavailable: ONNX Runtime was already initialized".to_string());
    }
    let provider = if args.directml {
        MlProvider::DirectMl
    } else {
        MlProvider::Cpu
    };
    let mut engine = semantic_engine::SemanticEngine::new(
        provider,
        args.dml_device_id,
        args.models_root,
        args.onnx_models_root,
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    write_response(
        &mut writer,
        &MlResponse::SemanticReady {
            protocol_version: ML_PROTOCOL_VERSION,
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            ort_version: "1.24.2".to_string(),
            provider,
            supported_models: engine.supported_models(),
        },
    )?;
    eprintln!(
        "[ML:SEMANTIC] worker ready provider={provider:?} runtime={} dml_device_id={}",
        args.ort_dylib.display(),
        args.dml_device_id
    );

    loop {
        let (request, body) = match read_request(&mut reader) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[ML:SEMANTIC] request stream closed: {error}");
                break;
            }
        };
        let request_id = request.request_id();
        let request_started = Instant::now();
        match request {
            MlRequest::Ping { request_id } => {
                write_response(&mut writer, &MlResponse::Pong { request_id })?;
            }
            MlRequest::Shutdown { request_id } => {
                write_response(&mut writer, &MlResponse::ShuttingDown { request_id })?;
                break;
            }
            MlRequest::SemanticStatus { request_id } => {
                let loaded = engine.loaded_model();
                write_response(
                    &mut writer,
                    &MlResponse::SemanticStatus {
                        request_id,
                        provider: engine.provider(),
                        loaded_model: loaded.map(|model| model.descriptor.model),
                        model_id: loaded.map(|model| model.descriptor.model_id.to_string()),
                        model_revision: loaded.map(|model| model.descriptor.revision.to_string()),
                        model_fingerprint: loaded.map(|model| model.model_fingerprint.clone()),
                    },
                )?;
            }
            MlRequest::Unload { request_id } => {
                engine.unload();
                write_response(&mut writer, &MlResponse::Unloaded { request_id })?;
            }
            MlRequest::InspectTokenization {
                request_id,
                model,
                texts,
                text_pairs,
            } => match engine.inspect_tokenization(model, &texts, text_pairs.as_deref()) {
                Ok(tokens) => write_response(
                    &mut writer,
                    &MlResponse::TokenizationComplete {
                        request_id,
                        model,
                        batch: tokens.batch,
                        sequence: tokens.sequence,
                        input_ids: tokens.input_ids,
                        attention_mask: tokens.attention_mask,
                        token_type_ids: tokens.token_type_ids,
                    },
                )?,
                Err(error) => write_semantic_error(&mut writer, request_id, error)?,
            },
            MlRequest::EmbedText {
                request_id,
                model,
                texts,
                ..
            } => match engine.embed_text(model, &texts) {
                Ok(result) => {
                    let response_timings = timings(&result, request_started);
                    write_response(
                        &mut writer,
                        &MlResponse::EmbeddingComplete {
                            request_id,
                            model,
                            dimensions: result.value.first().map(Vec::len).unwrap_or(0),
                            vectors: result.value,
                            timings: response_timings,
                        },
                    )?
                }
                Err(error) => write_semantic_error(&mut writer, request_id, error)?,
            },
            MlRequest::EmbedImage {
                request_id,
                model,
                images,
                ..
            } => match engine.embed_image(model, &images, &body) {
                Ok(result) => {
                    let response_timings = timings(&result, request_started);
                    write_response(
                        &mut writer,
                        &MlResponse::EmbeddingComplete {
                            request_id,
                            model,
                            dimensions: result.value.first().map(Vec::len).unwrap_or(0),
                            vectors: result.value,
                            timings: response_timings,
                        },
                    )?
                }
                Err(error) => write_semantic_error(&mut writer, request_id, error)?,
            },
            MlRequest::Rerank {
                request_id,
                model,
                query,
                documents,
                ..
            } => match engine.rerank(model, &query, &documents) {
                Ok(result) => {
                    let response_timings = timings(&result, request_started);
                    write_response(
                        &mut writer,
                        &MlResponse::RerankComplete {
                            request_id,
                            model,
                            scores: result.value,
                            timings: response_timings,
                        },
                    )?
                }
                Err(error) => write_semantic_error(&mut writer, request_id, error)?,
            },
            MlRequest::Ocr { .. } => write_semantic_error(
                &mut writer,
                request_id,
                "invalid_request: OCR request was sent to the semantic worker".to_string(),
            )?,
        }
    }
    engine.unload();
    eprintln!("[ML:SEMANTIC] worker stopped");
    Ok(())
}

fn timings<T>(
    result: &semantic_engine::SemanticInference<T>,
    request_started: Instant,
) -> MlSemanticTimings {
    MlSemanticTimings {
        model_load_ms: result.model_load_ms,
        preprocess_ms: result.preprocess_ms,
        inference_ms: result.inference_ms,
        request_total_ms: request_started.elapsed().as_secs_f64() * 1000.0,
    }
}

fn write_semantic_error<W: Write>(
    writer: &mut W,
    request_id: u64,
    error: String,
) -> Result<(), String> {
    let (kind, message) = error
        .split_once(": ")
        .map(|(kind, message)| (kind.to_string(), message.to_string()))
        .unwrap_or_else(|| ("inference".to_string(), error));
    eprintln!("[ML:SEMANTIC] request failed request_id={request_id} kind={kind} error={message}");
    write_response(
        writer,
        &MlResponse::Error {
            request_id,
            kind,
            message,
        },
    )
}

struct Args {
    models_root: PathBuf,
    onnx_models_root: PathBuf,
    ort_dylib: PathBuf,
    directml: bool,
    dml_device_id: i32,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut models_root = None;
        let mut onnx_models_root = None;
        let mut ort_dylib = None;
        let mut directml = false;
        let mut dml_device_id = 0i32;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--models-root" => {
                    models_root = Some(PathBuf::from(
                        args.next().ok_or("--models-root requires a path")?,
                    ));
                }
                "--onnx-models-root" => {
                    onnx_models_root = Some(PathBuf::from(
                        args.next().ok_or("--onnx-models-root requires a path")?,
                    ));
                }
                "--ort-dylib" => {
                    ort_dylib = Some(PathBuf::from(
                        args.next().ok_or("--ort-dylib requires a path")?,
                    ));
                }
                "--directml" => directml = true,
                "--dml-device-id" => {
                    dml_device_id = args
                        .next()
                        .ok_or("--dml-device-id requires a value")?
                        .parse::<i32>()
                        .map_err(|_| "invalid --dml-device-id value")?;
                    if dml_device_id < 0 {
                        return Err("--dml-device-id must be non-negative".to_string());
                    }
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            models_root: models_root.ok_or("--models-root is required")?,
            onnx_models_root: onnx_models_root.ok_or("--onnx-models-root is required")?,
            ort_dylib: ort_dylib.ok_or("--ort-dylib is required")?,
            directml,
            dml_device_id,
        })
    }
}
