//! Bounded, local executable metadata reads performed only by the idle task.
use super::types::UsageAppIdentity;
use crate::classification::scoring::{self, Input};
use regex::Regex;
use std::{io::Read, path::Path, sync::LazyLock, time::UNIX_EPOCH};

const CACHE_VERSION: u32 = 1;
const MAX_VERSION_BYTES: usize = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const DRIVE_REMOTE: u32 = 4; // Win32 GetDriveTypeW return value.

#[derive(Default)]
struct ApplicationMetadata {
    product: String,
    description: String,
    original_filename: String,
    installed_name: String,
}
pub(super) struct PreparedApplication {
    pub fingerprint: Vec<u8>,
    pub category: Option<String>,
    metadata: ApplicationMetadata,
}
impl PreparedApplication {
    pub fn input(&self, app: &UsageAppIdentity) -> Input {
        let names = [
            &self.metadata.product,
            &self.metadata.installed_name,
            &app.name,
        ]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" / ");
        Input {
            title: format!(
                "应用名称：{}\n应用说明：{}",
                names, self.metadata.description
            ),
            ocr_text: String::new(),
            process_name: app.process_name.clone(),
        }
    }
}
pub(super) fn fingerprint(path: &str) -> Vec<u8> {
    let file = is_local_path(path)
        .then(|| std::fs::metadata(path).ok())
        .flatten();
    let size = file.as_ref().map_or(0, |m| m.len());
    let modified = file
        .and_then(|m| m.modified().ok())
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut fingerprint = Vec::with_capacity(28);
    fingerprint.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    fingerprint.extend_from_slice(&size.to_le_bytes());
    fingerprint.extend_from_slice(&modified.to_le_bytes());
    fingerprint
}
pub(super) fn prepare(
    app: &UsageAppIdentity,
    fingerprint: Vec<u8>,
    can_read: impl Fn() -> bool,
) -> Option<PreparedApplication> {
    if !can_read() {
        return None;
    }
    let local = is_local_path(&app.process_path);
    let mut metadata = if local {
        version_metadata(&app.process_path).unwrap_or_default()
    } else {
        ApplicationMetadata::default()
    };
    if !can_read() {
        return None;
    }
    if local {
        metadata.installed_name =
            installed_name(Path::new(&app.process_path), &can_read).unwrap_or_default();
    }
    if !can_read() {
        return None;
    }
    let category = metadata_category(&metadata).map(str::to_owned);
    Some(PreparedApplication {
        fingerprint,
        category,
        metadata,
    })
}
fn is_local_path(path: &str) -> bool {
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDriveTypeW};
    let path = path.strip_prefix("\\\\?\\").unwrap_or(path);
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'/' | b'\\')
    {
        return false;
    }
    let root: Vec<u16> = format!("{}:\\", bytes[0] as char)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: the query-only root is NUL-terminated. Network-backed programs
    // still get name-based inference, without opening their remote files.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) != DRIVE_REMOTE }
}

fn metadata_category(metadata: &ApplicationMetadata) -> Option<&'static str> {
    scoring::process_category(&metadata.original_filename)
        .or_else(|| {
            [
                &metadata.product,
                &metadata.installed_name,
                &metadata.description,
            ]
            .into_iter()
            .find_map(|name| {
                scoring::application_name_category(name)
                    .or_else(|| scoring::process_category(&format!("{name}.exe")))
            })
        })
        .or_else(|| description_category(&metadata.description))
}
fn description_category(description: &str) -> Option<&'static str> {
    let description = description.to_lowercase();
    let mut matches = [
        (
            "网页浏览",
            &["web browser", "internet browser", "网页浏览器"][..],
        ),
        (
            "影音娱乐",
            &[
                "video player",
                "media player",
                "music player",
                "视频播放器",
                "音乐播放器",
            ][..],
        ),
        (
            "办公文档",
            &[
                "pdf reader",
                "pdf viewer",
                "spreadsheet",
                "word processor",
                "pdf 阅读器",
                "文档编辑器",
            ][..],
        ),
        (
            "编程开发",
            &[
                "code editor",
                "integrated development environment",
                "代码编辑器",
                "集成开发环境",
            ][..],
        ),
        ("游戏", &["video game", "game client", "游戏客户端"][..]),
        (
            "系统工具",
            &["terminal emulator", "system utility", "系统工具"][..],
        ),
    ]
    .into_iter()
    .filter(|(_, words)| words.iter().any(|word| description.contains(word)));
    let (category, _) = matches.next()?;
    matches.next().is_none().then_some(category)
}

