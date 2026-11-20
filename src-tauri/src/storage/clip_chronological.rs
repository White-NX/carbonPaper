//! Chronological CLIP retrieval stops at a complete page, rather than ranking
//! and hydrating the whole archive. Cached prefixes contain only ids and scores.

use super::connection::{IndependentReadConnection, QueryDeadline};
use super::derived_index::QUERY_VISIBLE_EMBEDDING_PREDICATE;
use super::{
    BackgroundScreenshotSummary, DerivedIndexKind, ScoredSubject, StorageState, VectorQueryMetrics,
};
use rusqlite::{params_from_iter, Connection};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const CANDIDATE_BATCH_SIZE: usize = 64;
const CACHE_IDLE_TTL: Duration = Duration::from_secs(300);
// Clock-relative legacy values cannot be indexed deterministically. Like other
// unknown capture times, they remain searchable at the end of the order.
const CAPTURE_TIME_SQL: &str = "CASE WHEN lower(created_at) = 'now' THEN 0
    ELSE MAX(COALESCE(CAST(strftime('%s', created_at) AS INTEGER), 0), 0) END";

pub(super) fn init_index(conn: &Connection) -> Result<(), String> {
    // Normalize missing, invalid and non-positive timestamps to the end. Both
    // cursor keys descend, while -id retains the existing ascending id ties.
    conn.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS idx_screenshots_clip_chronological
         ON screenshots({CAPTURE_TIME_SQL} DESC, (-id) DESC) WHERE is_deleted = 0;"
    ))
    .map_err(|error| format!("Failed to create chronological CLIP index: {error}"))
}

pub(crate) struct ClipChronologicalRequest<'a> {
    pub query: &'a [f32],
    pub offset: usize,
    pub limit: usize,
    pub process_names: &'a [String],
    pub categories: &'a [String],
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub min_similarity: f32,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
    pub metrics: &'a VectorQueryMetrics,
}

#[derive(Clone)]
pub(crate) struct ClipChronologicalHit {
    pub subject: ScoredSubject,
    pub summary: BackgroundScreenshotSummary,
}

pub(super) struct CachedClipOrder {
    key: [u8; 32],
    session: Option<Instant>,
    generation: u64,
    data_version: i64,
    reader: IndependentReadConnection,
    last_used: Instant,
    cursor: (i64, i64),
    exhausted: bool,
    hits: Vec<ClipChronologicalHit>,
}

struct Candidate {
    id: i64,
    hash: String,
    timestamp: i64,
}

fn data_version(conn: &Connection) -> Result<i64, String> {
    conn.pragma_query_value(None, "data_version", |row| row.get(0))
        .map_err(|error| error.to_string())
}

fn cache_key(request: &ClipChronologicalRequest<'_>) -> Result<[u8; 32], String> {
    let mut digest = Sha256::new();
    for value in request.query {
        digest.update(value.to_le_bytes());
    }
    digest.update(request.min_similarity.to_le_bytes());
    digest.update(
        serde_json::to_vec(&(
            request.process_names,
            request.categories,
            request.start_time.map(f64::to_bits),
            request.end_time.map(f64::to_bits),
        ))
        .map_err(|error| error.to_string())?,
    );
    Ok(digest.finalize().into())
}

fn check_request(request: &ClipChronologicalRequest<'_>) -> Result<(), String> {
    if request.cancelled.load(Ordering::Acquire) {
        return Err("query_cancelled".into());
    }
    if Instant::now() >= request.deadline {
        return Err("query_deadline_exceeded_during_chronological_scan".into());
    }
    Ok(())
}

