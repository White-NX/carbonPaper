//! IDF-weighted link scoring with entropy penalty.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use super::{ScoredLink, StorageState, VisibleLink};

impl StorageState {
    /// Compute character-level Shannon entropy of a string (in bits).
    /// Returns 0.0 for empty strings.
    fn char_entropy(text: &str) -> f64 {
        let mut freq: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
        let mut total: usize = 0;
        for ch in text.chars() {
            *freq.entry(ch).or_insert(0) += 1;
            total += 1;
        }
        if total == 0 {
            return 0.0;
        }
        let n = total as f64;
        freq.values().fold(0.0, |acc, &count| {
            let p = count as f64 / n;
            acc - p * p.log2()
        })
    }

    /// Compute an entropy penalty factor in [0, 1].
    ///
    /// Natural language text typically has entropy in [3.0, 5.0] bits/char.
    /// Text outside this range is penalized with a Gaussian-shaped falloff:
    ///   - Too low (e.g. "aaaa"): likely noise or repetitive filler
    ///   - Too high (e.g. random hex/base64): likely encoded data, not readable text
    fn entropy_penalty(text: &str) -> f64 {
        let h = Self::char_entropy(text);
        // Optimal range center=4.0, sigma=1.5
        let center = 4.0;
        let sigma = 1.5;
        let deviation = (h - center) / sigma;
        (-0.5 * deviation * deviation).exp()
    }

    /// Compute IDF-weighted scores for a list of visible links.
    ///
    /// For each link, tokenizes the anchor text into bigrams, looks up document
    /// frequencies from the blind_bitmap_index, and produces a score:
    ///   score = Σ idf(token) × ln(1 + text_len) × entropy_penalty(text) / ln(e + text_len)
    /// where idf(token) = ln(1 + N / (1 + df)).
    /// The entropy penalty dampens links whose anchor text has abnormally low
    /// or high character-level Shannon entropy.
    /// The density divisor ln(e + text_len) normalizes for text length, preventing
    /// long texts from dominating purely due to having more tokens.
    /// Links whose anchor text is a raw URL (http:// or https://) receive a score of 0.
    pub fn compute_link_scores(&self, links: &[VisibleLink]) -> Result<Vec<ScoredLink>, String> {
        if links.is_empty() {
            return Ok(vec![]);
        }

        let hmac_key = self.credential_state.get_hmac_key()?;
        let guard = self.get_connection_named("compute_link_scores")?;
        let conn = guard.as_ref().unwrap();

        // Use cached approximate OCR row count — O(1) instead of O(N) full table scan
        let n: f64 = self.ocr_row_count.load(Ordering::Relaxed) as f64;

        // Tokenize all links and collect unique token hashes
        let mut link_tokens: Vec<HashSet<String>> = Vec::with_capacity(links.len());
        let mut all_hashes: HashSet<String> = HashSet::new();

        for link in links {
            let tokens = Self::bigram_tokenize(&link.text);
            let hashes: HashSet<String> =
                tokens.iter().map(|t| Self::compute_hmac_hash(t, &hmac_key)).collect();
            all_hashes.extend(hashes.iter().cloned());
            link_tokens.push(hashes);
        }

        // Batch-query bitmap cardinalities for all unique token hashes
        let all_hashes_vec: Vec<String> = all_hashes.into_iter().collect();
        let mut df_map: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();

        for chunk in all_hashes_vec.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<&str>>().join(",");
            let sql = format!(
                "SELECT token_hash, postings_blob FROM blind_bitmap_index WHERE token_hash IN ({})",
                placeholders
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|h| h as &dyn rusqlite::ToSql).collect();
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare bitmap query: {}", e))?;
            let rows = stmt
                .query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| format!("Failed to query bitmaps: {}", e))?;
            for row in rows.filter_map(|r| r.ok()) {
                let (hash, blob) = row;
                if let Ok(rb) = roaring::RoaringBitmap::deserialize_from(&blob[..]) {
                    df_map.insert(hash, rb.len() as f64);
                }
            }
        }

        // Score each link
        let mut scored: Vec<ScoredLink> = links
            .iter()
            .zip(link_tokens.iter())
            .map(|(link, hashes)| {
                let trimmed = link.text.trim();
                // URLs as anchor text get zero score
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    return ScoredLink {
                        text: link.text.clone(),
                        url: link.url.clone(),
                        score: 0.0,
                    };
                }
                let text_len = link.text.chars().count() as f64;
                let len_factor = (1.0 + text_len).ln();
                let entropy_factor = Self::entropy_penalty(&link.text);
                let idf_sum: f64 = hashes
                    .iter()
                    .map(|h| {
                        let df = df_map.get(h).copied().unwrap_or(0.0);
                        (1.0 + n / (1.0 + df)).ln()
                    })
                    .sum();
                // Information density: normalize by length to prevent long text from dominating
                let density_divisor = (std::f64::consts::E + text_len).ln();
                ScoredLink {
                    text: link.text.clone(),
                    url: link.url.clone(),
                    score: idf_sum * len_factor * entropy_factor / density_divisor,
                }
            })
            .collect();

        scored
            .sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_char_entropy_empty() {
        assert_eq!(StorageState::char_entropy(""), 0.0);
    }

    #[test]
    fn test_char_entropy_single_char() {
        // All same characters -> zero entropy
        assert!(StorageState::char_entropy("aaaa") < 0.001);
    }

    #[test]
    fn test_char_entropy_two_chars() {
        // "ab" has 2 equally likely characters -> entropy = 1.0 bit
        let e = StorageState::char_entropy("ab");
        assert!((e - 1.0).abs() < 0.001, "entropy of 'ab' should be ~1.0, got {}", e);
    }

    #[test]
    fn test_char_entropy_four_chars() {
        // "abcd" has 4 equally likely characters -> entropy = 2.0 bits
        let e = StorageState::char_entropy("abcd");
        assert!((e - 2.0).abs() < 0.001, "entropy of 'abcd' should be ~2.0, got {}", e);
    }

    #[test]
    fn test_entropy_penalty_natural_text() {
        // Natural language typically has entropy near 4.0 (the center).
        // The penalty should be close to 1.0 for text near the center.
        let penalty = StorageState::entropy_penalty("the quick brown fox jumps over the lazy dog");
        assert!(penalty > 0.5, "Natural text should have penalty > 0.5, got {}", penalty);
    }

    #[test]
    fn test_entropy_penalty_repetitive() {
        // Very low entropy text -> should be penalized (low penalty value)
        let penalty = StorageState::entropy_penalty("aaaaaaaaaa");
        assert!(penalty < 0.1, "Repetitive text should have penalty < 0.1, got {}", penalty);
    }

    #[test]
    fn test_entropy_penalty_empty() {
        // Empty string has entropy 0.0, far from center of 4.0 -> heavily penalized
        let penalty = StorageState::entropy_penalty("");
        assert!(penalty < 0.05, "Empty string should have very low penalty, got {}", penalty);
    }
}
