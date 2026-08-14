//! Text search with blind bitmap index and tokenization.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jieba_rs::Jieba;
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use std::collections::{HashMap, HashSet};

use super::{wire_time, SearchResult, StorageState};

type SearchSqlParam = Box<dyn ToSql>;

/// Whether a hit falls inside the requested window.
///
/// The bounds arrive as Unix seconds and so does [`SearchResult::timestamp`],
/// so nothing here re-parses a formatted date. The earlier version did, against
/// a hard-coded `%Y-%m-%d %H:%M:%S`, which tied the filter to the exact text
/// layout of the column.
///
/// A row whose capture time is unknown is kept rather than dropped, matching
/// both the previous behaviour here (an unparseable string simply skipped the
/// comparison) and `clip_query.rs::apply_filters` on the vector path.
fn within_time_bounds(
    result: &SearchResult,
    start_time: Option<f64>,
    end_time: Option<f64>,
) -> bool {
    let Some(seconds) = result.timestamp.map(|value| value as f64) else {
        return true;
    };
    if start_time.is_some_and(|start| seconds < start) {
        return false;
    }
    if end_time.is_some_and(|end| seconds > end) {
        return false;
    }
    true
}

fn load_process_screenshot_ids(
    conn: &Connection,
    process_names: Option<&[String]>,
) -> Result<Option<HashSet<i64>>, String> {
    let Some(process_names) = process_names.filter(|names| !names.is_empty()) else {
        return Ok(None);
    };

    let placeholders = process_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id FROM screenshots INDEXED BY idx_screenshots_process_deleted_created_at
          WHERE is_deleted = 0 AND process_name IN ({placeholders})"
    );
    let params: Vec<&dyn ToSql> = process_names
        .iter()
        .map(|name| name as &dyn ToSql)
        .collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| format!("Failed to prepare process filter: {error}"))?;
    let ids = stmt
        .query_map(params.as_slice(), |row| row.get::<_, i64>(0))
        .map_err(|error| format!("Failed to execute process filter: {error}"))?
        .filter_map(Result::ok)
        .collect();

    Ok(Some(ids))
}

fn screenshot_matches_filters(
    screenshot_id: i64,
    category_screenshot_ids: Option<&HashSet<i64>>,
    process_screenshot_ids: Option<&HashSet<i64>>,
) -> bool {
    category_screenshot_ids.is_none_or(|ids| ids.contains(&screenshot_id))
        && process_screenshot_ids.is_none_or(|ids| ids.contains(&screenshot_id))
}

fn filter_ocr_ids_by_screenshot(
    conn: &Connection,
    ids: &[i64],
    needed: usize,
    category_screenshot_ids: Option<&HashSet<i64>>,
    process_screenshot_ids: Option<&HashSet<i64>>,
) -> Result<Vec<i64>, String> {
    if category_screenshot_ids.is_none() && process_screenshot_ids.is_none() {
        return Ok(ids.to_vec());
    }

    let mut filtered_ids = Vec::with_capacity(needed.min(ids.len()));
    for chunk in ids.chunks(500) {
        if filtered_ids.len() >= needed {
            break;
        }
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, screenshot_id FROM ocr_results
              WHERE id IN ({placeholders}) AND is_deleted = 0"
        );
        let params: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|error| format!("Failed to prepare screenshot filter: {error}"))?;
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| format!("Failed to execute screenshot filter: {error}"))?;
        let id_to_screenshot: HashMap<i64, i64> = rows.filter_map(Result::ok).collect();

        for id in chunk {
            if let Some(screenshot_id) = id_to_screenshot.get(id) {
                if screenshot_matches_filters(
                    *screenshot_id,
                    category_screenshot_ids,
                    process_screenshot_ids,
                ) {
                    filtered_ids.push(*id);
                }
            }
        }
    }

    Ok(filtered_ids)
}

