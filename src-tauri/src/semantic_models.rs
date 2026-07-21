//! Explicit descriptors for the Python-oracle semantic models.
//!
//! The Rust runtime intentionally refuses heuristic ONNX layouts. A model is usable only
//! when its pinned files, tensor names, preprocessing, pooling, and fingerprints match the
//! committed M2 oracle.

use crate::ml_protocol::MlSemanticModel;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticOperation {
    TextEmbedding,
    ImageEmbedding,
    Rerank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticPooling {
    None,
    AttentionMaskMean,
    Cls,
    RawLogit,
}

#[derive(Debug, Clone, Copy)]
pub struct SemanticFileSpec {
    pub relative_path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SemanticModelDescriptor {
    pub model: MlSemanticModel,
    pub model_id: &'static str,
    pub revision: &'static str,
    pub subdir: Option<&'static str>,
    pub model_file: &'static str,
    pub tokenizer_file: &'static str,
    pub pad_token: &'static str,
    pub preprocessor_file: Option<&'static str>,
    pub input_names: &'static [&'static str],
    pub output_names: &'static [&'static str],
    pub operations: &'static [SemanticOperation],
    pub max_length: Option<usize>,
    pub dimensions: Option<usize>,
    pub pooling: SemanticPooling,
    pub l2_normalize: bool,
    pub files: &'static [SemanticFileSpec],
}

#[derive(Debug, Clone)]
pub struct ResolvedSemanticModel {
    pub descriptor: &'static SemanticModelDescriptor,
    pub base_dir: PathBuf,
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub preprocessor_path: Option<PathBuf>,
    pub model_fingerprint: String,
}

const CLIP_FILES: &[SemanticFileSpec] = &[
    SemanticFileSpec {
        relative_path: "onnx/model_q4.onnx",
        size: 177_674_264,
        sha256: "c64c40f177a8756c7831cdaa932bfb30187ef2e85266e54ec838259d34d3fe2e",
    },
    SemanticFileSpec {
        relative_path: "tokenizer.json",
        size: 439_124,
        sha256: "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839",
    },
    SemanticFileSpec {
        relative_path: "preprocessor_config.json",
        size: 546,
        sha256: "61a78fdd2c7ac17b54b6190c0f4cb23423192c535003d52528d01e318a47608b",
    },
];

const MINILM_FILES: &[SemanticFileSpec] = &[
    SemanticFileSpec {
        relative_path: "onnx/model_quantized.onnx",
        size: 118_308_126,
        sha256: "66fc00f5f29afcaff34092e1bdd20008ca3918265a82fb9695a551e510cc4ebc",
    },
    SemanticFileSpec {
        relative_path: "tokenizer.json",
        size: 17_082_913,
        sha256: "b60b6b43406a48bf3638526314f3d232d97058bc93472ff2de930d43686fa441",
    },
];

const BGE_FILES: &[SemanticFileSpec] = &[
    SemanticFileSpec {
        relative_path: "onnx/model_quantized.onnx",
        size: 24_010_842,
        sha256: "15b717c382bcb518ba457b93ea6850ede7f4f1cd8937454aa06972366cd19bcc",
    },
    SemanticFileSpec {
        relative_path: "tokenizer.json",
        size: 439_125,
        sha256: "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26",
    },
];

const RERANKER_FILES: &[SemanticFileSpec] = &[
    SemanticFileSpec {
        relative_path: "onnx/model_uint8.onnx",
        size: 570_727_094,
        sha256: "753fd4a83b13c66f06c3cdc8734397399b7301274900f34a36afa68589a4c1f9",
    },
    SemanticFileSpec {
        relative_path: "tokenizer.json",
        size: 17_082_900,
        sha256: "8bf8afbfd11306bd872018c53bfdf2e160a56f8edbcf49933324404791c148d3",
    },
];