fn candidate_sql(categories: usize, same_time: bool) -> String {
    let time = CAPTURE_TIME_SQL.replace("created_at", "s.created_at");
    let cursor_filter = if same_time {
        format!("AND {time} = ?2 AND -s.id < ?3")
    } else {
        format!("AND {time} < ?2")
    };
    let order = if same_time {
        "-s.id DESC".to_owned()
    } else {
        format!("{time} DESC, -s.id DESC")
    };
    let category_filter = if categories == 0 {
        String::new()
    } else {
        format!("AND s.category IN ({})", vec!["?"; categories].join(","))
    };
    // CROSS JOIN keeps the chronological index as the outer loop even without
    // ANALYZE. The covering embedding/ledger lookups verify visibility without
    // reading encrypted payloads or sorting all qualifying vectors.
    format!(
        "SELECT s.id, s.image_hash, {time}
         FROM screenshots s INDEXED BY idx_screenshots_clip_chronological
         CROSS JOIN derived_embeddings e INDEXED BY idx_derived_embeddings_visibility
         CROSS JOIN derived_index_jobs j INDEXED BY idx_derived_jobs_completed_visibility
         WHERE s.is_deleted = 0 AND e.subject_key = s.image_hash
           AND j.index_kind = e.index_kind AND j.subject_key = e.subject_key
           AND {QUERY_VISIBLE_EMBEDDING_PREDICATE}
           {cursor_filter}
           AND (?4 IS NULL OR {time} = 0 OR {time} >= ?4)
           AND (?5 IS NULL OR {time} = 0 OR {time} <= ?5)
           {category_filter}
         ORDER BY {order} LIMIT ?"
    )
}

fn read_candidates(
    reader: &mut Connection,
    cursor: (i64, i64),
    request: &ClipChronologicalRequest<'_>,
) -> Result<Vec<Candidate>, String> {
    check_request(request)?;
    let guard = QueryDeadline::install_with_cancellation(
        reader,
        Some(request.deadline),
        Some(request.cancelled.clone()),
    )
    .map_err(|error| error.to_string())?;
    let mut parameters = vec![
        rusqlite::types::Value::Text(DerivedIndexKind::ClipImage.as_str().into()),
        cursor.0.into(),
        (-cursor.1).into(),
        request
            .start_time
            .filter(|value| !value.is_nan())
            .map_or(rusqlite::types::Value::Null, Into::into),
        request
            .end_time
            .filter(|value| !value.is_nan())
            .map_or(rusqlite::types::Value::Null, Into::into),
    ];
    parameters.extend(
        request
            .categories
            .iter()
            .cloned()
            .map(rusqlite::types::Value::Text),
    );
    let read_page = |same_time: bool, limit: usize| -> Result<Vec<Candidate>, String> {
        let mut values = parameters.clone();
        values.push((limit as i64).into());
        let mut statement = guard
            .prepare(&candidate_sql(request.categories.len(), same_time))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    hash: row.get(1)?,
                    timestamp: row.get(2)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    };
    // Separate equality and older-time ranges let SQLite seek both cursor
    // keys. A row-value comparison over expressions would scan the old prefix.
    let mut candidates = if cursor.0 == i64::MAX {
        Vec::new()
    } else {
        read_page(true, CANDIDATE_BATCH_SIZE)?
    };
    if candidates.len() < CANDIDATE_BATCH_SIZE && cursor.0 > 0 {
        candidates.extend(read_page(false, CANDIDATE_BATCH_SIZE - candidates.len())?);
    }
    check_request(request)?;
    Ok(candidates)
}

