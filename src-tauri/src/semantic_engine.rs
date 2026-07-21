//! Batch-capable ONNX semantic inference used only by the isolated ML worker.

use crate::ml_protocol::{MlImageInput, MlProvider, MlSemanticModel};
use crate::semantic_models::{
    resolve_semantic_model, ResolvedSemanticModel, SemanticPooling, SUPPORTED_SEMANTIC_MODELS,
};
use image::RgbImage;
use ort::{
    ep,
    session::{builder::GraphOptimizationLevel, Session},
    value::Tensor,
};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokenizers::{
    EncodeInput, PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationParams,
};

#[derive(Debug)]
pub struct SemanticInference<T> {
    pub value: T,
    pub model_load_ms: f64,
    pub preprocess_ms: f64,
    pub inference_ms: f64,
}

#[derive(Debug)]
pub struct SemanticTokenization {
    pub batch: usize,
    pub sequence: usize,
    pub input_ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub token_type_ids: Vec<i64>,
}

pub struct SemanticEngine {
    provider: MlProvider,
    dml_device_id: i32,
    models_root: PathBuf,
    onnx_models_root: PathBuf,
    loaded: Option<LoadedModel>,
}

struct LoadedModel {
    resolved: ResolvedSemanticModel,
    tokenizer: Tokenizer,
    session: Session,
    clip_preprocessor: Option<ClipPreprocessor>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClipPreprocessorConfig {
    size: serde_json::Value,
    image_mean: Vec<f32>,
    image_std: Vec<f32>,
    rescale_factor: f32,
}

#[derive(Debug, Clone)]
struct ClipPreprocessor {
    width: u32,
    height: u32,
    mean: [f32; 3],
    std: [f32; 3],
    rescale_factor: f32,
}

struct TokenizedBatch {
    batch: usize,
    sequence: usize,
    input_ids: Vec<i64>,
    attention_mask: Vec<i64>,
    token_type_ids: Vec<i64>,
}

impl SemanticEngine {
    pub fn new(
        provider: MlProvider,
        dml_device_id: i32,
        models_root: PathBuf,
        onnx_models_root: PathBuf,
    ) -> Self {
        Self {
            provider,
            dml_device_id,
            models_root,
            onnx_models_root,
            loaded: None,
        }
    }

    pub fn supported_models(&self) -> Vec<MlSemanticModel> {
        SUPPORTED_SEMANTIC_MODELS
            .iter()
            .copied()
            .filter(|model| provider_supports_model(self.provider, *model))
            .collect()
    }

    pub fn provider(&self) -> MlProvider {
        self.provider
    }

    pub fn loaded_model(&self) -> Option<&ResolvedSemanticModel> {
        self.loaded.as_ref().map(|loaded| &loaded.resolved)
    }

    pub fn unload(&mut self) {
        self.loaded = None;
    }

    pub fn embed_text(
        &mut self,
        model: MlSemanticModel,
        texts: &[String],
    ) -> Result<SemanticInference<Vec<Vec<f32>>>, String> {
        if model == MlSemanticModel::BgeRerankerV2M3 {
            return Err("invalid_request: reranker does not expose text embeddings".to_string());
        }
        let model_load_ms = self.ensure_loaded(model)?;
        let loaded = self.loaded.as_mut().expect("semantic model is loaded");

        let preprocess_started = Instant::now();
        let tokens = tokenize_texts(&mut loaded.tokenizer, loaded.resolved.descriptor, texts)?;
        let preprocess_ms = elapsed_ms(preprocess_started);
        let inference_started = Instant::now();
        let vectors = if model == MlSemanticModel::ChineseClip {
            run_clip_text(loaded, &tokens)?
        } else {
            run_transformer_embedding(loaded, &tokens)?
        };
        let inference_ms = elapsed_ms(inference_started);
        Ok(SemanticInference {
            value: vectors,
            model_load_ms,
            preprocess_ms,
            inference_ms,
        })
    }

    pub fn embed_image(
        &mut self,
        model: MlSemanticModel,
        images: &[MlImageInput],
        body: &[u8],
    ) -> Result<SemanticInference<Vec<Vec<f32>>>, String> {
        if model != MlSemanticModel::ChineseClip {
            return Err("invalid_request: only Chinese-CLIP accepts image embeddings".to_string());
        }
        let model_load_ms = self.ensure_loaded(model)?;
        let loaded = self.loaded.as_mut().expect("semantic model is loaded");
        let preprocessor = loaded
            .clip_preprocessor
            .clone()
            .ok_or_else(|| "model_mismatch: CLIP preprocessor is unavailable".to_string())?;

        let preprocess_started = Instant::now();
        let pixels = preprocess_clip_images(&preprocessor, images, body)?;
        let preprocess_ms = elapsed_ms(preprocess_started);
        let inference_started = Instant::now();
        let vectors = run_clip_image(loaded, images.len(), &preprocessor, pixels)?;
        let inference_ms = elapsed_ms(inference_started);
        Ok(SemanticInference {
            value: vectors,
            model_load_ms,
            preprocess_ms,
            inference_ms,
        })
    }

