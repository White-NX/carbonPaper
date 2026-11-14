//! ONNX Runtime OCR pipeline compatible with RapidOCR-style model layouts.
//!
//! The crate exposes a high-level [`RapidOcr`] runner, TOML-compatible
//! configuration in [`config`], model-set registration and cache helpers in
//! [`model`], and output data types in [`types`]. Detection, classification,
//! recognition, image preprocessing, and postprocessing modules are internal
//! implementation details.

pub mod cancellation;
pub(crate) mod cls;
pub mod config;
pub(crate) mod db_postprocess;
pub(crate) mod det;
pub(crate) mod geometry;
pub(crate) mod image_ops;
pub(crate) mod inference;
pub mod model;
pub(crate) mod rec;
pub mod types;

#[cfg(feature = "tokio")]
pub mod tokio;

pub use cancellation::{is_cancelled_error, OcrCancellationToken, OcrCancelled};
#[cfg(feature = "tokio")]
pub use tokio::{OcrTask, TokioOcrError, TokioRapidOcr};

#[cfg(test)]
mod e2e_tests;

use std::{path::Path, time::Instant};

use anyhow::{bail, Context, Result};
use ort::environment::Environment;

use crate::{
    cls::TextClassifier,
    config::{PipelineConfig, RapidOcrConfig},
    det::TextDetector,
    image_ops::{
        apply_vertical_padding, crop_perspective, load_rgb_image, resize_image_within_bounds,
    },
    rec::TextRecognizer,
    types::{OcrLine, OcrOutput, OcrTimings, Quad, TimedOcrOutput},
};

/// Stateful OCR pipeline runner.
///
/// The runner owns ONNX sessions for the enabled stages. Methods take `&mut self`
/// because ONNX Runtime session execution is stateful through the backend wrapper.
pub struct RapidOcr {
    cfg: RapidOcrConfig,
    detector: Option<TextDetector>,
    classifier: Option<TextClassifier>,
    recognizer: Option<TextRecognizer>,
}

impl RapidOcr {
    /// Builds an OCR pipeline from a validated configuration.
    ///
    /// Enabled pipeline stages load their ONNX sessions immediately. Disabled
    /// stages do not require their model files or dictionaries to exist.
    pub fn new(cfg: RapidOcrConfig) -> Result<Self> {
        cfg.validate().context("invalid OCR config")?;
        let environment = ort::init()
            .with_name("rapidocr")
            .build()
            .context("failed to initialize ONNX environment")?;
        Self::new_with_environment(cfg, &environment)
    }

    /// Builds a pipeline sharing the caller's ONNX environment across stages.
    pub fn new_with_environment(cfg: RapidOcrConfig, environment: &Environment) -> Result<Self> {
        cfg.validate().context("invalid OCR config")?;
        let detector = if cfg.pipeline.use_det {
            let det_cfg = cfg
                .det
                .clone()
                .context("pipeline.use_det is true but [det] config is missing")?;
            Some(
                TextDetector::new(det_cfg, cfg.inference, environment)
                    .context("failed to initialize detection stage")?,
            )
        } else {
            None
        };
        let classifier = if cfg.pipeline.use_cls {
            let cls_cfg = cfg
                .cls
                .clone()
                .context("pipeline.use_cls is true but [cls] config is missing")?;
            Some(
                TextClassifier::new(cls_cfg, cfg.inference, environment)
                    .context("failed to initialize classification stage")?,
            )
        } else {
            None
        };
        let recognizer = if cfg.pipeline.use_rec {
            let rec_cfg = cfg
                .rec
                .clone()
                .context("pipeline.use_rec is true but [rec] config is missing")?;
            Some(
                TextRecognizer::new(rec_cfg, cfg.inference, environment)
                    .context("failed to initialize recognition stage")?,
            )
        } else {
            None
        };
        Ok(Self {
            cfg,
            detector,
            classifier,
            recognizer,
        })
    }

    /// Alias for [`RapidOcr::new`].
    pub fn from_config(cfg: RapidOcrConfig) -> Result<Self> {
        Self::new(cfg)
    }

