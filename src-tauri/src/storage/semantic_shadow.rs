//! First-party persistence for MiniLM Rust-vs-Chroma shadow-query samples.
//!
//! M2.5 step 3 records local, telemetry-free parity and latency samples so the
//! shadow phase can prove the release gate (top-10 overlap, top-1 stability,
//! cosine error, latency budget) before any cutover. Only a query hash is
//! persisted; the query text never touches this table.

use super::StorageState;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

/// One shadow comparison between the authoritative Chroma/Python retrieval and
/// the Rust exact-scan retrieval for the same query.
#[derive(Debug, Clone)]
pub struct SemanticShadowSample {
    pub query_hash: String,
    pub k: u32,
    pub python_count: u32,
    pub rust_count: u32,
    pub shared: u32,
    pub top1_agreement: bool,
    pub overlap_k: f32,
    pub max_abs_err: Option<f32>,
    pub mean_abs_err: Option<f32>,
    pub embed_ms: f64,
    pub scan_ms: f64,
    pub python_ms: f64,
    pub rust_visible: u64,
    pub chroma_scope: Option<u64>,
    /// Python-top ids absent from the Rust store entirely (Rust under-covers).
    pub only_in_chroma: u32,
    /// Python-top ids present in the Rust store but ranked outside Rust top-K.
    pub in_both_diff_rank: u32,
    /// Rust-top ids not present in the Python top-K.
    pub only_in_rust: u32,
    /// True for the first probe sample, which bears the ONNX session-init cost.
    /// Steady-state latency percentiles exclude cold-start samples.
    pub cold_start: bool,
    pub note: Option<String>,
}

/// Aggregated view over the most recent shadow samples for the diagnostics UI.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SemanticShadowReport {
    pub window: u32,
    /// Comparable samples: the N behind every parity and latency statistic
    /// below. Rows carrying a `note` are excluded — see `note_sample_count`.
    pub sample_count: u64,
    /// Rows in the window that could not be compared (embed failure, Python
    /// failure, empty Chroma ranking, incomplete classification). They store
    /// `overlap_k = 0` / `top1 = false` as filler, not as a measurement, so
    /// counting them would let one run against an unavailable index bury the
    /// gate numbers for hundreds of subsequent real samples.
    pub note_sample_count: u64,
    /// Total rows examined in the window (`sample_count + note_sample_count`).
    pub window_row_count: u64,
    /// Samples that count toward steady-state latency (cold-start excluded).
    pub steady_sample_count: u64,
    /// What this report actually measures. The Rust path is bi-encoder
    /// retrieval only; the reranker is disabled, so these numbers do NOT
    /// represent the final online ranking (reranker parity is M2.5 step 5).
    pub retrieval_scope: String,
    pub top1_agreement_rate: Option<f32>,
    pub overlap_mean: Option<f32>,
    pub overlap_p50: Option<f32>,
    pub overlap_p05: Option<f32>,
    pub max_abs_err: Option<f32>,
    pub mean_abs_err: Option<f32>,
    pub python_ms_p50: Option<f64>,
    pub python_ms_p95: Option<f64>,
    /// Total Rust latency (embed + scan), steady-state only.
    pub rust_ms_p50: Option<f64>,
    pub rust_ms_p95: Option<f64>,
    /// Rust latency split so the O(N) exact scan is visible apart from encode.
    pub rust_embed_ms_p50: Option<f64>,
    pub rust_embed_ms_p95: Option<f64>,
    pub rust_scan_ms_p50: Option<f64>,
    pub rust_scan_ms_p95: Option<f64>,
    /// Latest cold-start sample's total Rust latency (embed + scan). This is the
    /// first-query cost including ONNX session init; it is excluded from the
    /// percentiles above so a single cold outlier cannot inflate p95.
    pub rust_cold_start_ms: Option<f64>,
    pub rust_visible_latest: Option<u64>,
    pub chroma_scope_latest: Option<u64>,
    /// Summed over the window: the decisive breakdown of top-K disagreement.
    /// A high `only_in_chroma_total` means Rust is missing documents Chroma
    /// serves (real divergence); disagreement concentrated in
    /// `in_both_diff_rank_total` is ranking/approximation, not missing data.
    pub only_in_chroma_total: u64,
    pub in_both_diff_rank_total: u64,
    pub only_in_rust_total: u64,
    pub last_note: Option<String>,
    /// Latest document-encoder parity run: Rust re-encoding the migrated docs
    /// vs the stored Python doc vectors. `None` until a doc probe has run.
    pub doc_encoder: Option<SemanticDocEncoderRun>,
}

