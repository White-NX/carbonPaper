//! On-demand screenshot pixels for interactive search. Images remain in memory
//! and are never included in the frontend's replayable conversation history.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tauri::Manager;

use super::config::{AiSettings, ImageInput, ImageInputOverride, ResolvedProvider};
use super::provider::{ImageDetail, ImagePayload, Message, ToolCall, ToolSpec};
use super::tools::{self, SnapshotRef, ToolOutcome};
use crate::credential_manager::CredentialManagerState;
use crate::pii::{self, Bounds};
use crate::sensitive_filter::{FilterMode, SensitiveFilterState};
use crate::storage::{OcrResult, StorageState};

pub const TOOL_NAME: &str = "view_snapshot";
const HIGH_WIDTH: u32 = 1920;
const HIGH_HEIGHT: u32 = 768;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGE_PATCHES: u64 = 30_000;
const MAX_IMAGE_READS: u32 = 3;
const MAX_RETAINED_IMAGE_BYTES: usize = 128 * 1024 * 1024;

struct CachedImage {
    binding: [u8; 32],
    call_id: String,
    screenshot_id: i64,
    image: ImagePayload,
}

#[derive(Debug, Clone)]
pub struct PreparedImage {
    image: ImagePayload,
    binding: [u8; 32],
}

/// Pixel buffers stay in Rust while replayable history carries random handles.
/// Permission, provider/account, privacy settings and dataset still scope reuse.
#[derive(Default)]
pub(crate) struct ImageCache {
    entries: HashMap<String, CachedImage>,
    order: VecDeque<String>,
    bytes: usize,
}

impl ImageCache {
    fn insert(&mut self, entry: CachedImage) -> String {
        let handle = hex::encode(rand::random::<[u8; 16]>());
        while self.bytes.saturating_add(entry.image.data.len()) > MAX_RETAINED_IMAGE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(old.image.data.len());
            }
        }
        self.bytes += entry.image.data.len();
        self.order.push_back(handle.clone());
        self.entries.insert(handle.clone(), entry);
        handle
    }

    fn get(&mut self, handle: &str, binding: &[u8; 32]) -> Option<Message> {
        let entry = self.entries.get(handle).filter(|e| &e.binding == binding)?;
        let message = Message::SnapshotImage {
            handle: handle.into(),
            call_id: entry.call_id.clone(),
            screenshot_id: entry.screenshot_id,
            image: entry.image.clone(),
        };
        self.order.retain(|value| value != handle);
        self.order.push_back(handle.into());
        Some(message)
    }
}

fn cache_binding(app: &tauri::AppHandle, provider: &ResolvedProvider) -> [u8; 32] {
    let storage = app.state::<Arc<StorageState>>();
    let filter = app.state::<Arc<SensitiveFilterState>>();
    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let bytes = serde_json::to_vec(&(
        provider.id.as_deref(),
        provider.kind,
        provider.base_url.as_str(),
        provider.model.as_str(),
        provider.api_key.as_deref(),
        data_dir,
        storage.db_generation(),
        filter.get_config(),
    ))
    .expect("image cache binding must serialize");
    Sha256::digest(bytes).into()
}

pub fn retain_image(
    app: &tauri::AppHandle,
    call_id: &str,
    screenshot_id: i64,
    prepared: PreparedImage,
) -> Message {
    let runtime = app.state::<super::AiRuntimeState>();
    let handle = runtime
        .image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(CachedImage {
            binding: prepared.binding,
            call_id: call_id.into(),
            screenshot_id,
            image: prepared.image,
        });
    Message::SnapshotImageRef { handle }
}

