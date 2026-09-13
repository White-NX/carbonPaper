//! Category scoring and feedback rules, independent of IPC and storage.
//! Synthetic fixtures record the retired Python classifier's observable behavior.

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::future::Future;
use std::sync::LazyLock;

pub(crate) type Anchors = IndexMap<String, Vec<Anchor>>;
type Scores = IndexMap<String, f64>;

fn default_source() -> String {
    "default".into()
}
fn default_scope() -> String {
    "global".into()
}
fn default_weight() -> f64 {
    1.0
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Anchor {
    pub text: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub process_name: Option<String>,
    #[serde(default = "now")]
    pub added_at: String,
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl Anchor {
    pub fn new(
        text: String,
        source: &str,
        weight: f64,
        scope: &str,
        process: Option<String>,
    ) -> Self {
        Self {
            text,
            source: source.into(),
            weight,
            scope: scope.into(),
            process_name: process,
            added_at: now(),
            extra: IndexMap::new(),
        }
    }
}

#[derive(Deserialize)]
struct Defaults {
    anchors: IndexMap<String, Vec<String>>,
    process_prior: IndexMap<String, String>,
}
static DEFAULTS: LazyLock<Defaults> = LazyLock::new(|| {
    serde_json::from_str(include_str!("defaults.json")).expect("embedded classification defaults")
});

pub(crate) fn validate_anchors(anchors: &Anchors) -> Result<(), String> {
    for (category, rows) in anchors {
        if category.trim().is_empty() || category.len() > 256 {
            return Err("invalid anchor category".into());
        }
        for anchor in rows {
            if !anchor.weight.is_finite() || anchor.weight < 0.0 || anchor.weight > f32::MAX as f64
            {
                return Err("invalid anchor weight".into());
            }
            if anchor.text.len() > crate::ml_protocol::MAX_SEMANTIC_TEXT_ITEM_BYTES {
                return Err("anchor text exceeds embedding limit".into());
            }
        }
    }
    Ok(())
}

/// Import both string-only and structured legacy anchors without writing the
/// source file. Invalid input fails the import, leaving it available for repair.
pub(crate) fn import_anchors(contents: Option<&str>) -> Result<Anchors, String> {
    let mut anchors = Anchors::new();
    if let Some(contents) = contents {
        let raw: IndexMap<String, Vec<Value>> =
            serde_json::from_str(contents).map_err(|e| format!("invalid anchors.json: {e}"))?;
        for (category, entries) in raw {
            let mut rows = Vec::new();
            for entry in entries {
                if let Some(text) = entry.as_str() {
                    rows.push(Anchor::new(text.into(), "default", 1.0, "global", None));
                } else if entry.is_object() && entry.get("text").is_some() {
                    let anchor: Anchor = serde_json::from_value(entry)
                        .map_err(|e| format!("invalid legacy anchor: {e}"))?;
                    rows.push(anchor);
                }
            }
            anchors.insert(category, rows);
        }
    }
    for (category, texts) in &DEFAULTS.anchors {
        let rows = anchors.entry(category.clone()).or_default();
        for text in texts {
            if !rows.iter().any(|anchor| anchor.text.trim() == text.trim()) {
                rows.push(Anchor::new(text.clone(), "default", 1.0, "global", None));
            }
        }
    }
    validate_anchors(&anchors)?;
    Ok(anchors)
}

pub(crate) trait TextEncoder: Send {
    fn encode(
        &mut self,
        texts: Vec<String>,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>, String>> + Send;
}

#[derive(Clone)]
struct IndexedAnchor {
    category: String,
    anchor: Anchor,
    vector: Vec<f32>,
}

pub(crate) struct Classifier {
    pub anchors: Anchors,
    index: Option<Vec<IndexedAnchor>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Input {
    pub title: String,
    pub ocr_text: String,
    pub process_name: String,
}

impl Drop for Input {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.title.zeroize();
        self.ocr_text.zeroize();
        self.process_name.zeroize();
    }
}

#[derive(Debug)]
pub(crate) struct Classification {
    pub category: String,
    pub score: f64,
    pub debug: Value,
}

pub(crate) fn round_score(value: f64) -> f64 {
    (value * 10_000.0).round_ties_even() / 10_000.0
}
fn prefix(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}
fn normalize_process(process: &str) -> String {
    process.trim().to_lowercase()
}

static BROWSER_PATTERNS: LazyLock<IndexMap<&'static str, Regex>> = LazyLock::new(|| {
    let mut patterns = IndexMap::new();
    for (process, name) in [
        (
            "msedge.exe",
            "Microsoft[\\u{200b}\\u{200c}\\u{200d}\\u{feff}]*\\s*Edge",
        ),
        (
            "chrome.exe",
            "Google[\\u{200b}\\u{200c}\\u{200d}\\u{feff}]*\\s*Chrome",
        ),
        (
            "firefox.exe",
            "Mozilla[\\u{200b}\\u{200c}\\u{200d}\\u{feff}]*\\s*Firefox",
        ),
        ("brave.exe", "Brave"),
        ("opera.exe", "Opera"),
        ("vivaldi.exe", "Vivaldi"),
    ] {
        patterns.insert(
            process,
            Regex::new(&format!(r"(?i)\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?{name}.*$")).unwrap(),
        );
    }
    patterns
});

pub(crate) fn strip_app_suffix(text: &str, process: &str) -> String {
    let process = normalize_process(process);
    let Some(pattern) = BROWSER_PATTERNS.get(process.as_str()) else {
        return text.into();
    };
    let cleaned = pattern.replace(text, "");
    if cleaned.trim().chars().count() < 2 {
        text.into()
    } else {
        cleaned.trim().into()
    }
}

pub(crate) fn clean_ocr(text: &str) -> String {
    static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\d:./\\-]+$").unwrap());
    prefix(
        &text
            .split_whitespace()
            .filter(|token| token.chars().count() > 1 && !NUMERIC.is_match(token))
            .take(24)
            .collect::<Vec<_>>()
            .join(" "),
        200,
    )
}