    pub fn rerank(
        &mut self,
        model: MlSemanticModel,
        query: &str,
        documents: &[String],
    ) -> Result<SemanticInference<Vec<f32>>, String> {
        if model != MlSemanticModel::BgeRerankerV2M3 {
            return Err("invalid_request: selected model is not a reranker".to_string());
        }
        let model_load_ms = self.ensure_loaded(model)?;
        let loaded = self.loaded.as_mut().expect("semantic model is loaded");

        let preprocess_started = Instant::now();
        let tokens = tokenize_pairs(
            &mut loaded.tokenizer,
            loaded.resolved.descriptor,
            query,
            documents,
        )?;
        let preprocess_ms = elapsed_ms(preprocess_started);
        let inference_started = Instant::now();
        let scores = run_reranker(loaded, &tokens)?;
        let inference_ms = elapsed_ms(inference_started);
        Ok(SemanticInference {
            value: scores,
            model_load_ms,
            preprocess_ms,
            inference_ms,
        })
    }

    pub fn inspect_tokenization(
        &mut self,
        model: MlSemanticModel,
        texts: &[String],
        text_pairs: Option<&[String]>,
    ) -> Result<SemanticTokenization, String> {
        self.ensure_loaded(model)?;
        let loaded = self.loaded.as_mut().expect("semantic model is loaded");
        let tokens = match text_pairs {
            Some(pairs) => tokenize_pair_lists(
                &mut loaded.tokenizer,
                loaded.resolved.descriptor,
                texts,
                pairs,
            )?,
            None => tokenize_texts(&mut loaded.tokenizer, loaded.resolved.descriptor, texts)?,
        };
        Ok(SemanticTokenization {
            batch: tokens.batch,
            sequence: tokens.sequence,
            input_ids: tokens.input_ids,
            attention_mask: tokens.attention_mask,
            token_type_ids: tokens.token_type_ids,
        })
    }

    fn ensure_loaded(&mut self, model: MlSemanticModel) -> Result<f64, String> {
        if !provider_supports_model(self.provider, model) {
            return Err(format!(
                "provider_unavailable: DirectML parity is not approved for {}; retry with CPU",
                descriptor_label(model)
            ));
        }
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.resolved.descriptor.model == model)
        {
            return Ok(0.0);
        }

        self.loaded = None;
        let started = Instant::now();
        let resolved = resolve_semantic_model(model, &self.models_root, &self.onnx_models_root)?;
        let tokenizer = load_tokenizer(&resolved)?;
        let clip_preprocessor = resolved
            .preprocessor_path
            .as_deref()
            .map(load_clip_preprocessor)
            .transpose()?;
        let session = load_session(&resolved, self.provider, self.dml_device_id)?;
        validate_session_layout(&session, &resolved)?;
        self.loaded = Some(LoadedModel {
            resolved,
            tokenizer,
            session,
            clip_preprocessor,
        });
        Ok(elapsed_ms(started))
    }
}

fn provider_supports_model(provider: MlProvider, model: MlSemanticModel) -> bool {
    provider == MlProvider::Cpu
        || !matches!(
            model,
            MlSemanticModel::MinilmL12 | MlSemanticModel::BgeRerankerV2M3
        )
}

fn descriptor_label(model: MlSemanticModel) -> &'static str {
    crate::semantic_models::descriptor(model).model_id
}

fn load_tokenizer(resolved: &ResolvedSemanticModel) -> Result<Tokenizer, String> {
    let mut tokenizer = Tokenizer::from_file(&resolved.tokenizer_path).map_err(|error| {
        format!(
            "model_mismatch: failed to load tokenizer {}: {error}",
            resolved.tokenizer_path.display()
        )
    })?;
    tokenizer.with_padding(None);
    tokenizer
        .with_truncation(None)
        .map_err(|error| format!("failed to reset tokenizer truncation: {error}"))?;
    Ok(tokenizer)
}