pub fn hydrate_images(
    app: &tauri::AppHandle,
    provider: &ResolvedProvider,
    messages: &[Message],
) -> Vec<Message> {
    let enabled = check_permission(app, provider).is_ok();
    let binding = cache_binding(app, provider);
    let runtime = app.state::<super::AiRuntimeState>();
    let mut cache = runtime
        .image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    messages.iter().map(|message| match message {
        Message::SnapshotImageRef { handle } => {
            if enabled {
                if let Some(image) = cache.get(handle,&binding) { return image; }
            }
            Message::User("Earlier screenshot pixels are unavailable under the current settings. Reuse recorded observations; reread for missing visual evidence when permitted.".into())
        }
        _ => message.clone(),
    }).collect()
}

pub fn image_reference(message: &Message) -> Message {
    match message {
        Message::SnapshotImage { handle, .. } => Message::SnapshotImageRef {
            handle: handle.clone(),
        },
        _ => message.clone(),
    }
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME.into(),
        description: "View one screenshot for missing visual evidence, explained in purpose. Default high fits within 1920 × 768 without enlarging; original preserves pixels. Optional crop uses x/y/width/height in normalized original-image coordinates [0,1] before resizing. Crop relevant regions for finer detail. Privacy, permission and image budgets apply. Reuse retained images.".into(),
        parameters: json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "screenshot_id": {"type":"integer","minimum":1},
                "purpose": {"type":"string","minLength":1,"maxLength":500},
                "quality": {"type":"string","enum":["high","original"],"default":"high"},
                "crop": {"type":"object","additionalProperties":false,"properties": {
                    "x":{"type":"number","minimum":0,"maximum":1},
                    "y":{"type":"number","minimum":0,"maximum":1},
                    "width":{"type":"number","exclusiveMinimum":0,"maximum":1},
                    "height":{"type":"number","exclusiveMinimum":0,"maximum":1}
                },"required":["x","y","width","height"]}
            }, "required":["screenshot_id","purpose"]
        }),
    }
}

/// GPT-6 Astra's documented 32px patches × 1.2 model multiplier, followed by
/// another 20% allowance. This is a budgeting baseline, not provider billing.
/// https://developers.openai.com/api/docs/guides/images-vision#calculating-costs
pub fn estimated_image_tokens(width: u32, height: u32) -> u64 {
    let patches = u64::from(width.div_ceil(32)) * u64::from(height.div_ceil(32));
    let model_tokens = (patches * 6).div_ceil(5);
    (model_tokens * 6).div_ceil(5)
}

pub struct ImageBudget {
    reads: u32,
    used_tokens: u64,
    token_limit: u64,
}

impl ImageBudget {
    pub fn new(context_tokens: u32) -> Self {
        Self {
            reads: 0,
            used_tokens: 0,
            token_limit: (u64::from(context_tokens) / 2).min(30_000),
        }
    }

    fn reserve_read(&mut self) -> Result<(), String> {
        if self.reads >= MAX_IMAGE_READS {
            return Err(
                "IMAGE_BUDGET_EXCEEDED: Image read limit reached. Answer from existing evidence."
                    .into(),
            );
        }
        self.reads += 1;
        Ok(())
    }