fn version_metadata(path: &str) -> Option<ApplicationMetadata> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW},
    };
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the filename is NUL-terminated. Version data is bounded and its
    // backing allocation is DWORD-aligned; all returned pointers are checked.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) as usize;
        if size == 0 || size > MAX_VERSION_BYTES {
            return None;
        }
        let mut storage = vec![0u32; size.div_ceil(4)];
        GetFileVersionInfoW(
            PCWSTR(path.as_ptr()),
            0,
            size as u32,
            storage.as_mut_ptr().cast(),
        )
        .ok()?;
        let data = std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), size);
        let mut languages = version_value(data, "\\VarFileInfo\\Translation", false)
            .map(|bytes| {
                bytes
                    .chunks_exact(4)
                    .take(16)
                    .map(|pair| {
                        (
                            u16::from_le_bytes([pair[0], pair[1]]),
                            u16::from_le_bytes([pair[2], pair[3]]),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        languages.extend([(0x0409, 0x04b0), (0x0409, 0x04e4)]);
        let text = |name: &str| {
            languages
                .iter()
                .find_map(|(language, codepage)| {
                    let query = format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\{name}");
                    let bytes = version_value(data, &query, true)?;
                    let chars = bytes
                        .chunks_exact(2)
                        .take(512)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .take_while(|c| *c != 0)
                        .collect::<Vec<_>>();
                    let value = String::from_utf16_lossy(&chars).trim().to_owned();
                    (!value.is_empty()).then_some(value)
                })
                .unwrap_or_default()
        };
        Some(ApplicationMetadata {
            product: text("ProductName"),
            description: text("FileDescription"),
            original_filename: text("OriginalFilename"),
            installed_name: String::new(),
        })
    }
}
unsafe fn version_value<'a>(data: &'a [u8], query: &str, utf16: bool) -> Option<&'a [u8]> {
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::VerQueryValueW};
    let query: Vec<u16> = query.encode_utf16().chain(Some(0)).collect();
    let (mut pointer, mut length) = (std::ptr::null_mut::<std::ffi::c_void>(), 0u32);
    // SAFETY: Windows returns a borrowed pointer into the live version buffer;
    // decode bytes instead of assuming the returned UTF-16 pointer is aligned.
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(query.as_ptr()),
            &mut pointer,
            &mut length,
        )
    }
    .as_bool()
    {
        return None;
    }
    let start = (pointer as usize).checked_sub(data.as_ptr() as usize)?;
    let bytes = (length as usize).checked_mul(if utf16 { 2 } else { 1 })?;
    data.get(start..start.checked_add(bytes)?)
}

fn installed_name(executable: &Path, can_read: &impl Fn() -> bool) -> Option<String> {
    let game_dir = executable.ancestors().take(16).find(|directory| {
        directory
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name.eq_ignore_ascii_case("common"))
            && directory
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
    })?;
    let name = game_dir.file_name()?.to_str()?;
    let steamapps = game_dir.parent()?.parent()?;
    // Only this executable's library is inspected, with bounded directory and
    // file reads. A folder named Games or steamapps alone supplies no category.
    for entry in std::fs::read_dir(steamapps).ok()?.take(512).flatten() {
        if !can_read() {
            return None;
        }
        let filename = entry.file_name().to_string_lossy().to_lowercase();
        if !filename.starts_with("appmanifest_") || !filename.ends_with(".acf") {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        if !file.metadata().is_ok_and(|m| m.len() <= MAX_MANIFEST_BYTES) {
            continue;
        }
        let mut contents = String::new();
        if file
            .take(MAX_MANIFEST_BYTES)
            .read_to_string(&mut contents)
            .is_err()
        {
            continue;
        }
        if let Some(name) = manifest_name(&contents, name) {
            return Some(name);
        }
    }
    None
}
fn manifest_name(contents: &str, directory: &str) -> Option<String> {
    static FIELDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""(name|installdir)"\s+"([^"\r\n]*)""#).unwrap());
    let mut name = None;
    let mut installed = None;
    for capture in FIELDS.captures_iter(contents) {
        match &capture[1] {
            "name" => name = Some(capture[2].trim().chars().take(256).collect::<String>()),
            "installdir" => installed = Some(capture[2].to_owned()),
            _ => {}
        }
    }
    (installed
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(directory)))
    .then_some(name)
    .flatten()
    .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_identifies_renamed_programs_and_keeps_generic_engines_unassigned() {
        let original = ApplicationMetadata {
            original_filename: "code.exe".into(),
            ..Default::default()
        };
        assert_eq!(metadata_category(&original), Some("编程开发"));
        let game = ApplicationMetadata {
            product: "Arknights".into(),
            ..Default::default()
        };
        assert_eq!(metadata_category(&game), Some("游戏"));
        let player = ApplicationMetadata {
            description: "Example video player".into(),
            ..Default::default()
        };
        assert_eq!(metadata_category(&player), Some("影音娱乐"));
        let engine = ApplicationMetadata {
            product: "Unity".into(),
            description: "Unity application".into(),
            ..Default::default()
        };
        assert_eq!(metadata_category(&engine), None);
    }
    #[test]
    fn a_library_manifest_must_match_the_executables_install_directory() {
        let manifest = "\"AppState\" { \"name\" \"Blender\" \"installdir\" \"Blender\" }";
        assert_eq!(
            manifest_name(manifest, "blender").as_deref(),
            Some("Blender")
        );
        assert_eq!(manifest_name(manifest, "AnotherApp"), None);
        let metadata = ApplicationMetadata {
            installed_name: "Blender".into(),
            ..Default::default()
        };
        assert_eq!(metadata_category(&metadata), Some("设计创作"));
    }
}