const CLIP_DESCRIPTOR: SemanticModelDescriptor = SemanticModelDescriptor {
    model: MlSemanticModel::ChineseClip,
    model_id: "chinese-clip-vit-base-patch16",
    revision: "f26904860903e70e050b8f48255e5f48401816e9",
    subdir: None,
    model_file: "onnx/model_q4.onnx",
    tokenizer_file: "tokenizer.json",
    pad_token: "[PAD]",
    preprocessor_file: Some("preprocessor_config.json"),
    input_names: &["attention_mask", "input_ids", "pixel_values"],
    output_names: &["text_embeds", "image_embeds"],
    operations: &[
        SemanticOperation::TextEmbedding,
        SemanticOperation::ImageEmbedding,
    ],
    max_length: None,
    dimensions: Some(512),
    pooling: SemanticPooling::None,
    l2_normalize: true,
    files: CLIP_FILES,
};

const MINILM_DESCRIPTOR: SemanticModelDescriptor = SemanticModelDescriptor {
    model: MlSemanticModel::MinilmL12,
    model_id: "paraphrase-multilingual-MiniLM-L12-v2",
    revision: "2c4055b12046f11709e9df2c122e59ffbdc2f900",
    subdir: Some("paraphrase-multilingual-MiniLM-L12-v2"),
    model_file: "onnx/model_quantized.onnx",
    tokenizer_file: "tokenizer.json",
    pad_token: "<pad>",
    preprocessor_file: None,
    input_names: &["input_ids", "attention_mask", "token_type_ids"],
    output_names: &["last_hidden_state"],
    operations: &[SemanticOperation::TextEmbedding],
    max_length: Some(256),
    dimensions: Some(384),
    pooling: SemanticPooling::AttentionMaskMean,
    l2_normalize: true,
    files: MINILM_FILES,
};

const BGE_DESCRIPTOR: SemanticModelDescriptor = SemanticModelDescriptor {
    model: MlSemanticModel::BgeSmallZh,
    model_id: "bge-small-zh-v1.5",
    revision: "75c43b069aac4d136ba6bc1122f995fedcfd2781",
    subdir: Some("bge-small-zh-v1.5"),
    model_file: "onnx/model_quantized.onnx",
    tokenizer_file: "tokenizer.json",
    pad_token: "[PAD]",
    preprocessor_file: None,
    input_names: &["input_ids", "attention_mask", "token_type_ids"],
    output_names: &["last_hidden_state"],
    operations: &[SemanticOperation::TextEmbedding],
    max_length: Some(512),
    dimensions: Some(512),
    pooling: SemanticPooling::Cls,
    l2_normalize: true,
    files: BGE_FILES,
};

const RERANKER_DESCRIPTOR: SemanticModelDescriptor = SemanticModelDescriptor {
    model: MlSemanticModel::BgeRerankerV2M3,
    model_id: "bge-reranker-v2-m3",
    revision: "6f5ff65298512715a1e669753bc754d2bc8f367b",
    subdir: Some("bge-reranker-v2-m3"),
    model_file: "onnx/model_uint8.onnx",
    tokenizer_file: "tokenizer.json",
    pad_token: "<pad>",
    preprocessor_file: None,
    input_names: &["input_ids", "attention_mask"],
    output_names: &["logits"],
    operations: &[SemanticOperation::Rerank],
    max_length: Some(512),
    dimensions: None,
    pooling: SemanticPooling::RawLogit,
    l2_normalize: false,
    files: RERANKER_FILES,
};

pub const SUPPORTED_SEMANTIC_MODELS: &[MlSemanticModel] = &[
    MlSemanticModel::ChineseClip,
    MlSemanticModel::MinilmL12,
    MlSemanticModel::BgeSmallZh,
    MlSemanticModel::BgeRerankerV2M3,
];

pub fn descriptor(model: MlSemanticModel) -> &'static SemanticModelDescriptor {
    match model {
        MlSemanticModel::ChineseClip => &CLIP_DESCRIPTOR,
        MlSemanticModel::MinilmL12 => &MINILM_DESCRIPTOR,
        MlSemanticModel::BgeSmallZh => &BGE_DESCRIPTOR,
        MlSemanticModel::BgeRerankerV2M3 => &RERANKER_DESCRIPTOR,
    }
}

