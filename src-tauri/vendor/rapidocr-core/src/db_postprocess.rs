use std::collections::VecDeque;

use anyhow::Result;
use ndarray::{ArrayD, Ix4};

use crate::{
    cancellation::OcrCancellationToken,
    config::DetConfig,
    geometry::{min_area_rect, unclip_quad, Point},
    types::Quad,
};

const SCORE_THRESHOLD_EPSILON: f32 = 0.005;
const MICRO_BOX_MAX_SIDE_PX: u32 = 4;

#[derive(Debug, Clone)]
pub(crate) struct DbPostProcessConfig {
    pub(crate) thresh: f32,
    pub(crate) box_thresh: f32,
    pub(crate) max_candidates: usize,
    pub(crate) unclip_ratio: f32,
    pub(crate) min_size: u32,
}

impl From<&DetConfig> for DbPostProcessConfig {
    fn from(cfg: &DetConfig) -> Self {
        Self {
            thresh: cfg.thresh,
            box_thresh: cfg.box_thresh,
            max_candidates: cfg.max_candidates,
            unclip_ratio: cfg.unclip_ratio,
            min_size: cfg.min_size,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DetCandidate {
    pub(crate) bbox: Quad,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) score: f32,
}

pub(crate) struct DbPostProcess {
    cfg: DbPostProcessConfig,
}

impl DbPostProcess {
    pub(crate) fn new(cfg: DbPostProcessConfig) -> Self {
        Self { cfg }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn process(
        &self,
        pred: ArrayD<f32>,
        dest_w: u32,
        dest_h: u32,
    ) -> Result<Vec<DetCandidate>> {
        self.process_cancellable(pred, dest_w, dest_h, &OcrCancellationToken::new())
    }

    pub(crate) fn process_cancellable(
        &self,
        pred: ArrayD<f32>,
        dest_w: u32,
        dest_h: u32,
        cancellation: &OcrCancellationToken,
    ) -> Result<Vec<DetCandidate>> {
        cancellation.checkpoint()?;
        let pred = pred.into_dimensionality::<Ix4>()?;
        let map_h = pred.shape()[2];
        let map_w = pred.shape()[3];
        // Python RapidOCR dilates the threshold mask before contour extraction.
        // This Rust path uses connected components plus boundary sampling instead
        // of OpenCV contours, but keeps the same mask-level intent.
        let mask = dilate_2x2(&pred, self.cfg.thresh, map_w, map_h, cancellation)?;
        let mut visited = vec![false; map_h * map_w];
        let mut boxes = Vec::new();

        for y in 0..map_h {
            if y.is_multiple_of(16) {
                cancellation.checkpoint()?;
            }
            for x in 0..map_w {
                let idx = y * map_w + x;
                if visited[idx] || !mask[idx] {
                    continue;
                }
                let component = collect_component(
                    &pred,
                    &mask,
                    &mut visited,
                    x,
                    y,
                    (map_w, map_h),
                    cancellation,
                )?;
                if component.score < self.cfg.box_thresh {
                    continue;
                }
                if component.width() < self.cfg.min_size || component.height() < self.cfg.min_size {
                    continue;
                }

                // Score once on the raw component rectangle, then expand and
                // re-fit a min-area rectangle to approximate DBPostProcess.
                let Some(base_box) = component.to_quad(&mask, map_w, map_h, 0.0) else {
                    continue;
                };
                if base_box.short_side() < self.cfg.min_size as f32 {
                    continue;
                }

                let score = polygon_score_fast(&pred, &base_box);
                if score + SCORE_THRESHOLD_EPSILON < self.cfg.box_thresh {
                    continue;
                }

                let Some(expanded_polygon) = unclip_quad(&base_box, self.cfg.unclip_ratio) else {
                    continue;
                };
                let Some(mut bbox) = min_area_rect(&expanded_polygon) else {
                    continue;
                };
                if bbox.short_side() < (self.cfg.min_size + 2) as f32 {
                    continue;
                }

                let sx = dest_w as f32 / map_w as f32;
                let sy = dest_h as f32 / map_h as f32;
                bbox.scale(sx, sy);
                round_and_clip_det_box(&mut bbox, dest_w, dest_h);
                bbox.order_clockwise_in_place();
                let pixel_width = bbox.width_f32() as u32;
                let pixel_height = bbox.height_f32() as u32;
                if pixel_width <= 3 || pixel_height <= 3 {
                    continue;
                }
                // Python's pyclipper round-join expansion can leave tiny edge artifacts at
                // 3 px after output rounding. The convex offset approximation used here may
                // over-expand the same square noise by one pixel, so only drop near-square
                // micro boxes while preserving real long thin text boxes.
                if pixel_width <= MICRO_BOX_MAX_SIDE_PX && pixel_height <= MICRO_BOX_MAX_SIDE_PX {
                    continue;
                }
                boxes.push(DetCandidate { bbox, score });
                if boxes.len() >= self.cfg.max_candidates {
                    return Ok(sort_candidates(boxes));
                }
            }
        }

        Ok(sort_candidates(boxes))
    }
}

fn round_and_clip_det_box(bbox: &mut Quad, width: u32, height: u32) {
    let max_x = width.saturating_sub(1) as f32;
    let max_y = height.saturating_sub(1) as f32;
    for point in &mut bbox.points {
        point[0] = point[0].round().clamp(0.0, max_x);
        point[1] = point[1].round().clamp(0.0, max_y);
    }
}

#[derive(Debug, Clone)]
struct Component {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    sum: f32,
    count: usize,
    score_sum: f32,
    score_count: usize,
    score: f32,
    pixels: Vec<(usize, usize)>,
}

impl Component {
    fn new(x: usize, y: usize) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
            sum: 0.0,
            count: 0,
            score_sum: 0.0,
            score_count: 0,
            score: 0.0,
            pixels: Vec::new(),
        }
    }

    fn add(&mut self, x: usize, y: usize, score: f32) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
        self.sum += score;
        self.count += 1;
        if score > 0.0 {
            self.score_sum += score;
            self.score_count += 1;
        }
        self.pixels.push((x, y));
    }