fn configure_tokenizer(
    tokenizer: &mut Tokenizer,
    descriptor: &crate::semantic_models::SemanticModelDescriptor,
) -> Result<(), String> {
    let pad_id = tokenizer.token_to_id(descriptor.pad_token).unwrap_or(0);
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        direction: PaddingDirection::Right,
        pad_to_multiple_of: None,
        pad_id,
        pad_type_id: 0,
        pad_token: descriptor.pad_token.to_string(),
    }));
    tokenizer
        .with_truncation(descriptor.max_length.map(|max_length| TruncationParams {
            max_length,
            ..Default::default()
        }))
        .map_err(|error| format!("tokenizer truncation configuration failed: {error}"))?;
    Ok(())
}

fn tokenize_texts(
    tokenizer: &mut Tokenizer,
    descriptor: &crate::semantic_models::SemanticModelDescriptor,
    texts: &[String],
) -> Result<TokenizedBatch, String> {
    configure_tokenizer(tokenizer, descriptor)?;
    let encodings = tokenizer
        .encode_batch(texts.to_vec(), true)
        .map_err(|error| format!("semantic tokenization failed: {error}"))?;
    tokenized_batch(encodings)
}

fn tokenize_pairs(
    tokenizer: &mut Tokenizer,
    descriptor: &crate::semantic_models::SemanticModelDescriptor,
    query: &str,
    documents: &[String],
) -> Result<TokenizedBatch, String> {
    configure_tokenizer(tokenizer, descriptor)?;
    let inputs = documents
        .iter()
        .map(|document| EncodeInput::Dual(query.to_string().into(), document.clone().into()))
        .collect::<Vec<_>>();
    let encodings = tokenizer
        .encode_batch(inputs, true)
        .map_err(|error| format!("reranker pair tokenization failed: {error}"))?;
    tokenized_batch(encodings)
}

fn tokenize_pair_lists(
    tokenizer: &mut Tokenizer,
    descriptor: &crate::semantic_models::SemanticModelDescriptor,
    texts: &[String],
    text_pairs: &[String],
) -> Result<TokenizedBatch, String> {
    configure_tokenizer(tokenizer, descriptor)?;
    let inputs = texts
        .iter()
        .zip(text_pairs)
        .map(|(text, pair)| EncodeInput::Dual(text.clone().into(), pair.clone().into()))
        .collect::<Vec<_>>();
    let encodings = tokenizer
        .encode_batch(inputs, true)
        .map_err(|error| format!("pair tokenization failed: {error}"))?;
    tokenized_batch(encodings)
}

fn tokenized_batch(encodings: Vec<tokenizers::Encoding>) -> Result<TokenizedBatch, String> {
    let batch = encodings.len();
    let sequence = encodings
        .first()
        .map(tokenizers::Encoding::len)
        .ok_or_else(|| "invalid_request: tokenizer produced an empty batch".to_string())?;
    if sequence == 0 || encodings.iter().any(|encoding| encoding.len() != sequence) {
        return Err(
            "model_mismatch: tokenizer did not return a padded rectangular batch".to_string(),
        );
    }
    let mut input_ids = Vec::with_capacity(batch * sequence);
    let mut attention_mask = Vec::with_capacity(batch * sequence);
    let mut token_type_ids = Vec::with_capacity(batch * sequence);
    for encoding in encodings {
        input_ids.extend(encoding.get_ids().iter().map(|value| i64::from(*value)));
        attention_mask.extend(
            encoding
                .get_attention_mask()
                .iter()
                .map(|value| i64::from(*value)),
        );
        token_type_ids.extend(
            encoding
                .get_type_ids()
                .iter()
                .map(|value| i64::from(*value)),
        );
    }
    Ok(TokenizedBatch {
        batch,
        sequence,
        input_ids,
        attention_mask,
        token_type_ids,
    })
}

