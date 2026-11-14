use std::time::Instant;

use anyhow::{bail, Context, Result};
use image::{imageops, RgbImage};
use ndarray::{s, Array4, Ix2};

use crate::{
    cancellation::OcrCancellationToken,
    config::{ClsConfig, InferenceOptions},
    inference::OnnxSession,
    types::OcrTimings,
};

#[derive(Debug, Clone)]
pub(crate) struct ClsResult {
    pub(crate) label: String,
    pub(crate) score: f32,
}

pub(crate) struct TextClassifier {
    cfg: ClsConfig,
    session: OnnxSession,
}

impl TextClassifier {
    pub(crate) fn new(
        cfg: ClsConfig,
        inference: InferenceOptions,
        environment: &ort::environment::Environment,
    ) -> Result<Self> {
        cfg.validate().context("invalid classification config")?;
        let session = OnnxSession::new(&cfg.model_path, inference, false, environment)
            .with_context(|| {
                format!(
                    "failed to load classification model {}",
                    cfg.model_path.display()
                )
            })?;
        Ok(Self { cfg, session })
    }

    pub(crate) fn classify_timed(
        &mut self,
        imgs: &[RgbImage],
        cancellation: &OcrCancellationToken,
    ) -> Result<ClassifyResult> {
        let mut timings = OcrTimings::default();
        cancellation.checkpoint()?;
        if imgs.is_empty() {
            return Ok(ClassifyResult {
                results: Vec::new(),
                timings,
            });
        }

        let mut results = Vec::with_capacity(imgs.len());
        for chunk in imgs.chunks(self.cfg.batch_size) {
            cancellation.checkpoint()?;
            let [channels, img_h, img_w] = self.cfg.image_shape;
            if channels != 3 {
                bail!("only 3-channel classification input is supported");
            }

            let start = Instant::now();
            let mut batch = Array4::<f32>::zeros((chunk.len(), channels, img_h, img_w));
            for (i, img) in chunk.iter().enumerate() {
                cancellation.checkpoint()?;
                let norm = self.resize_norm_img(img)?;
                batch.slice_mut(s![i, .., .., ..]).assign(&norm);
            }
            timings.cls_preprocess_ms += elapsed_ms(start);

            let start = Instant::now();
            let pred = self.session.run_f32(&batch, cancellation)?;
            timings.cls_inference_ms += elapsed_ms(start);

            let start = Instant::now();
            let pred = pred.into_dimensionality::<Ix2>()?;
            for row in pred.outer_iter() {
                cancellation.checkpoint()?;
                let (idx, score) = row
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.total_cmp(b))
                    .map(|(idx, score)| (idx, *score))
                    .unwrap_or((0, 0.0));
                let label = self
                    .cfg
                    .labels
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| idx.to_string());
                results.push(ClsResult { label, score });
            }
            timings.cls_postprocess_ms += elapsed_ms(start);
        }
        Ok(ClassifyResult { results, timings })
    }

    pub(crate) fn classify_and_rotate_owned_timed(
        &mut self,
        mut imgs: Vec<RgbImage>,
        cancellation: &OcrCancellationToken,
    ) -> Result<RotatedClassifyResult> {
        let ClassifyResult {
            results: cls,
            mut timings,
        } = self.classify_timed(&imgs, cancellation)?;
        let start = Instant::now();
        for (img, cls) in imgs.iter_mut().zip(cls) {
            cancellation.checkpoint()?;
            if cls.label.contains("180") && cls.score > self.cfg.thresh {
                *img = imageops::rotate180(img);
            }
        }
        timings.cls_postprocess_ms += elapsed_ms(start);
        Ok(RotatedClassifyResult { imgs, timings })
    }

    fn resize_norm_img(&self, img: &RgbImage) -> Result<ndarray::Array3<f32>> {
        let [channels, img_h, img_w] = self.cfg.image_shape;
        if channels != 3 {
            bail!("only 3-channel classification input is supported");
        }

        let ratio = img.width() as f32 / img.height().max(1) as f32;
        let resized_w = ((img_h as f32 * ratio).ceil() as usize).min(img_w).max(1);
        let resized = imageops::resize(
            img,
            resized_w as u32,
            img_h as u32,
            imageops::FilterType::Triangle,
        );

        let mut out = ndarray::Array3::<f32>::zeros((channels, img_h, img_w));
        for (x, y, pixel) in resized.enumerate_pixels() {
            for c in 0..3 {
                // Classifier models follow the PaddleOCR BGR normalization path;
                // the source image remains RGB everywhere else in the crate.
                out[[c, y as usize, x as usize]] = (pixel[2 - c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        Ok(out)
    }
}

pub(crate) struct ClassifyResult {
    pub(crate) results: Vec<ClsResult>,
    pub(crate) timings: OcrTimings,
}

pub(crate) struct RotatedClassifyResult {
    pub(crate) imgs: Vec<RgbImage>,
    pub(crate) timings: OcrTimings,
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