    /// Returns the configuration used to construct this pipeline.
    pub fn config(&self) -> &RapidOcrConfig {
        &self.cfg
    }

    /// Returns the enabled pipeline stages.
    pub fn pipeline(&self) -> PipelineConfig {
        self.cfg.pipeline
    }

    /// Loads an image from disk and returns OCR lines without timing details.
    ///
    /// Image loading applies EXIF orientation and alpha-channel handling before
    /// the OCR pipeline runs.
    pub fn run_path(&mut self, image_path: impl AsRef<Path>) -> Result<OcrOutput> {
        Ok(self.run_path_timed(image_path)?.output)
    }

    /// Loads an image and runs OCR with cooperative cancellation.
    pub fn run_path_cancellable(
        &mut self,
        image_path: impl AsRef<Path>,
        cancellation: &OcrCancellationToken,
    ) -> Result<OcrOutput> {
        Ok(self
            .run_path_cancellable_timed(image_path, cancellation)?
            .output)
    }

    /// Loads an image from disk and returns OCR lines with stage timings.
    pub fn run_path_timed(&mut self, image_path: impl AsRef<Path>) -> Result<TimedOcrOutput> {
        self.run_path_cancellable_timed(image_path, &OcrCancellationToken::new())
    }

    /// Loads an image and runs OCR with timing details and cooperative cancellation.
    pub fn run_path_cancellable_timed(
        &mut self,
        image_path: impl AsRef<Path>,
        cancellation: &OcrCancellationToken,
    ) -> Result<TimedOcrOutput> {
        let total_start = Instant::now();
        cancellation.checkpoint()?;
        let start = Instant::now();
        let image = load_rgb_image(image_path)?;
        let image_load_ms = elapsed_ms(start);
        cancellation.checkpoint()?;
        let mut timed = self.run_image_cancellable_timed(&image, cancellation)?;
        timed.timings.image_load_ms = image_load_ms;
        timed.timings.total_ms = elapsed_ms(total_start);
        Ok(timed)
    }

    /// Runs OCR on an RGB image already loaded by the caller.
    pub fn run_image(&mut self, image: &image::RgbImage) -> Result<OcrOutput> {
        Ok(self.run_image_timed(image)?.output)
    }

    /// Runs OCR on an RGB image with cooperative cancellation.
    pub fn run_image_cancellable(
        &mut self,
        image: &image::RgbImage,
        cancellation: &OcrCancellationToken,
    ) -> Result<OcrOutput> {
        Ok(self
            .run_image_cancellable_timed(image, cancellation)?
            .output)
    }

    /// Runs OCR on an RGB image already loaded by the caller and records timings.
    ///
    /// With detection disabled, the whole input image is treated as one
    /// recognition crop. With recognition disabled, the output contains detected
    /// boxes with empty text and score `0.0`.
    pub fn run_image_timed(&mut self, image: &image::RgbImage) -> Result<TimedOcrOutput> {
        self.run_image_cancellable_timed(image, &OcrCancellationToken::new())
    }