fn load_session(
    resolved: &ResolvedSemanticModel,
    provider: MlProvider,
    dml_device_id: i32,
) -> Result<Session, String> {
    let default_optimization = if resolved.descriptor.model == MlSemanticModel::BgeRerankerV2M3 {
        GraphOptimizationLevel::All
    } else {
        GraphOptimizationLevel::Level1
    };
    let optimization = match std::env::var("CARBONPAPER_ONNX_OPT_LEVEL")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "disable" => GraphOptimizationLevel::Disable,
        "basic" | "level1" => GraphOptimizationLevel::Level1,
        "extended" | "level2" => GraphOptimizationLevel::Level2,
        "layout" | "level3" => GraphOptimizationLevel::Level3,
        "all" => GraphOptimizationLevel::All,
        _ => default_optimization,
    };
    let providers = match provider {
        MlProvider::Cpu => vec![ep::CPU::default().with_arena_allocator(false).build()],
        MlProvider::DirectMl => vec![
            ep::DirectML::default()
                .with_device_id(dml_device_id)
                .build()
                .error_on_failure(),
            ep::CPU::default().with_arena_allocator(false).build(),
        ],
    };
    let builder = Session::builder()
        .map_err(|error| format!("provider_unavailable: failed to create ONNX session: {error}"))?;
    let builder = builder
        .with_optimization_level(optimization)
        .map_err(|error| format!("provider_unavailable: failed to set optimization: {error}"))?;
    let builder = builder
        .with_intra_threads(1)
        .map_err(|error| format!("provider_unavailable: failed to set intra threads: {error}"))?;
    let builder = builder
        .with_inter_threads(1)
        .map_err(|error| format!("provider_unavailable: failed to set inter threads: {error}"))?;
    let builder = builder
        .with_parallel_execution(false)
        .map_err(|error| format!("provider_unavailable: failed to set execution mode: {error}"))?;
    let builder = builder
        .with_memory_pattern(false)
        .map_err(|error| format!("provider_unavailable: failed to set memory pattern: {error}"))?;
    let builder = builder
        .with_config_entry("session.use_device_allocator_for_initializers", "0")
        .map_err(|error| {
            format!("provider_unavailable: failed to set allocator policy: {error}")
        })?;
    let mut builder = builder
        .with_execution_providers(providers)
        .map_err(|error| {
            format!("provider_unavailable: failed to configure ONNX provider: {error}")
        })?;

    let load_mode = std::env::var("CARBONPAPER_ONNX_LOAD_MODE")
        .unwrap_or_else(|_| "buffer".to_string())
        .to_ascii_lowercase();
    if load_mode == "path" {
        builder
            .commit_from_file(&resolved.model_path)
            .map_err(|error| format!("inference: failed to load ONNX model from path: {error}"))
    } else {
        let bytes = fs::read(&resolved.model_path).map_err(|error| {
            format!(
                "model_missing: failed to read {}: {error}",
                resolved.model_path.display()
            )
        })?;
        builder
            .commit_from_memory(&bytes)
            .map_err(|error| format!("inference: failed to load ONNX model from memory: {error}"))
    }
}

fn validate_session_layout(
    session: &Session,
    resolved: &ResolvedSemanticModel,
) -> Result<(), String> {
    let mut actual_inputs = session
        .inputs()
        .iter()
        .map(|outlet| outlet.name().to_string())
        .collect::<Vec<_>>();
    let mut expected_inputs = resolved
        .descriptor
        .input_names
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    actual_inputs.sort();
    expected_inputs.sort();
    if actual_inputs != expected_inputs {
        return Err(format!(
            "model_mismatch: ONNX inputs for {} differ: expected={expected_inputs:?}, actual={actual_inputs:?}",
            resolved.descriptor.model_id
        ));
    }

    let actual_outputs = session
        .outputs()
        .iter()
        .map(|outlet| outlet.name().to_string())
        .collect::<Vec<_>>();
    for expected in resolved.descriptor.output_names {
        if !actual_outputs.iter().any(|actual| actual == expected) {
            return Err(format!(
                "model_mismatch: ONNX output {expected} is missing for {}: actual={actual_outputs:?}",
                resolved.descriptor.model_id
            ));
        }
    }
    Ok(())
}

