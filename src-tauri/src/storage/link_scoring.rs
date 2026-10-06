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
    /// frequencies from the blind index, and produces a score:
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
            let hashes: HashSet<String> = tokens
                .iter()
                .map(|t| Self::compute_hmac_hash(t, &hmac_key))
                .collect();
            all_hashes.extend(hashes.iter().cloned());
            link_tokens.push(hashes);
        }

        // Batch-query bitmap cardinalities for all unique token hashes
        let all_hashes_vec: Vec<String> = all_hashes.into_iter().collect();
        let mut df_map: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

        let index = super::blind_index::BlindIndex::open(conn)?;
        for (hash, cardinality) in index.cardinalities(&all_hashes_vec)? {
            df_map.insert(hash, cardinality as f64);
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

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_scores_preserve_idf_when_large_postings_are_promoted() {
        use crate::credential_manager::CredentialManagerState;
        use crate::storage::blind_index::{BlindIndex, MutationStats};
        use roaring::RoaringBitmap;
        use rusqlite::Connection;
        use std::sync::Arc;

        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(temp.path().into()));
        credentials.cache_master_key_for_tests(vec![9; 32]);
        credentials.set_foreground_state(true);
        credentials.update_auth_time();
        let key = credentials.get_hmac_key().unwrap();
        let storage = StorageState::new(temp.path().into(), credentials);
        storage.ocr_row_count.store(100_000, Ordering::Relaxed);
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        let common: RoaringBitmap = (0..70_000).map(|n| n * 37).collect();
        let rare: RoaringBitmap = [7, 9].into_iter().collect();
        let mut common_bytes = Vec::new();
        common.serialize_into(&mut common_bytes).unwrap();
        let mut rare_bytes = Vec::new();
        rare.serialize_into(&mut rare_bytes).unwrap();
        let common_hash = StorageState::compute_hmac_hash("ab", &key);
        let rare_hash = StorageState::compute_hmac_hash("cd", &key);
        for (hash, bytes) in [(&common_hash, &common_bytes), (&rare_hash, &rare_bytes)] {
            conn.execute(
                "INSERT INTO blind_bitmap_inline VALUES(?1,?2)",
                rusqlite::params![hash, bytes],
            )
            .unwrap();
        }
        *storage.db.lock().unwrap() = Some(conn);
        let links = ["ab", "cd", "missing", "https://example.test"].map(|text| VisibleLink {
            text: text.into(),
            url: "https://example.test".into(),
        });
        let before = storage.compute_link_scores(&links).unwrap();
        {
            let guard = storage.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            BlindIndex::open(&tx)
                .unwrap()
                .replace(&common_hash, &common, &mut MutationStats::default())
                .unwrap();
            tx.commit().unwrap();
            assert!(
                BlindIndex::open(conn)
                    .unwrap()
                    .probe(&[common_hash])
                    .unwrap()
                    .postings[0]
                    .chunked
            );
        }
        let after = storage.compute_link_scores(&links).unwrap();
        for (before, after) in before.iter().zip(&after) {
            assert_eq!(before.text, after.text);
            assert!((before.score - after.score).abs() < 1e-12);
        }
    }

    #[test]
    fn character_entropy_matches_known_distributions() {
        for (text, expected) in [("", 0.0), ("aaaa", 0.0), ("ab", 1.0), ("abcd", 2.0)] {
            let actual = StorageState::char_entropy(text);
            assert!(
                (actual - expected).abs() < 0.001,
                "{text:?}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn entropy_penalty_preserves_natural_text_and_suppresses_empty_or_repeated_text() {
        for (text, lower, upper) in [
            ("the quick brown fox jumps over the lazy dog", 0.5, 1.001),
            ("aaaaaaaaaa", -0.001, 0.1),
            ("", -0.001, 0.05),
        ] {
            let penalty = StorageState::entropy_penalty(text);
            assert!(penalty > lower && penalty < upper, "{text:?}: {penalty}");
        }
    }
}
