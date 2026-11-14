//! Public OCR output and timing data types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Four-point text box.
///
/// Points are stored as `[x, y]` pairs. Public OCR output uses image pixel
/// coordinates after clipping to the source image bounds.
pub struct Quad {
    /// Box corners as four `[x, y]` points.
    pub points: [[f32; 2]; 4],
}

impl Quad {
    /// Creates an axis-aligned quadrilateral from inclusive corner coordinates.
    pub fn from_xyxy(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            points: [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        }
    }

    pub(crate) fn scale(&mut self, ratio_w: f32, ratio_h: f32) {
        for point in &mut self.points {
            point[0] *= ratio_w;
            point[1] *= ratio_h;
        }
    }

    pub(crate) fn clip(&mut self, width: u32, height: u32) {
        let max_x = width.saturating_sub(1) as f32;
        let max_y = height.saturating_sub(1) as f32;
        for point in &mut self.points {
            point[0] = point[0].clamp(0.0, max_x);
            point[1] = point[1].clamp(0.0, max_y);
        }
    }

    pub(crate) fn axis_aligned_bounds(&self) -> (u32, u32, u32, u32) {
        let min_x = self
            .points
            .iter()
            .map(|p| p[0])
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let min_y = self
            .points
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let max_x = self
            .points
            .iter()
            .map(|p| p[0])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .max(0.0) as u32;
        let max_y = self
            .points
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .max(0.0) as u32;
        (min_x, min_y, max_x, max_y)
    }

    pub(crate) fn ordered(mut self) -> Self {
        self.points = order_like_get_mini_boxes(self.points);
        self
    }

    pub(crate) fn order_clockwise_in_place(&mut self) {
        self.points = order_points_clockwise(self.points);
    }

    pub(crate) fn crop_width(&self) -> u32 {
        self.width_f32().floor().max(1.0) as u32
    }

    pub(crate) fn crop_height(&self) -> u32 {
        self.height_f32().floor().max(1.0) as u32
    }

    pub(crate) fn width_f32(&self) -> f32 {
        let top = distance(self.points[0], self.points[1]);
        let bottom = distance(self.points[3], self.points[2]);
        top.max(bottom)
    }

    pub(crate) fn height_f32(&self) -> f32 {
        let left = distance(self.points[0], self.points[3]);
        let right = distance(self.points[1], self.points[2]);
        left.max(right)
    }

    pub(crate) fn short_side(&self) -> f32 {
        self.width_f32().min(self.height_f32())
    }

    pub(crate) fn contains_point(&self, x: f32, y: f32) -> bool {
        let mut inside = false;
        let points = &self.points;
        let mut j = points.len() - 1;
        for i in 0..points.len() {
            let xi = points[i][0];
            let yi = points[i][1];
            let xj = points[j][0];
            let yj = points[j][1];
            if ((yi > y) != (yj > y))
                && (x < (xj - xi) * (y - yi) / ((yj - yi) + f32::EPSILON) + xi)
            {
                inside = !inside;
            }
            j = i;
        }
        inside
    }
}

fn order_like_get_mini_boxes(mut points: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));

    let (index_1, index_4) = if points[1][1] > points[0][1] {
        (0, 1)
    } else {
        (1, 0)
    };
    let (index_2, index_3) = if points[3][1] > points[2][1] {
        (2, 3)
    } else {
        (3, 2)
    };

    [
        points[index_1],
        points[index_2],
        points[index_3],
        points[index_4],
    ]
}

