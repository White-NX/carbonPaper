use std::{
    env, fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use image::{Rgb, RgbImage};

use serde::Deserialize;

use crate::{
    cancellation::{is_cancelled_error, OcrCancellationToken},
    config::{PipelineConfig, RapidOcrConfig},
    model::{model_set_by_name, ModelCache},
    types::OcrLine,
    RapidOcr,
};

#[cfg(feature = "tokio")]
use crate::tokio::{TokioOcrError, TokioRapidOcr};
#[cfg(feature = "tokio")]
use ::tokio as tokio_runtime;
#[cfg(feature = "tokio")]
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct E2eFixture {
    source: String,
    image: String,
    #[serde(default)]
    use_cls: Option<bool>,
    #[serde(default)]
    pipeline: Option<PipelineConfig>,
    #[serde(default)]
    tolerances: E2eTolerances,
    lines: Vec<ExpectedLine>,
}

impl E2eFixture {
    fn pipeline(&self) -> PipelineConfig {
        let mut pipeline = self.pipeline.unwrap_or_default();
        if let Some(use_cls) = self.use_cls {
            pipeline.use_cls = use_cls;
        }
        pipeline
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct E2eTolerances {
    #[serde(default = "default_min_exact_text_ratio")]
    min_exact_text_ratio: f32,
    #[serde(default = "default_min_char_accuracy")]
    min_char_accuracy: f32,
    #[serde(default = "default_max_mean_score_delta")]
    max_mean_score_delta: f32,
    #[serde(default = "default_max_mean_center_delta")]
    max_mean_center_delta: f32,
    #[serde(default = "default_max_mean_corner_delta")]
    max_mean_corner_delta: f32,
}

impl Default for E2eTolerances {
    fn default() -> Self {
        Self {
            min_exact_text_ratio: default_min_exact_text_ratio(),
            min_char_accuracy: default_min_char_accuracy(),
            max_mean_score_delta: default_max_mean_score_delta(),
            max_mean_center_delta: default_max_mean_center_delta(),
            max_mean_corner_delta: default_max_mean_corner_delta(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ExpectedLine {
    bbox: [[f32; 2]; 4],
    text: String,
    score: f32,
}

#[test]
#[ignore]
fn e2e_output_tracks_golden_metrics() {
    let rs_root = rapidocr_rs_root();
    let model_dir = rs_root.join("models");
    let python_repo = python_repo_root(&rs_root);
    let fixture_root = e2e_fixture_root(&rs_root);
    let mut executed = 0usize;

    for fixture_path in fixture_files(&fixture_root) {
        let fixture: E2eFixture =
            serde_json::from_str(&fs::read_to_string(&fixture_path).unwrap()).unwrap();
        let pipeline = fixture.pipeline();
        if !models_available(&model_dir, pipeline) {
            eprintln!(
                "skipping e2e fixture {} because required models are missing under {}",
                fixture_path.display(),
                model_dir.display()
            );
            continue;
        }

        let mut cfg = RapidOcrConfig::ppocr_v6_small(&model_dir);
        cfg.pipeline = pipeline;
        let mut ocr = RapidOcr::new(cfg).unwrap();
        let actual = ocr
            .run_path(resolve_python_asset(&python_repo, &fixture.image))
            .unwrap();
        let pairs = match_by_nearest_center(&actual.lines, &fixture.lines);
        let metrics = E2eMetrics::from_pairs(&actual.lines, &fixture.lines, &pairs);

        println!(
            "e2e metrics [{}:{}]: {metrics:#?}",
            fixture.source,
            fixture_path.file_name().unwrap().to_string_lossy()
        );

        assert_eq!(
            metrics.actual_count,
            metrics.expected_count,
            "line count mismatch for {}",
            fixture_path.display()
        );
        assert_eq!(
            metrics.matched_count,
            metrics.expected_count,
            "matched line count mismatch for {}",
            fixture_path.display()
        );

        if metrics.expected_count > 0 {
            if pipeline.use_rec {
                assert!(
                    metrics.exact_text_ratio >= fixture.tolerances.min_exact_text_ratio
                        || metrics.char_accuracy >= 0.99,
                    "text parity too low for {}: exact_text_ratio={}, char_accuracy={}",
                    fixture_path.display(),
                    metrics.exact_text_ratio,
                    metrics.char_accuracy
                );
                assert!(
                    metrics.char_accuracy >= fixture.tolerances.min_char_accuracy,
                    "char accuracy too low for {}: {}",
                    fixture_path.display(),
                    metrics.char_accuracy
                );
                assert!(
                    metrics.mean_score_delta < fixture.tolerances.max_mean_score_delta,
                    "mean score delta too high for {}: {}",
                    fixture_path.display(),
                    metrics.mean_score_delta
                );
            }
            assert!(
                metrics.mean_center_delta < fixture.tolerances.max_mean_center_delta,
                "mean center delta too high for {}: {}",
                fixture_path.display(),
                metrics.mean_center_delta
            );
            assert!(
                metrics.mean_corner_delta < fixture.tolerances.max_mean_corner_delta,
                "mean corner delta too high for {}: {}",
                fixture_path.display(),
                metrics.mean_corner_delta
            );
        }

        executed += 1;
    }

    assert!(
        executed > 0,
        "no e2e fixtures were executed; place models under {}",
        model_dir.display()
    );
}

#[test]
#[ignore]
fn non_default_model_sets_run_rec_only_smoke() {
    let rs_root = rapidocr_rs_root();
    let model_dir = rs_root.join("models");
    let python_repo = python_repo_root(&rs_root);
    let cache = ModelCache::new(&model_dir);
    let pipeline = PipelineConfig::recognition_only();

    for model_set_name in ["ppocrv4-en-mobile", "ppocrv5-en-mobile"] {
        let model_set = model_set_by_name(model_set_name).unwrap();
        let missing = cache.missing_assets_for_pipeline(model_set, pipeline);
        assert!(
            missing.is_empty(),
            "model matrix smoke for {model_set_name} requires missing assets under {}: {:?}",
            model_dir.display(),
            missing
                .iter()
                .map(|asset| asset.filename)
                .collect::<Vec<_>>()
        );

        let mut cfg = cache.config_for(model_set);
        cfg.pipeline = pipeline;
        let mut ocr = RapidOcr::new(cfg).unwrap();
        let output = ocr
            .run_path(resolve_python_asset(
                &python_repo,
                "python/tests/test_files/en_rec.jpg",
            ))
            .unwrap();

        assert_eq!(
            output.lines.len(),
            1,
            "{model_set_name} should recognize the rec-only English fixture as one line"
        );
        assert!(
            !output.lines[0].text.trim().is_empty(),
            "{model_set_name} produced empty rec-only text"
        );
        assert!(
            output.lines[0].score > 0.5,
            "{model_set_name} produced unexpectedly low rec-only score: {}",
            output.lines[0].score
        );
    }
}

#[test]
#[ignore]
fn cancellation_terminates_onnx_run_and_session_remains_reusable() {
    let mut ocr = cancellable_detection_engine();
    let cancellation = OcrCancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let large_image = cancellation_stress_image();

    let worker = thread::spawn(move || {
        let result = ocr.run_image_cancellable_timed(&large_image, &worker_cancellation);
        (ocr, result)
    });
    wait_for_active_onnx_run(&cancellation);
    cancellation.cancel();

    let (mut ocr, cancelled) = worker.join().unwrap();
    let error = cancelled.unwrap_err();
    assert!(is_cancelled_error(&error), "unexpected error: {error:#}");

    let small_image = RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]));
    ocr.run_image(&small_image)
        .expect("session should remain reusable after cancellation");
}

#[cfg(feature = "tokio")]
#[tokio_runtime::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn tokio_timeout_cancels_onnx_run_and_worker_remains_reusable() {
    let config = cancellable_detection_config();
    let ocr = TokioRapidOcr::new(config).await.unwrap();
    let large_image = Arc::new(cancellation_stress_image());

    let error = ocr
        .run_image_with_timeout(large_image, Duration::from_millis(25))
        .await
        .unwrap_err();
    assert!(matches!(error, TokioOcrError::TimedOut(_)));

    let small_image = Arc::new(RgbImage::from_pixel(64, 64, Rgb([255, 255, 255])));
    ocr.run_image(small_image)
        .await
        .expect("Tokio worker should remain reusable after timeout");
    ocr.shutdown().await.unwrap();
}

fn cancellable_detection_engine() -> RapidOcr {
    RapidOcr::new(cancellable_detection_config()).unwrap()
}

fn cancellable_detection_config() -> RapidOcrConfig {
    let rs_root = rapidocr_rs_root();
    let model_dir = rs_root.join("models");
    let cache = ModelCache::new(&model_dir);
    let model_set = model_set_by_name("ppocrv5-ch-mobile").unwrap();
    let pipeline = PipelineConfig::detection_only();
    let missing = cache.missing_assets_for_pipeline(model_set, pipeline);
    assert!(
        missing.is_empty(),
        "cancellation tests require missing assets under {}: {:?}",
        model_dir.display(),
        missing
            .iter()
            .map(|asset| asset.filename)
            .collect::<Vec<_>>()
    );
    let mut config = cache.config_for(model_set);
    config.pipeline = pipeline;
    config.inference.intra_threads = 2;
    config
}

fn cancellation_stress_image() -> RgbImage {
    RgbImage::from_fn(2000, 2000, |x, y| {
        let value = ((x.wrapping_mul(31) + y.wrapping_mul(17)) % 255) as u8;
        Rgb([value, value.wrapping_add(53), value.wrapping_add(107)])
    })
}

fn wait_for_active_onnx_run(cancellation: &OcrCancellationToken) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cancellation.has_active_onnx_run() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for ONNX Runtime inference to start"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn default_min_exact_text_ratio() -> f32 {
    0.85
}

fn default_min_char_accuracy() -> f32 {
    0.96
}

fn default_max_mean_score_delta() -> f32 {
    0.08
}

fn default_max_mean_center_delta() -> f32 {
    5.0
}

fn default_max_mean_corner_delta() -> f32 {
    6.0
}

#[derive(Debug)]
struct E2eMetrics {
    actual_count: usize,
    expected_count: usize,
    matched_count: usize,
    exact_text_ratio: f32,
    char_accuracy: f32,
    mean_score_delta: f32,
    mean_center_delta: f32,
    mean_corner_delta: f32,
}

impl E2eMetrics {
    fn from_pairs(actual: &[OcrLine], expected: &[ExpectedLine], pairs: &[(usize, usize)]) -> Self {
        let exact_text_matches = pairs
            .iter()
            .filter(|(actual_idx, expected_idx)| {
                actual[*actual_idx].text == expected[*expected_idx].text
            })
            .count();
        let exact_text_ratio = exact_text_matches as f32 / expected.len().max(1) as f32;

        let total_expected_chars = pairs
            .iter()
            .map(|(_, expected_idx)| expected[*expected_idx].text.chars().count())
            .sum::<usize>()
            .max(1);
        let total_edit_distance = pairs
            .iter()
            .map(|(actual_idx, expected_idx)| {
                levenshtein_chars(&actual[*actual_idx].text, &expected[*expected_idx].text)
            })
            .sum::<usize>();
        let char_accuracy =
            1.0 - (total_edit_distance as f32 / total_expected_chars as f32).min(1.0);

        let mean_score_delta = pairs
            .iter()
            .map(|(actual_idx, expected_idx)| {
                (actual[*actual_idx].score - expected[*expected_idx].score).abs()
            })
            .sum::<f32>()
            / pairs.len().max(1) as f32;

        let mean_center_delta = pairs
            .iter()
            .map(|(actual_idx, expected_idx)| {
                distance(
                    center(&actual[*actual_idx].bbox.points),
                    center(&expected[*expected_idx].bbox),
                )
            })
            .sum::<f32>()
            / pairs.len().max(1) as f32;

        let mean_corner_delta = pairs
            .iter()
            .map(|(actual_idx, expected_idx)| {
                corner_delta(
                    &actual[*actual_idx].bbox.points,
                    &expected[*expected_idx].bbox,
                )
            })
            .sum::<f32>()
            / pairs.len().max(1) as f32;

        Self {
            actual_count: actual.len(),
            expected_count: expected.len(),
            matched_count: pairs.len(),
            exact_text_ratio,
            char_accuracy,
            mean_score_delta,
            mean_center_delta,
            mean_corner_delta,
        }
    }
}

fn rapidocr_rs_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap()
}

fn python_repo_root(rs_root: &std::path::Path) -> PathBuf {
    if let Some(path) = env::var_os("RAPIDOCR_PYTHON_REPO") {
        return validate_python_repo(PathBuf::from(path));
    }

    let local_config = rs_root.join("config").join("local.toml");
    if local_config.exists() {
        let value = fs::read_to_string(&local_config)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", local_config.display()))
            .parse::<toml::Value>()
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", local_config.display()));
        if let Some(path) = value
            .get("parity")
            .and_then(|parity| parity.get("python_repo"))
            .and_then(|path| path.as_str())
        {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                rs_root.join(path)
            };
            return validate_python_repo(path);
        }
        panic!(
            "{} exists but does not contain [parity].python_repo",
            local_config.display()
        );
    }

    panic!(
        "e2e parity tests require the Python RapidOCR repo. Set RAPIDOCR_PYTHON_REPO or add [parity].python_repo to {}",
        local_config.display()
    );
}

