//! Explicit foreground screenshot edits. Inference finishes before publication
//! so an OCR failure cannot replace the stored image or its searchable text.

use crate::credential_manager::CredentialManagerState;
use crate::storage::{EditedScreenshotResponse, StorageState};
use base64::Engine;
use image::{ImageDecoder, RgbImage};
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

const MAX_ENCODED_EDIT_BYTES: usize = 64 * 1024 * 1024;

fn decode_edit(encoded: &str, dimensions: (u32, u32)) -> Result<(Vec<u8>, RgbImage), String> {
    if encoded.len() > MAX_ENCODED_EDIT_BYTES {
        return Err("The edited snapshot is too large".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid edited snapshot encoding")?;
    let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&bytes))
        .map_err(|_| "The edited snapshot must be a valid PNG image")?;
    let (width, height) = decoder.dimensions();
    if (width, height) != dimensions || width == 0 || height == 0 {
        return Err("The edited snapshot must preserve the original dimensions".into());
    }
    if u64::from(width) * u64::from(height) * 3 > crate::ml_protocol::MAX_ML_IMAGE_BYTES as u64 {
        return Err("The snapshot is too large for text recognition".into());
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(crate::ml_protocol::MAX_ML_IMAGE_BYTES as u64 * 4);
    decoder
        .set_limits(limits)
        .map_err(|error| error.to_string())?;
    let pixels = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("Failed to decode edited snapshot: {error}"))?
        .to_rgb8();
    Ok((bytes, pixels))
}

pub(crate) async fn save(
    app: tauri::AppHandle,
    storage: Arc<StorageState>,
    credentials: Arc<CredentialManagerState>,
    screenshot_id: i64,
    expected_hash: String,
    image_data: String,
) -> Result<EditedScreenshotResponse, String> {
    if screenshot_id <= 0 || expected_hash.len() > 64 {
        return Err("Invalid screenshot identity".into());
    }
    let prepare_storage = storage.clone();
    let prepare_hash = expected_hash.clone();
    let (record, generation, bytes, pixels) = tokio::task::spawn_blocking(move || {
        let _activity = prepare_storage.foreground_db_read();
        prepare_storage.check_screenshot_edit(screenshot_id, &prepare_hash)?;
        let record = prepare_storage
            .screenshots()
            .by_id(screenshot_id)?
            .ok_or("Screenshot not found")?;
        let dimensions = (
            record.width.ok_or("Snapshot width is unavailable")? as u32,
            record.height.ok_or("Snapshot height is unavailable")? as u32,
        );
        let (bytes, pixels) = decode_edit(&image_data, dimensions)?;
        Ok::<_, String>((
            record,
            prepare_storage.db_generation(),
            bytes,
            Arc::new(pixels),
        ))
    })
    .await
    .map_err(|error| format!("Snapshot preparation failed: {error}"))??;

    let timestamp_ms = record
        .timestamp
        .and_then(|seconds| seconds.checked_mul(1000))
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&record.created_at)
                .ok()
                .map(|time| time.timestamp_millis())
        })
        .ok_or("Invalid snapshot capture time")?;
    let use_directml = crate::capture::OcrRouteConfig::from_app(&app).use_directml;
    let ml = app
        .state::<Arc<crate::ml_runtime::MlRuntimeState>>()
        .inner()
        .clone();
    let output = ml
        .run_ocr(
            app.clone(),
            pixels.clone(),
            Duration::from_secs(180),
            use_directml,
        )
        .await?;
    let results = crate::capture::convert_ml_ocr_blocks(output.blocks)?;
    // Auth can expire during inference. Never start a CNG prompt or publish
    // changed local data after the explicit foreground session has ended.
    crate::commands::check_auth_required(&credentials)?;
    let write_storage = storage.clone();
    let write_results = results.clone();
    let write_pixels = pixels.clone();
    let saved = tokio::task::spawn_blocking(move || {
        write_storage.replace_edited_screenshot(
            screenshot_id,
            &expected_hash,
            generation,
            &bytes,
            &write_pixels,
            &write_results,
        )
    })
    .await
    .map_err(|error| format!("Snapshot save failed: {error}"))??;

    let _ = storage.set_ocr_status(
        screenshot_id,
        "completed",
        Some("rust"),
        Some("ppocrv5-ch-mobile"),
        Some(if use_directml { "directml" } else { "cpu" }),
        None,
        Some(output.timings.request_total_ms),
    );
    // Publication has succeeded. The capture pipeline records recoverable
    // index debt; a follow-up failure must not make the UI retry the image write.
    if let Err(error) = crate::capture::finish_ocr_processing(
        &app,
        &storage,
        screenshot_id,
        pixels,
        &saved.image_hash,
        record.window_title.as_deref().unwrap_or_default(),
        record.process_name.as_deref().unwrap_or_default(),
        timestamp_ms,
        &results,
    )
    .await
    {
        tracing::warn!("Edited screenshot postprocessing deferred id={screenshot_id}: {error}");
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_decoder_preserves_pixel_dimensions_and_rejects_other_formats() {
        let image =
            image::DynamicImage::ImageRgb8(RgbImage::from_pixel(3, 2, image::Rgb([50, 100, 150])));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let (_, pixels) = decode_edit(&encoded, (3, 2)).unwrap();
        assert_eq!(pixels.get_pixel(1, 1), &image::Rgb([50, 100, 150]));
        assert!(decode_edit(&encoded, (2, 3)).is_err());
        let mut jpeg = Cursor::new(Vec::new());
        image.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        assert!(decode_edit(
            &base64::engine::general_purpose::STANDARD.encode(jpeg.into_inner()),
            (3, 2)
        )
        .is_err());
    }
}