    fn finish(mut self) -> Self {
        if self.score_count > 0 {
            self.score = self.score_sum / self.score_count as f32;
        }
        self
    }

    fn width(&self) -> u32 {
        (self.max_x - self.min_x + 1) as u32
    }

    fn height(&self) -> u32 {
        (self.max_y - self.min_y + 1) as u32
    }

    fn to_quad(
        &self,
        mask: &[bool],
        width: usize,
        height: usize,
        unclip_ratio: f32,
    ) -> Option<Quad> {
        // OpenCV contour extraction is replaced with boundary-pixel corner
        // samples. Each boundary cell contributes its four cell corners, then
        // min_area_rect recovers the oriented candidate box.
        let mut boundary = Vec::new();
        for &(x, y) in &self.pixels {
            if !is_boundary(mask, x, y, width, height) {
                continue;
            }
            boundary.push(Point::new(x as f32, y as f32));
            boundary.push(Point::new((x + 1) as f32, y as f32));
            boundary.push(Point::new((x + 1) as f32, (y + 1) as f32));
            boundary.push(Point::new(x as f32, (y + 1) as f32));
        }

        if boundary.len() < 3 {
            return Some(Quad::from_xyxy(
                self.min_x as f32,
                self.min_y as f32,
                (self.max_x + 1) as f32,
                (self.max_y + 1) as f32,
            ));
        }

        let base_box = min_area_rect(&boundary)?;
        if unclip_ratio <= f32::EPSILON {
            return Some(base_box);
        }

        let expanded = unclip_quad(&base_box, unclip_ratio)?;
        min_area_rect(&expanded)
    }
}

fn collect_component(
    pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>,
    mask: &[bool],
    visited: &mut [bool],
    start_x: usize,
    start_y: usize,
    dimensions: (usize, usize),
    cancellation: &OcrCancellationToken,
) -> Result<Component> {
    let (width, height) = dimensions;
    // Use 8-connected components because text probability maps often connect
    // diagonally at thin strokes.
    let mut queue = VecDeque::from([(start_x, start_y)]);
    let mut c = Component::new(start_x, start_y);

    let mut visited_count = 0usize;
    while let Some((x, y)) = queue.pop_front() {
        visited_count += 1;
        if visited_count.is_multiple_of(1024) {
            cancellation.checkpoint()?;
        }
        let idx = y * width + x;
        if visited[idx] {
            continue;
        }
        visited[idx] = true;
        if !mask[idx] {
            continue;
        }
        let score = pred[[0, 0, y, x]];
        c.add(x, y, score);

        for (nx, ny) in neighbors(x, y, width, height) {
            if !visited[ny * width + nx] {
                queue.push_back((nx, ny));
            }
        }
    }

    Ok(c.finish())
}

fn is_boundary(mask: &[bool], x: usize, y: usize, width: usize, height: usize) -> bool {
    for dy in -1isize..=1 {
        for dx in -1isize..=1 {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx < 0 || ny < 0 || nx >= width as isize || ny >= height as isize {
                return true;
            }
            if !mask[ny as usize * width + nx as usize] {
                return true;
            }
        }
    }
    false
}