fn run_transformer_embedding(
    loaded: &mut LoadedModel,
    tokens: &TokenizedBatch,
) -> Result<Vec<Vec<f32>>, String> {
    let input_ids = Tensor::from_array(([tokens.batch, tokens.sequence], tokens.input_ids.clone()))
        .map_err(|error| format!("inference: failed to build input_ids: {error}"))?;
    let attention_mask = Tensor::from_array((
        [tokens.batch, tokens.sequence],
        tokens.attention_mask.clone(),
    ))
    .map_err(|error| format!("inference: failed to build attention_mask: {error}"))?;
    let token_type_ids = Tensor::from_array((
        [tokens.batch, tokens.sequence],
        tokens.token_type_ids.clone(),
    ))
    .map_err(|error| format!("inference: failed to build token_type_ids: {error}"))?;
    let outputs = loaded
        .session
        .run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
            "token_type_ids" => token_type_ids,
        ])
        .map_err(|error| format!("inference: transformer ONNX run failed: {error}"))?;
    let output = outputs
        .get("last_hidden_state")
        .ok_or_else(|| "model_mismatch: last_hidden_state output is missing".to_string())?;
    let (shape, values) = output
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("model_mismatch: invalid transformer output: {error}"))?;
    if shape.len() != 3 || shape[0] as usize != tokens.batch || shape[1] as usize != tokens.sequence
    {
        return Err(format!(
            "model_mismatch: unexpected transformer output shape {shape}"
        ));
    }
    let dimensions = shape[2] as usize;
    if loaded.resolved.descriptor.dimensions != Some(dimensions) {
        return Err(format!(
            "model_mismatch: embedding dimensions differ: expected={:?}, actual={dimensions}",
            loaded.resolved.descriptor.dimensions
        ));
    }
    let mut vectors = match loaded.resolved.descriptor.pooling {
        SemanticPooling::AttentionMaskMean => mean_pool(
            values,
            &tokens.attention_mask,
            tokens.batch,
            tokens.sequence,
            dimensions,
        ),
        SemanticPooling::Cls => cls_pool(values, tokens.batch, tokens.sequence, dimensions),
        other => {
            return Err(format!(
                "model_mismatch: unsupported transformer pooling {other:?}"
            ))
        }
    };
    if loaded.resolved.descriptor.l2_normalize {
        normalize_vectors(&mut vectors);
    }
    Ok(vectors)
}

fn run_clip_text(
    loaded: &mut LoadedModel,
    tokens: &TokenizedBatch,
) -> Result<Vec<Vec<f32>>, String> {
    let preprocessor = loaded
        .clip_preprocessor
        .as_ref()
        .ok_or_else(|| "model_mismatch: CLIP preprocessor is unavailable".to_string())?;
    let input_ids = Tensor::from_array(([tokens.batch, tokens.sequence], tokens.input_ids.clone()))
        .map_err(|error| format!("inference: failed to build CLIP input_ids: {error}"))?;
    let attention_mask = Tensor::from_array((
        [tokens.batch, tokens.sequence],
        tokens.attention_mask.clone(),
    ))
    .map_err(|error| format!("inference: failed to build CLIP attention_mask: {error}"))?;
    let pixels = Tensor::from_array((
        [
            tokens.batch,
            3,
            preprocessor.height as usize,
            preprocessor.width as usize,
        ],
        vec![0f32; tokens.batch * 3 * preprocessor.height as usize * preprocessor.width as usize],
    ))
    .map_err(|error| format!("inference: failed to build CLIP placeholder pixels: {error}"))?;
    let outputs = loaded
        .session
        .run(ort::inputs![
            "attention_mask" => attention_mask,
            "input_ids" => input_ids,
            "pixel_values" => pixels,
        ])
        .map_err(|error| format!("inference: CLIP text ONNX run failed: {error}"))?;
    extract_matrix(
        &outputs,
        "text_embeds",
        tokens.batch,
        loaded.resolved.descriptor.dimensions,
        true,
    )
}

fn run_clip_image(
    loaded: &mut LoadedModel,
    batch: usize,
    preprocessor: &ClipPreprocessor,
    pixels: Vec<f32>,
) -> Result<Vec<Vec<f32>>, String> {
    let input_ids = Tensor::from_array(([batch, 1], vec![0i64; batch]))
        .map_err(|error| format!("inference: failed to build CLIP placeholder ids: {error}"))?;
    let attention_mask = Tensor::from_array(([batch, 1], vec![0i64; batch])).map_err(|error| {
        format!("inference: failed to build CLIP placeholder attention mask: {error}")
    })?;
    let pixels = Tensor::from_array((
        [
            batch,
            3,
            preprocessor.height as usize,
            preprocessor.width as usize,
        ],
        pixels,
    ))
    .map_err(|error| format!("inference: failed to build CLIP pixels: {error}"))?;
    let outputs = loaded
        .session
        .run(ort::inputs![
            "attention_mask" => attention_mask,
            "input_ids" => input_ids,
            "pixel_values" => pixels,
        ])
        .map_err(|error| format!("inference: CLIP image ONNX run failed: {error}"))?;
    extract_matrix(
        &outputs,
        "image_embeds",
        batch,
        loaded.resolved.descriptor.dimensions,
        true,
    )
}

