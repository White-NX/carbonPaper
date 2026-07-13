#[path = "../ml_protocol.rs"]
mod ml_protocol;

use ml_protocol::{
    read_request, write_response, MlOcrBlock, MlOcrTimings, MlProvider, MlRequest, MlResponse,
    ML_PROTOCOL_VERSION,
};
use rapidocr_core::{
    config::{ExecutionProvider, InferenceOptions, PipelineConfig},
    model::{model_set_by_name, ModelCache, ModelDownloadMode},
    TokioOcrError, TokioRapidOcr,
};
use std::io::{self, BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MODEL_ID: &str = "ppocrv5-ch-mobile";

fn main() {
    if let Err(error) = run() {
        eprintln!("[ML] fatal: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("failed to create ML runtime: {error}"))?;

    let provider = if args.directml {
        MlProvider::DirectMl
    } else {
        MlProvider::Cpu
    };
    let model_set = model_set_by_name(MODEL_ID).ok_or("registered OCR model is missing")?;
    let cache = ModelCache::new(&args.model_dir);
    let pipeline = PipelineConfig::without_cls();
    cache
        .ensure_model_set_for_pipeline(model_set, pipeline, ModelDownloadMode::Never)
        .map_err(|error| format!("OCR model verification failed: {error:#}"))?;
    let config = cache
        .config_for(model_set)
        .with_pipeline(pipeline)
        .with_inference_options(InferenceOptions {
            intra_threads: args.threads,
            inter_threads: 1,
            parallel_execution: false,
            enable_cpu_mem_arena: false,
            execution_provider: if args.directml {
                ExecutionProvider::DirectMl
            } else {
                ExecutionProvider::Cpu
            },
        });

    eprintln!(
        "[ML] loading model={} provider={:?} intra_threads={} arena=false",
        MODEL_ID, provider, args.threads
    );
    let load_started = Instant::now();
    let engine = runtime
        .block_on(TokioRapidOcr::new(config))
        .map_err(|error| format!("failed to initialize OCR engine: {error}"))?;
    eprintln!(
        "[ML] model ready in {}ms",
        load_started.elapsed().as_millis()
    );

    if args.verify_models {
        println!(
            "{{\"status\":\"ok\",\"model_id\":\"{}\",\"provider\":\"{:?}\"}}",
            MODEL_ID, provider
        );
        drop(engine);
        return Ok(());
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    write_response(
        &mut writer,
        &MlResponse::Ready {
            protocol_version: ML_PROTOCOL_VERSION,
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            rapidocr_core_version: "0.2.1".to_string(),
            provider: provider.clone(),
            model_id: MODEL_ID.to_string(),
        },
    )?;

    loop {
        let (request, body) = match read_request(&mut reader) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[ML] request stream closed: {error}");
                break;
            }
        };
        match request {
            MlRequest::Ping { request_id } => {
                write_response(&mut writer, &MlResponse::Pong { request_id })?;
            }
            MlRequest::Shutdown { request_id } => {
                write_response(&mut writer, &MlResponse::ShuttingDown { request_id })?;
                break;
            }
            MlRequest::Ocr {
                request_id,
                timeout_ms,
                body_len,
            } => {
                let request_started = Instant::now();
                eprintln!(
                    "[ML] OCR start request_id={} bytes={} timeout_ms={} provider={:?}",
                    request_id, body_len, timeout_ms, provider
                );
                let decode_started = Instant::now();
                let image = match image::load_from_memory(&body) {
                    Ok(image) => Arc::new(image.to_rgb8()),
                    Err(error) => {
                        write_response(
                            &mut writer,
                            &MlResponse::Error {
                                request_id,
                                kind: "image_decode".to_string(),
                                message: error.to_string(),
                            },
                        )?;
                        continue;
                    }
                };
                let image_decode_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
                let model_started = Instant::now();
                let result = runtime.block_on(
                    engine.run_image_with_timeout(image, Duration::from_millis(timeout_ms.max(1))),
                );
                let model_total_ms = model_started.elapsed().as_secs_f64() * 1000.0;
                match result {
                    Ok(output) => {
                        let blocks = output
                            .output
                            .lines
                            .into_iter()
                            .map(|line| MlOcrBlock {
                                text: line.text,
                                confidence: line.score,
                                points: line.bbox.points,
                            })
                            .collect::<Vec<_>>();
                        eprintln!(
                            "[ML] OCR complete request_id={} blocks={} model_ms={:.1} total_ms={:.1}",
                            request_id,
                            blocks.len(),
                            model_total_ms,
                            request_started.elapsed().as_secs_f64() * 1000.0
                        );
                        write_response(
                            &mut writer,
                            &MlResponse::OcrComplete {
                                request_id,
                                blocks,
                                timings: MlOcrTimings {
                                    image_decode_ms,
                                    model_total_ms,
                                    request_total_ms: request_started.elapsed().as_secs_f64()
                                        * 1000.0,
                                },
                            },
                        )?;
                    }
                    Err(error) => {
                        let kind = match error {
                            TokioOcrError::TimedOut(_) => "timeout",
                            TokioOcrError::Cancelled => "cancelled",
                            TokioOcrError::QueueFull => "queue_full",
                            TokioOcrError::WorkerPanicked => "worker_panic",
                            TokioOcrError::WorkerStopped => "worker_stopped",
                            TokioOcrError::Ocr(_) => "ocr",
                        };
                        eprintln!(
                            "[ML] OCR failed request_id={} kind={} error={}",
                            request_id, kind, error
                        );
                        write_response(
                            &mut writer,
                            &MlResponse::Error {
                                request_id,
                                kind: kind.to_string(),
                                message: error.to_string(),
                            },
                        )?;
                    }
                }
            }
        }
    }

    runtime
        .block_on(engine.shutdown())
        .map_err(|error| format!("failed to shut down OCR engine: {error}"))?;
    eprintln!("[ML] worker stopped");
    Ok(())
}

struct Args {
    model_dir: PathBuf,
    threads: usize,
    directml: bool,
    verify_models: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut model_dir = None;
        let mut threads = 2usize;
        let mut directml = false;
        let mut verify_models = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--model-dir" => {
                    model_dir = Some(PathBuf::from(
                        args.next().ok_or("--model-dir requires a path")?,
                    ));
                }
                "--threads" => {
                    threads = args
                        .next()
                        .ok_or("--threads requires a value")?
                        .parse::<usize>()
                        .map_err(|_| "invalid --threads value")?
                        .clamp(1, 4);
                }
                "--directml" => directml = true,
                "--verify-models" => verify_models = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            model_dir: model_dir.ok_or("--model-dir is required")?,
            threads,
            directml,
            verify_models,
        })
    }
}