fn build_empty_search_sql(
    process_names: Option<&[String]>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    categories: Option<&[String]>,
    limit: i32,
    offset: i32,
) -> (String, Vec<SearchSqlParam>) {
    let has_process_filter = process_names.is_some_and(|names| !names.is_empty());
    let from_clause = if has_process_filter {
        // CROSS JOIN fixes screenshots as the outer loop, so the process/time
        // index narrows candidates before OCR rows are visited and sorted.
        "FROM screenshots s INDEXED BY idx_screenshots_process_deleted_created_at
         CROSS JOIN ocr_results r INDEXED BY idx_ocr_deleted_screenshot
                    ON r.screenshot_id = s.id"
    } else {
        "FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id"
    };
    let mut sql = format!(
        "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                s.image_path, s.window_title_enc, s.process_name,
                s.content_key_encrypted,
                CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
                CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
                s.category
           {from_clause}"
    );
    let mut where_clauses = vec![
        "s.is_deleted = 0".to_string(),
        "r.is_deleted = 0".to_string(),
    ];
    let mut params: Vec<SearchSqlParam> = Vec::new();

    if let Some(names) = process_names.filter(|names| !names.is_empty()) {
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        where_clauses.push(format!("s.process_name IN ({placeholders})"));
        params.extend(
            names
                .iter()
                .cloned()
                .map(|name| Box::new(name) as SearchSqlParam),
        );
    }
    if let Some(start) = start_time {
        let start_dt = DateTime::<Utc>::from_timestamp(start as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        where_clauses.push("s.created_at >= ?".to_string());
        params.push(Box::new(start_dt));
    }
    if let Some(end) = end_time {
        let end_dt = DateTime::<Utc>::from_timestamp(end as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        where_clauses.push("s.created_at <= ?".to_string());
        params.push(Box::new(end_dt));
    }
    if let Some(categories) = categories.filter(|categories| !categories.is_empty()) {
        let placeholders = categories.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        where_clauses.push(format!("s.category IN ({placeholders})"));
        params.extend(
            categories
                .iter()
                .cloned()
                .map(|category| Box::new(category) as SearchSqlParam),
        );
    }

    sql.push_str(" WHERE ");
    sql.push_str(&where_clauses.join(" AND "));
    sql.push_str(" ORDER BY s.created_at DESC, r.id DESC LIMIT ? OFFSET ?");
    params.push(Box::new(limit));
    params.push(Box::new(offset));

    (sql, params)
}

impl StorageState {
    /// Compute HMAC hash for blind index.
    pub(super) fn compute_hmac_hash(text: &str, hmac_key: &[u8]) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(hmac_key).expect("HMAC key length should be valid");
        mac.update(text.as_bytes());
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    /// Compute static hash for non-sensitive dedup (e.g. icons, link sets)
    pub(crate) fn compute_static_hash(text: &str) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;
        const STATIC_KEY: &[u8] = b"CarbonPaper-Search-HMAC-Key-v1";

        let mut mac =
            HmacSha256::new_from_slice(STATIC_KEY).expect("HMAC key length should be valid");
        mac.update(text.as_bytes());
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    pub(super) fn tokenize_text(text: &str) -> Vec<String> {
        static JIEBA: Lazy<Jieba> = Lazy::new(Jieba::new);

        let mut unique_tokens = HashSet::new();

        let keywords = JIEBA.cut(text, false);

        for token in keywords {
            let normalized = token
                .trim_matches(|c: char| !c.is_alphanumeric() && !Self::is_cjk(c))
                .to_lowercase();

            if normalized.is_empty() {
                continue;
            }

            // Filter pure punctuation or special characters
            let has_valid_char = normalized
                .chars()
                .any(|c| c.is_ascii_alphanumeric() || Self::is_cjk(c));

            if !has_valid_char {
                continue;
            }

            // Filter single-character ASCII alphanumerics ("a", "1"), keep single CJK characters
            if normalized.len() == 1 && normalized.chars().next().unwrap().is_ascii() {
                continue;
            }

            unique_tokens.insert(normalized);
        }

        unique_tokens.into_iter().collect()
    }

    /// Bigram tokenization (punctuation filtered).
    pub(crate) fn bigram_tokenize(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text
            .chars()
            .filter(|c| c.is_alphanumeric() || Self::is_cjk(*c))
            .collect();
        if chars.len() < 2 {
            return HashSet::new(); // ignore texts too short for bigrams
        }

        chars.windows(2).map(|w| w.iter().collect()).collect()
    }

    pub(super) fn is_cjk(ch: char) -> bool {
        let code = ch as u32;
        matches!(
            code,
            0x4E00..=0x9FFF        // CJK Unified Ideographs
            | 0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
            | 0x20000..=0x2A6DF    // Extension B
            | 0x2A700..=0x2B73F    // Extension C
            | 0x2B740..=0x2B81F    // Extension D
            | 0x2B820..=0x2CEAF    // Extension E/F
            | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
            | 0x2F800..=0x2FA1F    // CJK Compatibility Ideographs Supplement
        )
    }

    /// Search text using blind bigram bitmap index.
    pub fn search_text(
        &self,
        query: &str,
        limit: i32,
        offset: i32,
        fuzzy: bool,
        process_names: Option<Vec<String>>,
        start_time: Option<f64>,
        end_time: Option<f64>,
        categories: Option<Vec<String>>,
    ) -> Result<Vec<SearchResult>, String> {
        let hmac_key = self.credential_state.get_hmac_key()?;
        let conn = self.open_read_connection_named("search_text")?;

        // Pre-compute set of screenshot IDs matching the category filter.
        // This allows us to filter bitmap candidates BEFORE pagination,
        // avoiding the expensive fetch-decrypt-then-discard pattern.
        let category_screenshot_ids: Option<std::collections::HashSet<i64>> = match &categories {
            Some(cats) if !cats.is_empty() => {
                let placeholders = cats.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
                let sql = format!(
                    "SELECT id FROM screenshots WHERE is_deleted = 0 AND category IN ({})",
                    placeholders
                );
                let cat_params: Vec<&dyn rusqlite::ToSql> =
                    cats.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| format!("Failed to query category filter: {}", e))?;
                let ids: std::collections::HashSet<i64> = stmt
                    .query_map(cat_params.as_slice(), |row| row.get::<_, i64>(0))
                    .map_err(|e| format!("Failed to fetch category ids: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();
                Some(ids)
            }
            _ => None,
        };

        // Text searches start from OCR bitmap IDs, so resolve the plaintext
        // process filter to screenshot IDs once and intersect it before any
        // branch applies offset/limit. Empty searches push the same predicate
        // directly into their SQL below instead of materializing this set.
        let process_screenshot_ids = if query.trim().is_empty() {
            None
        } else {
            load_process_screenshot_ids(&conn, process_names.as_deref())?
        };

        // Split keywords by whitespace, compute bigrams for each keyword independently
        // to avoid generating invalid cross-keyword bigrams containing spaces
        let keywords: Vec<&str> = query.split_whitespace().collect();
        let per_keyword_bigrams: Vec<HashSet<String>> = keywords
            .iter()
            .map(|kw| Self::bigram_tokenize(kw))
            .filter(|set| !set.is_empty())
            .collect();

        // If no bigram tokens, try token-based bitmap index for short queries
        // If tokens are also empty, fall back to simple SQL query (ordered by time)
        if per_keyword_bigrams.is_empty() {
            if !query.is_empty() {
                // Use word segmentation (short query strategy), tokenize each keyword separately
                let per_keyword_tokens: Vec<Vec<String>> = keywords
                    .iter()
                    .map(|kw| Self::tokenize_text(kw))
                    .filter(|tokens| !tokens.is_empty())
                    .collect();

                if !per_keyword_tokens.is_empty() {
                    // Each keyword's token set -> corresponding OCR ID bitmap
                    let mut keyword_bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();

                    for kw_tokens in &per_keyword_tokens {
                        let mut bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
                        for token in kw_tokens {
                            let token_hash = Self::compute_hmac_hash(token, &hmac_key);
                            let blob: Option<Vec<u8>> = conn
                                .query_row(
                                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                                    params![&token_hash],
                                    |row| row.get(0),
                                )
                                .optional()
                                .map_err(|e| format!("Failed to query bitmap: {}", e))?;

                            if let Some(b) = blob {
                                let rb = roaring::RoaringBitmap::deserialize_from(&b[..])
                                    .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
                                bitmaps.push(rb);
                            } else {
                                bitmaps.clear();
                                break;
                            }
                        }

                        if bitmaps.is_empty() {
                            return Ok(vec![]);
                        }

                        let mut iter = bitmaps.into_iter();
                        let mut kw_intersection = iter.next().unwrap();
                        for bm in iter {
                            kw_intersection &= &bm;
                        }
                        keyword_bitmaps.push(kw_intersection);
                    }

                    // Multi-keyword: intersect at screenshot level
                    let is_multi_keyword = keyword_bitmaps.len() > 1;
                    let intersection = if is_multi_keyword {
                        let mut per_kw_screenshot_ids: Vec<std::collections::HashSet<i64>> =
                            Vec::new();

                        for kw_bitmap in &keyword_bitmaps {
                            let ocr_ids: Vec<i64> = kw_bitmap.iter().map(|v| v as i64).collect();
                            if ocr_ids.is_empty() {
                                return Ok(vec![]);
                            }

                            let mut screenshot_ids = std::collections::HashSet::new();
                            for chunk in ocr_ids.chunks(500) {
                                let placeholders =
                                    chunk.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
                                let sql = format!(
                                    "SELECT DISTINCT screenshot_id FROM ocr_results WHERE id IN ({}) AND is_deleted = 0",
                                    placeholders
                                );
                                let params: Vec<&dyn rusqlite::ToSql> =
                                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                                let mut stmt = conn.prepare(&sql).map_err(|e| {
                                    format!("Failed to prepare screenshot resolve: {}", e)
                                })?;
                                let rows = stmt
                                    .query_map(params.as_slice(), |row| row.get::<_, i64>(0))
                                    .map_err(|e| {
                                        format!("Failed to resolve screenshot ids: {}", e)
                                    })?;
                                for row in rows.filter_map(|r| r.ok()) {
                                    screenshot_ids.insert(row);
                                }
                            }
                            per_kw_screenshot_ids.push(screenshot_ids);
                        }

                        let mut iter = per_kw_screenshot_ids.into_iter();
                        let mut matching = iter.next().unwrap();
                        for s in iter {
                            matching.retain(|id| s.contains(id));
                        }

                        // Pre-filter screenshot-level predicates before pagination.
                        matching.retain(|id| {
                            screenshot_matches_filters(
                                *id,
                                category_screenshot_ids.as_ref(),
                                process_screenshot_ids.as_ref(),
                            )
                        });

                        if matching.is_empty() {
                            return Ok(vec![]);
                        }

                        // Convert to RoaringBitmap for uniform downstream processing
                        let mut rb = roaring::RoaringBitmap::new();
                        for sid in matching {
                            rb.insert(sid as u32);
                        }
                        rb
                    } else {
                        // Single keyword: use OCR-level intersection directly
                        keyword_bitmaps.into_iter().next().unwrap()
                    };

                    if intersection.is_empty() {
                        return Ok(vec![]);
                    }

                    let mut ids: Vec<i64> = intersection.into_iter().map(|v| v as i64).collect();
                    ids.sort_unstable_by(|a, b| b.cmp(a));

                    // For single-keyword paths, resolve OCR IDs to screenshots and
                    // apply screenshot-level predicates before pagination.
                    if !is_multi_keyword {
                        ids = filter_ocr_ids_by_screenshot(
                            &conn,
                            &ids,
                            (offset + limit).max(0) as usize,
                            category_screenshot_ids.as_ref(),
                            process_screenshot_ids.as_ref(),
                        )?;
                    }

                    // Pagination
                    let start = offset as usize;
                    let end = std::cmp::min(ids.len(), (offset + limit) as usize);
                    let page_ids = if start < end {
                        ids[start..end].to_vec()
                    } else {
                        Vec::new()
                    };

                    if page_ids.is_empty() {
                        return Ok(vec![]);
                    }

                    // Build SQL query
                    let placeholders: Vec<&str> = page_ids.iter().map(|_| "?").collect();
                    let sql = if is_multi_keyword {
                        // Multi-keyword: page_ids are screenshot_ids, get one representative OCR result per screenshot
                        format!(
                            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                                    s.image_path, s.window_title_enc, s.process_name,
                                    s.content_key_encrypted,
                                    CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
                                    CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
                                    s.category
                             FROM ocr_results r
                             JOIN screenshots s ON r.screenshot_id = s.id
                                                         WHERE s.id IN ({})
                                                             AND s.is_deleted = 0
                                                             AND r.is_deleted = 0
                                                             AND r.id = (SELECT MAX(r2.id) FROM ocr_results r2 WHERE r2.screenshot_id = s.id AND r2.is_deleted = 0)
                             ORDER BY s.created_at DESC",
                            placeholders.join(",")
                        )
                    } else {
                        // Single keyword: page_ids are ocr_result ids
                        format!(
                            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                                    s.image_path, s.window_title_enc, s.process_name,
                                    s.content_key_encrypted,
                                    CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
                                    CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
                                    s.category
                             FROM ocr_results r
                             JOIN screenshots s ON r.screenshot_id = s.id
                                                         WHERE r.id IN ({})
                                                             AND r.is_deleted = 0
                                                             AND s.is_deleted = 0
                             ORDER BY s.created_at DESC, r.id DESC",
                            placeholders.join(",")
                        )
                    };

                    let param_refs: Vec<&dyn rusqlite::ToSql> = page_ids
                        .iter()
                        .map(|id| id as &dyn rusqlite::ToSql)
                        .collect();
                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Failed to prepare query: {}", e))?;

                    let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
                        std::collections::HashMap::new();

                    let results: Vec<SearchResult> = stmt
                        .query_map(param_refs.as_slice(), |row| {
                            let screenshot_id: i64 = row.get(1)?;
                            let text_enc: Option<Vec<u8>> = row.get(2)?;
                            let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                            let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                            let process_name: Option<String> = row.get(15)?;
                            let screenshot_key_enc: Option<Vec<u8>> = row.get(16)?;

                            Ok((
                                screenshot_id,
                                row.get::<_, i64>(0)?,
                                text_enc,
                                text_key_enc,
                                row.get::<_, f64>(4)?,
                                vec![
                                    vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                                    vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                                    vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                                    vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                                ],
                                row.get::<_, String>(13)?,
                                window_title_enc,
                                process_name,
                                screenshot_key_enc,
                                row.get::<_, Option<i64>>(17)?,
                                row.get::<_, Option<i64>>(18)?,
                                row.get::<_, Option<String>>(19)?,
                            ))
                        })
                        .map_err(|e| format!("Failed to execute search query: {}", e))?
                        .filter_map(|r| r.ok())
                        .map(
                            |(
                                screenshot_id,
                                id,
                                text_enc,
                                text_key_enc,
                                confidence,
                                box_coords,
                                image_path,
                                window_title_enc,
                                process_name,
                                screenshot_key_enc,
                                created_ts,
                                screenshot_created_ts,
                                category,
                            )| {
                                let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                                    (Some(data), Some(key)) => self
                                        .decrypt_payload_with_row_key(data, key)
                                        .ok()
                                        .and_then(|v| String::from_utf8(v).ok()),
                                    _ => None,
                                };

                                let screenshot_key = match screenshot_key_cache.get(&screenshot_id)
                                {
                                    Some(key) => Some(key.clone()),
                                    None => match screenshot_key_enc.as_ref() {
                                        Some(enc) => {
                                            let key = decrypt_row_key_with_cng(enc).ok();
                                            if let Some(ref k) = key {
                                                screenshot_key_cache
                                                    .insert(screenshot_id, k.clone());
                                            }
                                            key
                                        }
                                        None => None,
                                    },
                                };

                                let window_title =
                                    match (window_title_enc.as_ref(), screenshot_key.as_ref()) {
                                        (Some(data), Some(key)) => {
                                            decrypt_with_master_key(key, data)
                                                .ok()
                                                .and_then(|v| String::from_utf8(v).ok())
                                        }
                                        _ => None,
                                    };
                                SearchResult {
                                    id,
                                    screenshot_id,
                                    text: text.unwrap_or_default(),
                                    confidence,
                                    box_coords,
                                    image_path,
                                    window_title,
                                    process_name,
                                    category,
                                    created_at: wire_time::from_optional_seconds(created_ts),
                                    screenshot_created_at: wire_time::from_optional_seconds(
                                        screenshot_created_ts,
                                    ),
                                    timestamp: screenshot_created_ts,
                                }
                            },
                        )
                        .collect();

                    for (_, mut key) in screenshot_key_cache.into_iter() {
                        Self::zeroize_bytes(&mut key);
                    }

                    // Time is checked after decryption because this path pages
                    // on the bitmap rather than in SQL.
                    let filtered: Vec<SearchResult> = results
                        .into_iter()
                        .filter(|r| within_time_bounds(r, start_time, end_time))
                        .collect();

                    return Ok(filtered);
                }
            }
            // Empty searches can push every filter into SQL. In particular,
            // process_name must constrain screenshots before LIMIT/OFFSET;
            // filtering decrypted rows afterward made the first OCR page look
            // like the complete result set for almost every process.
            let (sql, params) = build_empty_search_sql(
                process_names.as_deref(),
                start_time,
                end_time,
                categories.as_deref(),
                limit,
                offset,
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare search query: {}", e))?;
            let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

            let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
                std::collections::HashMap::new();

            let results: Vec<SearchResult> = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let screenshot_id: i64 = row.get(1)?;
                    let text_enc: Option<Vec<u8>> = row.get(2)?;
                    let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                    let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                    let process_name: Option<String> = row.get(15)?;
                    let screenshot_key_enc: Option<Vec<u8>> = row.get(16)?;

                    Ok((
                        screenshot_id,
                        row.get::<_, i64>(0)?,
                        text_enc,
                        text_key_enc,
                        row.get::<_, f64>(4)?,
                        vec![
                            vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                            vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                            vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                            vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                        ],
                        row.get::<_, String>(13)?,
                        window_title_enc,
                        process_name,
                        screenshot_key_enc,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, Option<i64>>(18)?,
                        row.get::<_, Option<String>>(19)?,
                    ))
                })
                .map_err(|e| format!("Failed to execute search query: {}", e))?
                .filter_map(|r| r.ok())
                .map(
                    |(
                        screenshot_id,
                        id,
                        text_enc,
                        text_key_enc,
                        confidence,
                        box_coords,
                        image_path,
                        window_title_enc,
                        process_name,
                        screenshot_key_enc,
                        created_ts,
                        screenshot_created_ts,
                        category,
                    )| {
                        let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                            (Some(data), Some(key)) => self
                                .decrypt_payload_with_row_key(data, key)
                                .ok()
                                .and_then(|v| String::from_utf8(v).ok()),
                            _ => None,
                        };

                        let screenshot_key = match screenshot_key_cache.get(&screenshot_id) {
                            Some(key) => Some(key.clone()),
                            None => match screenshot_key_enc.as_ref() {
                                Some(enc) => {
                                    let key = decrypt_row_key_with_cng(enc).ok();
                                    if let Some(ref k) = key {
                                        screenshot_key_cache.insert(screenshot_id, k.clone());
                                    }
                                    key
                                }
                                None => None,
                            },
                        };

                        let window_title =
                            match (window_title_enc.as_ref(), screenshot_key.as_ref()) {
                                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                                    .ok()
                                    .and_then(|v| String::from_utf8(v).ok()),
                                _ => None,
                            };
                        SearchResult {
                            id,
                            screenshot_id,
                            text: text.unwrap_or_default(),
                            confidence,
                            box_coords,
                            image_path,
                            window_title,
                            process_name,
                            category,
                            created_at: wire_time::from_optional_seconds(created_ts),
                            screenshot_created_at: wire_time::from_optional_seconds(
                                screenshot_created_ts,
                            ),
                            timestamp: screenshot_created_ts,
                        }
                    },
                )
                .collect();

            for (_, mut key) in screenshot_key_cache.into_iter() {
                Self::zeroize_bytes(&mut key);
            }

            return Ok(results);
        }

        // Has bigram tokens: load bitmaps per keyword
        // In fuzzy mode, union bigram bitmaps and count matches per OCR ID.
        // In strict mode, intersect bigram bitmaps (original behavior).
        let mut keyword_bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
        let mut keyword_count_maps: Vec<HashMap<u32, u32>> = Vec::new();
        for kw_bigrams in &per_keyword_bigrams {
            let mut bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
            for token in kw_bigrams {
                let token_hash = Self::compute_hmac_hash(token, &hmac_key);
                let blob: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                        params![&token_hash],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| format!("Failed to query bitmap: {}", e))?;

                if let Some(b) = blob {
                    let rb = roaring::RoaringBitmap::deserialize_from(&b[..])
                        .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
                    bitmaps.push(rb);
                } else if !fuzzy {
                    // Strict mode: a missing bigram means no matches for this keyword
                    bitmaps.clear();
                    break;
                }
                // Fuzzy mode: skip missing bigrams
            }

            if bitmaps.is_empty() {
                // This keyword has no matches => entire query has no matches
                return Ok(vec![]);
            }

            if fuzzy {
                // Union all bigram bitmaps and count how many bigrams each OCR ID matches
                let mut count_map: HashMap<u32, u32> = HashMap::new();
                let mut union = roaring::RoaringBitmap::new();
                for bm in &bitmaps {
                    union |= bm;
                    for id in bm.iter() {
                        *count_map.entry(id).or_insert(0) += 1;
                    }
                }
                keyword_bitmaps.push(union);
                keyword_count_maps.push(count_map);
            } else {
                // Strict mode: intra-keyword bigram intersection
                let mut iter = bitmaps.into_iter();
                let mut kw_intersection = iter.next().unwrap();
                for bm in iter {
                    kw_intersection &= &bm;
                }
                keyword_bitmaps.push(kw_intersection);
            }
        }

        // Cross-keyword intersection
        let is_multi_keyword = keyword_bitmaps.len() > 1;

        if is_multi_keyword {
            // Multi-keyword: intersect at screenshot level (different keywords may appear in different text boxes of the same screenshot)
            let mut per_kw_screenshot_ids: Vec<std::collections::HashSet<i64>> = Vec::new();

            for kw_bitmap in &keyword_bitmaps {
                let ocr_ids: Vec<i64> = kw_bitmap.iter().map(|v| v as i64).collect();
                if ocr_ids.is_empty() {
                    return Ok(vec![]);
                }

                let mut screenshot_ids = std::collections::HashSet::new();
                for chunk in ocr_ids.chunks(500) {
                    let placeholders = chunk.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
                    let sql = format!(
                        "SELECT DISTINCT screenshot_id FROM ocr_results WHERE id IN ({}) AND is_deleted = 0",
                        placeholders
                    );
                    let params: Vec<&dyn rusqlite::ToSql> =
                        chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Failed to prepare screenshot resolve: {}", e))?;
                    let rows = stmt
                        .query_map(params.as_slice(), |row| row.get::<_, i64>(0))
                        .map_err(|e| format!("Failed to resolve screenshot ids: {}", e))?;
                    for row in rows.filter_map(|r| r.ok()) {
                        screenshot_ids.insert(row);
                    }
                }
                per_kw_screenshot_ids.push(screenshot_ids);
            }

            // Intersect screenshot_ids across keywords
            let mut iter = per_kw_screenshot_ids.into_iter();
            let mut matching_screenshots: std::collections::HashSet<i64> = iter.next().unwrap();
            for s in iter {
                matching_screenshots.retain(|id| s.contains(id));
            }

            // Pre-filter screenshot-level predicates before pagination.
            matching_screenshots.retain(|id| {
                screenshot_matches_filters(
                    *id,
                    category_screenshot_ids.as_ref(),
                    process_screenshot_ids.as_ref(),
                )
            });

            if matching_screenshots.is_empty() {
                return Ok(vec![]);
            }

            let mut screenshot_ids_vec: Vec<i64> = matching_screenshots.into_iter().collect();
            screenshot_ids_vec.sort_unstable_by(|a, b| b.cmp(a));

            // Pagination (by screenshot)
            let start = offset as usize;
            let end = std::cmp::min(screenshot_ids_vec.len(), (offset + limit) as usize);
            let page_screenshot_ids = if start < end {
                screenshot_ids_vec[start..end].to_vec()
            } else {
                Vec::new()
            };

            if page_screenshot_ids.is_empty() {
                return Ok(vec![]);
            }

            // Get one representative OCR result per screenshot
            let placeholders = page_screenshot_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<&str>>()
                .join(",");
            let sql = format!(
                "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                        r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                        r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                        s.image_path, s.window_title_enc, s.process_name,
                        s.content_key_encrypted,
                        CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
                        CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
                        s.category
                 FROM ocr_results r
                 JOIN screenshots s ON r.screenshot_id = s.id
                                 WHERE s.id IN ({})
                                     AND s.is_deleted = 0
                                     AND r.is_deleted = 0
                                     AND r.id = (SELECT MAX(r2.id) FROM ocr_results r2 WHERE r2.screenshot_id = s.id AND r2.is_deleted = 0)
                 ORDER BY s.created_at DESC",
                placeholders
            );

            let param_refs: Vec<&dyn rusqlite::ToSql> = page_screenshot_ids
                .iter()
                .map(|id| id as &dyn rusqlite::ToSql)
                .collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
                std::collections::HashMap::new();

            let results: Vec<SearchResult> = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let screenshot_id: i64 = row.get(1)?;
                    let text_enc: Option<Vec<u8>> = row.get(2)?;
                    let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                    let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                    let process_name: Option<String> = row.get(15)?;
                    let screenshot_key_enc: Option<Vec<u8>> = row.get(16)?;

                    Ok((
                        screenshot_id,
                        row.get::<_, i64>(0)?,
                        text_enc,
                        text_key_enc,
                        row.get::<_, f64>(4)?,
                        vec![
                            vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                            vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                            vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                            vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                        ],
                        row.get::<_, String>(13)?,
                        window_title_enc,
                        process_name,
                        screenshot_key_enc,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, Option<i64>>(18)?,
                        row.get::<_, Option<String>>(19)?,
                    ))
                })
                .map_err(|e| format!("Failed to execute search query: {}", e))?
                .filter_map(|r| r.ok())
                .map(
                    |(
                        screenshot_id,
                        id,
                        text_enc,
                        text_key_enc,
                        confidence,
                        box_coords,
                        image_path,
                        window_title_enc,
                        process_name,
                        screenshot_key_enc,
                        created_ts,
                        screenshot_created_ts,
                        category,
                    )| {
                        let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                            (Some(data), Some(key)) => self
                                .decrypt_payload_with_row_key(data, key)
                                .ok()
                                .and_then(|v| String::from_utf8(v).ok()),
                            _ => None,
                        };

                        let screenshot_key = match screenshot_key_cache.get(&screenshot_id) {
                            Some(key) => Some(key.clone()),
                            None => match screenshot_key_enc.as_ref() {
                                Some(enc) => {
                                    let key = decrypt_row_key_with_cng(enc).ok();
                                    if let Some(ref k) = key {
                                        screenshot_key_cache.insert(screenshot_id, k.clone());
                                    }
                                    key
                                }
                                None => None,
                            },
                        };

                        let window_title =
                            match (window_title_enc.as_ref(), screenshot_key.as_ref()) {
                                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                                    .ok()
                                    .and_then(|v| String::from_utf8(v).ok()),
                                _ => None,
                            };
                        SearchResult {
                            id,
                            screenshot_id,
                            text: text.unwrap_or_default(),
                            confidence,
                            box_coords,
                            image_path,
                            window_title,
                            process_name,
                            category,
                            created_at: wire_time::from_optional_seconds(created_ts),
                            screenshot_created_at: wire_time::from_optional_seconds(
                                screenshot_created_ts,
                            ),
                            timestamp: screenshot_created_ts,
                        }
                    },
                )
                .collect();

            for (_, mut key) in screenshot_key_cache.into_iter() {
                Self::zeroize_bytes(&mut key);
            }

            // Time is checked after decryption because this path pages on the
            // bitmap rather than in SQL.
            let filtered: Vec<SearchResult> = results
                .into_iter()
                .filter(|r| within_time_bounds(r, start_time, end_time))
                .collect();

            return Ok(filtered);
        }

        // Single keyword: use OCR-level bitmap
        let mut kw_iter = keyword_bitmaps.into_iter();
        let bitmap = if let Some(first) = kw_iter.next() {
            first
        } else {
            roaring::RoaringBitmap::new()
        };

        if bitmap.is_empty() {
            return Ok(vec![]);
        }

        let mut ids: Vec<i64> = bitmap.iter().map(|v| v as i64).collect();

        if fuzzy && !keyword_count_maps.is_empty() {
            // Sort by bigram match count descending, then id descending (time order tiebreak)
            let count_map = &keyword_count_maps[0];
            ids.sort_unstable_by(|a, b| {
                let ca = count_map.get(&(*a as u32)).copied().unwrap_or(0);
                let cb = count_map.get(&(*b as u32)).copied().unwrap_or(0);
                cb.cmp(&ca).then_with(|| b.cmp(a))
            });
        } else {
            // Sort by id descending (approximate time order)
            ids.sort_unstable_by(|a, b| b.cmp(a));
        }

        // Apply screenshot-level predicates to the ordered OCR candidates
        // before slicing out the requested page.
        ids = filter_ocr_ids_by_screenshot(
            &conn,
            &ids,
            (offset + limit).max(0) as usize,
            category_screenshot_ids.as_ref(),
            process_screenshot_ids.as_ref(),
        )?;

        // Pagination
        let start = offset as usize;
        let end = std::cmp::min(ids.len(), (offset + limit) as usize);
        let page_ids = if start < end {
            ids[start..end].to_vec()
        } else {
            Vec::new()
        };

        if page_ids.is_empty() {
            return Ok(vec![]);
        }

        // Build SQL query for these ocr_result ids
        let placeholders: Vec<&str> = page_ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                    s.image_path, s.window_title_enc, s.process_name,
                    s.content_key_encrypted,
                    CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
                    CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
                    s.category
             FROM ocr_results r
             JOIN screenshots s ON r.screenshot_id = s.id
                         WHERE r.id IN ({})
                             AND r.is_deleted = 0
                             AND s.is_deleted = 0
             ORDER BY s.created_at DESC, r.id DESC",
            placeholders.join(",")
        );

        let param_refs: Vec<&dyn rusqlite::ToSql> = page_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
            std::collections::HashMap::new();

        let results: Vec<SearchResult> = stmt
            .query_map(param_refs.as_slice(), |row| {
                let screenshot_id: i64 = row.get(1)?;
                let text_enc: Option<Vec<u8>> = row.get(2)?;
                let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                let process_name: Option<String> = row.get(15)?;
                let screenshot_key_enc: Option<Vec<u8>> = row.get(16)?;

                Ok((
                    screenshot_id,
                    row.get::<_, i64>(0)?,
                    text_enc,
                    text_key_enc,
                    row.get::<_, f64>(4)?,
                    vec![
                        vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                        vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                        vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                        vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                    ],
                    row.get::<_, String>(13)?,
                    window_title_enc,
                    process_name,
                    screenshot_key_enc,
                    row.get::<_, Option<i64>>(17)?,
                    row.get::<_, Option<i64>>(18)?,
                    row.get::<_, Option<String>>(19)?,
                ))
            })
            .map_err(|e| format!("Failed to execute search query: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(
                |(
                    screenshot_id,
                    id,
                    text_enc,
                    text_key_enc,
                    confidence,
                    box_coords,
                    image_path,
                    window_title_enc,
                    process_name,
                    screenshot_key_enc,
                    created_ts,
                    screenshot_created_ts,
                    category,
                )| {
                    let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                        (Some(data), Some(key)) => self
                            .decrypt_payload_with_row_key(data, key)
                            .ok()
                            .and_then(|v| String::from_utf8(v).ok()),
                        _ => None,
                    };

                    let screenshot_key = match screenshot_key_cache.get(&screenshot_id) {
                        Some(key) => Some(key.clone()),
                        None => match screenshot_key_enc.as_ref() {
                            Some(enc) => {
                                let key = decrypt_row_key_with_cng(enc).ok();
                                if let Some(ref k) = key {
                                    screenshot_key_cache.insert(screenshot_id, k.clone());
                                }
                                key
                            }
                            None => None,
                        },
                    };

                    let window_title = match (window_title_enc.as_ref(), screenshot_key.as_ref()) {
                        (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                            .ok()
                            .and_then(|v| String::from_utf8(v).ok()),
                        _ => None,
                    };
                    Some(SearchResult {
                        id,
                        screenshot_id,
                        text: text.unwrap_or_default(),
                        confidence,
                        box_coords,
                        image_path,
                        window_title,
                        process_name,
                        category,
                        created_at: wire_time::from_optional_seconds(created_ts),
                        screenshot_created_at: wire_time::from_optional_seconds(
                            screenshot_created_ts,
                        ),
                        timestamp: screenshot_created_ts,
                    })
                },
            )
            .collect();

        // In fuzzy mode, re-sort results to match the relevance order of page_ids
        // (SQL ORDER BY destroys our score-based ordering)
        let mut results = results;
        if fuzzy {
            let id_order: HashMap<i64, usize> = page_ids
                .iter()
                .enumerate()
                .map(|(i, &id)| (id, i))
                .collect();
            results.sort_by_key(|r| id_order.get(&r.id).copied().unwrap_or(usize::MAX));
        }

        for (_, mut key) in screenshot_key_cache.into_iter() {
            Self::zeroize_bytes(&mut key);
        }

        // Time is checked after decryption because this path pages on the
        // bitmap rather than in SQL.
        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| within_time_bounds(r, start_time, end_time))
            .collect();

        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_fixture() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory search database");
        conn.execute_batch(
            "CREATE TABLE screenshots (
                 id INTEGER PRIMARY KEY,
                 image_path TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 is_deleted INTEGER NOT NULL DEFAULT 0,
                 process_name TEXT,
                 window_title_enc BLOB,
                 content_key_encrypted BLOB,
                 category TEXT
             );
             CREATE TABLE ocr_results (
                 id INTEGER PRIMARY KEY,
                 screenshot_id INTEGER NOT NULL,
                 text_enc BLOB,
                 text_key_encrypted BLOB,
                 confidence REAL NOT NULL DEFAULT 1,
                 box_x1 REAL NOT NULL DEFAULT 0,
                 box_y1 REAL NOT NULL DEFAULT 0,
                 box_x2 REAL NOT NULL DEFAULT 0,
                 box_y2 REAL NOT NULL DEFAULT 0,
                 box_x3 REAL NOT NULL DEFAULT 0,
                 box_y3 REAL NOT NULL DEFAULT 0,
                 box_x4 REAL NOT NULL DEFAULT 0,
                 box_y4 REAL NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL,
                 is_deleted INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX idx_screenshots_process_deleted_created_at
                 ON screenshots(process_name, is_deleted, created_at);
             CREATE INDEX idx_ocr_deleted_screenshot
                 ON ocr_results(is_deleted, screenshot_id);",
        )
        .expect("search fixture schema");
        conn
    }

    #[test]
    fn empty_search_applies_process_filter_before_limit() {
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots
                 (id, image_path, created_at, process_name)
             VALUES
                 (1, 'newest.enc', '2026-08-12 12:00:00', 'newest.exe'),
                 (2, 'target.enc', '2026-08-12 11:00:00', 'target.exe');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (101, 1, '2026-08-12 12:00:01'),
                 (102, 1, '2026-08-12 12:00:02'),
                 (103, 1, '2026-08-12 12:00:03'),
                 (104, 1, '2026-08-12 12:00:04'),
                 (201, 2, '2026-08-12 11:00:01'),
                 (202, 2, '2026-08-12 11:00:02');",
        )
        .expect("search fixture rows");

        let processes = vec!["target.exe".to_string()];
        let (sql, params) = build_empty_search_sql(Some(&processes), None, None, None, 2, 0);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
        let query_plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare empty process search plan")
            .query_map(param_refs.as_slice(), |row| row.get(3))
            .expect("explain empty process search")
            .map(Result::unwrap)
            .collect();
        assert!(query_plan
            .iter()
            .any(|detail| detail.contains("idx_screenshots_process_deleted_created_at")));
        assert!(query_plan
            .iter()
            .any(|detail| detail.contains("idx_ocr_deleted_screenshot")));

        let mut stmt = conn.prepare(&sql).expect("empty process search");
        let rows: Vec<(i64, i64, Option<String>)> = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(15)?))
            })
            .expect("execute empty process search")
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            rows,
            vec![
                (202, 2, Some("target.exe".to_string())),
                (201, 2, Some("target.exe".to_string())),
            ]
        );
    }

    #[test]
    fn search_projection_returns_capture_time_as_unix_seconds() {
        // Issue #166: this query used to hand `screenshots.created_at` through
        // exactly as SQLite wrote it — UTC wall clock with no zone marker — and
        // the frontend read that as local time, so every hit was displayed one
        // UTC offset behind the timeline. Selecting seconds leaves nothing to
        // interpret, and `wire_time` renders the string the frontend sees.
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots (id, image_path, created_at, process_name)
             VALUES (1, 'shot.enc', '2026-08-11 06:07:40', 'code.exe');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (11, 1, '2026-08-11 06:09:12');",
        )
        .expect("wire format fixture rows");

        let (sql, params) = build_empty_search_sql(None, None, None, None, 10, 0);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
        let (created_ts, screenshot_created_ts): (Option<i64>, Option<i64>) = conn
            .prepare(&sql)
            .expect("prepare projection query")
            .query_row(param_refs.as_slice(), |row| {
                Ok((row.get(17)?, row.get(18)?))
            })
            .expect("read projection row");

        assert_eq!(screenshot_created_ts, Some(1_786_428_460));
        assert_eq!(
            wire_time::from_optional_seconds(screenshot_created_ts),
            "2026-08-11T06:07:40Z"
        );
        // The OCR row is written when recognition finishes, so it trails the
        // capture — which is why the two are reported separately.
        assert!(created_ts > screenshot_created_ts);
    }

    fn result_at(timestamp: Option<i64>) -> SearchResult {
        SearchResult {
            id: 1,
            screenshot_id: 1,
            text: String::new(),
            confidence: 1.0,
            box_coords: Vec::new(),
            image_path: "shot.enc".to_string(),
            window_title: None,
            process_name: None,
            category: None,
            created_at: String::new(),
            screenshot_created_at: wire_time::from_optional_seconds(timestamp),
            timestamp,
        }
    }

    #[test]
    fn time_bounds_compare_seconds_and_keep_unknown_times() {
        let inside = result_at(Some(1_786_428_460));
        assert!(within_time_bounds(&inside, None, None));
        assert!(within_time_bounds(
            &inside,
            Some(1_786_428_000.0),
            Some(1_786_429_000.0)
        ));
        assert!(!within_time_bounds(&inside, Some(1_786_429_000.0), None));
        assert!(!within_time_bounds(&inside, None, Some(1_786_428_000.0)));

        // A row with no usable capture time is kept rather than dropped, which
        // is what the vector path does too.
        let unknown = result_at(None);
        assert!(within_time_bounds(
            &unknown,
            Some(1_786_429_000.0),
            Some(1_786_429_100.0)
        ));
    }

    #[test]
    fn bitmap_candidates_apply_process_filter_before_pagination() {
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots
                 (id, image_path, created_at, process_name)
             VALUES
                 (1, 'other.enc', '2026-08-12 12:00:00', 'other.exe'),
                 (2, 'target.enc', '2026-08-12 11:00:00', 'target.exe');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (6, 1, '2026-08-12 12:00:06'),
                 (5, 1, '2026-08-12 12:00:05'),
                 (4, 1, '2026-08-12 12:00:04'),
                 (3, 1, '2026-08-12 12:00:03'),
                 (2, 2, '2026-08-12 11:00:02'),
                 (1, 2, '2026-08-12 11:00:01');",
        )
        .expect("bitmap fixture rows");

        let processes = vec!["target.exe".to_string()];
        let process_ids =
            load_process_screenshot_ids(&conn, Some(&processes)).expect("load process screenshots");
        let filtered =
            filter_ocr_ids_by_screenshot(&conn, &[6, 5, 4, 3, 2, 1], 2, None, process_ids.as_ref())
                .expect("filter bitmap candidates");

        assert_eq!(filtered, vec![2, 1]);
    }
}