fn run_reranker(loaded: &mut LoadedModel, tokens: &TokenizedBatch) -> Result<Vec<f32>, String> {
    let input_ids = Tensor::from_array(([tokens.batch, tokens.sequence], tokens.input_ids.clone()))
        .map_err(|error| format!("inference: failed to build reranker input_ids: {error}"))?;
    let attention_mask = Tensor::from_array((
        [tokens.batch, tokens.sequence],
        tokens.attention_mask.clone(),
    ))
    .map_err(|error| format!("inference: failed to build reranker attention_mask: {error}"))?;
    let outputs = loaded
        .session
        .run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
        ])
        .map_err(|error| format!("inference: reranker ONNX run failed: {error}"))?;
    let output = outputs
        .get("logits")
        .ok_or_else(|| "model_mismatch: logits output is missing".to_string())?;
    let (shape, values) = output
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("model_mismatch: invalid reranker output: {error}"))?;
    if values.len() != tokens.batch {
        return Err(format!(
            "model_mismatch: reranker output shape {shape} has {} values for batch {}",
            values.len(),
            tokens.batch
        ));
    }
    Ok(values.to_vec())
}

fn extract_matrix(
    outputs: &ort::session::SessionOutputs<'_>,
    output_name: &str,
    batch: usize,
    expected_dimensions: Option<usize>,
    normalize: bool,
) -> Result<Vec<Vec<f32>>, String> {
    let output = outputs
        .get(output_name)
        .ok_or_else(|| format!("model_mismatch: {output_name} output is missing"))?;
    let (shape, values) = output
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("model_mismatch: invalid {output_name} output: {error}"))?;
    if shape.len() != 2 || shape[0] as usize != batch {
        return Err(format!(
            "model_mismatch: unexpected {output_name} shape {shape}"
        ));
    }
    let dimensions = shape[1] as usize;
    if expected_dimensions != Some(dimensions) {
        return Err(format!(
            "model_mismatch: {output_name} dimensions differ: expected={expected_dimensions:?}, actual={dimensions}"
        ));
    }
    let mut vectors = values
        .chunks_exact(dimensions)
        .map(|row| row.to_vec())
        .collect::<Vec<_>>();
    if normalize {
        normalize_vectors(&mut vectors);
    }
    Ok(vectors)
}

fn mean_pool(
    values: &[f32],
    attention_mask: &[i64],
    batch: usize,
    sequence: usize,
    dimensions: usize,
) -> Vec<Vec<f32>> {
    let mut vectors = vec![vec![0f32; dimensions]; batch];
    for row in 0..batch {
        let masks = (0..sequence)
            .map(|token| attention_mask[row * sequence + token] as f32)
            .collect::<Vec<_>>();
        let count = numpy_pairwise_sum(&masks);
        let denominator = count.max(1e-9);
        for dimension in 0..dimensions {
            let column = (0..sequence)
                .map(|token| {
                    let base = (row * sequence + token) * dimensions;
                    values[base + dimension] * masks[token]
                })
                .collect::<Vec<_>>();
            vectors[row][dimension] = numpy_pairwise_sum(&column) / denominator;
        }
    }
    vectors
}

/// Mirrors NumPy's float32 pairwise reduction order (`PW_BLOCKSIZE = 128`).
/// Numeric parity matters here because MiniLM's mean pooling is part of the shipped
/// Python behavior contract, not an implementation detail that may drift freely.
fn numpy_pairwise_sum(values: &[f32]) -> f32 {
    const BLOCK: usize = 128;
    match values.len() {
        0 => -0.0,
        1..=7 => values.iter().copied().fold(-0.0, |sum, value| sum + value),
        len if len <= BLOCK => {
            let mut accumulators = [0f32; 8];
            accumulators.copy_from_slice(&values[..8]);
            let mut index = 8usize;
            while index + 7 < len {
                for lane in 0..8 {
                    accumulators[lane] += values[index + lane];
                }
                index += 8;
            }
            let mut sum = ((accumulators[0] + accumulators[1])
                + (accumulators[2] + accumulators[3]))
                + ((accumulators[4] + accumulators[5]) + (accumulators[6] + accumulators[7]));
            while index < len {
                sum += values[index];
                index += 1;
            }
            sum
        }
        len => {
            let mut midpoint = len / 2;
            midpoint -= midpoint % 8;
            numpy_pairwise_sum(&values[..midpoint]) + numpy_pairwise_sum(&values[midpoint..])
        }
    }
}

fn cls_pool(values: &[f32], batch: usize, sequence: usize, dimensions: usize) -> Vec<Vec<f32>> {
    (0..batch)
        .map(|row| {
            let start = row * sequence * dimensions;
            values[start..start + dimensions].to_vec()
        })
        .collect()
}

fn normalize_vectors(vectors: &mut [Vec<f32>]) {
    for vector in vectors {
        let norm = vector
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt()
            .max(1e-9);
        for value in vector {
            *value /= norm;
        }
    }
}