    fn reserve_tokens(&mut self, tokens: u64) -> Result<(), String> {
        if tokens > self.token_limit.saturating_sub(self.used_tokens) {
            return Err("IMAGE_BUDGET_EXCEEDED: This image exceeds the remaining budget. Read a smaller crop or use OCR.".into());
        }
        self.used_tokens += tokens;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct Crop {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Crop {
    fn pixels(self, width: u32, height: u32) -> Result<(u32, u32, u32, u32), String> {
        if ![self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            || self.x < 0.0
            || self.y < 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            return Err(
                "INVALID_ARGUMENT: crop must fit inside normalized original-image coordinates."
                    .into(),
            );
        }
        let left = (self.x * f64::from(width)).floor() as u32;
        let top = (self.y * f64::from(height)).floor() as u32;
        let right = ((self.x + self.width) * f64::from(width)).ceil() as u32;
        let bottom = ((self.y + self.height) * f64::from(height)).ceil() as u32;
        if right.min(width) <= left || bottom.min(height) <= top {
            return Err("INVALID_ARGUMENT: crop must include at least one source pixel.".into());
        }
        Ok((left, top, right.min(width) - left, bottom.min(height) - top))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    screenshot_id: i64,
    purpose: String,
    #[serde(default)]
    quality: ImageDetail,
    crop: Option<Crop>,
}

/// Recheck a saved endpoint's permission immediately before reads and sends.
pub fn check_permission(app: &tauri::AppHandle, provider: &ResolvedProvider) -> Result<(), String> {
    let credentials = app.state::<Arc<CredentialManagerState>>();
    crate::commands::check_auth_required(&credentials)?;
    if crate::maintenance::is_active() {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
    }
    if !provider.allow_images || provider.image_input != ImageInput::Supported {
        return Err("AI_IMAGES_DISABLED".into());
    }
    let id = provider.id.as_deref().ok_or("AI_IMAGES_DISABLED")?;
    let storage = app.state::<Arc<StorageState>>();
    let settings = AiSettings::load(&storage)?;
    let current = settings.find(id).ok_or("AI_IMAGES_DISABLED")?;
    if !current.allow_images
        || current.image_input_override == ImageInputOverride::Unsupported
        || current.kind != provider.kind
        || current.base_url != provider.base_url
        || current.model != provider.model
    {
        return Err("AI_IMAGES_DISABLED".into());
    }
    let current = settings.resolve(&storage, &credentials, id)?;
    if current.api_key != provider.api_key
        || super::catalog::resolve_image_input(&storage, &current) != ImageInput::Supported
    {
        return Err("AI_IMAGES_DISABLED".into());
    }
    Ok(())
}

pub async fn execute(
    app: &tauri::AppHandle,
    call: &ToolCall,
    provider: &ResolvedProvider,
    budget: &mut ImageBudget,
) -> ToolOutcome {
    let work = async {
        check_permission(app, provider)?;
        let binding = cache_binding(app, provider);
        let input: ReadInput = serde_json::from_str(&call.arguments)
            .map_err(|_| "INVALID_ARGUMENT: Invalid screenshot image arguments.".to_string())?;
        if input.screenshot_id <= 0
            || input.purpose.trim().is_empty()
            || input.purpose.chars().count() > 500
        {
            return Err(
                "INVALID_ARGUMENT: A screenshot_id and a short purpose are required.".into(),
            );
        }
        budget.reserve_read()?;
        let storage = app.state::<Arc<StorageState>>().inner().clone();
        let filter = app.state::<Arc<SensitiveFilterState>>().inner().clone();
        let prepared = tokio::task::spawn_blocking(move || prepare(&storage, &filter, input))
            .await
            .map_err(|_| "IMAGE_READ_FAILED: Image preparation failed.".to_string())??;
        check_permission(app, provider)?;
        if cache_binding(app, provider) != binding {
            return Err("PRIVACY_REJECTED: Settings or the dataset changed while preparing the image. Retry the read.".into());
        }
        budget.reserve_tokens(estimated_image_tokens(prepared.0.width, prepared.0.height))?;
        Ok::<_, String>((prepared, binding))
    }
    .await;
    match work {
        Ok(((image, reference, metadata), binding)) => ToolOutcome {
            content: metadata.to_string(),
            snapshots: vec![reference],
            item_count: Some(1),
            error: None,
            image: Some(PreparedImage { image, binding }),
        },
        Err(error) => tools::failure(error),
    }
}

fn prepare(
    storage: &StorageState,
    filter: &SensitiveFilterState,
    input: ReadInput,
) -> Result<(ImagePayload, SnapshotRef, serde_json::Value), String> {
    let _activity = storage.foreground_db_read();
    let record = storage
        .screenshots()
        .by_id(input.screenshot_id)?
        .ok_or_else(|| {
            format!(
                "SNAPSHOT_NOT_FOUND: Screenshot {} was not found.",
                input.screenshot_id
            )
        })?;
    // A filtered identity has no reliable pixel location. Keep the whole image
    // withheld, including crops, rather than reveal it through another path.
    for text in [
        record.window_title.as_deref(),
        record.process_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let clean = crate::mcp_server::filter_identity(filter, filter.mode(), text)
            .map_err(|_| "PRIVACY_REJECTED: Rejected by user's privacy settings.".to_string())?;
        if clean != text {
            return Err(
                "PRIVACY_REJECTED: Filtered identity cannot be located reliably in the image."
                    .into(),
            );
        }
    }
    if let Some(url) = &record.page_url {
        let clean = crate::mcp_server::filter_url(filter, filter.mode(), url)
            .map_err(|_| "PRIVACY_REJECTED: Rejected by user's privacy settings.".to_string())?;
        if clean != *url {
            return Err(
                "PRIVACY_REJECTED: Filtered page identity cannot be located reliably in the image."
                    .into(),
            );
        }
    }
    let blocks = storage.get_screenshot_ocr_results(input.screenshot_id)?;
    if let Some(links) = &record.visible_links {
        for link in links {
            let findings = filter.inspect(&link.text, pii::Context::default());
            if (findings.is_flagged() || !findings.pii.is_empty())
                && !blocks.iter().any(|block| block.text.contains(&link.text))
            {
                return Err(
                    "PRIVACY_REJECTED: Filtered link text has no reliable image coordinates."
                        .into(),
                );
            }
        }
    }
    let (encoded, _) = storage.read_image_by_id(input.screenshot_id)?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "IMAGE_READ_FAILED: Invalid image encoding.".to_string())?;
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| "IMAGE_READ_FAILED: Unknown image format.".to_string())?;
    let dimensions = reader
        .into_dimensions()
        .map_err(|_| "IMAGE_READ_FAILED: Image dimensions are unavailable.".to_string())?;
    if dimensions.0 == 0
        || dimensions.1 == 0
        || u64::from(dimensions.0) * u64::from(dimensions.1) > 32_000_000
    {
        return Err("IMAGE_BUDGET_EXCEEDED: Image dimensions are too large. Use OCR or a smaller source image.".into());
    }
    let mut image = image::load_from_memory(&bytes)
        .map_err(|_| "IMAGE_READ_FAILED: Image could not be decoded.".to_string())?;
    let redacted = redact(&mut image, filter, &blocks)?;
    let original = image.dimensions();
    if redacted
        && (record.width.is_some_and(|w| w != original.0 as i32)
            || record.height.is_some_and(|h| h != original.1 as i32))
    {
        return Err("PRIVACY_REJECTED: OCR coordinates do not match the image dimensions.".into());
    }
    let crop = input
        .crop
        .map(|crop| crop.pixels(original.0, original.1))
        .transpose()?;
    if let Some((x, y, width, height)) = crop {
        image = image.crop_imm(x, y, width, height);
    }
    if input.quality == ImageDetail::High {
        let (width, height) = high_dimensions(image.width(), image.height());
        if image.dimensions() != (width, height) {
            image = image.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
        }
    }
    let (width, height) = image.dimensions();
    if u64::from(width.div_ceil(32)) * u64::from(height.div_ceil(32)) > MAX_IMAGE_PATCHES {
        return Err("IMAGE_BUDGET_EXCEEDED: Original image exceeds the patch limit. Read a smaller crop or high quality.".into());
    }
    let (encoded, mime_type) = encode(image)?;
    let reference = SnapshotRef {
        screenshot_id: record.id,
        captured_at_ms: record.timestamp.and_then(|s| s.checked_mul(1000)),
        window_title: record.window_title,
        process_name: record.process_name,
    };
    let metadata = json!({
        "status":"found", "screenshot_id":record.id, "captured_at_ms":reference.captured_at_ms,
        "quality":input.quality, "width":width, "height":height,
        "estimated_tokens":estimated_image_tokens(width,height),
        "original_width":original.0, "original_height":original.1,
        "crop_pixels":crop.map(|(x,y,width,height)| json!({"x":x,"y":y,"width":width,"height":height})),
        "image_retention":"Image remains in the conversation while context permits. Reuse it for follow-up questions; reread only when omitted or a different crop or quality is needed."
    });
    Ok((
        ImagePayload {
            data: Arc::from(encoded),
            mime_type,
            width,
            height,
            detail: input.quality,
        },
        reference,
        metadata,
    ))
}

fn high_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(HIGH_WIDTH) / f64::from(width))
        .min(f64::from(HIGH_HEIGHT) / f64::from(height))
        .min(1.0);
    (
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    )
}

