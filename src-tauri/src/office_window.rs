//! Cheap Win32 fingerprinting for Office document view windows.
//!
//! This module deliberately performs no COM work. It is shared by the capture
//! process and the isolated Office worker so both sides agree on the native view
//! HWND that identifies a document surface.

use crate::office_protocol::{OfficeApplication, MAX_OFFICE_TITLE_CHARS};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetAncestor, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, GA_ROOT,
};

struct EnumerationContext {
    application: OfficeApplication,
    pid: u32,
    first_match: isize,
    first_priority: usize,
    visible_match: isize,
    visible_priority: usize,
}

/// Return the Office native document-view HWND below `root_hwnd`.
pub fn find_native_document_window(
    root_hwnd: i64,
    pid: u32,
    application: OfficeApplication,
) -> Option<i64> {
    let root_raw = isize::try_from(root_hwnd).ok()?;
    let root = HWND(root_raw as *mut _);
    if root.0.is_null() || pid == 0 {
        return None;
    }

    // SAFETY: `root` comes from foreground-window discovery. The callback only
    // borrows the stack context for the duration of the synchronous enumeration.
    unsafe {
        if !IsWindow(root).as_bool() || window_pid(root) != pid {
            return None;
        }
        let mut context = EnumerationContext {
            application,
            pid,
            first_match: 0,
            first_priority: usize::MAX,
            visible_match: 0,
            visible_priority: usize::MAX,
        };
        let _ = EnumChildWindows(
            root,
            Some(enumerate_document_window),
            LPARAM((&mut context as *mut EnumerationContext) as isize),
        );
        let result = if context.visible_match != 0 {
            context.visible_match
        } else {
            context.first_match
        };
        (result != 0).then_some(result as i64)
    }
}

/// Validate an HWND supplied by the capture process before using it for COM.
pub fn validate_native_document_window(
    root_hwnd: i64,
    document_hwnd: i64,
    pid: u32,
    application: OfficeApplication,
) -> bool {
    let Ok(root_raw) = isize::try_from(root_hwnd) else {
        return false;
    };
    let Ok(document_raw) = isize::try_from(document_hwnd) else {
        return false;
    };
    let root = HWND(root_raw as *mut _);
    let document = HWND(document_raw as *mut _);

    // SAFETY: both handles are treated as untrusted and checked with IsWindow
    // before querying process, ancestry, visibility, or class metadata.
    unsafe {
        IsWindow(root).as_bool()
            && IsWindow(document).as_bool()
            && window_pid(root) == pid
            && window_pid(document) == pid
            && GetAncestor(document, GA_ROOT) == root
            && application
                .native_window_class_priority(&window_class(document))
                .is_some()
    }
}

/// Validate the complete foreground context captured by the parent process.
///
/// HWND values and PIDs are necessary but not sufficient: a reused top-level
/// window can keep the same handle while Office changes its document title.
/// Reading the title again on both sides of the COM call lets the worker fail
/// closed when the foreground document changed during resolution.
pub fn validate_office_window_context(
    root_hwnd: i64,
    document_hwnd: i64,
    pid: u32,
    application: OfficeApplication,
    expected_title: &str,
) -> bool {
    if expected_title.chars().count() > MAX_OFFICE_TITLE_CHARS {
        return false;
    }
    if !validate_native_document_window(root_hwnd, document_hwnd, pid, application) {
        return false;
    }

    let Ok(root_raw) = isize::try_from(root_hwnd) else {
        return false;
    };
    let root = HWND(root_raw as *mut _);

    // SAFETY: `root` was validated above and the buffer is a documented,
    // writable UTF-16 destination for GetWindowTextW.
    unsafe { window_title(root) == expected_title }
}

unsafe extern "system" fn enumerate_document_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the live `EnumerationContext` pointer passed to the
    // synchronous EnumChildWindows call above.
    let context = unsafe { &mut *(lparam.0 as *mut EnumerationContext) };
    // SAFETY: EnumChildWindows supplied this HWND; the helper only queries its
    // process ID into a local initialized output value.
    if unsafe { window_pid(hwnd) } != context.pid {
        return true.into();
    }
    // SAFETY: the callback HWND is queried with a bounded local UTF-16 buffer.
    let class_name = unsafe { window_class(hwnd) };
    let Some(priority) = context
        .application
        .native_window_class_priority(&class_name)
    else {
        return true.into();
    };

    if priority < context.first_priority {
        context.first_match = hwnd.0 as isize;
        context.first_priority = priority;
    }
    // SAFETY: this only queries the callback HWND and retains no Rust pointers.
    if unsafe { IsWindowVisible(hwnd).as_bool() } && priority < context.visible_priority {
        context.visible_match = hwnd.0 as isize;
        context.visible_priority = priority;
    }
    true.into()
}

unsafe fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: the caller supplies a window handle and `pid` remains exclusively
    // writable for the synchronous query; an invalid handle leaves it zero.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

unsafe fn window_class(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the HWND is supplied by the caller; the API writes at most the
    // slice's capacity and its returned character count bounds the subsequent read.
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        String::new()
    }
}

unsafe fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    // SAFETY: the bounded UTF-16 buffer stays live for this synchronous HWND
    // query; only the returned written characters are read afterward.
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        String::new()
    }
}
