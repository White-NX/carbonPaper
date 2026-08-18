//! STA Office automation used only by the isolated Office worker process.

use crate::office_protocol::{
    OfficeApplication, OfficeDocumentKind, OfficeDocumentRef, MAX_OFFICE_DISPLAY_NAME_CHARS,
    MAX_OFFICE_LOCATOR_CHARS,
};
use crate::office_window::validate_office_window_context;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::core::{Interface, BSTR, GUID, PCWSTR, VARIANT};
use windows::Win32::Foundation::{HWND, RPC_E_CALL_REJECTED, RPC_E_SERVERCALL_RETRYLATER};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IDispatch, COINIT_APARTMENTTHREADED, DISPATCH_PROPERTYGET,
    DISPPARAMS,
};
use windows::Win32::System::Variant::{VT_DISPATCH, VT_EMPTY, VT_NULL, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::AccessibleObjectFromWindow;
use windows::Win32::UI::WindowsAndMessaging::OBJID_NATIVEOM;

const PROPERTY_RETRY_DELAYS_MS: &[u64] = &[0, 40, 80, 160, 320];
const LOCALE_USER_DEFAULT: u32 = 0x0400;

#[derive(Debug)]
pub struct OfficeComError {
    pub kind: String,
    pub message: String,
}

impl OfficeComError {
    fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

/// RAII guard proving that the worker's request thread is an STA.
pub struct OfficeComApartment;

impl OfficeComApartment {
    pub fn initialize() -> Result<Self, String> {
        // SAFETY: the helper initializes COM once on its main thread and the
        // returned guard calls CoUninitialize on that same thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|error| format!("failed to initialize Office STA COM apartment: {error:?}"))?;
        Ok(Self)
    }
}

impl Drop for OfficeComApartment {
    fn drop(&mut self) {
        // SAFETY: paired with the successful CoInitializeEx call above.
        unsafe { CoUninitialize() };
    }
}

/// Resolve the document represented by a verified Office native view window.
pub fn resolve_document(
    application: OfficeApplication,
    root_hwnd: i64,
    requested_document_hwnd: i64,
    pid: u32,
    title: &str,
) -> Result<(i64, Option<OfficeDocumentRef>), OfficeComError> {
    if !validate_office_window_context(root_hwnd, requested_document_hwnd, pid, application, title)
    {
        return Err(OfficeComError::new(
            "window_changed",
            "Office window context changed before COM resolution",
        ));
    }

    // Do not substitute a newly discovered child HWND here. The parent captured
    // a specific document surface and will associate the result with that
    // fingerprint; resolving a replacement child could bind the wrong document.
    let document_hwnd = requested_document_hwnd;

    let native = native_object(document_hwnd)?;
    let document = match application {
        OfficeApplication::Word => property_dispatch_optional(&native, "Document")?,
        OfficeApplication::Excel => resolve_excel_workbook(&native)?,
        OfficeApplication::PowerPoint => property_dispatch_optional(&native, "Presentation")?,
    };

    let Some(document) = document else {
        if !validate_office_window_context(root_hwnd, document_hwnd, pid, application, title) {
            return Err(OfficeComError::new(
                "window_changed",
                "Office window context changed during COM resolution",
            ));
        }
        return Ok((document_hwnd, None));
    };
    let reference = read_document_reference(application, &document)?;
    if !validate_office_window_context(root_hwnd, document_hwnd, pid, application, title) {
        return Err(OfficeComError::new(
            "window_changed",
            "Office window context changed during COM resolution",
        ));
    }
    Ok((document_hwnd, reference))
}

fn resolve_excel_workbook(window: &IDispatch) -> Result<Option<IDispatch>, OfficeComError> {
    if let Some(sheet) = property_dispatch_optional(window, "ActiveSheet")? {
        if let Some(workbook) = property_dispatch_optional(&sheet, "Parent")? {
            return Ok(Some(workbook));
        }
    }

    let Some(application) = property_dispatch_optional(window, "Application")? else {
        return Ok(None);
    };
    property_dispatch_optional(&application, "ActiveWorkbook")
}

fn read_document_reference(
    application: OfficeApplication,
    document: &IDispatch,
) -> Result<Option<OfficeDocumentRef>, OfficeComError> {
    let name = property_string(document, "Name")?.trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_OFFICE_DISPLAY_NAME_CHARS {
        return Err(OfficeComError::new(
            "limit_exceeded",
            "Office document name exceeds the protocol limit",
        ));
    }

    let path = property_string(document, "Path")?.trim().to_string();
    let full_name = property_string(document, "FullName")?.trim().to_string();
    let (kind, locator) = classify_locator(&name, &path, &full_name)?;
    let reference = OfficeDocumentRef {
        provider: "office_nativeom".to_string(),
        application,
        kind,
        display_name: name,
        locator,
        observed_at_ms: chrono::Utc::now().timestamp_millis(),
        confidence: "exact".to_string(),
    };
    reference
        .validate()
        .map_err(|message| OfficeComError::new("invalid_document", message))?;
    Ok(Some(reference))
}

fn classify_locator(
    name: &str,
    path: &str,
    full_name: &str,
) -> Result<(OfficeDocumentKind, Option<String>), OfficeComError> {
    let full_is_cloud = is_http_locator(full_name);
    let path_is_cloud = is_http_locator(path);
    let full_is_local = !full_name.is_empty() && Path::new(full_name).is_absolute();

    let result = if full_is_cloud {
        (
            OfficeDocumentKind::CloudDocument,
            Some(full_name.to_string()),
        )
    } else if path_is_cloud {
        let locator = if full_name.is_empty() || full_name == name {
            format!("{}/{}", path.trim_end_matches('/'), name)
        } else {
            full_name.to_string()
        };
        (OfficeDocumentKind::CloudDocument, Some(locator))
    } else if full_is_local {
        (OfficeDocumentKind::LocalFile, Some(full_name.to_string()))
    } else if !path.is_empty() && Path::new(path).is_absolute() {
        let locator = PathBuf::from(path).join(name).to_string_lossy().to_string();
        (OfficeDocumentKind::LocalFile, Some(locator))
    } else {
        (OfficeDocumentKind::Unsaved, None)
    };

    if let Some(locator) = &result.1 {
        if locator.chars().count() > MAX_OFFICE_LOCATOR_CHARS {
            return Err(OfficeComError::new(
                "limit_exceeded",
                "Office document locator exceeds the protocol limit",
            ));
        }
    }
    Ok(result)
}

fn is_http_locator(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn native_object(document_hwnd: i64) -> Result<IDispatch, OfficeComError> {
    let hwnd_raw = isize::try_from(document_hwnd)
        .map_err(|_| OfficeComError::new("invalid_window", "Office HWND does not fit isize"))?;
    let hwnd = HWND(hwnd_raw as *mut _);
    let mut raw = std::ptr::null_mut();

    for (index, delay_ms) in PROPERTY_RETRY_DELAYS_MS.iter().copied().enumerate() {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        // SAFETY: the HWND was validated immediately before this call; `raw`
        // receives one owned IDispatch reference when the call succeeds.
        match unsafe {
            AccessibleObjectFromWindow(hwnd, OBJID_NATIVEOM.0 as u32, &IDispatch::IID, &mut raw)
        } {
            Ok(()) if !raw.is_null() => {
                // SAFETY: AccessibleObjectFromWindow returned an owned pointer
                // for IID_IDispatch, transferred into the RAII interface wrapper.
                return Ok(unsafe { IDispatch::from_raw(raw) });
            }
            Ok(()) => {
                return Err(OfficeComError::new(
                    "native_object_unavailable",
                    "Office returned a null native object",
                ));
            }
            Err(error)
                if is_retryable_call(&error) && index + 1 < PROPERTY_RETRY_DELAYS_MS.len() =>
            {
                continue;
            }
            Err(error) => return Err(map_com_error("AccessibleObjectFromWindow", error)),
        }
    }
    Err(OfficeComError::new(
        "call_rejected",
        "Office native object call remained busy after retries",
    ))
}

fn property_dispatch_optional(
    dispatch: &IDispatch,
    name: &str,
) -> Result<Option<IDispatch>, OfficeComError> {
    let value = invoke_property(dispatch, name)?;
    let variant_type = variant_type(&value);
    if variant_type == VT_EMPTY.0 || variant_type == VT_NULL.0 {
        return Ok(None);
    }
    variant_into_dispatch(value).map(Some)
}

fn property_string(dispatch: &IDispatch, name: &str) -> Result<String, OfficeComError> {
    let value = invoke_property(dispatch, name)?;
    let variant_type = variant_type(&value);
    if variant_type == VT_EMPTY.0 || variant_type == VT_NULL.0 {
        return Ok(String::new());
    }
    BSTR::try_from(&value)
        .map(|value| value.to_string())
        .map_err(|error| {
            OfficeComError::new(
                "invalid_property",
                format!("Office property {name} was not text: {error:?}"),
            )
        })
}

fn invoke_property(dispatch: &IDispatch, name: &str) -> Result<VARIANT, OfficeComError> {
    for (index, delay_ms) in PROPERTY_RETRY_DELAYS_MS.iter().copied().enumerate() {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        match invoke_property_once(dispatch, name) {
            Ok(value) => return Ok(value),
            Err(error)
                if is_retryable_call(&error) && index + 1 < PROPERTY_RETRY_DELAYS_MS.len() =>
            {
                continue;
            }
            Err(error) => return Err(map_com_error(name, error)),
        }
    }
    Err(OfficeComError::new(
        "call_rejected",
        format!("Office property {name} remained busy after retries"),
    ))
}

fn invoke_property_once(dispatch: &IDispatch, name: &str) -> windows::core::Result<VARIANT> {
    let wide_name = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let name_ptr = PCWSTR(wide_name.as_ptr());
    let iid_null = GUID::zeroed();
    let mut dispid = 0;
    let parameters = DISPPARAMS::default();
    let mut result = VARIANT::new();

    // SAFETY: all pointers reference stack data valid for the synchronous COM
    // calls; no arguments are supplied for a DISPATCH_PROPERTYGET invocation.
    unsafe {
        dispatch.GetIDsOfNames(&iid_null, &name_ptr, 1, LOCALE_USER_DEFAULT, &mut dispid)?;
        dispatch.Invoke(
            dispid,
            &iid_null,
            LOCALE_USER_DEFAULT,
            DISPATCH_PROPERTYGET,
            &parameters,
            Some(&mut result),
            None,
            None,
        )?;
    }
    Ok(result)
}

fn variant_type(value: &VARIANT) -> u16 {
    // SAFETY: every initialized VARIANT has a readable discriminator.
    unsafe { value.as_raw().Anonymous.Anonymous.vt }
}

fn variant_into_dispatch(value: VARIANT) -> Result<IDispatch, OfficeComError> {
    let variant_type = variant_type(&value);
    if variant_type == VT_DISPATCH.0 {
        // SAFETY: the discriminator proves the active union member is pdispVal.
        let raw = unsafe { value.as_raw().Anonymous.Anonymous.Anonymous.pdispVal };
        if raw.is_null() {
            return Err(OfficeComError::new(
                "invalid_property",
                "Office returned a null IDispatch property",
            ));
        }
        std::mem::forget(value);
        // SAFETY: ownership of the VARIANT's IDispatch reference is transferred
        // into this wrapper after suppressing VariantClear.
        return Ok(unsafe { IDispatch::from_raw(raw) });
    }
    if variant_type == VT_UNKNOWN.0 {
        // SAFETY: the discriminator proves the active union member is punkVal.
        let raw = unsafe { value.as_raw().Anonymous.Anonymous.Anonymous.punkVal };
        if raw.is_null() {
            return Err(OfficeComError::new(
                "invalid_property",
                "Office returned a null IUnknown property",
            ));
        }
        std::mem::forget(value);
        // SAFETY: ownership is transferred from the VARIANT into IUnknown.
        let unknown = unsafe { windows::core::IUnknown::from_raw(raw) };
        return unknown.cast::<IDispatch>().map_err(|error| {
            OfficeComError::new(
                "invalid_property",
                format!("Office object does not implement IDispatch: {error:?}"),
            )
        });
    }
    Err(OfficeComError::new(
        "invalid_property",
        format!("Office object property used unsupported VARIANT type {variant_type}"),
    ))
}

fn is_retryable_call(error: &windows::core::Error) -> bool {
    error.code() == RPC_E_CALL_REJECTED || error.code() == RPC_E_SERVERCALL_RETRYLATER
}

fn map_com_error(operation: &str, error: windows::core::Error) -> OfficeComError {
    let kind = if is_retryable_call(&error) {
        "call_rejected"
    } else {
        "automation_failed"
    };
    OfficeComError::new(kind, format!("Office {operation} failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_local_cloud_and_unsaved_documents() {
        assert_eq!(
            classify_locator("Report.docx", r"C:\\Work", r"C:\\Work\\Report.docx").unwrap(),
            (
                OfficeDocumentKind::LocalFile,
                Some(r"C:\\Work\\Report.docx".to_string())
            )
        );
        assert_eq!(
            classify_locator(
                "Report.docx",
                "https://tenant.sharepoint.com/docs",
                "https://tenant.sharepoint.com/docs/Report.docx"
            )
            .unwrap()
            .0,
            OfficeDocumentKind::CloudDocument
        );
        assert_eq!(
            classify_locator("Document1", "", "Document1").unwrap(),
            (OfficeDocumentKind::Unsaved, None)
        );
    }
}
