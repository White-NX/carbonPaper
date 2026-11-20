//! Foreground image saves and independently owned, restartable OCR processing.

use crate::credential_manager::CredentialManagerState;
use crate::storage::{EditOcrJob, EditOcrStatus, EditedScreenshotResponse, StorageState};
use base64::Engine;
use image::{ImageDecoder, RgbImage};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tokio::sync::Notify;

const MAX_ENCODED_EDIT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct ScreenshotEditRuntime {
    wake: Notify,
    snapshots: Mutex<HashMap<i64, Weak<tokio::sync::Mutex<()>>>>,
}

impl ScreenshotEditRuntime {
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    // Only publication and postprocessing of the same snapshot share a gate.
    // Model inference and unrelated foreground operations never hold it.
    pub(crate) fn snapshot_gate(&self, id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        snapshots.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = snapshots.get(&id).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        snapshots.insert(id, Arc::downgrade(&gate));
        gate
    }
}

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
    let (generation, bytes, pixels) = tokio::task::spawn_blocking(move || {
        let _activity = prepare_storage.foreground_db_read()?;
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
        Ok::<_, String>((prepare_storage.db_generation(), bytes, pixels))
    })
    .await
    .map_err(|error| format!("Snapshot preparation failed: {error}"))??;

    let runtime = app.state::<Arc<ScreenshotEditRuntime>>().inner().clone();
    let gate = runtime.snapshot_gate(screenshot_id);
    let _publication = gate.lock().await;
    crate::commands::check_auth_required(&credentials)?;
    let write_storage = storage.clone();
    let saved = tokio::task::spawn_blocking(move || {
        write_storage.save_edited_screenshot_image(
            screenshot_id,
            &expected_hash,
            generation,
            &bytes,
            &pixels,
        )
    })
    .await
    .map_err(|error| format!("Snapshot save failed: {error}"))??;

    runtime.wake();
    Ok(saved)
}

pub(crate) async fn run_edit_ocr_loop(app: tauri::AppHandle) {
    let runtime = app.state::<Arc<ScreenshotEditRuntime>>().inner().clone();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let mut recovered_generation = None;
    loop {
        if storage.is_initialized() && !crate::maintenance::is_active() {
            let generation = storage.db_generation();
            if recovered_generation != Some(generation) {
                let recovery_storage = storage.clone();
                if matches!(
                    tokio::task::spawn_blocking(
                        move || recovery_storage.recover_edited_ocr_jobs(generation)
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    recovered_generation = Some(generation);
                }
            }
            let claim_storage = storage.clone();
            match tokio::task::spawn_blocking(move || claim_storage.claim_edited_ocr_job()).await {
                Ok(Ok(Some(job))) => {
                    let _ = app.emit(
                        "screenshot-ocr-updated",
                        EditOcrStatus {
                            screenshot_id: job.screenshot_id,
                            image_hash: job.image_hash.clone(),
                            status: "processing".into(),
                        },
                    );
                    if let Err(error) = recognize_edit(&app, storage.clone(), job.clone()).await {
                        tracing::warn!(
                            "[EDIT:OCR] recognition deferred screenshot_id={} image_hash={}: {}",
                            job.screenshot_id,
                            job.image_hash,
                            error
                        );
                        let waiting =
                            !storage.is_silent_read_authorized() || error == "AUTH_REQUIRED";
                        let retry_storage = storage.clone();
                        if let Ok(Ok(Some(status))) = tokio::task::spawn_blocking(move || {
                            retry_storage.defer_edited_ocr_job(&job, &error, waiting)
                        })
                        .await
                        {
                            let _ = app.emit("screenshot-ocr-updated", status);
                        }
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Err(error)) => tracing::debug!("[EDIT:OCR] queue unavailable: {error}"),
                Err(error) => tracing::warn!("[EDIT:OCR] queue task failed: {error}"),
                Ok(Ok(None)) => {}
            }
        }
        tokio::select! {
            _ = runtime.wake.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
        }
    }
}

async fn recognize_edit(
    app: &tauri::AppHandle,
    storage: Arc<StorageState>,
    job: EditOcrJob,
) -> Result<(), String> {
    let read_storage = storage.clone();
    let read_job = job.clone();
    let (record, pixels) = tokio::task::spawn_blocking(move || {
        let _activity = read_storage.background_db_read();
        if read_storage.db_generation() != read_job.generation
            || !read_storage
                .screenshot_image_is_current(read_job.screenshot_id, &read_job.image_hash)?
        {
            return Err("Snapshot changed before recognition".into());
        }
        if !read_storage.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        let record = read_storage
            .get_screenshot_summaries_by_ids_silent(&[read_job.screenshot_id])
            .map_err(|error| match error {
                crate::storage::BackgroundReadError::AuthRequired => "AUTH_REQUIRED".into(),
                other => other.to_string(),
            })?
            .into_iter()
            .next()
            .ok_or("Snapshot not found")?;
        let (bytes, _) = read_storage
            .read_image_bytes_silent(&read_job.image_path)
            .map_err(|error| match error {
                crate::storage::BackgroundReadError::AuthRequired => "AUTH_REQUIRED".into(),
                other => other.to_string(),
            })?;
        let pixels = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|error| format!("Failed to decode saved snapshot: {error}"))?
            .to_rgb8();
        Ok::<_, String>((record, Arc::new(pixels)))
    })
    .await
    .map_err(|error| error.to_string())??;
    let use_directml = crate::capture::OcrRouteConfig::from_app(app).use_directml;
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
    let write_storage = storage.clone();
    let write_job = job.clone();
    let write_results = results.clone();
    let completed = tokio::task::spawn_blocking(move || {
        write_storage.complete_edited_ocr_job(
            &write_job,
            &write_results,
            output.timings.request_total_ms,
            if use_directml { "directml" } else { "cpu" },
        )
    })
    .await
    .map_err(|error| error.to_string())??;
    if !completed {
        return Ok(());
    }
    if storage.db_generation() != job.generation {
        return Ok(());
    }
    let _ = app.emit(
        "screenshot-ocr-updated",
        EditOcrStatus {
            screenshot_id: job.screenshot_id,
            image_hash: job.image_hash.clone(),
            status: "completed".into(),
        },
    );
    // Recognition is durable. Derived work has its own recoverable debt and
    // must not turn a completed recognition into another OCR attempt.
    if let Err(error) = crate::capture::finish_ocr_processing(
        app,
        &storage,
        job.screenshot_id,
        pixels,
        &job.image_hash,
        record.window_title.as_deref().unwrap_or_default(),
        record.process_name.as_deref().unwrap_or_default(),
        job.timestamp_ms,
        &results,
        job.generation,
    )
    .await
    {
        tracing::warn!(
            "[EDIT:OCR] postprocessing deferred screenshot_id={}: {error}",
            job.screenshot_id
        );
    }
    Ok(())
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