fn validate_python_repo(path: PathBuf) -> PathBuf {
    let path = path.canonicalize().unwrap_or_else(|err| {
        panic!(
            "invalid RAPIDOCR_PYTHON_REPO path {}: {err}",
            path.display()
        )
    });
    let package_dir = path.join("python").join("rapidocr");
    assert!(
        package_dir.is_dir(),
        "Python RapidOCR repo path {} is missing {}",
        path.display(),
        package_dir.display()
    );
    path
}

fn resolve_python_asset(python_repo: &std::path::Path, fixture_image: &str) -> PathBuf {
    python_repo.join(fixture_image)
}

fn e2e_fixture_root(rs_root: &std::path::Path) -> PathBuf {
    env::var_os("RAPIDOCR_E2E_FIXTURE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| rs_root.join("fixtures").join("e2e"))
}

fn fixture_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn models_available(model_dir: &std::path::Path, pipeline: PipelineConfig) -> bool {
    let mut required = Vec::new();
    if pipeline.use_det {
        required.push(model_dir.join("PP-OCRv6_det_small.onnx"));
    }
    if pipeline.use_rec {
        required.push(model_dir.join("PP-OCRv6_rec_small.onnx"));
        required.push(model_dir.join("ppocrv6_dict.txt"));
    }
    if pipeline.use_cls {
        required.push(model_dir.join("ch_ppocr_mobile_v2.0_cls_mobile.onnx"));
    }
    required.iter().all(|path| path.exists())
}

fn center(points: &[[f32; 2]; 4]) -> [f32; 2] {
    let mut x = 0.0;
    let mut y = 0.0;
    for point in points {
        x += point[0];
        y += point[1];
    }
    [x / 4.0, y / 4.0]
}

fn corner_delta(actual: &[[f32; 2]; 4], expected: &[[f32; 2]; 4]) -> f32 {
    actual
        .iter()
        .zip(expected)
        .map(|(a, e)| distance(*a, *e))
        .sum::<f32>()
        / 4.0
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

fn match_by_nearest_center(actual: &[OcrLine], expected: &[ExpectedLine]) -> Vec<(usize, usize)> {
    let mut unmatched_expected = (0..expected.len()).collect::<Vec<_>>();
    let mut pairs = Vec::new();

    for (actual_idx, line) in actual.iter().enumerate() {
        let actual_center = center(&line.bbox.points);
        let Some((best_pos, _)) = unmatched_expected
            .iter()
            .enumerate()
            .map(|(pos, expected_idx)| {
                (
                    pos,
                    distance(actual_center, center(&expected[*expected_idx].bbox)),
                )
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
        else {
            break;
        };
        let expected_idx = unmatched_expected.remove(best_pos);
        pairs.push((actual_idx, expected_idx));
    }

    pairs
}

fn levenshtein_chars(a: &str, b: &str) -> usize {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    let mut prev = (0..=b.len()).collect::<Vec<_>>();
    let mut curr = vec![0; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}