/// Apply the same neighbour-aware findings as OCR filtering. Opaque whole-block
/// rectangles also cover numbers that the text filter masks without rejecting.
fn redact(
    image: &mut DynamicImage,
    filter: &SensitiveFilterState,
    blocks: &[OcrResult],
) -> Result<bool, String> {
    let layout: Vec<_> = blocks
        .iter()
        .map(|b| (b.text.as_str(), Bounds::from_points(&b.box_coords)))
        .collect();
    let contexts = pii::block_contexts(&layout);
    let (width, height) = image.dimensions();
    let mut regions = Vec::new();
    for (((block, bounds), original), context) in layout.iter().zip(blocks).zip(contexts) {
        let findings = filter.inspect(block, context);
        if findings.is_flagged() && filter.mode() == FilterMode::Reject {
            return Err("PRIVACY_REJECTED: Rejected by user's privacy settings.".into());
        }
        if !findings.is_flagged() && findings.pii.is_empty() {
            continue;
        }
        if original
            .box_coords
            .iter()
            .any(|point| point.len() < 2 || !point[0].is_finite() || !point[1].is_finite())
        {
            return Err("PRIVACY_REJECTED: Filtered OCR has invalid image coordinates.".into());
        }
        let bounds =
            bounds.ok_or("PRIVACY_REJECTED: Filtered OCR has no reliable image coordinates.")?;
        if bounds.left < 0.0
            || bounds.top < 0.0
            || bounds.right > f64::from(width)
            || bounds.bottom > f64::from(height)
        {
            return Err("PRIVACY_REJECTED: Filtered OCR lies outside the image.".into());
        }
        regions.push((
            (bounds.left.floor() as u32).saturating_sub(4),
            (bounds.top.floor() as u32).saturating_sub(4),
            (bounds.right.ceil() as u32).saturating_add(4).min(width),
            (bounds.bottom.ceil() as u32).saturating_add(4).min(height),
        ));
    }
    let redacted = !regions.is_empty();
    if redacted {
        let mut pixels = image.to_rgba8();
        for (left, top, right, bottom) in regions {
            for y in top..bottom {
                for x in left..right {
                    pixels.put_pixel(x, y, Rgba([32, 32, 32, 255]));
                }
            }
        }
        *image = DynamicImage::ImageRgba8(pixels);
    }
    Ok(redacted)
}