/// Summary of one document-encoder parity run. Measures the half of the chain
/// the per-query samples cannot: `rust_encode(build_minilm_task_text(...))`
/// against the stored Python document vector, over a sample of migrated docs.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SemanticDocEncoderRun {
    pub created_at: String,
    /// Documents actually re-encoded and compared.
    pub doc_sample_count: u64,
    /// Documents requested for the sample (before skips).
    pub requested: u64,
    /// Skipped: current SQLite text no longer matches the stored fingerprint
    /// (OCR/title changed since migration), so the stored vector and a fresh
    /// Rust encode would not describe the same text.
    pub source_changed: u64,
    /// Skipped: no active screenshot / empty reconstructed text.
    pub missing_text: u64,
    pub cos_min: Option<f32>,
    pub cos_p05: Option<f32>,
    pub cos_p50: Option<f32>,
    pub cos_mean: Option<f32>,
    pub cos_max: Option<f32>,
    /// Worst-case cosine distance (1 - cos_min): the document-side analogue of
    /// the query-side `max_abs_err`.
    pub max_abs_err: Option<f32>,
    /// First (cold) encode batch total ms, including ONNX session init.
    pub cold_start_ms: Option<f64>,
    /// Amortized steady-state per-document encode ms (cold batch excluded).
    pub steady_ms_per_doc: Option<f64>,
    /// Free-text note, e.g. why a run compared zero documents.
    pub note: Option<String>,
}

/// Cap on retained samples so the diagnostic table cannot grow without bound.
const MAX_RETAINED_SAMPLES: u32 = 5_000;

/// Cap on retained document-encoder run summaries.
const MAX_RETAINED_DOC_RUNS: i64 = 100;

/// Fixed label describing what the shadow report measures. Surfaced verbatim so
/// the UI cannot present bi-encoder retrieval parity as end-to-end parity.
pub const RETRIEVAL_SCOPE_BI_ENCODER: &str = "bi_encoder_only";

impl StorageState {
    pub fn record_semantic_shadow_sample(
        &self,
        sample: &SemanticShadowSample,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("record_semantic_shadow_sample")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO semantic_shadow_samples (
                query_hash, k, python_count, rust_count, shared, top1_agreement,
                overlap_k, max_abs_err, mean_abs_err, embed_ms, scan_ms, python_ms,
                rust_visible, chroma_scope, only_in_chroma, in_both_diff_rank,
                only_in_rust, cold_start, note
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            "#,
            params![
                sample.query_hash,
                sample.k,
                sample.python_count,
                sample.rust_count,
                sample.shared,
                i64::from(sample.top1_agreement),
                f64::from(sample.overlap_k),
                sample.max_abs_err.map(f64::from),
                sample.mean_abs_err.map(f64::from),
                sample.embed_ms,
                sample.scan_ms,
                sample.python_ms,
                i64::try_from(sample.rust_visible).unwrap_or(i64::MAX),
                sample
                    .chroma_scope
                    .map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
                sample.only_in_chroma,
                sample.in_both_diff_rank,
                sample.only_in_rust,
                i64::from(sample.cold_start),
                sample.note,
            ],
        )
        .map_err(|error| format!("Failed to record semantic shadow sample: {error}"))?;
        // Bounded retention: keep only the most recent rows.
        conn.execute(
            r#"
            DELETE FROM semantic_shadow_samples
            WHERE id NOT IN (
                SELECT id FROM semantic_shadow_samples ORDER BY id DESC LIMIT ?1
            )
            "#,
            params![MAX_RETAINED_SAMPLES],
        )
        .ok();
        Ok(())
    }