    /// Runs OCR on an RGB image with timing details and cooperative cancellation.
    pub fn run_image_cancellable_timed(
        &mut self,
        image: &image::RgbImage,
        cancellation: &OcrCancellationToken,
    ) -> Result<TimedOcrOutput> {
        let total_start = Instant::now();
        let mut timings = OcrTimings::default();
        cancellation.checkpoint()?;
        if image.width() == 0 || image.height() == 0 {
            bail!("invalid image: width and height must be greater than 0");
        }

        let detection_enabled = self.detector.is_some();
        let needs_recognition = self.recognizer.is_some();
        let (boxes, crops) = if detection_enabled {
            let (boxes, crops, det_timings) =
                self.detect_and_crop_timed(image, needs_recognition, cancellation)?;
            timings.add_assign(&det_timings);
            (boxes, crops)
        } else {
            let bbox = Quad::from_xyxy(
                0.0,
                0.0,
                image.width().saturating_sub(1) as f32,
                image.height().saturating_sub(1) as f32,
            );
            (vec![bbox], vec![image.clone()])
        };
        cancellation.checkpoint()?;

        let Some(recognizer) = &mut self.recognizer else {
            let lines = boxes
                .into_iter()
                .map(|bbox| OcrLine {
                    bbox,
                    text: String::new(),
                    score: 0.0,
                })
                .collect();
            timings.total_ms = elapsed_ms(total_start);
            return Ok(TimedOcrOutput {
                output: OcrOutput { lines },
                timings,
            });
        };

        let crops = if let Some(classifier) = &mut self.classifier {
            let cls = classifier.classify_and_rotate_owned_timed(crops, cancellation)?;
            timings.add_assign(&cls.timings);
            cls.imgs
        } else {
            crops
        };
        let rec = recognizer.recognize_timed(&crops, cancellation)?;
        timings.add_assign(&rec.timings);

        let start = Instant::now();
        cancellation.checkpoint()?;
        let lines = boxes
            .into_iter()
            .zip(rec.texts)
            .filter_map(|(bbox, text)| {
                if text.text.trim().is_empty() {
                    return None;
                }
                if detection_enabled && text.score < self.cfg.text_score {
                    return None;
                }
                Some(OcrLine {
                    bbox,
                    text: text.text,
                    score: text.score,
                })
            })
            .collect();
        timings.output_filter_ms = elapsed_ms(start);
        timings.total_ms = elapsed_ms(total_start);

        Ok(TimedOcrOutput {
            output: OcrOutput { lines },
            timings,
        })
    }

    fn detect_and_crop_timed(
        &mut self,
        image: &image::RgbImage,
        needs_crops: bool,
        cancellation: &OcrCancellationToken,
    ) -> Result<(Vec<Quad>, Vec<image::RgbImage>, OcrTimings)> {
        let mut timings = OcrTimings::default();
        cancellation.checkpoint()?;
        let original_width = image.width();
        let original_height = image.height();

        let start = Instant::now();
        let (resized, ratio_w, ratio_h) =
            resize_image_within_bounds(image, self.cfg.min_side_len, self.cfg.max_side_len)?;
        let (det_image, padding_top) =
            apply_vertical_padding(&resized, self.cfg.width_height_ratio, self.cfg.min_height)?;
        timings.pipeline_preprocess_ms = elapsed_ms(start);

        let detector = self
            .detector
            .as_mut()
            .context("detection stage is not enabled")?;
        let det = detector.detect_timed(&det_image, cancellation)?;
        timings.add_assign(&det.timings);
        let mut boxes = det.boxes;

        let start = Instant::now();
        // Crops are extracted from the exact padded detector image that produced
        // the boxes. Only after crop generation do we remove padding and scale
        // coordinates back to the caller's original image space.
        let crops = if needs_crops {
            let mut crops = Vec::with_capacity(boxes.len());
            for bbox in &boxes {
                cancellation.checkpoint()?;
                crops.push(crop_perspective(&det_image, bbox)?);
            }
            crops
        } else {
            Vec::new()
        };

        for b in &mut boxes {
            cancellation.checkpoint()?;
            if padding_top > 0 {
                for point in &mut b.points {
                    point[1] -= padding_top as f32;
                }
            }
            b.scale(ratio_w, ratio_h);
            b.clip(original_width, original_height);
        }
        timings.crop_ms = elapsed_ms(start);

        Ok((boxes, crops, timings))
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reports_missing_detection_model_with_path() {
        let root = std::env::temp_dir().join(format!(
            "rapidocr-rs-missing-api-model-{}",
            std::process::id()
        ));
        let cfg = RapidOcrConfig::ppocr_v6_small(&root);

        let err = match RapidOcr::new(cfg) {
            Ok(_) => panic!("RapidOcr::new unexpectedly succeeded with a missing model"),
            Err(err) => err,
        };
        let message = format!("{err:#}");

        assert!(message.contains("failed to initialize detection stage"));
        assert!(message.contains("ONNX model file not found"));
        assert!(message.contains("PP-OCRv6_det_small.onnx"));
    }
}
