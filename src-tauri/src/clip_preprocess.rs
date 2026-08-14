//! Pillow-compatible bicubic resize, shared by the semantic worker and the
//! capture path.
//!
//! This lives in its own module for one reason: two processes have to produce
//! byte-identical pixels from it. The worker resizes every image it is handed
//! down to the CLIP input size, and the capture path resizes a screenshot to
//! that same size while the plaintext is still in memory. If the two ever used
//! different filters, a pre-resized capture would embed differently from the
//! same image read back from disk, and nothing in the system would notice —
//! the vectors would simply be wrong.
//!
//! The algorithm reproduces Pillow's `Image.resize(..., BICUBIC)`: fixed-point
//! coefficients at [`PILLOW_PRECISION_BITS`], a horizontal pass into an
//! intermediate buffer, then a vertical pass. It is a port rather than an
//! approximation because the Python oracle the CLIP tower is validated against
//! preprocesses with Pillow.

use image::RgbImage;

/// The square target the CLIP preprocessor config specifies, as `(width, height)`.
///
/// Shared so the worker and the capture path cannot disagree about the size
/// they are resizing to. The config names either an explicit `height`/`width`
/// pair or a single `shortest_edge`; a lone `shortest_edge` means a square,
/// which is what the pinned Chinese-CLIP config uses.
///
/// Returns `None` for a config this does not understand, which callers treat as
/// "do not pre-resize" rather than as an error — the worker still resizes
/// whatever it is handed, so a capture path that declines to help stays correct.
pub fn target_size_from_config(config: &[u8]) -> Option<(u32, u32)> {
    let parsed: serde_json::Value = serde_json::from_slice(config).ok()?;
    target_size_from_size_value(parsed.get("size")?)
}

