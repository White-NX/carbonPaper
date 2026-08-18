//! Bounded protocol shared by CarbonPaper and the isolated Office automation worker.
//!
//! The worker is deliberately a separate process because Office COM calls have no
//! dependable upper latency bound.  The desktop process owns deadlines and may kill
//! the worker; this module keeps the local transport small and validates every frame
//! before either side allocates or trusts it.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const OFFICE_PROTOCOL_VERSION: u32 = 1;
pub const MAX_OFFICE_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_OFFICE_DISPLAY_NAME_CHARS: usize = 1024;
pub const MAX_OFFICE_TITLE_CHARS: usize = 1024;
pub const MAX_OFFICE_LOCATOR_CHARS: usize = 32 * 1024;
pub const MAX_OFFICE_ERROR_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficeApplication {
    Word,
    Excel,
    PowerPoint,
}

impl OfficeApplication {
    pub fn from_process_name(process_name: &str) -> Option<Self> {
        match process_name.trim().to_ascii_lowercase().as_str() {
            "winword" | "winword.exe" => Some(Self::Word),
            "excel" | "excel.exe" => Some(Self::Excel),
            "powerpnt" | "powerpnt.exe" => Some(Self::PowerPoint),
            _ => None,
        }
    }

    pub fn native_window_classes(self) -> &'static [&'static str] {
        match self {
            Self::Word => &["_WwG"],
            Self::Excel => &["EXCEL7"],
            // Current Microsoft 365 builds may expose the editable document
            // surface as mdiClass while older builds use paneClassDC.
            Self::PowerPoint => &["paneClassDC", "mdiClass"],
        }
    }

    pub fn native_window_class_priority(self, class_name: &str) -> Option<usize> {
        self.native_window_classes()
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(class_name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficeDocumentKind {
    LocalFile,
    CloudDocument,
    Unsaved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficeDocumentRef {
    pub provider: String,
    pub application: OfficeApplication,
    pub kind: OfficeDocumentKind,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub observed_at_ms: i64,
    pub confidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OfficeDocumentRefView {
    pub provider: String,
    pub application: OfficeApplication,
    pub kind: OfficeDocumentKind,
    pub display_name: String,
    pub observed_at_ms: i64,
    pub confidence: String,
    pub resumable: bool,
}

impl OfficeDocumentRef {
    pub fn validate(&self) -> Result<(), String> {
        if self.provider != "office_nativeom" {
            return Err("invalid_response: unsupported Office document provider".to_string());
        }
        if self.confidence != "exact" {
            return Err("invalid_response: unsupported Office confidence".to_string());
        }
        if self.observed_at_ms <= 0 {
            return Err("invalid_response: Office observation timestamp is invalid".to_string());
        }
        let display_chars = self.display_name.chars().count();
        if self.display_name.trim().is_empty() || display_chars > MAX_OFFICE_DISPLAY_NAME_CHARS {
            return Err("invalid_response: Office display name is empty or too long".to_string());
        }
        if let Some(locator) = &self.locator {
            if locator.trim().is_empty() || locator.chars().count() > MAX_OFFICE_LOCATOR_CHARS {
                return Err(
                    "invalid_response: Office document locator is empty or too long".to_string(),
                );
            }
        }
        match self.kind {
            OfficeDocumentKind::Unsaved if self.locator.is_some() => {
                Err("invalid_response: unsaved Office document has a locator".to_string())
            }
            OfficeDocumentKind::LocalFile | OfficeDocumentKind::CloudDocument
                if self.locator.is_none() =>
            {
                Err("invalid_response: saved Office document is missing a locator".to_string())
            }
            OfficeDocumentKind::LocalFile => {
                let locator = self.locator.as_deref().unwrap_or_default();
                if std::path::Path::new(locator).is_absolute() {
                    Ok(())
                } else {
                    Err("invalid_response: local Office locator is not absolute".to_string())
                }
            }
            OfficeDocumentKind::CloudDocument => {
                let locator = self.locator.as_deref().unwrap_or_default();
                if locator.starts_with("https://") || locator.starts_with("http://") {
                    Ok(())
                } else {
                    Err("invalid_response: cloud Office locator is not HTTP(S)".to_string())
                }
            }
            _ => Ok(()),
        }
    }

    pub fn public_view(&self) -> OfficeDocumentRefView {
        OfficeDocumentRefView {
            provider: self.provider.clone(),
            application: self.application,
            kind: self.kind,
            display_name: self.display_name.clone(),
            observed_at_ms: self.observed_at_ms,
            confidence: self.confidence.clone(),
            resumable: self.locator.is_some() && self.kind != OfficeDocumentKind::Unsaved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum OfficeRequest {
    Ping {
        request_id: u64,
    },
    Resolve {
        request_id: u64,
        generation: u64,
        application: OfficeApplication,
        root_hwnd: i64,
        document_hwnd: i64,
        pid: u32,
        title: String,
    },
    Shutdown {
        request_id: u64,
    },
}

impl OfficeRequest {
    #[allow(dead_code)]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Ping { request_id }
            | Self::Resolve { request_id, .. }
            | Self::Shutdown { request_id } => *request_id,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if let Self::Resolve {
            generation,
            root_hwnd,
            document_hwnd,
            pid,
            title,
            ..
        } = self
        {
            if *generation == 0 {
                return Err("invalid_request: Office generation must be positive".to_string());
            }
            if *root_hwnd == 0 || *document_hwnd == 0 || *pid == 0 {
                return Err("invalid_request: Office HWND and PID must be positive".to_string());
            }
            if title.chars().count() > MAX_OFFICE_TITLE_CHARS {
                return Err("limit_exceeded: Office window title is too long".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OfficeResponse {
    Ready {
        protocol_version: u32,
        worker_version: String,
    },
    Pong {
        request_id: u64,
    },
    Resolved {
        request_id: u64,
        generation: u64,
        document_hwnd: i64,
        document: Option<OfficeDocumentRef>,
        elapsed_ms: f64,
    },
    Error {
        request_id: u64,
        generation: Option<u64>,
        kind: String,
        message: String,
        elapsed_ms: f64,
    },
    ShuttingDown {
        request_id: u64,
    },
}

impl OfficeResponse {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Ready {
                protocol_version,
                worker_version,
            } => {
                if *protocol_version != OFFICE_PROTOCOL_VERSION || worker_version.trim().is_empty()
                {
                    return Err("invalid_response: invalid Office worker handshake".to_string());
                }
            }
            Self::Resolved {
                generation,
                document_hwnd,
                document,
                elapsed_ms,
                ..
            } => {
                if *generation == 0
                    || *document_hwnd == 0
                    || !elapsed_ms.is_finite()
                    || *elapsed_ms < 0.0
                {
                    return Err("invalid_response: invalid Office resolution envelope".to_string());
                }
                if let Some(document) = document {
                    document.validate()?;
                }
            }
            Self::Error {
                kind,
                message,
                elapsed_ms,
                ..
            } => {
                if kind.trim().is_empty()
                    || kind.chars().count() > 128
                    || message.chars().count() > MAX_OFFICE_ERROR_CHARS
                    || !elapsed_ms.is_finite()
                    || *elapsed_ms < 0.0
                {
                    return Err("invalid_response: invalid Office error envelope".to_string());
                }
            }
            Self::Pong { .. } | Self::ShuttingDown { .. } => {}
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub fn read_request<R: Read>(reader: &mut R) -> Result<OfficeRequest, String> {
    let bytes = read_frame(reader)?;
    let request: OfficeRequest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("protocol: invalid Office request JSON: {error}"))?;
    request.validate()?;
    Ok(request)
}

pub fn write_request<W: Write>(writer: &mut W, request: &OfficeRequest) -> Result<(), String> {
    request.validate()?;
    write_json_frame(writer, request)
}

pub fn read_response<R: Read>(reader: &mut R) -> Result<OfficeResponse, String> {
    let bytes = read_frame(reader)?;
    let response: OfficeResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("protocol: invalid Office response JSON: {error}"))?;
    response.validate()?;
    Ok(response)
}

#[allow(dead_code)]
pub fn write_response<W: Write>(writer: &mut W, response: &OfficeResponse) -> Result<(), String> {
    response.validate()?;
    write_json_frame(writer, response)
}

fn write_json_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("protocol: failed to encode Office frame: {error}"))?;
    if bytes.len() > MAX_OFFICE_FRAME_BYTES {
        return Err(format!(
            "limit_exceeded: Office frame is {} bytes; maximum is {}",
            bytes.len(),
            MAX_OFFICE_FRAME_BYTES
        ));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| "limit_exceeded: Office frame length does not fit u32".to_string())?;
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("transport: failed to write Office frame: {error}"))
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut len_bytes = [0u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .map_err(|error| format!("transport: failed to read Office frame length: {error}"))?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_OFFICE_FRAME_BYTES {
        return Err(format!(
            "limit_exceeded: Office frame length {len} is outside 1..={MAX_OFFICE_FRAME_BYTES}"
        ));
    }
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("transport: failed to read Office frame body: {error}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn process_names_map_to_supported_office_apps() {
        assert_eq!(
            OfficeApplication::from_process_name("WINWORD.EXE"),
            Some(OfficeApplication::Word)
        );
        assert_eq!(
            OfficeApplication::from_process_name("excel"),
            Some(OfficeApplication::Excel)
        );
        assert_eq!(OfficeApplication::from_process_name("outlook.exe"), None);
    }

    #[test]
    fn powerpoint_native_window_classes_are_strict_and_prioritized() {
        let application = OfficeApplication::PowerPoint;
        assert_eq!(
            application.native_window_class_priority("paneClassDC"),
            Some(0)
        );
        assert_eq!(
            application.native_window_class_priority("MDICLASS"),
            Some(1)
        );
        assert_eq!(
            application.native_window_class_priority("MsoWorkPane"),
            None
        );
    }

    #[test]
    fn request_round_trip_is_framed_and_validated() {
        let request = OfficeRequest::Resolve {
            request_id: 7,
            generation: 9,
            application: OfficeApplication::PowerPoint,
            root_hwnd: 42,
            document_hwnd: 43,
            pid: 99,
            title: "Roadmap - PowerPoint".to_string(),
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request).unwrap();
        assert_eq!(read_request(&mut Cursor::new(bytes)).unwrap(), request);
    }

    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let mut bytes = ((MAX_OFFICE_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"ignored");
        let error = read_response(&mut Cursor::new(bytes)).unwrap_err();
        assert!(error.starts_with("limit_exceeded:"));
    }

    #[test]
    fn resolve_requests_reject_oversized_window_titles() {
        let request = OfficeRequest::Resolve {
            request_id: 7,
            generation: 9,
            application: OfficeApplication::Word,
            root_hwnd: 42,
            document_hwnd: 43,
            pid: 99,
            title: "x".repeat(MAX_OFFICE_TITLE_CHARS + 1),
        };

        assert!(write_request(&mut Vec::new(), &request)
            .unwrap_err()
            .starts_with("limit_exceeded:"));
    }

    #[test]
    fn unsaved_documents_cannot_carry_a_locator() {
        let reference = OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::Word,
            kind: OfficeDocumentKind::Unsaved,
            display_name: "Document1".to_string(),
            locator: Some(r"C:\\Document1.docx".to_string()),
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        };
        assert!(reference.validate().is_err());
    }

    #[test]
    fn relative_local_locators_are_rejected() {
        let reference = OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::Excel,
            kind: OfficeDocumentKind::LocalFile,
            display_name: "Book.xlsx".to_string(),
            locator: Some("Book.xlsx".to_string()),
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        };
        assert!(reference.validate().is_err());
    }

    #[test]
    fn public_document_view_does_not_serialize_the_locator() {
        let reference = OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::Word,
            kind: OfficeDocumentKind::LocalFile,
            display_name: "Plan.docx".to_string(),
            locator: Some(r"C:\\private\\Plan.docx".to_string()),
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        };

        let value = serde_json::to_value(reference.public_view()).unwrap();
        assert_eq!(value["resumable"], true);
        assert!(value.get("locator").is_none());
    }
}