fn neighbors(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let mut out = Vec::with_capacity(8);
    for dy in -1isize..=1 {
        for dx in -1isize..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0 && ny >= 0 && nx < width as isize && ny < height as isize {
                out.push((nx as usize, ny as usize));
            }
        }
    }
    out.into_iter()
}

fn dilate_2x2(
    pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>,
    thresh: f32,
    width: usize,
    height: usize,
    cancellation: &OcrCancellationToken,
) -> Result<Vec<bool>> {
    let mut mask = vec![false; width * height];
    for y in 0..height {
        if y.is_multiple_of(16) {
            cancellation.checkpoint()?;
        }
        for x in 0..width {
            if pred[[0, 0, y, x]] <= thresh {
                continue;
            }
            for dy in 0..=1 {
                for dx in 0..=1 {
                    let nx = (x + dx).min(width - 1);
                    let ny = (y + dy).min(height - 1);
                    mask[ny * width + nx] = true;
                }
            }
        }
    }
    Ok(mask)
}

fn polygon_score_fast(pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>, bbox: &Quad) -> f32 {
    let height = pred.shape()[2];
    let width = pred.shape()[3];
    let (mut x0, mut y0, mut x1, mut y1) = bbox.axis_aligned_bounds();
    x0 = x0.min(width.saturating_sub(1) as u32);
    y0 = y0.min(height.saturating_sub(1) as u32);
    x1 = x1.min(width as u32);
    y1 = y1.min(height as u32);

    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }

    // The Python implementation rasterizes the polygon mask and averages scores
    // under it. Scanline spans give the same intent without allocating a local mask.
    let local_poly = bbox
        .points
        .map(|point| [point[0] - x0 as f32, point[1] - y0 as f32]);
    let local_h = (y1 - y0 + 1) as usize;
    let local_w = (x1 - x0 + 1) as usize;

    let mut sum = 0.0;
    let mut count = 0usize;
    for local_y in 0..local_h {
        let image_y = y0 as usize + local_y;
        if image_y >= height {
            continue;
        }
        let spans = polygon_scanline_spans(&local_poly, local_y as f32 + 0.5, local_w);
        for (start_x, end_x) in spans {
            for local_x in start_x..=end_x {
                let image_x = x0 as usize + local_x;
                if image_x >= width {
                    continue;
                }
                sum += pred[[0, 0, image_y, image_x]];
                count += 1;
            }
        }
    }

    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

fn polygon_scanline_spans(
    polygon: &[[f32; 2]; 4],
    scan_y: f32,
    width: usize,
) -> Vec<(usize, usize)> {
    // Rasterize by intersecting one horizontal scanline with polygon edges.
    // Paired intersections define inclusive x spans; ceil/floor mirrors the
    // integer pixels whose centers fall inside the polygon.
    let mut intersections = Vec::with_capacity(4);
    for i in 0..polygon.len() {
        let p0 = polygon[i];
        let p1 = polygon[(i + 1) % polygon.len()];
        let y0 = p0[1];
        let y1 = p1[1];
        if (y0 <= scan_y && y1 > scan_y) || (y1 <= scan_y && y0 > scan_y) {
            let t = (scan_y - y0) / (y1 - y0);
            intersections.push(p0[0] + t * (p1[0] - p0[0]));
        }
    }

    intersections.sort_by(|a, b| a.total_cmp(b));
    let mut spans = Vec::new();
    for pair in intersections.chunks_exact(2) {
        let start = pair[0].ceil().max(0.0) as isize;
        let end = pair[1].floor().min(width.saturating_sub(1) as f32) as isize;
        if start <= end {
            spans.push((start as usize, end as usize));
        }
    }
    spans
}

#[allow(dead_code)]
fn polygon_score_point_in_poly(
    pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>,
    bbox: &Quad,
) -> f32 {
    let height = pred.shape()[2];
    let width = pred.shape()[3];
    let (mut x0, mut y0, mut x1, mut y1) = bbox.axis_aligned_bounds();
    x0 = x0.min(width.saturating_sub(1) as u32);
    y0 = y0.min(height.saturating_sub(1) as u32);
    x1 = x1.min(width as u32);
    y1 = y1.min(height as u32);

    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }

    let mut sum = 0.0;
    let mut count = 0usize;
    for y in y0..=y1 {
        if y as usize >= height {
            continue;
        }
        for x in x0..=x1 {
            if x as usize >= width {
                continue;
            }
            if bbox.contains_point(x as f32 + 0.5, y as f32 + 0.5) {
                sum += pred[[0, 0, y as usize, x as usize]];
                count += 1;
            }
        }
    }

    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