impl StorageState {
    pub(crate) fn clip_image_chronological(
        &self,
        request: ClipChronologicalRequest<'_>,
    ) -> Result<Vec<ClipChronologicalHit>, String> {
        check_request(&request)?;
        let authorization = self.protected_query_session()?;
        let session = authorization.as_ref().map(|(_, marker)| *marker);
        let check_authorization = || -> Result<(), String> {
            if let Some((credentials, marker)) = &authorization {
                let credentials = credentials.upgrade().ok_or("AUTH_REQUIRED")?;
                if !credentials.is_session_valid()
                    || credentials.silent_read_auth_required()
                    || credentials.session_marker() != Some(*marker)
                {
                    return Err("AUTH_REQUIRED".into());
                }
            }
            check_request(&request)
        };
        let key = cache_key(&request)?;
        let generation = self.db_generation();
        let previous = self
            .clip_chronological_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let mut cached = match previous.filter(|cached| {
            cached.key == key
                && cached.session == session
                && cached.generation == generation
                && cached.last_used.elapsed() < CACHE_IDLE_TTL
                && data_version(&cached.reader) == Ok(cached.data_version)
        }) {
            Some(cached) => cached,
            None => {
                let reader = self.open_read_connection_named("clip_chronological")?;
                CachedClipOrder {
                    key,
                    session,
                    generation,
                    data_version: data_version(&reader)?,
                    reader,
                    last_used: Instant::now(),
                    cursor: (i64::MAX, 0),
                    exhausted: false,
                    hits: Vec::new(),
                }
            }
        };
        let target = request.offset.saturating_add(request.limit);
        let scan_started = Instant::now();
        request.metrics.update(|progress| {
            progress.mode = "chronological";
            progress.phase = "chronological_scan";
        });
        while cached.hits.len() < target && !cached.exhausted {
            check_authorization()?;
            let candidates = read_candidates(&mut cached.reader, cached.cursor, &request)?;
            if candidates.is_empty() {
                cached.exhausted = true;
                break;
            }
            let keys: Vec<String> = candidates
                .iter()
                .map(|candidate| candidate.hash.clone())
                .collect();
            let vectors = self
                .get_query_visible_embeddings_by_subjects_with_control(
                    DerivedIndexKind::ClipImage,
                    &keys,
                    Some(request.deadline),
                    Some(request.cancelled.clone()),
                )?
                .into_iter()
                .map(|(key, vector)| (key, Zeroizing::new(vector)))
                .collect::<std::collections::HashMap<_, _>>();
            check_authorization()?;
            let mut scores = std::collections::HashMap::with_capacity(vectors.len());
            request
                .metrics
                .update(|progress| progress.rows_scanned += vectors.len());
            for (key, vector) in vectors {
                check_authorization()?;
                if vector.len() != request.query.len() {
                    return Err("dimension_mismatch: chronological CLIP vector".into());
                }
                scores.insert(
                    key,
                    vector
                        .iter()
                        .zip(request.query)
                        .map(|(left, right)| left * right)
                        .sum::<f32>(),
                );
            }
            let process_metadata = if request.process_names.is_empty() {
                std::collections::HashMap::new()
            } else {
                let ids: Vec<i64> = candidates
                    .iter()
                    .filter(|candidate| {
                        scores
                            .get(&candidate.hash)
                            .is_some_and(|score| *score >= request.min_similarity)
                    })
                    .map(|candidate| candidate.id)
                    .collect();
                self.get_screenshot_summaries_by_ids_silent_with_deadline(
                    &ids,
                    Some(request.deadline),
                )
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|summary| (summary.id, summary.process_name))
                .collect()
            };
            request
                .metrics
                .update(|progress| progress.pages_scanned += 1);
            for candidate in candidates {
                check_authorization()?;
                cached.cursor = (candidate.timestamp, candidate.id);
                let Some(score) = scores.get(&candidate.hash).copied() else {
                    continue;
                };
                if score < request.min_similarity || !score.is_finite() {
                    continue;
                }
                if !request.process_names.is_empty() {
                    // The encrypted process remains authoritative for legacy
                    // rows. Only semantic hits need this private metadata read.
                    let Some(process) = process_metadata.get(&candidate.id) else {
                        continue;
                    };
                    if !request
                        .process_names
                        .iter()
                        .any(|name| Some(name.as_str()) == process.as_deref())
                    {
                        continue;
                    }
                }
                cached.hits.push(ClipChronologicalHit {
                    subject: ScoredSubject {
                        subject_key: candidate.hash,
                        score,
                    },
                    summary: BackgroundScreenshotSummary {
                        id: candidate.id,
                        timestamp: (candidate.timestamp > 0).then_some(candidate.timestamp),
                        process_name: None,
                        window_title: None,
                        category: None,
                    },
                });
                // Retain this small batch's remaining matches too, so opening
                // the gallery does not decrypt and score the same rows again.
            }
        }
        check_authorization()?;
        if self.db_generation() != generation {
            return Err("storage_changed_during_chronological_scan".into());
        }
        request.metrics.update(|progress| {
            progress.phase = "complete";
            progress.scan_ms = scan_started.elapsed().as_secs_f64() * 1000.0;
        });
        let page = cached
            .hits
            .iter()
            .skip(request.offset)
            .take(request.limit)
            .cloned()
            .collect();
        cached.last_used = Instant::now();
        let mut slot = self
            .clip_chronological_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.is_database_maintenance_pending()
            && self.db_generation() == generation
            && data_version(&cached.reader) == Ok(cached.data_version)
        {
            *slot = Some(cached);
        }
        Ok(page)
    }

    pub(super) fn clear_clip_chronological_cache(&self) {
        *self
            .clip_chronological_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    pub(super) fn evict_clip_chronological_cache_if_idle(&self, ttl: Duration) -> bool {
        let mut slot = self
            .clip_chronological_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot
            .as_ref()
            .is_some_and(|cached| cached.last_used.elapsed() >= ttl.min(CACHE_IDLE_TTL))
        {
            *slot = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests;
