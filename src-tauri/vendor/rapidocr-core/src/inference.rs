use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ndarray::{Array4, ArrayD};
use ort::{
    environment::Environment, session::builder::GraphOptimizationLevel, session::Session,
    value::TensorRef,
};

#[cfg(feature = "directml")]
use crate::config::ExecutionProvider;
use crate::{cancellation::OcrCancellationToken, config::InferenceOptions};

pub(crate) struct OnnxSession {
    session: Session,
}

impl OnnxSession {
    pub(crate) fn new(
        model_path: impl AsRef<Path>,
        options: InferenceOptions,
        dynamic_input_shape: bool,
        environment: &Environment,
    ) -> Result<Self> {
        let model_path = model_path.as_ref();
        if !model_path.exists() {
            bail!(
                "ONNX model file not found at {}; check the config model path or prepare the model cache",
                model_path.display()
            );
        }
        if !model_path.is_file() {
            bail!("ONNX model path is not a file: {}", model_path.display());
        }
        options.validate()?;
        let mut session_builder = Session::builder(environment)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_intra_threads(options.intra_threads)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_inter_threads(options.inter_threads)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_parallel_execution(options.parallel_execution)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_memory_pattern(
                !dynamic_input_shape
                    && options.execution_provider != crate::config::ExecutionProvider::DirectMl,
            )
            .map_err(|e| anyhow!(e.to_string()))?;

        #[cfg(feature = "directml")]
        if options.execution_provider == ExecutionProvider::DirectMl {
            session_builder = session_builder
                .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
                .map_err(|e| anyhow!(e.to_string()))?;
        }

        if options.execution_provider == crate::config::ExecutionProvider::Cpu {
            session_builder = session_builder
                .with_execution_providers([ort::ep::CPU::default()
                    .with_arena_allocator(options.enable_cpu_mem_arena)
                    .build()])
                .map_err(|e| anyhow!(e.to_string()))?;
        }

        let session = session_builder
            .commit_from_file(model_path)
            .map_err(|e| anyhow!(e.to_string()))
            .with_context(|| format!("failed to load ONNX model {}", model_path.display()))?;
        Ok(Self { session })
    }

    pub(crate) fn run_f32(
        &mut self,
        input: &Array4<f32>,
        cancellation: &OcrCancellationToken,
    ) -> Result<ArrayD<f32>> {
        cancellation.checkpoint()?;
        let run_options = cancellation.begin_onnx_run()?;
        let output = {
            let outputs = self
                .session
                .run_with_options(
                    ort::inputs![TensorRef::from_array_view(input)?],
                    run_options.options(),
                )
                .map_err(|e| {
                    if cancellation.is_cancelled() {
                        crate::cancellation::OcrCancelled.into()
                    } else {
                        anyhow!(e.to_string())
                    }
                })?;
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| anyhow!(e.to_string()))?
                .to_owned()
        };
        drop(run_options);
        cancellation.checkpoint()?;
        Ok(output)
    }
}