    pub fn get_semantic_shadow_report(
        &self,
        window: u32,
    ) -> Result<SemanticShadowReport, String> {
        let window = window.clamp(1, MAX_RETAINED_SAMPLES);
        // Collect the sample rows and RELEASE the connection lock (drop `guard`)
        // before fetching the doc-encoder run, which re-acquires the same
        // non-reentrant DB mutex. Holding the guard across that call would
        // self-deadlock the storage lock — and, transitively, every reverse-IPC
        // storage request waiting on it (the watchdog then kills the pipe).
        let rows: Vec<SampleRow> = {
            let guard = self.get_connection_named("get_semantic_shadow_report")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let mut statement = conn
                .prepare(
                    r#"
                    SELECT top1_agreement, overlap_k, max_abs_err, mean_abs_err,
                           embed_ms, scan_ms, python_ms, rust_visible, chroma_scope,
                           only_in_chroma, in_both_diff_rank, only_in_rust, cold_start,
                           note
                    FROM semantic_shadow_samples
                    ORDER BY id DESC
                    LIMIT ?1
                    "#,
                )
                .map_err(|error| format!("Failed to prepare shadow report query: {error}"))?;
            let collected = statement
                .query_map(params![window], |row| {
                    Ok(SampleRow {
                        top1_agreement: row.get::<_, i64>(0)? != 0,
                        overlap_k: row.get::<_, f64>(1)?,
                        max_abs_err: row.get::<_, Option<f64>>(2)?,
                        mean_abs_err: row.get::<_, Option<f64>>(3)?,
                        embed_ms: row.get::<_, f64>(4)?,
                        scan_ms: row.get::<_, f64>(5)?,
                        python_ms: row.get::<_, f64>(6)?,
                        rust_visible: row.get::<_, i64>(7)?,
                        chroma_scope: row.get::<_, Option<i64>>(8)?,
                        only_in_chroma: row.get::<_, i64>(9)?,
                        in_both_diff_rank: row.get::<_, i64>(10)?,
                        only_in_rust: row.get::<_, i64>(11)?,
                        cold_start: row.get::<_, i64>(12)? != 0,
                        note: row.get::<_, Option<String>>(13)?,
                    })
                })
                .map_err(|error| format!("Failed to query shadow samples: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Failed to read shadow sample: {error}"))?;
            // Bind before the block ends so the borrowing `MappedRows` temporary
            // is dropped before `statement`/`conn`/`guard` (drop-order borrow).
            collected
        };
        // Lock released above; safe to re-acquire it here.
        let doc_encoder = self.get_latest_doc_encoder_run()?;
        Ok(aggregate_report(window, rows, doc_encoder))
    }