fn sort_candidates(mut boxes: Vec<DetCandidate>) -> Vec<DetCandidate> {
    const LINE_Y_THRESHOLD: f32 = 10.0;

    // Match RapidOCR's two-step reading-order sort. Using the threshold inside
    // a comparison function is not transitive (A can share a line with B, B
    // with C, while A and C are compared by Y), which can make Rust's sort
    // detect an invalid total order and panic.
    boxes.sort_by(|a, b| a.bbox.points[0][1].total_cmp(&b.bbox.points[0][1]));

    let mut line_start = 0;
    while line_start < boxes.len() {
        let mut line_end = line_start + 1;
        while line_end < boxes.len()
            && boxes[line_end].bbox.points[0][1] - boxes[line_end - 1].bbox.points[0][1]
                < LINE_Y_THRESHOLD
        {
            line_end += 1;
        }
        boxes[line_start..line_end]
            .sort_by(|a, b| a.bbox.points[0][0].total_cmp(&b.bbox.points[0][0]));
        line_start = line_end;
    }
    boxes
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use ndarray::ArrayD;
    use ndarray_npy::read_npy;
    use serde::Deserialize;

    use super::*;

    #[test]
    fn candidate_sort_handles_chained_line_threshold_without_panicking() {
        let boxes = vec![
            DetCandidate {
                bbox: Quad::from_xyxy(0.0, 18.0, 4.0, 22.0),
                score: 1.0,
            },
            DetCandidate {
                bbox: Quad::from_xyxy(1.0, 9.0, 5.0, 13.0),
                score: 1.0,
            },
            DetCandidate {
                bbox: Quad::from_xyxy(2.0, 0.0, 6.0, 4.0),
                score: 1.0,
            },
        ];

        let sorted = sort_candidates(boxes);
        let xs = sorted
            .iter()
            .map(|candidate| candidate.bbox.points[0][0])
            .collect::<Vec<_>>();

        assert_eq!(xs, vec![0.0, 1.0, 2.0]);
    }

    #[derive(Debug, Deserialize)]
    struct Fixture {
        dest_shape: [u32; 2],
        boxes: Vec<[[f32; 2]; 4]>,
        scores: Vec<f32>,
        #[serde(default)]
        tolerances: DbTolerances,
    }

    #[derive(Debug, Clone, Copy, Deserialize)]
    struct DbTolerances {
        #[serde(default = "default_max_mean_center_delta")]
        max_mean_center_delta: f32,
        #[serde(default = "default_max_mean_score_delta")]
        max_mean_score_delta: f32,
        #[serde(default = "default_max_mean_corner_delta")]
        max_mean_corner_delta: f32,
        #[serde(default = "default_max_mean_size_delta")]
        max_mean_size_delta: f32,
    }

    impl Default for DbTolerances {
        fn default() -> Self {
            Self {
                max_mean_center_delta: default_max_mean_center_delta(),
                max_mean_score_delta: default_max_mean_score_delta(),
                max_mean_corner_delta: default_max_mean_corner_delta(),
                max_mean_size_delta: default_max_mean_size_delta(),
            }
        }
    }

    #[test]
    fn db_postprocess_tracks_python_fixture_count_and_geometry() {
        for fixture_dir in fixture_dirs() {
            let pred: ArrayD<f32> = read_npy(fixture_dir.join("pred.npy")).unwrap();
            let expected: Fixture = serde_json::from_str(
                &fs::read_to_string(fixture_dir.join("expected.json")).unwrap(),
            )
            .unwrap();

            let processor = DbPostProcess::new(DbPostProcessConfig {
                thresh: 0.3,
                box_thresh: 0.5,
                max_candidates: 1000,
                unclip_ratio: 1.6,
                min_size: 3,
            });
            let actual = processor
                .process(pred, expected.dest_shape[0], expected.dest_shape[1])
                .unwrap();

            let pairs = match_by_nearest_center(&actual, &expected.boxes);
            let metrics = ParityMetrics::from_pairs(&actual, &expected, &pairs);
            println!(
                "db_postprocess parity metrics [{}]: {metrics:#?}",
                fixture_dir.file_name().unwrap().to_string_lossy()
            );

            assert_eq!(
                metrics.actual_count,
                metrics.expected_count,
                "candidate count mismatch for {}: actual={actual:#?}, expected={:#?}",
                fixture_dir.display(),
                expected.boxes
            );
            assert_eq!(
                metrics.matched_count,
                metrics.expected_count,
                "matched count mismatch for {}",
                fixture_dir.display()
            );

            if metrics.expected_count == 0 {
                continue;
            }

            assert!(
                metrics.mean_center_delta < expected.tolerances.max_mean_center_delta,
                "mean center delta too high for {}: {}",
                fixture_dir.display(),
                metrics.mean_center_delta
            );
            assert!(
                metrics.mean_score_delta < expected.tolerances.max_mean_score_delta,
                "mean score delta too high for {}: {}",
                fixture_dir.display(),
                metrics.mean_score_delta
            );
            assert!(
                metrics.mean_corner_delta < expected.tolerances.max_mean_corner_delta,
                "mean corner delta too high for {}: {}",
                fixture_dir.display(),
                metrics.mean_corner_delta
            );
            assert!(
                metrics.mean_size_delta < expected.tolerances.max_mean_size_delta,
                "mean size delta too high for {}: {}",
                fixture_dir.display(),
                metrics.mean_size_delta
            );
        }
    }

    fn default_max_mean_center_delta() -> f32 {
        20.0
    }

    fn default_max_mean_score_delta() -> f32 {
        0.15
    }

    fn default_max_mean_corner_delta() -> f32 {
        5.0
    }

    fn default_max_mean_size_delta() -> f32 {
        5.0
    }

    #[derive(Debug)]
    struct ParityMetrics {
        actual_count: usize,
        expected_count: usize,
        matched_count: usize,
        mean_center_delta: f32,
        mean_score_delta: f32,
        mean_corner_delta: f32,
        mean_size_delta: f32,
    }

    impl ParityMetrics {
        fn from_pairs(
            actual: &[DetCandidate],
            expected: &Fixture,
            pairs: &[(usize, usize)],
        ) -> Self {
            let mean_center_delta = pairs
                .iter()
                .map(|(actual_idx, expected_idx)| {
                    distance(
                        center(&actual[*actual_idx].bbox.points),
                        center(&expected.boxes[*expected_idx]),
                    )
                })
                .sum::<f32>()
                / pairs.len().max(1) as f32;

            let mean_score_delta = pairs
                .iter()
                .map(|(actual_idx, expected_idx)| {
                    (actual[*actual_idx].score - expected.scores[*expected_idx]).abs()
                })
                .sum::<f32>()
                / pairs.len().max(1) as f32;

            let mean_corner_delta = pairs
                .iter()
                .map(|(actual_idx, expected_idx)| {
                    corner_delta(
                        &actual[*actual_idx].bbox.points,
                        &expected.boxes[*expected_idx],
                    )
                })
                .sum::<f32>()
                / pairs.len().max(1) as f32;

            let mean_size_delta = pairs
                .iter()
                .map(|(actual_idx, expected_idx)| {
                    size_delta(
                        &actual[*actual_idx].bbox.points,
                        &expected.boxes[*expected_idx],
                    )
                })
                .sum::<f32>()
                / pairs.len().max(1) as f32;

            Self {
                actual_count: actual.len(),
                expected_count: expected.boxes.len(),
                matched_count: pairs.len(),
                mean_center_delta,
                mean_score_delta,
                mean_corner_delta,
                mean_size_delta,
            }
        }
    }

    fn fixture_dirs() -> Vec<PathBuf> {
        let root = env::var_os("RAPIDOCR_DB_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("fixtures")
                    .join("db_postprocess")
            });
        let mut dirs = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        dirs.sort();
        dirs
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

    fn size_delta(actual: &[[f32; 2]; 4], expected: &[[f32; 2]; 4]) -> f32 {
        let actual_w = distance(actual[0], actual[1]).max(distance(actual[3], actual[2]));
        let actual_h = distance(actual[0], actual[3]).max(distance(actual[1], actual[2]));
        let expected_w = distance(expected[0], expected[1]).max(distance(expected[3], expected[2]));
        let expected_h = distance(expected[0], expected[3]).max(distance(expected[1], expected[2]));
        ((actual_w - expected_w).abs() + (actual_h - expected_h).abs()) * 0.5
    }

    fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        (dx * dx + dy * dy).sqrt()
    }

    fn match_by_nearest_center(
        actual: &[DetCandidate],
        expected: &[[[f32; 2]; 4]],
    ) -> Vec<(usize, usize)> {
        let mut unmatched_expected = (0..expected.len()).collect::<Vec<_>>();
        let mut pairs = Vec::new();

        for (actual_idx, candidate) in actual.iter().enumerate() {
            let actual_center = center(&candidate.bbox.points);
            let Some((best_pos, _)) = unmatched_expected
                .iter()
                .enumerate()
                .map(|(pos, expected_idx)| {
                    (
                        pos,
                        distance(actual_center, center(&expected[*expected_idx])),
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
}
