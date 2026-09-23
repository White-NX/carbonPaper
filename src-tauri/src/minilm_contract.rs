//! The MiniLM vector-space contract: identifiers, dimensions, the task-text
//! assembly, and the per-screenshot job specification every writer and reader
//! of the `semantic_text` derived index agrees on.
//!
//! This used to be the head of `minilm_migration.rs`, whose body copied the old
//! Chroma `task_vectors` hot layer through the Python exporter. That copy is
//! gone (see `legacy_vector_discard.rs` for what replaced it); what stayed is
//! the part the live index still needs, kept as one module so the capture
//! encoder, the idle indexer, the Smart Cluster scorer, and the query path
//! cannot drift on the text contract or the revision.

use crate::maintenance_support;
use crate::storage::{DerivedIndexJobSpec, DerivedIndexKind};
use sha2::{Digest, Sha256};

pub const MINILM_MODEL_ID: &str = "paraphrase-multilingual-MiniLM-L12-v2";
/// Compatibility contract for the shared MiniLM vector space. Rows copied from
/// the retired Chroma collection carried no per-row runtime provenance, so the
/// derived index records the reviewed vector-space contract instead of
/// pretending every row came from one concrete ONNX artifact revision.
pub const MINILM_VECTOR_SPACE_REVISION: &str = "minilm-l12-vector-space-v1";
pub const MINILM_EMBEDDING_VERSION: u32 = 1;
pub const MINILM_DIMENSIONS: usize = 384;
/// Zero (or numerically negligible) vectors would poison cosine/ANN queries;
/// they are quarantined as diagnostics instead of written.
pub const MINILM_MIN_L2_NORM: f32 = 1e-6;
/// The MiniLM source contract consumes at most this many OCR characters, so
/// batch reads only need to decrypt boxes until the prefix is covered.
pub const MINILM_OCR_SNIPPET_CHARS: usize = 200;

pub fn build_minilm_task_text(process_name: &str, window_title: &str, ocr_text: &str) -> String {
    let mut parts = Vec::with_capacity(3);
    if !process_name.is_empty() {
        parts.push(process_name.to_string());
    }
    if !window_title.is_empty() {
        parts.push(window_title.to_string());
    }
    if !ocr_text.is_empty() {
        let snippet: String = ocr_text.chars().take(MINILM_OCR_SNIPPET_CHARS).collect();
        let snippet = snippet.trim();
        if !snippet.is_empty() {
            parts.push(snippet.to_string());
        }
    }
    parts.join(" | ")
}

pub fn minilm_source_fingerprint(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"minilm-task-text-v1\0");
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

pub(crate) fn minilm_job_spec(screenshot_id: i64, text: &str) -> DerivedIndexJobSpec {
    DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::SemanticText,
        subject_key: screenshot_id.to_string(),
        model_id: MINILM_MODEL_ID.to_string(),
        model_revision: MINILM_VECTOR_SPACE_REVISION.to_string(),
        embedding_version: MINILM_EMBEDDING_VERSION,
        source_fingerprint: minilm_source_fingerprint(text),
    }
}

pub(crate) fn validate_minilm_vector(vector: &[f32]) -> Result<(), String> {
    maintenance_support::validate_migrated_vector(vector, MINILM_DIMENSIONS, MINILM_MIN_L2_NORM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_text_matches_python_contract_and_unicode_slice() {
        let ocr = format!("  {}tail", "界".repeat(199));
        let text = build_minilm_task_text("proc", "title", &ocr);
        assert!(text.starts_with("proc | title | "));
        assert_eq!(text.chars().filter(|c| *c == '界').count(), 198);
        assert!(!text.ends_with("tail"));
        assert_eq!(build_minilm_task_text("", "", "  "), "");
    }

    #[test]
    fn fingerprint_is_versioned_and_deterministic() {
        let first = minilm_source_fingerprint("proc | title | OCR");
        assert_eq!(first, minilm_source_fingerprint("proc | title | OCR"));
        assert_ne!(first, minilm_source_fingerprint("proc | title | OCR2"));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn spec_records_vector_space_contract_not_one_runtime_artifact() {
        let spec = minilm_job_spec(7, "proc | title | OCR");
        assert_eq!(spec.model_revision, MINILM_VECTOR_SPACE_REVISION);
        assert_eq!(spec.model_revision, "minilm-l12-vector-space-v1");
        assert!(!spec
            .model_revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn vector_validation_pins_the_minilm_width() {
        // The shared validator covers non-finite and zero vectors; what is
        // MiniLM's own is that 384 is the only accepted width, so a CLIP row
        // cannot be written into this index by mistake.
        assert!(validate_minilm_vector(&vec![0.25; MINILM_DIMENSIONS]).is_ok());
        assert!(validate_minilm_vector(&vec![0.25; 512]).is_err());
        assert!(validate_minilm_vector(&vec![0.0; MINILM_DIMENSIONS])
            .unwrap_err()
            .contains("zero vector"));
    }
}