    /// Persist one document-encoder parity run summary, enforcing bounded
    /// retention so the diagnostic table cannot grow without bound.
    pub fn record_doc_encoder_run(&self, run: &SemanticDocEncoderRun) -> Result<(), String> {
        let guard = self.get_connection_named("record_doc_encoder_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO semantic_doc_encoder_runs (
                doc_sample_count, requested, source_changed, missing_text,
                cos_min, cos_p05, cos_p50, cos_mean, cos_max,
                cold_start_ms, steady_ms_per_doc, note
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                i64::try_from(run.doc_sample_count).unwrap_or(i64::MAX),
                i64::try_from(run.requested).unwrap_or(i64::MAX),
                i64::try_from(run.source_changed).unwrap_or(i64::MAX),
                i64::try_from(run.missing_text).unwrap_or(i64::MAX),
                run.cos_min.map(f64::from),
                run.cos_p05.map(f64::from),
                run.cos_p50.map(f64::from),
                run.cos_mean.map(f64::from),
                run.cos_max.map(f64::from),
                run.cold_start_ms,
                run.steady_ms_per_doc,
                run.note,
            ],
        )
        .map_err(|error| format!("Failed to record doc-encoder run: {error}"))?;
        conn.execute(
            r#"
            DELETE FROM semantic_doc_encoder_runs
            WHERE id NOT IN (
                SELECT id FROM semantic_doc_encoder_runs ORDER BY id DESC LIMIT ?1
            )
            "#,
            params![MAX_RETAINED_DOC_RUNS],
        )
        .ok();
        Ok(())
    }

    /// The most recent document-encoder run, or `None` if no probe has run.
    pub fn get_latest_doc_encoder_run(&self) -> Result<Option<SemanticDocEncoderRun>, String> {
        let guard = self.get_connection_named("get_latest_doc_encoder_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            r#"
            SELECT created_at, doc_sample_count, requested, source_changed,
                   missing_text, cos_min, cos_p05, cos_p50, cos_mean, cos_max,
                   cold_start_ms, steady_ms_per_doc, note
            FROM semantic_doc_encoder_runs
            ORDER BY id DESC
            LIMIT 1
            "#,
            [],
            |row| {
                let cos_min = row.get::<_, Option<f64>>(5)?;
                Ok(SemanticDocEncoderRun {
                    created_at: row.get::<_, String>(0)?,
                    doc_sample_count: row.get::<_, i64>(1)?.max(0) as u64,
                    requested: row.get::<_, i64>(2)?.max(0) as u64,
                    source_changed: row.get::<_, i64>(3)?.max(0) as u64,
                    missing_text: row.get::<_, i64>(4)?.max(0) as u64,
                    cos_min: cos_min.map(|value| value as f32),
                    cos_p05: row.get::<_, Option<f64>>(6)?.map(|value| value as f32),
                    cos_p50: row.get::<_, Option<f64>>(7)?.map(|value| value as f32),
                    cos_mean: row.get::<_, Option<f64>>(8)?.map(|value| value as f32),
                    cos_max: row.get::<_, Option<f64>>(9)?.map(|value| value as f32),
                    max_abs_err: cos_min.map(|value| (1.0 - value) as f32),
                    cold_start_ms: row.get::<_, Option<f64>>(10)?,
                    steady_ms_per_doc: row.get::<_, Option<f64>>(11)?,
                    note: row.get::<_, Option<String>>(12)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Failed to read doc-encoder run: {error}"))
    }
}

#[cfg_attr(test, derive(Clone))]
struct SampleRow {
    top1_agreement: bool,
    overlap_k: f64,
    max_abs_err: Option<f64>,
    mean_abs_err: Option<f64>,
    embed_ms: f64,
    scan_ms: f64,
    python_ms: f64,
    rust_visible: i64,
    chroma_scope: Option<i64>,
    only_in_chroma: i64,
    in_both_diff_rank: i64,
    only_in_rust: i64,
    cold_start: bool,
    note: Option<String>,
}

fn aggregate_report(
    window: u32,
    rows: Vec<SampleRow>,
    doc_encoder: Option<SemanticDocEncoderRun>,
) -> SemanticShadowReport {
    let mut report = SemanticShadowReport {
        window,
        window_row_count: rows.len() as u64,
        retrieval_scope: RETRIEVAL_SCOPE_BI_ENCODER.to_string(),
        doc_encoder,
        ..Default::default()
    };
    // Rows arrive newest-first, so the first row is the latest sample. Coverage
    // counters and the note text are meaningful on note rows too.
    if let Some(latest) = rows.first() {
        report.rust_visible_latest = u64::try_from(latest.rust_visible).ok();
        report.chroma_scope_latest = latest.chroma_scope.and_then(|value| u64::try_from(value).ok());
    }
    report.last_note = rows.iter().find_map(|row| row.note.clone());

    // Everything below is a measurement, so it is computed over comparable rows
    // only. A note row never made the Rust-vs-Chroma comparison at all.
    let comparable: Vec<&SampleRow> = rows.iter().filter(|row| row.note.is_none()).collect();
    report.sample_count = comparable.len() as u64;
    report.note_sample_count = report.window_row_count - report.sample_count;
    if comparable.is_empty() {
        return report;
    }

    let agreements = comparable.iter().filter(|row| row.top1_agreement).count();
    report.top1_agreement_rate = Some(agreements as f32 / comparable.len() as f32);

    report.only_in_chroma_total = comparable
        .iter()
        .map(|row| row.only_in_chroma.max(0) as u64)
        .sum();
    report.in_both_diff_rank_total = comparable
        .iter()
        .map(|row| row.in_both_diff_rank.max(0) as u64)
        .sum();
    report.only_in_rust_total = comparable
        .iter()
        .map(|row| row.only_in_rust.max(0) as u64)
        .sum();

    let overlaps: Vec<f64> = comparable.iter().map(|row| row.overlap_k).collect();
    report.overlap_mean = mean(&overlaps).map(|value| value as f32);
    report.overlap_p50 = percentile(&overlaps, 0.50).map(|value| value as f32);
    report.overlap_p05 = percentile(&overlaps, 0.05).map(|value| value as f32);

    let max_errs: Vec<f64> = comparable.iter().filter_map(|row| row.max_abs_err).collect();
    report.max_abs_err = max_errs
        .iter()
        .copied()
        .fold(None, |acc: Option<f64>, value| {
            Some(acc.map_or(value, |current| current.max(value)))
        })
        .map(|value| value as f32);
    let mean_errs: Vec<f64> = comparable
        .iter()
        .filter_map(|row| row.mean_abs_err)
        .collect();
    report.mean_abs_err = mean(&mean_errs).map(|value| value as f32);

    let python_ms: Vec<f64> = comparable.iter().map(|row| row.python_ms).collect();
    report.python_ms_p50 = percentile(&python_ms, 0.50);
    report.python_ms_p95 = percentile(&python_ms, 0.95);

    // Latency: a single cold-start sample (first query after worker/ONNX init)
    // can dwarf every steady-state query, so it is reported on its own and
    // excluded from the percentiles. The newest cold-start row is the headline
    // cold figure. embed vs scan are split so the O(N) exact scan is visible.
    report.rust_cold_start_ms = comparable
        .iter()
        .find(|row| row.cold_start)
        .map(|row| row.embed_ms + row.scan_ms);
    let steady: Vec<&&SampleRow> = comparable.iter().filter(|row| !row.cold_start).collect();
    report.steady_sample_count = steady.len() as u64;
    let steady_total: Vec<f64> = steady.iter().map(|row| row.embed_ms + row.scan_ms).collect();
    report.rust_ms_p50 = percentile(&steady_total, 0.50);
    report.rust_ms_p95 = percentile(&steady_total, 0.95);
    let steady_embed: Vec<f64> = steady.iter().map(|row| row.embed_ms).collect();
    report.rust_embed_ms_p50 = percentile(&steady_embed, 0.50);
    report.rust_embed_ms_p95 = percentile(&steady_embed, 0.95);
    let steady_scan: Vec<f64> = steady.iter().map(|row| row.scan_ms).collect();
    report.rust_scan_ms_p50 = percentile(&steady_scan, 0.50);
    report.rust_scan_ms_p95 = percentile(&steady_scan, 0.95);
    report
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

/// Nearest-rank percentile over a copy of `values`. `q` is in `[0, 1]`.
fn percentile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(overlap: f64, top1: bool, python_ms: f64) -> SampleRow {
        SampleRow {
            top1_agreement: top1,
            overlap_k: overlap,
            max_abs_err: Some(0.001),
            mean_abs_err: Some(0.0005),
            embed_ms: 20.0,
            scan_ms: 5.0,
            python_ms,
            rust_visible: 1000,
            chroma_scope: Some(1200),
            only_in_chroma: 0,
            in_both_diff_rank: 1,
            only_in_rust: 2,
            cold_start: false,
            note: None,
        }
    }

    #[test]
    fn empty_rows_produce_zeroed_report() {
        let report = aggregate_report(100, Vec::new(), None);
        assert_eq!(report.sample_count, 0);
        assert!(report.overlap_mean.is_none());
        assert!(report.top1_agreement_rate.is_none());
        assert_eq!(report.retrieval_scope, RETRIEVAL_SCOPE_BI_ENCODER);
    }

    #[test]
    fn aggregates_overlap_top1_and_latency() {
        let rows = vec![
            sample_row(1.0, true, 100.0),
            sample_row(0.8, false, 200.0),
            sample_row(0.6, true, 300.0),
        ];
        let report = aggregate_report(100, rows, None);
        assert_eq!(report.sample_count, 3);
        assert!((report.top1_agreement_rate.unwrap() - 2.0 / 3.0).abs() < 1e-6);
        assert!((report.overlap_mean.unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(report.rust_visible_latest, Some(1000));
        assert_eq!(report.chroma_scope_latest, Some(1200));
        // rust_ms = embed(20) + scan(5) = 25 for every row.
        assert!((report.rust_ms_p50.unwrap() - 25.0).abs() < 1e-6);
        // Steady-state splits: embed=20, scan=5 for every row.
        assert!((report.rust_embed_ms_p50.unwrap() - 20.0).abs() < 1e-6);
        assert!((report.rust_scan_ms_p50.unwrap() - 5.0).abs() < 1e-6);
        assert_eq!(report.steady_sample_count, 3);
        assert!(report.rust_cold_start_ms.is_none());
        assert_eq!(report.only_in_chroma_total, 0);
        assert_eq!(report.in_both_diff_rank_total, 3);
        assert_eq!(report.only_in_rust_total, 6);
    }

    #[test]
    fn cold_start_sample_is_excluded_from_steady_latency() {
        let mut cold = sample_row(1.0, true, 100.0);
        cold.cold_start = true;
        cold.embed_ms = 5000.0;
        cold.scan_ms = 100.0;
        let rows = vec![
            cold,
            sample_row(1.0, true, 100.0),
            sample_row(1.0, true, 100.0),
        ];
        let report = aggregate_report(100, rows, None);
        // Cold row reported separately: 5000 + 100 = 5100.
        assert!((report.rust_cold_start_ms.unwrap() - 5100.0).abs() < 1e-6);
        // Steady percentiles ignore the cold outlier: only the two 25ms rows.
        assert_eq!(report.steady_sample_count, 2);
        assert!((report.rust_ms_p95.unwrap() - 25.0).abs() < 1e-6);
    }

    #[test]
    fn doc_encoder_run_is_attached_to_report() {
        let run = SemanticDocEncoderRun {
            doc_sample_count: 256,
            cos_min: Some(0.98),
            max_abs_err: Some(0.02),
            ..Default::default()
        };
        let report = aggregate_report(100, Vec::new(), Some(run));
        let doc = report.doc_encoder.expect("doc encoder attached");
        assert_eq!(doc.doc_sample_count, 256);
        assert!((doc.max_abs_err.unwrap() - 0.02).abs() < 1e-6);
    }

    #[test]
    fn note_rows_are_excluded_from_every_measurement() {
        // Two real samples at overlap 1.0, plus three unavailable-index rows
        // that carry filler zeros. The filler must not move any statistic.
        let mut empty_a = sample_row(0.0, false, 0.0);
        empty_a.note = Some("python_empty_results: ...".to_string());
        empty_a.max_abs_err = None;
        empty_a.mean_abs_err = None;
        let mut empty_b = empty_a.clone();
        empty_b.note = Some("embed_failed: worker down".to_string());
        let mut empty_c = empty_a.clone();
        empty_c.note = Some("classification_lookup_failed: 2 of 10".to_string());

        let rows = vec![
            empty_a,
            empty_b,
            empty_c,
            sample_row(1.0, true, 100.0),
            sample_row(1.0, true, 100.0),
        ];
        let report = aggregate_report(500, rows, None);

        assert_eq!(report.window_row_count, 5);
        assert_eq!(report.note_sample_count, 3);
        assert_eq!(report.sample_count, 2);
        // Gate numbers reflect the two comparable rows, not 2/5.
        assert!((report.top1_agreement_rate.unwrap() - 1.0).abs() < 1e-6);
        assert!((report.overlap_mean.unwrap() - 1.0).abs() < 1e-6);
        assert!((report.overlap_p05.unwrap() - 1.0).abs() < 1e-6);
        // python_ms percentiles ignore the note rows' 0.0 filler.
        assert!((report.python_ms_p50.unwrap() - 100.0).abs() < 1e-6);
        // The newest note is still surfaced for diagnosis.
        assert_eq!(report.last_note.as_deref(), Some("python_empty_results: ..."));
        // Divergence totals come from comparable rows only.
        assert_eq!(report.in_both_diff_rank_total, 2);
    }

    #[test]
    fn a_window_of_only_note_rows_reports_no_measurements() {
        let mut note = sample_row(0.0, false, 0.0);
        note.note = Some("python_empty_results: ...".to_string());
        let report = aggregate_report(500, vec![note], None);
        assert_eq!(report.sample_count, 0);
        assert_eq!(report.note_sample_count, 1);
        // Critically: NOT Some(0.0), which would read as "0% top-1 agreement".
        assert!(report.top1_agreement_rate.is_none());
        assert!(report.overlap_mean.is_none());
        assert!(report.python_ms_p50.is_none());
        // Coverage counters survive: they are recorded on note rows too.
        assert_eq!(report.rust_visible_latest, Some(1000));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = vec![10.0, 20.0, 30.0, 40.0, 100.0];
        assert_eq!(percentile(&values, 0.0), Some(10.0));
        assert_eq!(percentile(&values, 0.5), Some(30.0));
        assert_eq!(percentile(&values, 0.95), Some(100.0));
    }

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        use crate::credential_manager::CredentialManagerState;
        use rusqlite::Connection;
        use std::sync::Arc;
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    /// Regression guard: `get_semantic_shadow_report` used to fetch the
    /// doc-encoder run WHILE still holding the connection guard, re-locking the
    /// non-reentrant DB mutex and self-deadlocking the whole storage layer. This
    /// test would HANG under that bug; it must return quickly and reflect the run.
    #[test]
    fn report_reads_doc_run_without_holding_the_connection_lock() {
        let (_temp, storage) = test_storage();

        // Empty path: no samples, no doc run, scope label still set.
        let report = storage.get_semantic_shadow_report(500).expect("empty report");
        assert_eq!(report.sample_count, 0);
        assert!(report.doc_encoder.is_none());
        assert_eq!(report.retrieval_scope, RETRIEVAL_SCOPE_BI_ENCODER);

        storage
            .record_doc_encoder_run(&SemanticDocEncoderRun {
                doc_sample_count: 128,
                requested: 130,
                source_changed: 2,
                cos_min: Some(0.97),
                cos_p50: Some(0.999),
                ..Default::default()
            })
            .expect("record doc-encoder run");

        // The previously-deadlocking path: report reads the just-recorded run.
        let report = storage.get_semantic_shadow_report(500).expect("report with doc run");
        let doc = report.doc_encoder.expect("doc encoder present");
        assert_eq!(doc.doc_sample_count, 128);
        assert_eq!(doc.requested, 130);
        assert_eq!(doc.source_changed, 2);
        // max_abs_err is derived from cos_min on read (1 - 0.97).
        assert!((doc.max_abs_err.unwrap() - 0.03).abs() < 1e-6);
    }
}