pub fn expected_model_fingerprint(model: MlSemanticModel) -> &'static str {
    let descriptor = descriptor(model);
    descriptor
        .files
        .iter()
        .find(|file| file.relative_path == descriptor.model_file)
        .map(|file| file.sha256)
        .expect("semantic descriptor includes its ONNX model file")
}

pub fn resolve_semantic_model(
    model: MlSemanticModel,
    models_root: &Path,
    onnx_models_root: &Path,
) -> Result<ResolvedSemanticModel, String> {
    let descriptor = descriptor(model);
    let append_subdir = |root: &Path| match descriptor.subdir {
        Some(subdir) => root.join(subdir),
        None => root.to_path_buf(),
    };
    let candidates = if model == MlSemanticModel::BgeRerankerV2M3 {
        vec![append_subdir(models_root)]
    } else {
        vec![append_subdir(onnx_models_root), append_subdir(models_root)]
    };

    let base_dir = candidates
        .into_iter()
        .find(|base| {
            descriptor
                .files
                .iter()
                .all(|file| base.join(file.relative_path).is_file())
        })
        .ok_or_else(|| {
            format!(
                "model_missing: pinned files for {} were not found under {} or {}",
                descriptor.model_id,
                onnx_models_root.display(),
                models_root.display()
            )
        })?;

    let model_fingerprint = verify_model_files(descriptor, &base_dir)?;
    Ok(ResolvedSemanticModel {
        descriptor,
        model_path: base_dir.join(descriptor.model_file),
        tokenizer_path: base_dir.join(descriptor.tokenizer_file),
        preprocessor_path: descriptor
            .preprocessor_file
            .map(|relative| base_dir.join(relative)),
        base_dir,
        model_fingerprint,
    })
}

fn verify_model_files(
    descriptor: &SemanticModelDescriptor,
    base_dir: &Path,
) -> Result<String, String> {
    let mut model_fingerprint = None;
    for spec in descriptor.files {
        let path = base_dir.join(spec.relative_path);
        let metadata = path.metadata().map_err(|error| {
            format!(
                "model_missing: failed to inspect {}: {error}",
                path.display()
            )
        })?;
        if metadata.len() != spec.size {
            return Err(format!(
                "model_mismatch: size mismatch for {}: expected={}, actual={}",
                path.display(),
                spec.size,
                metadata.len()
            ));
        }
        let actual = sha256_file(&path)?;
        if !actual.eq_ignore_ascii_case(spec.sha256) {
            return Err(format!(
                "model_mismatch: sha256 mismatch for {}: expected={}, actual={}",
                path.display(),
                spec.sha256,
                actual
            ));
        }
        if spec.relative_path == descriptor.model_file {
            model_fingerprint = Some(actual);
        }
    }
    model_fingerprint.ok_or_else(|| {
        format!(
            "model_mismatch: descriptor for {} has no model fingerprint",
            descriptor.model_id
        )
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("failed to open {} for hashing: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_match_committed_oracle_contracts() {
        let minilm = descriptor(MlSemanticModel::MinilmL12);
        assert_eq!(minilm.max_length, Some(256));
        assert_eq!(minilm.pooling, SemanticPooling::AttentionMaskMean);
        assert_eq!(minilm.output_names, ["last_hidden_state"]);

        let clip = descriptor(MlSemanticModel::ChineseClip);
        assert_eq!(clip.output_names, ["text_embeds", "image_embeds"]);
        assert!(clip.operations.contains(&SemanticOperation::ImageEmbedding));

        let reranker = descriptor(MlSemanticModel::BgeRerankerV2M3);
        assert_eq!(reranker.max_length, Some(512));
        assert_eq!(reranker.pooling, SemanticPooling::RawLogit);
    }
}