fn order_points_clockwise(mut points: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));

    let mut left_most = [points[0], points[1]];
    let mut right_most = [points[2], points[3]];
    left_most.sort_by(|a, b| a[1].total_cmp(&b[1]));
    right_most.sort_by(|a, b| a[1].total_cmp(&b[1]));

    [left_most[0], right_most[0], right_most[1], left_most[1]]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecText {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One OCR result line.
pub struct OcrLine {
    /// Detected or synthetic text box.
    pub bbox: Quad,
    /// Recognized text. Detection-only output leaves this empty.
    pub text: String,
    /// Mean recognition confidence. Detection-only output uses `0.0`.
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// OCR output lines in reading order.
pub struct OcrOutput {
    /// Recognized lines.
    pub lines: Vec<OcrLine>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Millisecond timings for OCR stages.
pub struct OcrTimings {
    /// Image decoding, EXIF orientation, and RGB conversion time.
    pub image_load_ms: f64,
    /// High-level pipeline resize and padding time before detection.
    pub pipeline_preprocess_ms: f64,
    /// Detection input resize and tensor normalization time.
    pub det_preprocess_ms: f64,
    /// Detection ONNX inference time.
    pub det_inference_ms: f64,
    /// Detection DB postprocessing time.
    pub det_postprocess_ms: f64,
    /// Perspective crop generation time.
    pub crop_ms: f64,
    /// Classification input resize and tensor normalization time.
    pub cls_preprocess_ms: f64,
    /// Classification ONNX inference time.
    pub cls_inference_ms: f64,
    /// Classification decode and rotation time.
    pub cls_postprocess_ms: f64,
    /// Recognition input resize and tensor normalization time.
    pub rec_preprocess_ms: f64,
    /// Recognition ONNX inference time.
    pub rec_inference_ms: f64,
    /// Recognition CTC decode time.
    pub rec_decode_ms: f64,
    /// Empty-text and low-score output filtering time.
    pub output_filter_ms: f64,
    /// Total elapsed time for the requested run.
    pub total_ms: f64,
}

impl OcrTimings {
    /// Adds another timing sample into this one field by field.
    pub fn add_assign(&mut self, other: &Self) {
        self.image_load_ms += other.image_load_ms;
        self.pipeline_preprocess_ms += other.pipeline_preprocess_ms;
        self.det_preprocess_ms += other.det_preprocess_ms;
        self.det_inference_ms += other.det_inference_ms;
        self.det_postprocess_ms += other.det_postprocess_ms;
        self.crop_ms += other.crop_ms;
        self.cls_preprocess_ms += other.cls_preprocess_ms;
        self.cls_inference_ms += other.cls_inference_ms;
        self.cls_postprocess_ms += other.cls_postprocess_ms;
        self.rec_preprocess_ms += other.rec_preprocess_ms;
        self.rec_inference_ms += other.rec_inference_ms;
        self.rec_decode_ms += other.rec_decode_ms;
        self.output_filter_ms += other.output_filter_ms;
        self.total_ms += other.total_ms;
    }

    /// Divides every timing field by `denominator`.
    ///
    /// A zero denominator returns the input unchanged.
    pub fn div_by(self, denominator: f64) -> Self {
        if denominator == 0.0 {
            return self;
        }

        Self {
            image_load_ms: self.image_load_ms / denominator,
            pipeline_preprocess_ms: self.pipeline_preprocess_ms / denominator,
            det_preprocess_ms: self.det_preprocess_ms / denominator,
            det_inference_ms: self.det_inference_ms / denominator,
            det_postprocess_ms: self.det_postprocess_ms / denominator,
            crop_ms: self.crop_ms / denominator,
            cls_preprocess_ms: self.cls_preprocess_ms / denominator,
            cls_inference_ms: self.cls_inference_ms / denominator,
            cls_postprocess_ms: self.cls_postprocess_ms / denominator,
            rec_preprocess_ms: self.rec_preprocess_ms / denominator,
            rec_inference_ms: self.rec_inference_ms / denominator,
            rec_decode_ms: self.rec_decode_ms / denominator,
            output_filter_ms: self.output_filter_ms / denominator,
            total_ms: self.total_ms / denominator,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// OCR output paired with per-stage timings.
pub struct TimedOcrOutput {
    /// Recognized OCR lines.
    pub output: OcrOutput,
    /// Timing breakdown in milliseconds.
    pub timings: OcrTimings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_mini_boxes_order_matches_python_left_group_logic() {
        let points = [[10.0, 7.0], [2.0, 1.0], [12.0, 2.0], [0.0, 6.0]];
        let ordered = order_like_get_mini_boxes(points);
        assert_eq!(ordered, [[2.0, 1.0], [12.0, 2.0], [10.0, 7.0], [0.0, 6.0]]);
    }

    #[test]
    fn order_points_clockwise_matches_python_filter_logic() {
        let points = [[10.0, 7.0], [2.0, 1.0], [12.0, 2.0], [0.0, 6.0]];
        let ordered = order_points_clockwise(points);
        assert_eq!(ordered, [[2.0, 1.0], [12.0, 2.0], [10.0, 7.0], [0.0, 6.0]]);
    }

    #[test]
    fn order_points_clockwise_handles_axis_aligned_shuffle() {
        let points = [[10.0, 10.0], [0.0, 0.0], [0.0, 10.0], [10.0, 0.0]];
        let ordered = order_points_clockwise(points);
        assert_eq!(
            ordered,
            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]
        );
    }
}
