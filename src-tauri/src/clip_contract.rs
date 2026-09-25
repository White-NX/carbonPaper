//! The Chinese-CLIP vector-space contract: identifiers, dimensions, and the
//! per-image job specification every writer and reader of the `clip_image`
//! derived index agrees on.
//!
//! This used to be the head of `clip_migration.rs`, whose body copied the old
//! Chroma `screenshots` collection through the Python exporter. That copy is
//! gone (see `legacy_vector_discard.rs` for what replaced it); what stayed is
//! the part the live index still needs, kept as one module so the encoder,
//! the query path, the ANN builder, and the repair scan cannot drift on the
//! width or the revision.

use crate::maintenance_support;
use crate::storage::{DerivedIndexJobSpec, DerivedIndexKind};
use md5::Md5;
use sha2::{Digest, Sha256};

pub const CLIP_MODEL_ID: &str = "chinese-clip-vit-base-patch16";
/// Compatibility contract revision for the shared CLIP vector space.
pub const CLIP_VECTOR_SPACE_REVISION: &str = "chinese-clip-vit-b16-vector-space-v1";
pub const CLIP_EMBEDDING_VERSION: u32 = 1;
pub const CLIP_DIMENSIONS: usize = 512;
/// Zero (or numerically negligible) vectors would poison cosine queries; they
/// are quarantined as diagnostics instead of written.
pub const CLIP_MIN_L2_NORM: f32 = 1e-6;

/// Formats the synthetic memory URI for an image hash.
pub fn clip_memory_uri(image_hash: &str) -> String {
    format!("memory://{image_hash}")
}

/// The document id the retired Python vector store used for one image hash.
///
/// Still reproduced on the query path because the MCP/IPC result envelope
/// carries it, and callers persisted before the Rust cutover key on it.
pub fn clip_document_id(image_hash: &str) -> String {
    let mut digest = Md5::new();
    digest.update(clip_memory_uri(image_hash).as_bytes());
    format!("{:x}", digest.finalize())
}

/// Versioned fingerprint of a CLIP row's source.
///
/// Versioned so that a future change to what the encoder is fed — a different
/// crop, say — invalidates every stored row rather than leaving two contracts
/// silently sharing one index.
pub fn clip_source_fingerprint(image_hash: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"clip-image-source-v1\0");
    digest.update(image_hash.as_bytes());
    format!("{:x}", digest.finalize())
}

pub fn clip_job_spec(image_hash: &str) -> DerivedIndexJobSpec {
    DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::ClipImage,
        subject_key: image_hash.to_string(),
        model_id: CLIP_MODEL_ID.to_string(),
        model_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
        embedding_version: CLIP_EMBEDDING_VERSION,
        source_fingerprint: clip_source_fingerprint(image_hash),
    }
}

pub(crate) fn validate_clip_vector(vector: &[f32]) -> Result<(), String> {
    maintenance_support::validate_migrated_vector(vector, CLIP_DIMENSIONS, CLIP_MIN_L2_NORM)
}

/// Codes the retired Chroma copy recorded against `derived_migration_run_errors`.
///
/// Still named here because installations that ran that copy keep its run and
/// its diagnostics, and `clip_index.rs::get_clip_backfill_offer` still splits
/// that census into "rows correctly skipped" and "rows that could not be
/// imported". A literal retyped there would silently move a whole population
/// from one column to the other the first time somebody renamed one here.
pub mod diagnostic_code {
    /// The Chroma snapshot listed an id whose row was gone by the time the page
    /// was read. Expected under concurrent deletion.
    pub const SNAPSHOT_ROW_MISSING: &str = "snapshot_row_missing";
    /// No live SQLite image hash reproduced this document id — the ordinary
    /// result of the user having deleted the screenshot.
    pub const ORPHAN_DOCUMENT_ID: &str = "orphan_document_id";
    /// The screenshot went away between building the id map and committing.
    pub const SCREENSHOT_DISAPPEARED: &str = "screenshot_disappeared";
    /// Python could not read the stored vector back out of Chroma. Only
    /// historical copy runs wrote this; it is kept so the code is not reused.
    #[allow(dead_code)]
    pub const LEGACY_VECTOR_DECODE_FAILED: &str = "legacy_vector_decode_failed";
    /// The vector arrived, and was not usable for cosine scoring. Only
    /// historical copy runs wrote this; it is kept so the code is not reused.
    #[allow(dead_code)]
    pub const INVALID_VECTOR: &str = "invalid_vector";
    /// The run itself stopped, recorded against no subject.
    pub const RUN_FAILED: &str = "run_failed";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_id_reproduces_the_python_key_scheme() {
        // The retired `vector_store.py::_compute_id` was `md5(image_path)` with
        // `memory://{image_hash}` as the path. These are pinned against values
        // produced by CPython's own hashlib, not against this module's
        // arithmetic restated, because MCP callers persisted these ids.
        assert_eq!(clip_memory_uri("abc123"), "memory://abc123");
        assert_eq!(
            clip_document_id("abc123"),
            "200c8cdc45dea346718762f394f2ac40"
        );
        assert_eq!(
            clip_document_id(&"deadbeef".repeat(8)),
            "16adb578e258ebbe026c81a1c1fae6cb"
        );
        assert_eq!(clip_document_id(""), "7f6e0a2288f241643c9cbe37e8f07cd3");
        assert_ne!(clip_document_id("abc123"), clip_document_id("abc124"));
    }

    #[test]
    fn source_fingerprint_is_versioned_and_derived_from_the_hash_alone() {
        let fingerprint = clip_source_fingerprint("abc123");
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(fingerprint, clip_source_fingerprint("abc123"));
        assert_ne!(fingerprint, clip_source_fingerprint("abc124"));
        // Not a bare hash of the input: the version prefix is what lets a
        // future contract change invalidate every stored row.
        assert_ne!(fingerprint, format!("{:x}", Sha256::digest(b"abc123")));
    }

    #[test]
    fn job_spec_keys_on_the_image_hash_and_records_the_vector_space() {
        let spec = clip_job_spec("abc123");
        assert_eq!(spec.index_kind, DerivedIndexKind::ClipImage);
        // The subject key is the hash itself, which is what
        // `derived_subject_is_active` matches against `screenshots.image_hash`.
        assert_eq!(spec.subject_key, "abc123");
        assert_eq!(spec.model_revision, CLIP_VECTOR_SPACE_REVISION);
        assert_eq!(spec.embedding_version, CLIP_EMBEDDING_VERSION);
    }

    #[test]
    fn vector_validation_pins_the_clip_width() {
        assert!(validate_clip_vector(&vec![0.25; CLIP_DIMENSIONS]).is_ok());
        // A MiniLM row must not be writable into the image index.
        assert!(validate_clip_vector(&vec![0.25; 384]).is_err());
        assert!(validate_clip_vector(&vec![0.0; CLIP_DIMENSIONS])
            .unwrap_err()
            .contains("zero vector"));
    }
}