fn load_clip_preprocessor(path: &Path) -> Result<ClipPreprocessor, String> {
    let config: ClipPreprocessorConfig =
        serde_json::from_slice(&fs::read(path).map_err(|error| {
            format!("model_missing: failed to read {}: {error}", path.display())
        })?)
        .map_err(|error| format!("model_mismatch: invalid {}: {error}", path.display()))?;
    let height = config
        .size
        .get("height")
        .or_else(|| config.size.get("shortest_edge"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "model_mismatch: CLIP image height is missing".to_string())?
        as u32;
    let width = config
        .size
        .get("width")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::from(height)) as u32;
    let mean: [f32; 3] = config
        .image_mean
        .try_into()
        .map_err(|_| "model_mismatch: CLIP image_mean must have 3 values".to_string())?;
    let std: [f32; 3] = config
        .image_std
        .try_into()
        .map_err(|_| "model_mismatch: CLIP image_std must have 3 values".to_string())?;
    Ok(ClipPreprocessor {
        width,
        height,
        mean,
        std,
        rescale_factor: config.rescale_factor,
    })
}

fn preprocess_clip_images(
    config: &ClipPreprocessor,
    images: &[MlImageInput],
    body: &[u8],
) -> Result<Vec<f32>, String> {
    let plane = config.width as usize * config.height as usize;
    let mut output = Vec::with_capacity(images.len() * plane * 3);
    for input in images {
        let end = input
            .offset
            .checked_add(input.body_len)
            .ok_or_else(|| "invalid_request: CLIP image offset overflow".to_string())?;
        let raw = body
            .get(input.offset..end)
            .ok_or_else(|| "invalid_request: CLIP image body is truncated".to_string())?;
        let image =
            RgbImage::from_raw(input.width, input.height, raw.to_vec()).ok_or_else(|| {
                format!(
                    "invalid_request: failed to construct RGB image {}x{}",
                    input.width, input.height
                )
            })?;
        let resized = pillow_bicubic_resize_rgb(&image, config.width, config.height);
        for channel in 0..3usize {
            for pixel in resized.pixels() {
                let value = f32::from(pixel[channel]) * config.rescale_factor;
                output.push((value - config.mean[channel]) / config.std[channel]);
            }
        }
    }
    Ok(output)
}

const PILLOW_PRECISION_BITS: i32 = 22;

struct PillowCoefficients {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    coefficients: Vec<i32>,
}

fn pillow_bicubic_resize_rgb(image: &RgbImage, width: u32, height: u32) -> RgbImage {
    if image.width() == width && image.height() == height {
        return image.clone();
    }
    let horizontal = pillow_coefficients(image.width() as usize, width as usize);
    let vertical = pillow_coefficients(image.height() as usize, height as usize);
    let mut temporary = RgbImage::new(width, image.height());
    for y in 0..image.height() as usize {
        for out_x in 0..width as usize {
            let (start, count) = horizontal.bounds[out_x];
            let weights = &horizontal.coefficients
                [out_x * horizontal.ksize..out_x * horizontal.ksize + count];
            let mut sums = [1 << (PILLOW_PRECISION_BITS - 1); 3];
            for (offset, weight) in weights.iter().enumerate() {
                let pixel = image.get_pixel((start + offset) as u32, y as u32);
                for channel in 0..3 {
                    sums[channel] += i32::from(pixel[channel]) * *weight;
                }
            }
            temporary.put_pixel(out_x as u32, y as u32, image::Rgb(sums.map(pillow_clip8)));
        }
    }

    let mut output = RgbImage::new(width, height);
    for out_y in 0..height as usize {
        let (start, count) = vertical.bounds[out_y];
        let weights =
            &vertical.coefficients[out_y * vertical.ksize..out_y * vertical.ksize + count];
        for x in 0..width as usize {
            let mut sums = [1 << (PILLOW_PRECISION_BITS - 1); 3];
            for (offset, weight) in weights.iter().enumerate() {
                let pixel = temporary.get_pixel(x as u32, (start + offset) as u32);
                for channel in 0..3 {
                    sums[channel] += i32::from(pixel[channel]) * *weight;
                }
            }
            output.put_pixel(x as u32, out_y as u32, image::Rgb(sums.map(pillow_clip8)));
        }
    }
    output
}