/// The same reading, applied to an already-parsed `size` object.
///
/// The engine deserializes the config into a struct before it gets here, so it
/// holds the `size` value rather than the bytes. Both entry points exist so
/// neither caller has to reimplement the height/width/shortest_edge rule.
pub fn target_size_from_size_value(size: &serde_json::Value) -> Option<(u32, u32)> {
    let height = size
        .get("height")
        .or_else(|| size.get("shortest_edge"))?
        .as_u64()?;
    let width = size
        .get("width")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(height);
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

const PILLOW_PRECISION_BITS: i32 = 22;

struct PillowCoefficients {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    coefficients: Vec<i32>,
}

/// Resize to exactly `width` x `height`, ignoring the source aspect ratio.
///
/// Aspect ratio is deliberately not preserved: the CLIP preprocessor config
/// this serves specifies a square target and no center crop, so a screenshot is
/// squashed rather than cropped. Matching that exactly matters more than
/// producing a nicer-looking thumbnail.
///
/// An image already at the target size is returned unchanged, which is what
/// makes pre-resizing on the capture path free on the worker side.
pub fn pillow_bicubic_resize_rgb(image: &RgbImage, width: u32, height: u32) -> RgbImage {
    if image.width() == width && image.height() == height {
        return image.clone();
    }
    let horizontal = pillow_coefficients(image.width() as usize, width as usize);
    let vertical = pillow_coefficients(image.height() as usize, height as usize);
    let mut temporary = RgbImage::new(width, image.height());
    for y in 0..image.height() as usize {
        for out_x in 0..width as usize {
            let (start, count) = horizontal.bounds[out_x];
            let weights = &horizontal.coefficients
                [out_x * horizontal.ksize..out_x * horizontal.ksize + count];
            let mut sums = [1 << (PILLOW_PRECISION_BITS - 1); 3];
            for (offset, weight) in weights.iter().enumerate() {
                let pixel = image.get_pixel((start + offset) as u32, y as u32);
                for channel in 0..3 {
                    sums[channel] += i32::from(pixel[channel]) * *weight;
                }
            }
            temporary.put_pixel(out_x as u32, y as u32, image::Rgb(sums.map(pillow_clip8)));
        }
    }

    let mut output = RgbImage::new(width, height);
    for out_y in 0..height as usize {
        let (start, count) = vertical.bounds[out_y];
        let weights =
            &vertical.coefficients[out_y * vertical.ksize..out_y * vertical.ksize + count];
        for x in 0..width as usize {
            let mut sums = [1 << (PILLOW_PRECISION_BITS - 1); 3];
            for (offset, weight) in weights.iter().enumerate() {
                let pixel = temporary.get_pixel(x as u32, (start + offset) as u32);
                for channel in 0..3 {
                    sums[channel] += i32::from(pixel[channel]) * *weight;
                }
            }
            output.put_pixel(x as u32, out_y as u32, image::Rgb(sums.map(pillow_clip8)));
        }
    }
    output
}

fn pillow_coefficients(input_size: usize, output_size: usize) -> PillowCoefficients {
    let scale = input_size as f64 / output_size as f64;
    let filter_scale = scale.max(1.0);
    let support = 2.0 * filter_scale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(output_size);
    let mut coefficients = vec![0i32; output_size * ksize];
    let coefficient_scale = (1u64 << PILLOW_PRECISION_BITS) as f64;

    for output in 0..output_size {
        let center = (output as f64 + 0.5) * scale;
        let mut minimum = (center - support + 0.5) as isize;
        minimum = minimum.max(0);
        let mut maximum = (center + support + 0.5) as isize;
        maximum = maximum.min(input_size as isize);
        let count = (maximum - minimum).max(0) as usize;
        let inverse_filter_scale = 1.0 / filter_scale;
        let mut weights = Vec::with_capacity(count);
        let mut weight_sum = 0.0f64;
        for offset in 0..count {
            let distance = (offset as f64 + minimum as f64 - center + 0.5) * inverse_filter_scale;
            let weight = pillow_bicubic_kernel(distance);
            weights.push(weight);
            weight_sum += weight;
        }
        for (offset, weight) in weights.into_iter().enumerate() {
            let normalized = if weight_sum == 0.0 {
                weight
            } else {
                weight / weight_sum
            };
            coefficients[output * ksize + offset] = (normalized * coefficient_scale).round() as i32;
        }
        bounds.push((minimum as usize, count));
    }
    PillowCoefficients {
        ksize,
        bounds,
        coefficients,
    }
}

fn pillow_bicubic_kernel(mut value: f64) -> f64 {
    const A: f64 = -0.5;
    value = value.abs();
    if value < 1.0 {
        return ((A + 2.0) * value - (A + 3.0)) * value * value + 1.0;
    }
    if value < 2.0 {
        return (((value - 5.0) * value + 8.0) * value - 4.0) * A;
    }
    0.0
}

fn pillow_clip8(value: i32) -> u8 {
    (value >> PILLOW_PRECISION_BITS).clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        })
    }

    #[test]
    fn an_image_at_the_target_size_is_returned_unchanged() {
        // The whole capture-side optimisation rests on this: a pre-resized
        // image must pass through the worker's resize untouched, or the two
        // paths would produce different pixels for the same screenshot.
        let image = gradient(224, 224);
        let resized = pillow_bicubic_resize_rgb(&image, 224, 224);
        assert_eq!(resized.as_raw(), image.as_raw());
    }

    #[test]
    fn resizing_twice_through_the_target_size_is_idempotent() {
        // Pre-resizing on the capture path and then letting the worker resize
        // again must equal resizing once. This is the identity that lets the
        // capture path skip the worker's work without changing its output.
        let source = gradient(1920, 1080);
        let once = pillow_bicubic_resize_rgb(&source, 224, 224);
        let twice = pillow_bicubic_resize_rgb(&once, 224, 224);
        assert_eq!(once.as_raw(), twice.as_raw());
    }

    #[test]
    fn a_downscale_squashes_rather_than_crops() {
        // A non-square source must fill the whole square target. If this ever
        // became a center crop, the capture path and the worker would still
        // agree, but both would disagree with the Python oracle.
        let source = gradient(640, 160);
        let resized = pillow_bicubic_resize_rgb(&source, 224, 224);
        assert_eq!(resized.width(), 224);
        assert_eq!(resized.height(), 224);
    }

    #[test]
    fn pillow_bicubic_keeps_constant_rgb_images_constant() {
        // Upscaling a flat image must not ring or drift: the kernel weights sum
        // to one, so every output pixel keeps the input colour.
        let image = RgbImage::from_pixel(3, 5, image::Rgb([24, 92, 180]));
        let resized = pillow_bicubic_resize_rgb(&image, 17, 11);
        assert!(resized.pixels().all(|pixel| pixel.0 == [24, 92, 180]));
    }

    #[test]
    fn a_shortest_edge_config_reads_as_a_square() {
        // The pinned Chinese-CLIP config states only `shortest_edge`, and the
        // engine has always read that as both dimensions. The capture path now
        // depends on the same reading.
        let config = br#"{"size": {"shortest_edge": 224}}"#;
        assert_eq!(target_size_from_config(config), Some((224, 224)));
    }

    #[test]
    fn an_explicit_pair_wins_over_the_shortest_edge() {
        let config = br#"{"size": {"height": 336, "width": 448, "shortest_edge": 224}}"#;
        assert_eq!(target_size_from_config(config), Some((448, 336)));
    }

    #[test]
    fn an_unreadable_config_declines_rather_than_guessing() {
        // Guessing here would mean pre-resizing to a size the worker does not
        // want, and two disagreeing resizes produce a wrong vector rather than
        // a slow one.
        assert_eq!(target_size_from_config(b"not json"), None);
        assert_eq!(target_size_from_config(br#"{"size": {}}"#), None);
    }
}