fn informative(text: &str) -> bool {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().count() >= 8
        && text.chars().filter(|c| c.is_alphanumeric()).count() >= 4
        && text.chars().collect::<HashSet<_>>().len() >= 4
}

fn dot(left: &[f32], right: &[f32]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>() as f64
}

fn validate_vectors(
    vectors: &[Vec<f32>],
    count: usize,
    expected: Option<usize>,
) -> Result<(), String> {
    let dimensions = expected
        .or_else(|| vectors.first().map(Vec::len))
        .unwrap_or(0);
    if vectors.len() != count
        || dimensions == 0
        || vectors
            .iter()
            .any(|row| row.len() != dimensions || row.iter().any(|v| !v.is_finite()))
    {
        return Err("invalid BGE classification vectors".into());
    }
    Ok(())
}

fn best(scores: &Scores) -> (&str, f64) {
    scores.iter().fold(
        ("未分类", f64::NEG_INFINITY),
        |winner, (category, score)| {
            if *score > winner.1 {
                (category.as_str(), *score)
            } else {
                winner
            }
        },
    )
}

fn blend(local: &Scores, global: &Scores, categories: impl Iterator<Item = String>) -> Scores {
    let categories: Vec<_> = categories.collect();
    let diversity = categories
        .iter()
        .filter(|c| local.get(*c).copied().unwrap_or(0.0) > 0.0)
        .count();
    let boost = match diversity {
        0 => 0.0,
        1 => 0.15,
        2 | 3 => 0.30,
        _ => 0.50,
    };
    categories
        .into_iter()
        .map(|cat| {
            let score = global.get(&cat).copied().unwrap_or(0.0)
                + boost * local.get(&cat).copied().unwrap_or(0.0);
            (cat, score)
        })
        .collect()
}

fn top(scores: &Scores, count: usize) -> Value {
    let mut rows: Vec<_> = scores.iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(a.1));
    json!(rows
        .into_iter()
        .take(count)
        .map(|(category, score)| json!({"category":category,"score":round_score(*score)}))
        .collect::<Vec<_>>())
}

impl Classifier {
    pub fn new(anchors: Anchors) -> Result<Self, String> {
        validate_anchors(&anchors)?;
        Ok(Self {
            anchors,
            index: None,
        })
    }

    async fn ensure_index<E: TextEncoder>(&mut self, encoder: &mut E) -> Result<(), String> {
        if self.index.is_some() {
            return Ok(());
        }
        let flat: Vec<_> = self
            .anchors
            .iter()
            .flat_map(|(category, anchors)| {
                anchors
                    .iter()
                    .filter(|a| !a.text.trim().is_empty())
                    .map(move |a| (category.clone(), a.clone()))
            })
            .collect();
        let mut index = Vec::with_capacity(flat.len());
        let mut dimensions = None;
        // Bound both the native protocol batch and total request bytes.
        for chunk in flat.chunks(crate::ml_protocol::MAX_SEMANTIC_BATCH) {
            let texts = chunk.iter().map(|(_, a)| a.text.clone()).collect();
            let vectors = encoder.encode(texts).await?;
            validate_vectors(&vectors, chunk.len(), dimensions)?;
            dimensions = Some(vectors[0].len());
            index.extend(
                chunk
                    .iter()
                    .cloned()
                    .zip(vectors)
                    .map(|((category, anchor), vector)| IndexedAnchor {
                        category,
                        anchor,
                        vector,
                    }),
            );
        }
        self.index = Some(index);
        Ok(())
    }