fn pillow_coefficients(input_size: usize, output_size: usize) -> PillowCoefficients {
    let scale = input_size as f64 / output_size as f64;
    let filter_scale = scale.max(1.0);
    let support = 2.0 * filter_scale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(output_size);
    let mut coefficients = vec![0i32; output_size * ksize];
    let coefficient_scale = (1u64 << PILLOW_PRECISION_BITS) as f64;

    for output in 0..output_size {
        let center = (output as f64 + 0.5) * scale;
        let mut minimum = (center - support + 0.5) as isize;
        minimum = minimum.max(0);
        let mut maximum = (center + support + 0.5) as isize;
        maximum = maximum.min(input_size as isize);
        let count = (maximum - minimum).max(0) as usize;
        let inverse_filter_scale = 1.0 / filter_scale;
        let mut weights = Vec::with_capacity(count);
        let mut weight_sum = 0.0f64;
        for offset in 0..count {
            let distance = (offset as f64 + minimum as f64 - center + 0.5) * inverse_filter_scale;
            let weight = pillow_bicubic_kernel(distance);
            weights.push(weight);
            weight_sum += weight;
        }
        for (offset, weight) in weights.into_iter().enumerate() {
            let normalized = if weight_sum == 0.0 {
                weight
            } else {
                weight / weight_sum
            };
            coefficients[output * ksize + offset] = (normalized * coefficient_scale).round() as i32;
        }
        bounds.push((minimum as usize, count));
    }
    PillowCoefficients {
        ksize,
        bounds,
        coefficients,
    }
}

fn pillow_bicubic_kernel(mut value: f64) -> f64 {
    const A: f64 = -0.5;
    value = value.abs();
    if value < 1.0 {
        return ((A + 2.0) * value - (A + 3.0)) * value * value + 1.0;
    }
    if value < 2.0 {
        return (((value - 5.0) * value + 8.0) * value - 4.0) * A;
    }
    0.0
}

fn pillow_clip8(value: i32) -> u8 {
    (value >> PILLOW_PRECISION_BITS).clamp(0, 255) as u8
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directml_only_advertises_models_that_pass_the_parity_gate() {
        assert!(provider_supports_model(
            MlProvider::DirectMl,
            MlSemanticModel::ChineseClip
        ));
        assert!(provider_supports_model(
            MlProvider::DirectMl,
            MlSemanticModel::BgeSmallZh
        ));
        assert!(!provider_supports_model(
            MlProvider::DirectMl,
            MlSemanticModel::MinilmL12
        ));
        assert!(!provider_supports_model(
            MlProvider::DirectMl,
            MlSemanticModel::BgeRerankerV2M3
        ));
    }

    #[test]
    fn pooling_and_normalization_match_contract_shape() {
        let values = vec![1.0, 3.0, 5.0, 7.0, 9.0, 11.0];
        let mask = vec![1, 1, 0];
        let mut pooled = mean_pool(&values, &mask, 1, 3, 2);
        assert_eq!(pooled, vec![vec![3.0, 5.0]]);
        normalize_vectors(&mut pooled);
        let norm = pooled[0].iter().map(|value| value * value).sum::<f32>();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn numpy_pairwise_sum_uses_the_eight_lane_reduction_order() {
        let values = (0..124).map(|value| value as f32 / 7.0).collect::<Vec<_>>();
        let expected = values.chunks_exact(8).fold([0f32; 8], |mut lanes, chunk| {
            for lane in 0..8 {
                lanes[lane] += chunk[lane];
            }
            lanes
        });
        let remainder_start = values.len() - values.len() % 8;
        let mut expected_sum = ((expected[0] + expected[1]) + (expected[2] + expected[3]))
            + ((expected[4] + expected[5]) + (expected[6] + expected[7]));
        for value in &values[remainder_start..] {
            expected_sum += *value;
        }
        assert_eq!(numpy_pairwise_sum(&values), expected_sum);
    }

    #[test]
    fn clip_preprocess_is_chw_and_normalized() {
        let config = ClipPreprocessor {
            width: 1,
            height: 1,
            mean: [0.0; 3],
            std: [1.0; 3],
            rescale_factor: 1.0 / 255.0,
        };
        let images = vec![MlImageInput {
            width: 1,
            height: 1,
            stride: 3,
            offset: 0,
            body_len: 3,
        }];
        let output = preprocess_clip_images(&config, &images, &[255, 128, 0]).unwrap();
        assert_eq!(output, vec![1.0, 128.0 / 255.0, 0.0]);
    }

    #[test]
    fn pillow_bicubic_keeps_constant_rgb_images_constant() {
        let image = RgbImage::from_pixel(3, 5, image::Rgb([24, 92, 180]));
        let resized = pillow_bicubic_resize_rgb(&image, 17, 11);
        assert!(resized.pixels().all(|pixel| pixel.0 == [24, 92, 180]));
    }
}