fn encode(image: DynamicImage) -> Result<(String, String), String> {
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| "IMAGE_READ_FAILED: Image encoding failed.".to_string())?;
    let mut bytes = png.into_inner();
    let mut mime = "image/png";
    if bytes.len() > MAX_IMAGE_BYTES {
        bytes.clear();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image.to_rgb8())
            .map_err(|_| "IMAGE_READ_FAILED: Image encoding failed.".to_string())?;
        mime = "image/jpeg";
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("IMAGE_BUDGET_EXCEEDED: Encoded image is too large. Read a smaller crop or high quality.".into());
    }
    Ok((STANDARD.encode(bytes), mime.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_preserves_aspect_ratio_without_upscaling_and_crop_uses_original_pixels() {
        assert_eq!(high_dimensions(3840, 2160), (1365, 768));
        assert_eq!(high_dimensions(2560, 800), (1920, 600));
        assert_eq!(high_dimensions(800, 600), (800, 600));
        let crop = Crop {
            x: 0.5,
            y: 0.25,
            width: 0.25,
            height: 0.5,
        };
        assert_eq!(crop.pixels(3840, 2160).unwrap(), (1920, 540, 960, 1080));
        assert!(Crop {
            width: 0.75,
            ..crop
        }
        .pixels(3840, 2160)
        .is_err());
        assert!(Crop {
            x: 1.0,
            width: f64::EPSILON / 2.0,
            ..crop
        }
        .pixels(3840, 2160)
        .is_err());
    }

    #[test]
    fn patch_budget_includes_model_multiplier_and_separate_safety_margin() {
        assert_eq!(estimated_image_tokens(1024, 1024), 1475);
        assert_eq!(estimated_image_tokens(1920, 768), 2074);
        assert!(estimated_image_tokens(33, 32) > estimated_image_tokens(32, 32));
        let mut budget = ImageBudget::new(8192);
        budget.reserve_read().unwrap();
        budget
            .reserve_tokens(estimated_image_tokens(1920, 768))
            .unwrap();
        assert!(budget
            .reserve_tokens(estimated_image_tokens(1920, 768))
            .is_err());
        budget.reserve_read().unwrap();
        budget.reserve_read().unwrap();
        assert!(budget.reserve_read().is_err());
    }

    #[test]
    fn privacy_masks_contextual_pii_and_rejects_missing_coordinates() {
        let filter = SensitiveFilterState::default();
        let block = |text: &str, coords: Vec<Vec<f64>>| OcrResult {
            text_layout: None,
            id: 1,
            screenshot_id: 1,
            text: text.into(),
            confidence: 1.0,
            box_coords: coords,
            created_at: String::new(),
        };
        let mut image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            200,
            100,
            Rgba([255, 255, 255, 255]),
        ));
        let blocks = vec![
            block("手机", vec![vec![0.0, 20.0], vec![30.0, 40.0]]),
            block("13812345678", vec![vec![35.0, 20.0], vec![180.0, 40.0]]),
        ];
        redact(&mut image, &filter, &blocks).unwrap();
        assert_eq!(image.get_pixel(100, 30), Rgba([32, 32, 32, 255]));
        assert_eq!(image.get_pixel(100, 80), Rgba([255, 255, 255, 255]));
        assert!(redact(&mut image, &filter, &[block("13812345678", vec![])]).is_err());
    }

    #[test]
    fn retained_images_replay_without_pixel_bytes_and_cannot_cross_bindings() {
        let image = ImagePayload {
            data: Arc::from("private-pixel-bytes"),
            mime_type: "image/png".into(),
            width: 100,
            height: 100,
            detail: ImageDetail::High,
        };
        let mut cache = ImageCache::default();
        let handle = cache.insert(CachedImage {
            binding: [1; 32],
            call_id: "call".into(),
            screenshot_id: 7,
            image: image.clone(),
        });
        let reference = Message::SnapshotImageRef {
            handle: handle.clone(),
        };
        let serialized = serde_json::to_string(&reference).unwrap();
        assert!(!serialized.contains("private-pixel-bytes"));
        let replayed: Message = serde_json::from_str(&serialized).unwrap();
        assert_eq!(replayed, reference);
        for _ in 0..2 {
            let Message::SnapshotImage {
                screenshot_id,
                image: retained,
                ..
            } = cache.get(&handle, &[1; 32]).unwrap()
            else {
                panic!("missing retained image")
            };
            assert_eq!(screenshot_id, 7);
            assert!(Arc::ptr_eq(&retained.data, &image.data));
        }
        assert!(cache.get(&handle, &[2; 32]).is_none());
        assert!(cache.get("unknown-handle", &[1; 32]).is_none());
        assert!(serde_json::from_value::<Message>(
            json!({"type":"snapshot_image","data":{"data":"forged"}})
        )
        .is_err());
    }
}