    async fn encode_one<E: TextEncoder>(
        &self,
        encoder: &mut E,
        text: &str,
    ) -> Result<Vec<f32>, String> {
        let vectors = encoder.encode(vec![text.into()]).await?;
        let dimensions = self
            .index
            .as_ref()
            .and_then(|rows| rows.first())
            .map(|row| row.vector.len());
        validate_vectors(&vectors, 1, dimensions)?;
        Ok(vectors.into_iter().next().unwrap())
    }

    fn score(&self, query: &[f32], process: &str, local: bool) -> Scores {
        let process = normalize_process(process);
        let mut winners: IndexMap<String, (f64, f32)> = IndexMap::new();
        for row in self.index.as_ref().expect("built anchor index") {
            let is_local = row.anchor.scope.to_lowercase() == "local";
            if local != is_local
                || (local
                    && (process.is_empty()
                        || normalize_process(row.anchor.process_name.as_deref().unwrap_or(""))
                            != process))
            {
                continue;
            }
            let cosine = dot(&row.vector, query);
            if cosine < 0.3 {
                continue;
            }
            let candidate = winners
                .entry(row.category.clone())
                .or_insert((cosine, row.anchor.weight as f32));
            // Python's stable raw-cosine ordering gives ties to the first anchor.
            if cosine > candidate.0 {
                *candidate = (cosine, row.anchor.weight as f32);
            }
        }
        let mut scores: Scores = winners
            .into_iter()
            .map(|(cat, (cosine, weight))| (cat, cosine + 0.05 * (f64::from(weight) - 1.0)))
            .collect();
        for category in self.anchors.keys() {
            scores.entry(category.clone()).or_insert(0.0);
        }
        scores
    }

    pub async fn classify<E: TextEncoder>(
        &mut self,
        encoder: &mut E,
        input: &Input,
        debug: bool,
    ) -> Result<Classification, String> {
        self.ensure_index(encoder).await?;
        let empty = |reason| Classification {
            category: "未分类".into(),
            score: 0.0,
            debug: json!({"category":"未分类","category_confidence":0.0,"reason":reason}),
        };
        if self.index.as_ref().unwrap().is_empty() {
            return Ok(empty("empty_anchor_index"));
        }
        let title = if debug {
            input.title.trim()
        } else {
            &input.title
        };
        let ocr = if debug {
            input.ocr_text.trim()
        } else {
            &input.ocr_text
        };
        if title.trim().is_empty() && ocr.trim().is_empty() {
            return Ok(empty("empty_input"));
        }
        let title = if title.trim().is_empty() {
            prefix(ocr, 200)
        } else {
            title.to_string()
        };
        let clean_title = strip_app_suffix(&title, &input.process_name);
        // Preserve the diagnostic's full-title local channel and the production
        // classifier's cleaned-title channel as recorded by the parity fixtures.
        let local_text = if debug { &title } else { &clean_title };
        let local_vector = self.encode_one(encoder, local_text).await?;
        let global_vector = if local_text == &clean_title {
            local_vector.clone()
        } else {
            self.encode_one(encoder, &clean_title).await?
        };
        let local = self.score(&local_vector, &input.process_name, true);
        let global = self.score(&global_vector, &input.process_name, false);
        let mut scores = blend(&local, &global, self.anchors.keys().cloned());
        let (category, score) = best(&scores);
        let (local_category, local_score) = best(&local);
        let veto = local_score >= 0.5 && local_category == category;
        let needs_ocr = score < 0.55 && !ocr.trim().is_empty();
        let used_ocr = needs_ocr && !veto;
        if used_ocr {
            let snippet = clean_ocr(ocr);
            let snippet = if snippet.is_empty() {
                prefix(ocr, 200)
            } else {
                snippet
            };
            let vector = self.encode_one(encoder, &snippet).await?;
            let ocr_local = self.score(&vector, &input.process_name, true);
            let ocr_global = self.score(&vector, &input.process_name, false);
            let ocr_scores = blend(&ocr_local, &ocr_global, self.anchors.keys().cloned());
            for (category, score) in &mut scores {
                *score = 0.8 * *score + 0.2 * ocr_scores.get(category).copied().unwrap_or(0.0);
            }
        }
        let prior = DEFAULTS
            .process_prior
            .get(&normalize_process(&input.process_name));
        if let Some(score) = prior.and_then(|category| scores.get_mut(category)) {
            *score += 0.12;
        }
        let (category, score) = best(&scores);
        if !score.is_finite() || score < 0.0 {
            return Err("invalid weighted classification score".into());
        }
        let category = if score < 0.38 { "未分类" } else { category }.to_string();
        let local_categories: Vec<_> = self
            .anchors
            .keys()
            .filter(|cat| local.get(*cat).copied().unwrap_or(0.0) > 0.0)
            .collect();
        let details = json!({"category":category,"category_confidence":round_score(score),
            "used_ocr":used_ocr,"local_veto_active":needs_ocr && veto,"process_prior_applied":prior,
            "process_name":input.process_name,"cleaned_title":clean_title,
            "top_scores":top(&scores,5),"local_diversity":local_categories.len(),"local_categories":local_categories,
            "local_top":top(&local,3),"global_top":top(&global,3)});
        Ok(Classification {
            category,
            score,
            debug: details,
        })
    }

