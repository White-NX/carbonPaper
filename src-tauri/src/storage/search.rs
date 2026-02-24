//! Text search with blind bitmap index and tokenization.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jieba_rs::Jieba;
use once_cell::sync::Lazy;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;

use super::{SearchResult, StorageState};

impl StorageState {
    /// Compute HMAC hash for blind index.
    pub(super) fn compute_hmac_hash(text: &str) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;
        const HMAC_KEY: &[u8] = b"CarbonPaper-Search-HMAC-Key-v1";

        let mut mac =
            HmacSha256::new_from_slice(HMAC_KEY).expect("HMAC key length should be valid");
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

    /// Bigram tokenization.
    pub(super) fn bigram_tokenize(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text.chars().collect();
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
    ) -> Result<Vec<SearchResult>, String> {
        let mut guard = self.get_connection_named("search_text")?;
        let conn = guard.as_mut().unwrap();

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
                            let token_hash = Self::compute_hmac_hash(token);
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
                                    .map_err(|e| {
                                        format!("Failed to deserialize bitmap: {}", e)
                                    })?;
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
                            let ocr_ids: Vec<i64> =
                                kw_bitmap.iter().map(|v| v as i64).collect();
                            if ocr_ids.is_empty() {
                                return Ok(vec![]);
                            }

                            let mut screenshot_ids = std::collections::HashSet::new();
                            for chunk in ocr_ids.chunks(500) {
                                let placeholders =
                                    chunk.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
                                let sql = format!(
                                    "SELECT DISTINCT screenshot_id FROM ocr_results WHERE id IN ({})",
                                    placeholders
                                );
                                let params: Vec<&dyn rusqlite::ToSql> = chunk
                                    .iter()
                                    .map(|id| id as &dyn rusqlite::ToSql)
                                    .collect();
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

                    let mut ids: Vec<i64> =
                        intersection.into_iter().map(|v| v as i64).collect();
                    ids.sort_unstable_by(|a, b| b.cmp(a));

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
                                    s.image_path, s.window_title_enc, s.process_name_enc,
                                    s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
                             FROM ocr_results r
                             JOIN screenshots s ON r.screenshot_id = s.id
                             WHERE s.id IN ({})
                               AND r.id = (SELECT MAX(r2.id) FROM ocr_results r2 WHERE r2.screenshot_id = s.id)
                             ORDER BY s.created_at DESC",
                            placeholders.join(",")
                        )
                    } else {
                        // Single keyword: page_ids are ocr_result ids
                        format!(
                            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                                    s.image_path, s.window_title_enc, s.process_name_enc,
                                    s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
                             FROM ocr_results r
                             JOIN screenshots s ON r.screenshot_id = s.id
                             WHERE r.id IN ({})
                             ORDER BY s.created_at DESC, r.id DESC",
                            placeholders.join(",")
                        )
                    };

