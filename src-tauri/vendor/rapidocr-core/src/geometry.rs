use crate::types::Quad;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl Point {
    pub(crate) fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

pub(crate) fn min_area_rect(points: &[Point]) -> Option<Quad> {
    if points.len() < 3 {
        return None;
    }

    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }

    // OpenCV's minAreaRect is approximated by checking each convex hull edge as
    // a candidate rectangle angle and selecting the minimum-area projection.
    let mut best: Option<RotatedRect> = None;
    for i in 0..hull.len() {
        let p0 = hull[i];
        let p1 = hull[(i + 1) % hull.len()];
        let angle = (p1.y - p0.y).atan2(p1.x - p0.x);
        let (sin, cos) = angle.sin_cos();

        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;

        for p in &hull {
            let x = p.x * cos + p.y * sin;
            let y = -p.x * sin + p.y * cos;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }

        let width = max_x - min_x;
        let height = max_y - min_y;
        if width <= f32::EPSILON || height <= f32::EPSILON {
            continue;
        }

        let area = width * height;
        let rect = RotatedRect {
            angle,
            min_x,
            min_y,
            max_x,
            max_y,
            area,
        };

        if best.as_ref().is_none_or(|b| rect.area < b.area) {
            best = Some(rect);
        }
    }

    best.map(|rect| rect.to_quad())
}

pub(crate) fn unclip_quad(quad: &Quad, unclip_ratio: f32) -> Option<Vec<Point>> {
    let points = quad
        .points
        .iter()
        .map(|point| Point::new(point[0], point[1]))
        .collect::<Vec<_>>();
    let area = polygon_area(&points);
    let perimeter = polygon_perimeter(&points);
    if area <= f32::EPSILON || perimeter <= f32::EPSILON {
        return None;
    }

    // DBPostProcess expands text polygons by area * ratio / perimeter, matching
    // the pyclipper distance formula used by the Python implementation.
    let distance = area * unclip_ratio / perimeter;
    offset_convex_polygon(&points, distance)
}

pub(crate) fn polygon_area(points: &[Point]) -> f32 {
    signed_polygon_area(points).abs()
}

pub(crate) fn polygon_perimeter(points: &[Point]) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }

    let mut perimeter = 0.0;
    for i in 0..points.len() {
        let p0 = points[i];
        let p1 = points[(i + 1) % points.len()];
        perimeter += distance(p0, p1);
    }
    perimeter
}

fn signed_polygon_area(points: &[Point]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }

    let mut area = 0.0;
    for i in 0..points.len() {
        let p0 = points[i];
        let p1 = points[(i + 1) % points.len()];
        area += p0.x * p1.y - p1.x * p0.y;
    }
    area * 0.5
}

fn offset_convex_polygon(points: &[Point], distance: f32) -> Option<Vec<Point>> {
    if points.len() < 3 {
        return None;
    }

    let orientation = signed_polygon_area(points).signum();
    if orientation == 0.0 {
        return None;
    }

    let shifted_edges = (0..points.len())
        .filter_map(|i| {
            let p0 = points[i];
            let p1 = points[(i + 1) % points.len()];
            let edge_dx = p1.x - p0.x;
            let edge_dy = p1.y - p0.y;
            let len = (edge_dx * edge_dx + edge_dy * edge_dy).sqrt();
            if len <= f32::EPSILON {
                return None;
            }

            // The outward normal flips with polygon winding. Getting this sign
            // wrong shrinks text boxes instead of expanding them.
            let normal = if orientation > 0.0 {
                Point::new(edge_dy / len, -edge_dx / len)
            } else {
                Point::new(-edge_dy / len, edge_dx / len)
            };
            Some((
                Point::new(p0.x + normal.x * distance, p0.y + normal.y * distance),
                Point::new(p1.x + normal.x * distance, p1.y + normal.y * distance),
            ))
        })
        .collect::<Vec<_>>();

    if shifted_edges.len() != points.len() {
        return None;
    }

    let mut out = Vec::with_capacity(points.len());
    for i in 0..shifted_edges.len() {
        let prev = shifted_edges[(i + shifted_edges.len() - 1) % shifted_edges.len()];
        let cur = shifted_edges[i];
        if let Some(intersection) = line_intersection(prev.0, prev.1, cur.0, cur.1) {
            out.push(intersection);
        } else {
            out.push(Point::new(
                (prev.1.x + cur.0.x) * 0.5,
                (prev.1.y + cur.0.y) * 0.5,
            ));
        }
    }

    Some(out)
}

fn convex_hull(points: &[Point]) -> Vec<Point> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);

    if pts.len() <= 1 {
        return pts;
    }

    let mut lower = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }

    let mut upper = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross(o: Point, a: Point, b: Point) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn distance(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

fn line_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let r = Point::new(a1.x - a0.x, a1.y - a0.y);
    let s = Point::new(b1.x - b0.x, b1.y - b0.y);
    let denom = r.x * s.y - r.y * s.x;
    if denom.abs() <= 1e-6 {
        return None;
    }

    let qp = Point::new(b0.x - a0.x, b0.y - a0.y);
    let t = (qp.x * s.y - qp.y * s.x) / denom;
    Some(Point::new(a0.x + t * r.x, a0.y + t * r.y))
}

#[derive(Debug, Clone, Copy)]
struct RotatedRect {
    angle: f32,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    area: f32,
}

impl RotatedRect {
    fn to_quad(self) -> Quad {
        let local = [
            Point::new(self.min_x, self.min_y),
            Point::new(self.max_x, self.min_y),
            Point::new(self.max_x, self.max_y),
            Point::new(self.min_x, self.max_y),
        ];
        let (sin, cos) = self.angle.sin_cos();
        let mut points = [[0.0; 2]; 4];
        for (i, p) in local.into_iter().enumerate() {
            points[i] = [p.x * cos - p.y * sin, p.x * sin + p.y * cos];
        }
        Quad { points }.ordered()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_area_rect_handles_axis_aligned_points() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 4.0),
            Point::new(0.0, 4.0),
        ];
        let quad = min_area_rect(&points).unwrap();
        assert!((quad.crop_width() as i32 - 10).abs() <= 1);
        assert!((quad.crop_height() as i32 - 4).abs() <= 1);
    }

    #[test]
    fn min_area_rect_expands_with_unclip_ratio() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 4.0),
            Point::new(0.0, 4.0),
        ];
        let base = min_area_rect(&points).unwrap();
        let expanded_polygon = unclip_quad(&base, 1.6).unwrap();
        let expanded = min_area_rect(&expanded_polygon).unwrap();
        assert!(expanded.crop_width() > base.crop_width());
        assert!(expanded.crop_height() > base.crop_height());
    }

    #[test]
    fn polygon_metrics_match_axis_aligned_rectangle() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 4.0),
            Point::new(0.0, 4.0),
        ];
        assert!((polygon_area(&points) - 40.0).abs() < 1e-5);
        assert!((polygon_perimeter(&points) - 28.0).abs() < 1e-5);
    }
}