    fn duplicate(&self, category: &str, vector: &[f32], local: bool, process: &str) -> bool {
        let process = normalize_process(process);
        self.index.as_ref().is_some_and(|rows| {
            rows.iter().any(|row| {
                row.category == category
                    && (row.anchor.scope.to_lowercase() == "local") == local
                    && (!local
                        || normalize_process(row.anchor.process_name.as_deref().unwrap_or(""))
                            == process)
                    && dot(&row.vector, vector) > 0.95
            })
        })
    }

    fn remove_text(
        anchors: &mut Anchors,
        category: &str,
        text: &str,
        scope: &str,
        process: &str,
    ) -> bool {
        let Some(rows) = anchors.get_mut(category) else {
            return false;
        };
        let before = rows.len();
        rows.retain(|a| {
            !(a.text == text
                && a.scope.to_lowercase() == scope
                && (scope != "local"
                    || normalize_process(a.process_name.as_deref().unwrap_or(""))
                        == normalize_process(process)))
        });
        let removed = rows.len() != before;
        if rows.is_empty() {
            anchors.shift_remove(category);
        }
        removed
    }

    pub async fn learn<E: TextEncoder>(
        &mut self,
        encoder: &mut E,
        category: &str,
        input: &Input,
        old_category: Option<&str>,
    ) -> Result<Value, String> {
        let mut result = json!({"title_local_added":false,"title_global_added":false,"title_local_dedup":false,
            "title_global_dedup":false,"ocr_local_added":false,"ocr_global_added":false,"ocr_local_dedup":false,
            "ocr_global_dedup":false,"negative_removed":false});
        if input.title.trim().is_empty() {
            return Ok(result);
        }
        self.ensure_index(encoder).await?;
        let mut anchors = self.anchors.clone();
        let process = input.process_name.trim();
        if let Some(old) = old_category.filter(|old| *old != category && *old != "未分类") {
            let removed = (!process.is_empty()
                && Self::remove_text(&mut anchors, old, &input.title, "local", process))
                || Self::remove_text(&mut anchors, old, &input.title, "global", process);
            result["negative_removed"] = json!(removed);
        }
        let title_vector = self.encode_one(encoder, &input.title).await?;
        let clean_title = strip_app_suffix(&input.title, process);
        let clean_vector = if clean_title == input.title {
            title_vector.clone()
        } else {
            self.encode_one(encoder, &clean_title).await?
        };
        if !process.is_empty() {
            if self.duplicate(category, &title_vector, true, process) {
                result["title_local_dedup"] = json!(true);
            } else {
                anchors
                    .entry(category.into())
                    .or_default()
                    .push(Anchor::new(
                        input.title.clone(),
                        "user_feedback",
                        2.0,
                        "local",
                        Some(process.into()),
                    ));
                result["title_local_added"] = json!(true);
            }
        }
        if informative(&clean_title) {
            if self.duplicate(category, &clean_vector, false, "") {
                result["title_global_dedup"] = json!(true);
            } else {
                anchors
                    .entry(category.into())
                    .or_default()
                    .push(Anchor::new(
                        clean_title,
                        "user_feedback",
                        2.0,
                        "global",
                        None,
                    ));
                result["title_global_added"] = json!(true);
            }
        }
        if input.ocr_text.trim().chars().count() >= 20 {
            let snippet = clean_ocr(&input.ocr_text);
            let snippet = if snippet.is_empty() {
                prefix(input.ocr_text.trim(), 200)
            } else {
                snippet
            };
            let vector = self.encode_one(encoder, &snippet).await?;
            if dot(&title_vector, &vector) < 0.7 && !process.is_empty() {
                if self.duplicate(category, &vector, true, process) {
                    result["ocr_local_dedup"] = json!(true);
                } else {
                    anchors
                        .entry(category.into())
                        .or_default()
                        .push(Anchor::new(
                            snippet,
                            "ocr_feedback",
                            1.5,
                            "local",
                            Some(process.into()),
                        ));
                    result["ocr_local_added"] = json!(true);
                }
            }
        }
        validate_anchors(&anchors)?;
        self.anchors = anchors;
        self.index = None;
        Ok(result)
    }