                    let param_refs: Vec<&dyn rusqlite::ToSql> =
                        page_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
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
                            let process_name_enc: Option<Vec<u8>> = row.get(15)?;
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
                                process_name_enc,
                                screenshot_key_enc,
                                row.get::<_, String>(17)?,
                                row.get::<_, String>(18)?,
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
                                process_name_enc,
                                screenshot_key_enc,
                                created_at,
                                screenshot_created_at,
                            )| {
                                let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                                    (Some(data), Some(key)) => self
                                        .decrypt_payload_with_row_key(data, key)
                                        .ok()
                                        .and_then(|v| String::from_utf8(v).ok()),
                                    _ => None,
                                };

                                let screenshot_key =
                                    match screenshot_key_cache.get(&screenshot_id) {
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

                                let window_title = match (
                                    window_title_enc.as_ref(),
                                    screenshot_key.as_ref(),
                                ) {
                                    (Some(data), Some(key)) => {
                                        decrypt_with_master_key(key, data)
                                            .ok()
                                            .and_then(|v| String::from_utf8(v).ok())
                                    }
                                    _ => None,
                                };
                                let process_name = match (
                                    process_name_enc.as_ref(),
                                    screenshot_key.as_ref(),
                                ) {
                                    (Some(data), Some(key)) => {
                                        decrypt_with_master_key(key, data)
                                            .ok()
                                            .and_then(|v| String::from_utf8(v).ok())
                                    }
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
                                    created_at,
                                    screenshot_created_at,
                                })
                            },
                        )
                        .collect();

                    for (_, mut key) in screenshot_key_cache.into_iter() {
                        Self::zeroize_bytes(&mut key);
                    }

                    // Post-processing: filter by process name and time range
                    let filtered: Vec<SearchResult> = results
                        .into_iter()
                        .filter(|r| {
                            if let Some(ref names) = process_names {
                                if !names.is_empty() {
                                    if let Some(p) = &r.process_name {
                                        if !names.contains(p) {
                                            return false;
                                        }
                                    } else {
                                        return false;
                                    }
                                }
                            }

                            if let Some(start) = start_time {
                                if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                                    &r.screenshot_created_at,
                                    "%Y-%m-%d %H:%M:%S",
                                ) {
                                    if (nd.timestamp() as f64) < start {
                                        return false;
                                    }
                                }
                            }
                            if let Some(end) = end_time {
                                if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                                    &r.screenshot_created_at,
                                    "%Y-%m-%d %H:%M:%S",
                                ) {
                                    if (nd.timestamp() as f64) > end {
                                        return false;
                                    }
                                }
                            }

                            true
                        })
                        .collect();

                    return Ok(filtered);
                }
            }
            // Fall back to simple SQL query (no text query, ordered by time with filters)
            let mut sql = String::from(
                "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                        r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                        r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                        s.image_path, s.window_title_enc, s.process_name_enc,
                        s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
                 FROM ocr_results r
                 JOIN screenshots s ON r.screenshot_id = s.id",
            );

            let mut where_clauses: Vec<String> = Vec::new();
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

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

            if !where_clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&where_clauses.join(" AND "));
            }

            sql.push_str(" ORDER BY s.created_at DESC, r.id DESC LIMIT ? OFFSET ?");
            params.push(Box::new(limit));
            params.push(Box::new(offset));

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare search query: {}", e))?;
            let param_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
                std::collections::HashMap::new();

            let results: Vec<SearchResult> = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let screenshot_id: i64 = row.get(1)?;
                    let text_enc: Option<Vec<u8>> = row.get(2)?;
                    let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                    let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                    let process_name_enc: Option<Vec<u8>> = row.get(15)?;
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
                        process_name_enc,
                        screenshot_key_enc,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
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
                        process_name_enc,
                        screenshot_key_enc,
                        created_at,
                        screenshot_created_at,
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
                        let process_name =
                            match (process_name_enc.as_ref(), screenshot_key.as_ref()) {
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
                            created_at,
                            screenshot_created_at,
                        })
                    },
                )
                .collect();

            for (_, mut key) in screenshot_key_cache.into_iter() {
                Self::zeroize_bytes(&mut key);
            }

            // Post-processing: filter by process name
            let filtered = if let Some(names) = process_names {
                if names.is_empty() {
                    results
                } else {
                    results
                        .into_iter()
                        .filter(|r| {
                            r.process_name
                                .as_ref()
                                .map(|p| names.contains(p))
                                .unwrap_or(false)
                        })
                        .collect()
                }
            } else {
                results
            };

            return Ok(filtered);
        }

        // Has bigram tokens: intersect each keyword's bigrams, then cross-keyword intersection
        let mut keyword_bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
        for kw_bigrams in &per_keyword_bigrams {
            let mut bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
            for token in kw_bigrams {
                let token_hash = Self::compute_hmac_hash(token);
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
                    // A keyword's bigram has no posting => this keyword has no matches
                    bitmaps.clear();
                    break;
                }
            }

            if bitmaps.is_empty() {
                // This keyword has no matches => entire query has no matches
                return Ok(vec![]);
            }

            // Intra-keyword bigram intersection
            let mut iter = bitmaps.into_iter();
            let mut kw_intersection = iter.next().unwrap();
            for bm in iter {
                kw_intersection &= &bm;
            }
            keyword_bitmaps.push(kw_intersection);
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
                    let placeholders =
                        chunk.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
                    let sql = format!(
                        "SELECT DISTINCT screenshot_id FROM ocr_results WHERE id IN ({})",
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
                        s.image_path, s.window_title_enc, s.process_name_enc,
                        s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
                 FROM ocr_results r
                 JOIN screenshots s ON r.screenshot_id = s.id
                 WHERE s.id IN ({})
                   AND r.id = (SELECT MAX(r2.id) FROM ocr_results r2 WHERE r2.screenshot_id = s.id)
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
                    let process_name_enc: Option<Vec<u8>> = row.get(15)?;
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
                        process_name_enc,
                        screenshot_key_enc,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
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
                        process_name_enc,
                        screenshot_key_enc,
                        created_at,
                        screenshot_created_at,
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
                        let process_name =
                            match (process_name_enc.as_ref(), screenshot_key.as_ref()) {
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
                            created_at,
                            screenshot_created_at,
                        })
                    },
                )
                .collect();

            for (_, mut key) in screenshot_key_cache.into_iter() {
                Self::zeroize_bytes(&mut key);
            }

            // Post-processing: filter by process name and time range
            let filtered: Vec<SearchResult> = results
                .into_iter()
                .filter(|r| {
                    if let Some(ref names) = process_names {
                        if !names.is_empty() {
                            if let Some(p) = &r.process_name {
                                if !names.contains(p) {
                                    return false;
                                }
                            } else {
                                return false;
                            }
                        }
                    }
                    if let Some(s) = start_time {
                        if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                            &r.screenshot_created_at,
                            "%Y-%m-%d %H:%M:%S",
                        ) {
                            if (nd.and_utc().timestamp() as f64) < s {
                                return false;
                            }
                        }
                    }
                    if let Some(e) = end_time {
                        if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                            &r.screenshot_created_at,
                            "%Y-%m-%d %H:%M:%S",
                        ) {
                            if (nd.and_utc().timestamp() as f64) > e {
                                return false;
                            }
                        }
                    }
                    true
                })
                .collect();

            return Ok(filtered);
        }

        // Single keyword: use OCR-level intersection
        let mut kw_iter = keyword_bitmaps.into_iter();
        let mut intersection = if let Some(first) = kw_iter.next() {
            first
        } else {
            roaring::RoaringBitmap::new()
        };
        for bm in kw_iter {
            intersection &= &bm;
        }

        if intersection.is_empty() {
            return Ok(vec![]);
        }

        let mut ids: Vec<i64> = intersection.into_iter().map(|v| v as i64).collect();
        // Sort by id descending (approximate time order)
        ids.sort_unstable_by(|a, b| b.cmp(a));

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
                    s.image_path, s.window_title_enc, s.process_name_enc,
                    s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
             FROM ocr_results r
             JOIN screenshots s ON r.screenshot_id = s.id
             WHERE r.id IN ({})
             ORDER BY s.created_at DESC, r.id DESC",
            placeholders.join(",")
        );

        let param_refs: Vec<&dyn rusqlite::ToSql> =
            page_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

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
                let process_name_enc: Option<Vec<u8>> = row.get(15)?;
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
                    process_name_enc,
                    screenshot_key_enc,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
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
                    process_name_enc,
                    screenshot_key_enc,
                    created_at,
                    screenshot_created_at,
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
                    let process_name =
                        match (process_name_enc.as_ref(), screenshot_key.as_ref()) {
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
                        created_at,
                        screenshot_created_at,
                    })
                },
            )
            .collect();

        for (_, mut key) in screenshot_key_cache.into_iter() {
            Self::zeroize_bytes(&mut key);
        }

        // Post-processing: filter by process name and time range
        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| {
                if let Some(ref names) = process_names {
                    if !names.is_empty() {
                        if let Some(p) = &r.process_name {
                            if !names.contains(p) {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    }
                }

                if let Some(start) = start_time {
                    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                        &r.screenshot_created_at,
                        "%Y-%m-%d %H:%M:%S",
                    ) {
                        if (nd.timestamp() as f64) < start {
                            return false;
                        }
                    }
                }
                if let Some(end) = end_time {
                    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(
                        &r.screenshot_created_at,
                        "%Y-%m-%d %H:%M:%S",
                    ) {
                        if (nd.timestamp() as f64) > end {
                            return false;
                        }
                    }
                }

                true
            })
            .collect();

        Ok(filtered)
    }
}
