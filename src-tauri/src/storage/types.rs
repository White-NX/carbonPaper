//! Data type definitions for the storage module.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};
use serde::{Deserialize, Serialize};

use super::StorageState;

/// Screenshot record representing a row in the screenshots table, with decrypted fields.
/// This is the main struct used for returning screenshot data to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRecord {
    pub id: i64,
    pub image_path: String,
    pub image_hash: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub created_at: String,
    pub metadata: Option<String>,
    pub timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_links: Option<Vec<VisibleLink>>,
}

/// OcrResult representing a row in the ocr_results table, with decrypted fields.
/// Used for returning OCR data to the frontend and for search results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResult {
    pub id: i64,
    pub screenshot_id: i64,
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
    pub created_at: String,
}

/// A visible link collected from the browser extension, containing the link text and URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibleLink {
    pub text: String,
    pub url: String,
}

/// A visible link with an IDF-weighted relevance score, used for ranking links in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredLink {
    pub text: String,
    pub url: String,
    pub score: f64,
}

/// SearchResult representing the combined data from screenshots and ocr_results
/// for search results returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: i64,
    pub screenshot_id: i64,
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
    pub image_path: String,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub created_at: String,
    pub screenshot_created_at: String,
}

/// The input for saving a screenshot, containing all necessary data and metadata.
/// The image data is expected to be Base64 encoded to allow passing through JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveScreenshotRequest {
    pub image_data: String, // Base64 encoded image data
    pub image_hash: String,
    pub width: i32,
    pub height: i32,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub ocr_results: Option<Vec<OcrResultInput>>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub page_url: Option<String>,
    #[serde(default)]
    pub page_icon: Option<String>,
    #[serde(default)]
    pub visible_links: Option<Vec<VisibleLink>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResultInput {
    pub text: String,
    pub confidence: f64,
    #[serde(rename = "box")]
    pub box_coords: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveScreenshotResponse {
    pub status: String,
    pub screenshot_id: Option<i64>,
    pub image_path: Option<String>,
    pub added: i32,
    pub skipped: i32,
}

/// Raw row data extracted from DB without decryption (for releasing mutex early).
///
/// The `_plain` fields are for backward compatibility with old unencrypted records;
/// they will be ignored if the corresponding `_enc` fields are present and can be
/// decrypted successfully.
pub(super) struct RawScreenshotRow {
    pub(super) id: i64,
    pub(super) image_path: String,
    pub(super) image_hash: String,
    pub(super) width: Option<i32>,
    pub(super) height: Option<i32>,
    pub(super) window_title_plain: Option<String>,
    pub(super) process_name_plain: Option<String>,
    pub(super) metadata_plain: Option<String>,
    pub(super) window_title_enc: Option<Vec<u8>>,
    pub(super) process_name_enc: Option<Vec<u8>>,
    pub(super) metadata_enc: Option<Vec<u8>>,
    pub(super) content_key_enc: Option<Vec<u8>>,
    pub(super) timestamp: Option<i64>,
    pub(super) created_at: String,
    pub(super) source: Option<String>,
    pub(super) page_url_enc: Option<Vec<u8>>,
    pub(super) page_icon_enc: Option<Vec<u8>>,
    pub(super) visible_links_enc: Option<Vec<u8>>,
}

impl RawScreenshotRow {
    /// Decrypt encrypted fields and produce a ScreenshotRecord.
    /// CNG decryption happens here, outside of the DB mutex.
    pub(super) fn into_record(self) -> ScreenshotRecord {
        let mut row_key = self
            .content_key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(enc).ok());

        let window_title = match (self.window_title_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => self.window_title_plain,
        };
        let process_name = match (self.process_name_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => self.process_name_plain,
        };
        let metadata = match (self.metadata_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => self.metadata_plain,
        };

        let page_url = match (self.page_url_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => None,
        };
        let page_icon = match (self.page_icon_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => None,
        };
        let visible_links = match (self.visible_links_enc.as_ref(), row_key.as_ref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .ok()
                .and_then(|v| String::from_utf8(v).ok())
                .and_then(|s| serde_json::from_str::<Vec<VisibleLink>>(&s).ok()),
            _ => None,
        };

        if let Some(ref mut key) = row_key {
            StorageState::zeroize_bytes(key);
        }

        ScreenshotRecord {
            id: self.id,
            image_path: self.image_path,
            image_hash: self.image_hash,
            width: self.width,
            height: self.height,
            window_title,
            process_name,
            timestamp: self.timestamp,
            metadata,
            created_at: self.created_at,
            source: self.source,
            page_url,
            page_icon,
            visible_links,
        }
    }

    /// Extract raw data from a rusqlite Row without any decryption.
    /// Column order must match the standard SELECT used in get_screenshots_by_time_range / get_screenshot_by_id.
    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let timestamp_str: Option<String> = row.get(12)?;
        let timestamp = timestamp_str.and_then(|s| s.parse::<i64>().ok());

        Ok(RawScreenshotRow {
            id: row.get(0)?,
            image_path: row.get(1)?,
            image_hash: row.get(2)?,
            width: row.get(3)?,
            height: row.get(4)?,
            window_title_plain: row.get(5)?,
            process_name_plain: row.get(6)?,
            metadata_plain: row.get(7)?,
            window_title_enc: row.get(8)?,
            process_name_enc: row.get(9)?,
            metadata_enc: row.get(10)?,
            content_key_enc: row.get(11)?,
            timestamp,
            created_at: row.get(13)?,
            source: row.get(14)?,
            page_url_enc: row.get(15)?,
            page_icon_enc: row.get(16)?,
            visible_links_enc: row.get(17)?,
        })
    }
}

/// Migration statistics result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub total_files: usize,
    pub migrated: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}