    pub fn remove_local(&mut self, category: &str, process: &str) -> usize {
        if process.trim().is_empty() {
            return 0;
        }
        let Some(rows) = self.anchors.get_mut(category) else {
            return 0;
        };
        let before = rows.len();
        rows.retain(|a| {
            !(a.scope.to_lowercase() == "local"
                && normalize_process(a.process_name.as_deref().unwrap_or(""))
                    == normalize_process(process))
        });
        let removed = before - rows.len();
        if rows.is_empty() {
            self.anchors.shift_remove(category);
        }
        if removed > 0 {
            self.index = None;
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FixtureEncoder(IndexMap<String, Vec<f32>>);
    impl TextEncoder for FixtureEncoder {
        async fn encode(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
            texts
                .iter()
                .map(|text| {
                    self.0
                        .get(text)
                        .cloned()
                        .ok_or_else(|| format!("unexpected embedding input: {text}"))
                })
                .collect()
        }
    }
    fn assert_json_close(actual: &Value, expected: &Value) {
        match (actual, expected) {
            (Value::Number(a), Value::Number(b)) => assert!(
                (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 0.00011,
                "{actual} != {expected}"
            ),
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b) {
                    assert_json_close(a, b);
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len());
                for (key, value) in b {
                    assert_json_close(&a[key], value);
                }
            }
            _ => assert_eq!(actual, expected),
        }
    }
    #[derive(Deserialize)]
    struct Case {
        anchors: Anchors,
        vectors: IndexMap<String, Vec<f32>>,
        title: String,
        ocr_text: String,
        process_name: String,
        category: String,
        score: f64,
        debug: Value,
    }
    #[derive(Deserialize)]
    struct LearningCase {
        anchors: Anchors,
        vectors: IndexMap<String, Vec<f32>>,
        title: String,
        ocr_text: String,
        process_name: String,
        first: Value,
        second: Value,
        result: Value,
    }
    #[derive(Deserialize)]
    struct Fixtures {
        classification: Vec<Case>,
        learning: Vec<LearningCase>,
    }
    #[tokio::test]
    async fn production_and_debug_match_frozen_python_results() {
        let fixtures: Fixtures =
            serde_json::from_str(include_str!("fixtures/parity.json")).unwrap();
        for (i, case) in fixtures.classification.into_iter().enumerate() {
            let mut classifier = Classifier::new(case.anchors).unwrap();
            let mut encoder = FixtureEncoder(case.vectors);
            let input = Input {
                title: case.title,
                ocr_text: case.ocr_text,
                process_name: case.process_name,
            };
            let result = classifier
                .classify(&mut encoder, &input, false)
                .await
                .unwrap();
            assert_eq!(result.category, case.category, "case {i}");
            assert!(
                (result.score - case.score).abs() < 0.00001,
                "case {i}: {}",
                result.score
            );
            let debug = classifier
                .classify(&mut encoder, &input, true)
                .await
                .unwrap();
            assert_json_close(&debug.debug, &case.debug);
        }
    }
    #[tokio::test]
    async fn feedback_matches_frozen_negative_dedup_and_ocr_sequences() {
        let fixtures: Fixtures =
            serde_json::from_str(include_str!("fixtures/parity.json")).unwrap();
        for case in fixtures.learning {
            let mut classifier = Classifier::new(case.anchors).unwrap();
            let mut encoder = FixtureEncoder(case.vectors);
            let input = Input {
                title: case.title,
                ocr_text: case.ocr_text,
                process_name: case.process_name,
            };
            assert_eq!(
                classifier
                    .learn(&mut encoder, "新类别", &input, Some("旧类别"))
                    .await
                    .unwrap(),
                case.first
            );
            assert_eq!(
                classifier
                    .learn(&mut encoder, "新类别", &input, Some("旧类别"))
                    .await
                    .unwrap(),
                case.second
            );
            let mut result = serde_json::to_value(classifier.anchors).unwrap();
            for anchors in result.as_object_mut().unwrap().values_mut() {
                for anchor in anchors.as_array_mut().unwrap() {
                    anchor.as_object_mut().unwrap().remove("added_at");
                }
            }
            assert_json_close(&result, &case.result);
        }
    }
    #[test]
    fn legacy_formats_preserve_scopes_weights_and_unknown_metadata() {
        let anchors=import_anchors(Some(r#"{"custom":["legacy",{"text":"learned","source":"user_feedback","weight":2,"scope":"local","process_name":"QQ.exe","added_at":"old","extra_key":"kept"}]}"#)).unwrap();
        assert_eq!(anchors["custom"][0].source, "default");
        let learned = &anchors["custom"][1];
        assert_eq!(learned.weight, 2.0);
        assert_eq!(learned.added_at, "old");
        assert_eq!(learned.extra["extra_key"], json!("kept"));
        assert!(import_anchors(Some("invalid json")).is_err());
    }
    #[test]
    fn browser_cleanup_preserves_unicode_and_non_browser_titles() {
        assert_eq!(
            strip_app_suffix("Python教程 - Google Chrome", "chrome.exe"),
            "Python教程"
        );
        assert_eq!(
            strip_app_suffix(
                "bilibili视频 - 个人 - Microsoft\u{200b} Edge Beta",
                "msedge.exe"
            ),
            "bilibili视频"
        );
        assert_eq!(
            strip_app_suffix("记事本 - foo.txt", "notepad.exe"),
            "记事本 - foo.txt"
        );
    }

    #[tokio::test]
    async fn strong_local_agreement_vetoes_ocr_and_process_prior_is_additive() {
        let anchors=serde_json::from_str(r#"{"社交通讯":[{"text":"global"},{"text":"local","scope":"local","process_name":"qq.exe"}],"编程开发":[{"text":"code"}]}"#).unwrap();
        let mut classifier = Classifier::new(anchors).unwrap();
        let mut encoder = FixtureEncoder(IndexMap::from([
            ("global".into(), vec![0.35, 0.93675]),
            ("local".into(), vec![0.5, 0.866]),
            ("code".into(), vec![0.3, 0.954]),
            ("query".into(), vec![1.0, 0.0]),
        ]));
        // No OCR vector exists in this encoder: using that channel is a failure.
        let input = Input {
            title: "query".into(),
            ocr_text: "should not be encoded".into(),
            process_name: " QQ.EXE ".into(),
        };
        let result = classifier
            .classify(&mut encoder, &input, true)
            .await
            .unwrap();
        assert_eq!(result.category, "社交通讯");
        assert!((result.score - 0.545).abs() < 0.00001);
        assert_eq!(result.debug["local_veto_active"], true);
        assert_eq!(result.debug["used_ocr"], false);
    }

    #[tokio::test]
    async fn inference_failure_does_not_apply_partial_negative_feedback() {
        let anchors: Anchors = serde_json::from_str(
            r#"{"old":[{"text":"query","scope":"local","process_name":"app.exe"}]}"#,
        )
        .unwrap();
        let mut classifier = Classifier::new(anchors.clone()).unwrap();
        let mut encoder = FixtureEncoder(IndexMap::new());
        let input = Input {
            title: "query".into(),
            ocr_text: String::new(),
            process_name: "app.exe".into(),
        };
        assert!(classifier
            .learn(&mut encoder, "new", &input, Some("old"))
            .await
            .is_err());
        assert_eq!(classifier.anchors, anchors);
        assert!(validate_vectors(&[vec![f32::NAN]], 1, Some(1)).is_err());
        assert!(validate_vectors(&[vec![1.0, 0.0]], 1, Some(3)).is_err());
    }

    #[test]
    fn process_priors_only_reference_default_categories() {
        for (process, category) in &DEFAULTS.process_prior {
            assert_eq!(*process, process.to_lowercase());
            assert!(process.ends_with(".exe"));
            assert!(DEFAULTS.anchors.contains_key(category));
        }
    }
}
