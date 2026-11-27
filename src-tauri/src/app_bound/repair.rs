//! Local-first repair with bounded, independently verified component sources.
use super::RepairOutcome;
use carbonpaper_app_bound::{
    manifest,
    protocol::{Request, Response},
    repair::{self, RepairRelease},
    windows::{identity, repair as native, update},
};
use futures::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{Emitter, Window};

const MAX_FULL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
static REPAIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Serialize)]
struct Progress {
    phase: &'static str,
    downloaded: u64,
    total: u64,
}

fn progress(window: &Window, phase: &'static str, downloaded: u64, total: u64) {
    let _ = window.emit(
        "app-bound-repair-progress",
        Progress {
            phase,
            downloaded,
            total,
        },
    );
}

fn invalid(_: impl std::fmt::Display) -> String {
    "APP_BOUND_REPAIR_PACKAGE_INVALID".into()
}

struct Scratch {
    path: PathBuf,
    locks: update::DirectoryLocks,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.locks = update::DirectoryLocks::default();
        let _ = remove_generated_tree(&self.path);
    }
}

fn remove_generated_tree(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    identity::no_reparse(path).map_err(invalid)?;
    let mut lock = update::DirectoryLocks::default();
    lock.pin(path).map_err(invalid)?;
    for item in fs::read_dir(path).map_err(invalid)? {
        let path = item.map_err(invalid)?.path();
        identity::no_reparse(&path).map_err(invalid)?;
        if path.is_dir() {
            remove_generated_tree(&path)?;
        } else if path.is_file() {
            fs::remove_file(path).map_err(invalid)?;
        } else {
            return Err(invalid("Unexpected cache entry"));
        }
    }
    drop(lock);
    fs::remove_dir(path).map_err(invalid)
}

struct Cache {
    root: PathBuf,
    _locks: update::DirectoryLocks,
}

impl Cache {
    fn open() -> Result<Self, String> {
        let mut root = identity::local_app_data_directory().map_err(invalid)?;
        let mut locks = update::DirectoryLocks::default();
        locks.pin(&root).map_err(invalid)?;
        for name in ["CarbonPaper", "app-bound-repair"] {
            root.push(name);
            if !root.exists() {
                fs::create_dir(&root).map_err(invalid)?;
            }
            locks.pin(&root).map_err(invalid)?;
        }
        Ok(Self {
            root,
            _locks: locks,
        })
    }

    fn scratch(&self) -> Result<Scratch, String> {
        let path = self.root.join(format!(
            "work-{}",
            &carbonpaper_app_bound::protocol::random_id()[..16]
        ));
        fs::create_dir(&path).map_err(invalid)?;
        let mut locks = update::DirectoryLocks::default();
        locks.pin(&path).map_err(invalid)?;
        Ok(Scratch { path, locks })
    }

    fn prune(&self, targets: &[Target]) -> Result<(), String> {
        let mut retained = 0;
        for item in fs::read_dir(&self.root).map_err(invalid)? {
            let path = item.map_err(invalid)?.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| invalid("Cache name"))?;
            let keep = name
                .strip_prefix("bundle-")
                .is_some_and(|id| targets.iter().any(|target| target.accepts(id)) && retained < 2);
            if keep {
                retained += 1;
            } else if name.starts_with("work-") || name.starts_with("bundle-") {
                remove_generated_tree(&path)?;
            }
        }
        Ok(())
    }

    fn cached(&self, target: &Target) -> Option<native::RepairPackage> {
        fs::read_dir(&self.root)
            .ok()?
            .filter_map(|item| item.ok())
            .find_map(|item| {
                let name = item.file_name();
                let id = name.to_str()?.strip_prefix("bundle-")?;
                if !target.accepts(id) {
                    return None;
                }
                let package = native::read_package(&item.path()).ok()?;
                target.accepts(&package.runtime_id).then_some(package)
            })
    }

    fn commit(&self, mut scratch: Scratch, id: &str) -> Result<PathBuf, String> {
        repair::runtime_version(id).map_err(invalid)?;
        let target = self.root.join(format!("bundle-{id}"));
        if target.exists() {
            remove_generated_tree(&target)?;
        }
        scratch.locks = update::DirectoryLocks::default();
        fs::rename(&scratch.path, &target).map_err(invalid)?;
        Ok(target)
    }

    fn component_copy(&self, package: &native::RepairPackage) -> Result<Scratch, String> {
        let scratch = self.scratch()?;
        for name in repair::COMPONENT_FILES {
            copy_verified(
                &package.directory.join(name),
                &scratch.path.join(name),
                package
                    .release
                    .files
                    .get(*name)
                    .ok_or_else(|| invalid("Missing component"))?,
            )?;
        }
        copy_metadata(&package.directory, &scratch.path)?;
        let copied = native::read_package(&scratch.path).map_err(invalid)?;
        if copied.runtime_id != package.runtime_id {
            return Err(invalid("Changed component source"));
        }
        drop(copied);
        Ok(scratch)
    }
}

#[derive(Clone)]
struct Target {
    id: Option<String>,
    version: String,
}

struct CacheCleanup<'a> {
    cache: &'a Cache,
    targets: [Target; 2],
}

impl Drop for CacheCleanup<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.cache.prune(&self.targets) {
            tracing::warn!("Component repair cache cleanup deferred: {error}");
        }
    }
}

impl Target {
    fn accepts(&self, id: &str) -> bool {
        repair::runtime_version(id).is_ok_and(|version| version.to_string() == self.version)
            && self.id.as_deref().is_none_or(|expected| expected == id)
    }
}

struct Source {
    package: native::RepairPackage,
    // Package file handles drop before the private extraction directory.
    _scratch: Option<Scratch>,
    full: bool,
}

enum PreparationError {
    DownloadRequired(Option<u64>),
    Failure(String),
}
impl From<String> for PreparationError {
    fn from(value: String) -> Self {
        Self::Failure(value)
    }
}

fn pointer(path: &Path) -> Option<String> {
    identity::assert_protected(path).ok()?;
    let bytes = manifest::read_bounded(path, 1024).ok()?;
    let pointer: identity::Activation = serde_json::from_slice(&bytes).ok()?;
    repair::runtime_version(&pointer.runtime_id).ok()?;
    Some(pointer.runtime_id)
}

fn targets() -> Result<(Target, Target), String> {
    let root = identity::protected_root().map_err(invalid)?;
    let sid = identity::current_sid().map_err(invalid)?;
    let current = super::executable_directory()?;
    let owner = pointer(&root.join("Activations").join(format!("{sid}.json")))
        .or_else(|| {
            current
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|id| repair::runtime_version(id).is_ok())
                .filter(|_| {
                    current
                        .parent()
                        .is_some_and(|parent| identity::path_eq(parent, &root.join("Runtime")))
                })
                .map(str::to_string)
        })
        .or_else(|| manifest::read_manifest(&current).ok().map(|(_, id)| id));
    let owner_version = owner
        .as_deref()
        .and_then(|id| repair::runtime_version(id).ok())
        .map(|v| v.to_string())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    let service = native::service_checkpoint()
        .ok()
        .flatten()
        .or_else(|| owner.clone());
    let service_version = service
        .as_deref()
        .and_then(|id| repair::runtime_version(id).ok())
        .map(|v| v.to_string())
        .unwrap_or_else(|| owner_version.clone());
    Ok((
        Target {
            id: owner,
            version: owner_version,
        },
        Target {
            id: service,
            version: service_version,
        },
    ))
}

fn candidate_directories(target: &Target) -> Result<Vec<PathBuf>, String> {
    let mut candidates = Vec::new();
    if let Some(id) = &target.id {
        candidates.push(
            identity::protected_root()
                .map_err(invalid)?
                .join("Runtime")
                .join(id),
        );
    }
    candidates.push(super::installation_source()?);
    if let Some(source) = crate::registry_config::get_string(super::PACKAGE_SOURCE) {
        candidates.push(PathBuf::from(source));
    }
    Ok(candidates)
}

fn copy_verified(source: &Path, target: &Path, expected: &str) -> Result<(), String> {
    let mut input = identity::locked_file(source).map_err(invalid)?;
    identity::verify_file(&mut input, expected).map_err(invalid)?;
    input.rewind().map_err(invalid)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(target)
        .map_err(invalid)?;
    std::io::copy(&mut input, &mut output).map_err(invalid)?;
    output.sync_all().map_err(invalid)?;
    drop(output);
    identity::verify_file(
        &mut identity::locked_file(target).map_err(invalid)?,
        expected,
    )
    .map_err(invalid)
}

fn copy_metadata(source: &Path, destination: &Path) -> Result<(), String> {
    for name in [manifest::MANIFEST_NAME, manifest::SIGNATURE_NAME] {
        let bytes = manifest::read_bounded(
            &source.join(name),
            if name == manifest::MANIFEST_NAME {
                1024 * 1024
            } else {
                256
            },
        )
        .map_err(invalid)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(destination.join(name))
            .map_err(invalid)?;
        output.write_all(&bytes).map_err(invalid)?;
        output.sync_all().map_err(invalid)?;
    }
    Ok(())
}

/// Assemble an independent source from matching, signed local files. The
/// runtime and System copies can each supply a missing component of the other.
fn local_bundle(
    cache: &Cache,
    target: &Target,
    system: bool,
) -> Result<Option<native::RepairPackage>, String> {
    let candidates = candidate_directories(target)?;
    for directory in &candidates {
        let Ok((release, id)) = manifest::read_manifest(directory) else {
            continue;
        };
        if !target.accepts(&id) {
            continue;
        }
        let mut component_dirs = candidates.clone();
        if system {
            component_dirs.push(identity::protected_root().map_err(invalid)?.join("System"));
        }
        let mut selected = Vec::new();
        for name in repair::COMPONENT_FILES {
            let expected = release
                .files
                .get(*name)
                .ok_or_else(|| invalid("Missing component"))?;
            let source = component_dirs
                .iter()
                .map(|base| base.join(name))
                .find(|path| {
                    identity::locked_file(path)
                        .and_then(|mut file| identity::verify_file(&mut file, expected))
                        .is_ok()
                });
            if let Some(source) = source {
                selected.push((name, source, expected));
            }
        }
        if selected.len() != repair::COMPONENT_FILES.len() {
            continue;
        }
        let scratch = cache.scratch()?;
        for (name, source, expected) in selected {
            copy_verified(&source, &scratch.path.join(name), expected)?;
        }
        copy_metadata(directory, &scratch.path)?;
        let package = native::read_package(&scratch.path).map_err(invalid)?;
        if package.runtime_id != id {
            return Err(invalid("Changed runtime metadata"));
        }
        drop(package);
        let directory = cache.commit(scratch, &id)?;
        return native::read_package(&directory).map(Some).map_err(invalid);
    }
    Ok(None)
}

fn client() -> Result<reqwest::Client, String> {
    if !crate::registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("APP_BOUND_REPAIR_NETWORK_DISABLED".into());
    }
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(invalid)
}

async fn metadata(url: &str) -> Result<Vec<u8>, String> {
    let response = client()?
        .get(url)
        .send()
        .await
        .map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("APP_BOUND_REPAIR_NOT_FOUND".into());
    }
    let mut response = response
        .error_for_status()
        .map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?
    {
        if bytes.len().saturating_add(chunk.len()) > 64 * 1024 {
            return Err(invalid("Oversized metadata"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn verify_archive(file: &mut File, hash: &str) -> Result<(), String> {
    file.rewind().map_err(invalid)?;
    let mut actual = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(invalid)?;
        if count == 0 {
            break;
        }
        actual.update(&buffer[..count]);
    }
    if format!("{:x}", actual.finalize()) != hash.to_ascii_lowercase() {
        return Err(invalid("ZIP checksum"));
    }
    file.rewind().map_err(invalid)
}

async fn download(
    url: &str,
    hash: &str,
    size: Option<u64>,
    max: u64,
    scratch: &Scratch,
    window: &Window,
) -> Result<File, String> {
    let response = client()?
        .get(url)
        .send()
        .await
        .map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?
        .error_for_status()
        .map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?;
    if size.is_some_and(|length| length > max)
        || response.content_length().is_some_and(|length| length > max)
    {
        return Err(invalid("Oversized ZIP"));
    }
    let total = size.or_else(|| response.content_length()).unwrap_or(0);
    let path = scratch.path.join("download.zip");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0)
        .open(&path)
        .map_err(invalid)?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0u64;
    let mut reported = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "APP_BOUND_REPAIR_DOWNLOAD_FAILED")?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > max || size.is_some_and(|expected| downloaded > expected) {
            return Err(invalid("ZIP size"));
        }
        output.write_all(&chunk).map_err(invalid)?;
        if reported.elapsed() >= Duration::from_millis(100) {
            progress(window, "downloading", downloaded, total);
            reported = Instant::now();
        }
    }
    if size.is_some_and(|expected| expected != downloaded) {
        return Err(invalid("Truncated ZIP"));
    }
    output.sync_all().map_err(invalid)?;
    drop(output);
    let mut file = identity::locked_file(&path).map_err(invalid)?;
    verify_archive(&mut file, hash)?;
    progress(window, "downloading", downloaded, total);
    Ok(file)
}

fn extract_archive(
    file: &mut File,
    scratch: &mut Scratch,
    components: bool,
) -> Result<PathBuf, String> {
    let root = scratch.path.join("extracted");
    fs::create_dir(&root).map_err(invalid)?;
    scratch.locks.pin(&root).map_err(invalid)?;
    let mut archive = zip::ZipArchive::new(file).map_err(invalid)?;
    archive.validate_header_consistency().map_err(invalid)?;
    if archive.len() > if components { 4 } else { 8192 } {
        return Err(invalid("ZIP entries"));
    }
    let mut names = BTreeSet::new();
    let mut expanded = 0u64;
    let max = if components {
        repair::MAX_COMPONENT_EXPANDED_BYTES
    } else {
        MAX_FULL_BYTES
    };
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(invalid)?;
        let name = entry.name().map_err(invalid)?.into_owned();
        if entry.is_symlink() || !names.insert(name.trim_end_matches('/').to_ascii_lowercase()) {
            return Err(invalid("ZIP alias"));
        }
        let relative = manifest::safe_relative_path(name.trim_end_matches('/')).map_err(invalid)?;
        if components
            && (!repair::COMPONENT_FILES.contains(&name.as_str())
                && name != manifest::MANIFEST_NAME
                && name != manifest::SIGNATURE_NAME)
        {
            return Err(invalid("Unexpected repair file"));
        }
        if entry.size() > max || expanded.saturating_add(entry.size()) > max {
            return Err(invalid("Expanded ZIP size"));
        }
        let destination = root.join(&relative);
        let parent = if entry.is_dir() {
            destination.as_path()
        } else {
            destination.parent().ok_or_else(|| invalid("ZIP path"))?
        };
        let mut directory = root.clone();
        for component in parent.strip_prefix(&root).map_err(invalid)?.components() {
            directory.push(component);
            if !directory.exists() {
                fs::create_dir(&directory).map_err(invalid)?;
            }
            scratch.locks.pin(&directory).map_err(invalid)?;
        }
        if entry.is_dir() {
            continue;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(destination)
            .map_err(invalid)?;
        let count = std::io::copy(
            &mut entry.take(max.saturating_sub(expanded) + 1),
            &mut output,
        )
        .map_err(invalid)?;
        expanded += count;
        if expanded > max {
            return Err(invalid("Expanded ZIP size"));
        }
        output.sync_all().map_err(invalid)?;
    }
    Ok(root)
}

fn signed_contents(directory: &Path, release: &manifest::ReleaseManifest) -> Result<(), String> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(parent) = pending.pop() {
        for item in fs::read_dir(&parent).map_err(invalid)? {
            let path = item.map_err(invalid)?.path();
            identity::no_reparse(&path).map_err(invalid)?;
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(directory)
                    .map_err(invalid)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative != manifest::MANIFEST_NAME
                    && relative != manifest::SIGNATURE_NAME
                    && !release.files.contains_key(&relative)
                {
                    return Err(invalid("Unsigned package file"));
                }
            }
        }
    }
    Ok(())
}

async fn full_metadata(
    target: &Target,
) -> Result<(crate::updater::UpdateManifest, Option<u64>), String> {
    let bytes = metadata(&format!(
        "{}/v{}/latest.json",
        repair::RELEASE_ROOT,
        target.version
    ))
    .await?;
    let release: crate::updater::UpdateManifest =
        serde_json::from_slice(&bytes).map_err(invalid)?;
    crate::updater::verify_update_manifest_signature(&release)?;
    if release.version != target.version
        || release.url
            != format!(
                "{}/v{}/carbonpaper_{}_x64_portable.zip",
                repair::RELEASE_ROOT,
                target.version,
                target.version
            )
    {
        return Err(invalid("Unexpected full runtime version"));
    }
    let size = client()?
        .head(&release.url)
        .send()
        .await
        .ok()
        .and_then(|response| response.error_for_status().ok())
        .and_then(|response| response.content_length())
        .filter(|length| *length > 0);
    Ok((release, size))
}

async fn full_source(
    cache: &Cache,
    target: &Target,
    allow: bool,
    window: &Window,
) -> Result<Source, PreparationError> {
    for directory in candidate_directories(target)? {
        if let Ok(package) = native::read_package(&directory) {
            if target.accepts(&package.runtime_id)
                && native::files_valid(&directory, &package.release, true)
            {
                // An installed application can also contain its uninstaller or
                // user files. The helper copies only this signed manifest's files.
                return Ok(Source {
                    package,
                    _scratch: None,
                    full: true,
                });
            }
        }
    }
    let (release, size) = full_metadata(target).await?;
    if !allow {
        return Err(PreparationError::DownloadRequired(size));
    }
    let mut scratch = cache.scratch()?;
    progress(window, "downloading", 0, size.unwrap_or(0));
    let mut file = download(
        &release.url,
        &release.sha256,
        None,
        MAX_FULL_BYTES,
        &scratch,
        window,
    )
    .await?;
    progress(window, "extracting", 0, 0);
    let directory = extract_archive(&mut file, &mut scratch, false)?;
    let package = native::read_package(&directory).map_err(invalid)?;
    if !target.accepts(&package.runtime_id)
        || !native::files_valid(&directory, &package.release, true)
    {
        return Err(invalid("Incomplete runtime").into());
    }
    signed_contents(&directory, &package.release)?;
    Ok(Source {
        package,
        _scratch: Some(scratch),
        full: true,
    })
}

async fn source(
    cache: &Cache,
    target: &Target,
    system: bool,
    complete: bool,
    allow_full: bool,
    window: &Window,
) -> Result<Source, PreparationError> {
    let mut package = cache.cached(target);
    if package.is_none() {
        package = local_bundle(cache, target, system)?;
    }
    let package = if let Some(package) = package {
        package
    } else {
        progress(window, "downloading", 0, 0);
        let bytes = match metadata(&repair::metadata_url(&target.version).map_err(invalid)?).await {
            Ok(bytes) => bytes,
            Err(error) if error == "APP_BOUND_REPAIR_NOT_FOUND" => {
                return full_source(cache, target, allow_full, window).await
            }
            Err(error) => return Err(error.into()),
        };
        let release: RepairRelease = serde_json::from_slice(&bytes).map_err(invalid)?;
        release.verify(&target.version).map_err(invalid)?;
        if !target.accepts(&release.runtime_id) {
            return Err("APP_BOUND_REPAIR_VERSION_MISMATCH".to_string().into());
        }
        let mut scratch = cache.scratch()?;
        let mut file = download(
            &release.url,
            &release.sha256,
            Some(release.size_bytes),
            repair::MAX_COMPONENT_BYTES,
            &scratch,
            window,
        )
        .await?;
        progress(window, "extracting", 0, 0);
        let directory = extract_archive(&mut file, &mut scratch, true)?;
        let package = native::read_package(&directory).map_err(invalid)?;
        release
            .verify_runtime(
                &manifest::read_bounded(&directory.join(manifest::MANIFEST_NAME), 1024 * 1024)
                    .map_err(invalid)?,
            )
            .map_err(invalid)?;
        if package.runtime_id != release.runtime_id {
            return Err(invalid("Runtime identity").into());
        }
        // Keep only four verified files; download ZIPs and extraction leftovers
        // remain private temporary work and are removed after this operation.
        let cache_scratch = cache.component_copy(&package)?;
        drop(package);
        let directory = cache.commit(cache_scratch, &release.runtime_id)?;
        native::read_package(&directory).map_err(invalid)?
    };
    let destination = identity::protected_root()
        .map_err(invalid)?
        .join("Runtime")
        .join(&package.runtime_id);
    if complete && !native::files_valid(&destination, &package.release, false) {
        drop(package);
        return full_source(cache, target, allow_full, window).await;
    }
    Ok(Source {
        package,
        _scratch: None,
        full: false,
    })
}

fn healthy(owner: &Source, service: &Source) -> bool {
    let Ok(root) = identity::protected_root() else {
        return false;
    };
    if pointer(&root.join("System/runtime.json")).as_deref() != Some(&service.package.runtime_id) {
        return false;
    }
    for package in [&owner.package, &service.package] {
        let directory = root.join("Runtime").join(&package.runtime_id);
        if manifest::read_manifest(&directory)
            .ok()
            .map(|(_, id)| id)
            .as_deref()
            != Some(&package.runtime_id)
        {
            return false;
        }
    }
    if !native::files_valid(
        &root.join("Runtime").join(&owner.package.runtime_id),
        &owner.package.release,
        true,
    ) {
        return false;
    }
    for name in repair::COMPONENT_FILES {
        let Some(hash) = service.package.release.files.get(*name) else {
            return false;
        };
        if identity::locked_file(&root.join("System").join(name))
            .and_then(|mut file| identity::verify_file(&mut file, hash))
            .is_err()
        {
            return false;
        }
    }
    matches!(
        carbonpaper_app_bound::windows::call(Request::Status {}),
        Ok(Response::Status(_))
    )
}

fn apply(
    owner: &Source,
    service: &Source,
    tool: &native::RepairPackage,
    cache: &Cache,
) -> Result<(), String> {
    // Execute the helper from a clean, four-file source even for full runtime
    // repair. The full signed package is data supplied to that native helper.
    let launcher = {
        let scratch = cache.component_copy(tool)?;
        let package = native::read_package(&scratch.path).map_err(invalid)?;
        Source {
            package,
            _scratch: Some(scratch),
            full: false,
        }
    };
    let _execution_locks =
        update::DirectoryLocks::for_execution(&launcher.package.directory).map_err(invalid)?;
    let mut bootstrap = identity::locked_file(
        &launcher
            .package
            .directory
            .join("carbonpaper-protected-setup.exe"),
    )
    .map_err(invalid)?;
    carbonpaper_app_bound::privileged::verify_loader_policy(&mut bootstrap).map_err(invalid)?;
    let mut command = runas::Command::new(
        launcher
            .package
            .directory
            .join("carbonpaper-protected-setup.exe"),
    );
    command
        .arg("--repair-service")
        .arg("--source")
        .arg(&service.package.directory)
        .arg("--runtime-source")
        .arg(&owner.package.directory)
        .arg("--tool-source")
        .arg(&launcher.package.directory)
        .arg("--caller-pid")
        .arg(std::process::id().to_string())
        .gui(true)
        .show(false);
    if owner.full {
        command.arg("--full-runtime");
    }
    let status = command.status().map_err(|error| {
        if error.raw_os_error() == Some(1223) {
            "APP_BOUND_CANCELLED".to_string()
        } else {
            "APP_BOUND_REPAIR_FAILED".to_string()
        }
    })?;
    if status.success() {
        Ok(())
    } else if status.code() == Some(native::FULL_RUNTIME_EXIT_CODE) {
        Err("APP_BOUND_REPAIR_FULL_RUNTIME_REQUIRED".into())
    } else if status.code() == Some(native::VERSION_EXIT_CODE) {
        Err("APP_BOUND_REPAIR_VERSION_MISMATCH".into())
    } else {
        Err("APP_BOUND_REPAIR_FAILED".into())
    }
}

async fn run_once(
    window: &Window,
    package_path: Option<PathBuf>,
    allow_full: bool,
) -> Result<RepairOutcome, String> {
    progress(window, "checking", 0, 0);
    let (owner_target, service_target) = targets()?;
    let cache = Cache::open()?;
    cache.prune(&[owner_target.clone(), service_target.clone()])?;
    let _cleanup = CacheCleanup {
        cache: &cache,
        targets: [owner_target.clone(), service_target.clone()],
    };
    let mut offline = None;
    if let Some(path) = package_path {
        let mut file = identity::locked_file(&path).map_err(invalid)?;
        if file.metadata().map_err(invalid)?.len() > MAX_FULL_BYTES {
            return Err(invalid("Oversized local ZIP"));
        }
        let mut scratch = cache.scratch()?;
        let directory = extract_archive(&mut file, &mut scratch, false)?;
        let package = native::read_package(&directory).map_err(invalid)?;
        signed_contents(&directory, &package.release)?;
        let full = native::files_valid(&directory, &package.release, true);
        if !owner_target.accepts(&package.runtime_id)
            && !service_target.accepts(&package.runtime_id)
            && package.release.version != env!("CARGO_PKG_VERSION")
        {
            return Err("APP_BOUND_REPAIR_VERSION_MISMATCH".into());
        }
        if !full
            && owner_target.accepts(&package.runtime_id)
            && !native::files_valid(
                &identity::protected_root()
                    .map_err(invalid)?
                    .join("Runtime")
                    .join(&package.runtime_id),
                &package.release,
                false,
            )
        {
            return Err("APP_BOUND_REPAIR_FULL_RUNTIME_REQUIRED".into());
        }
        if owner_target.accepts(&package.runtime_id) || service_target.accepts(&package.runtime_id)
        {
            // A later local selection can reuse this version for shared-service
            // repair. Full archives remain temporary; only the four-file subset
            // is cached.
            let subset = cache.component_copy(&package)?;
            cache.commit(subset, &package.runtime_id)?;
        }
        offline = Some(Source {
            package,
            _scratch: Some(scratch),
            full,
        });
    }
    let owner = if let Some(source) = offline
        .as_ref()
        .filter(|source| owner_target.accepts(&source.package.runtime_id))
    {
        Source {
            package: native::read_package(&source.package.directory).map_err(invalid)?,
            _scratch: None,
            full: source.full,
        }
    } else {
        match source(
            &cache,
            &owner_target,
            owner_target.id == service_target.id,
            true,
            allow_full,
            window,
        )
        .await
        {
            Ok(source) => source,
            Err(PreparationError::DownloadRequired(size_bytes)) => {
                return Ok(RepairOutcome::DownloadRequired { size_bytes })
            }
            Err(PreparationError::Failure(error)) => return Err(error),
        }
    };
    let service = if service_target.accepts(&owner.package.runtime_id) {
        Source {
            package: native::read_package(&owner.package.directory).map_err(invalid)?,
            _scratch: None,
            full: owner.full,
        }
    } else if let Some(source) = offline
        .as_ref()
        .filter(|source| service_target.accepts(&source.package.runtime_id))
    {
        Source {
            package: native::read_package(&source.package.directory).map_err(invalid)?,
            _scratch: None,
            full: source.full,
        }
    } else {
        match source(&cache, &service_target, true, false, allow_full, window).await {
            Ok(source) => source,
            Err(PreparationError::DownloadRequired(size_bytes)) => {
                return Ok(RepairOutcome::DownloadRequired { size_bytes })
            }
            Err(PreparationError::Failure(error)) => return Err(error),
        }
    };
    let changed = !healthy(&owner, &service);
    {
        // Explicit repair also restores SCM configuration and restart policy,
        // even when the running service and its signed files look healthy.
        let local_tool = native::read_package(&super::installation_source()?)
            .ok()
            .filter(|package| package.release.version == env!("CARGO_PKG_VERSION"));
        let mut tool = local_tool.map(|package| Source {
            package,
            _scratch: None,
            full: false,
        });
        if let Some(selected) = offline
            .as_ref()
            .filter(|source| source.package.release.version == env!("CARGO_PKG_VERSION"))
        {
            tool = Some(Source {
                package: native::read_package(&selected.package.directory).map_err(invalid)?,
                _scratch: None,
                full: false,
            });
        }
        let newer =
            manifest::compare_versions(&service.package.release.version, env!("CARGO_PKG_VERSION"))
                .map_err(invalid)?;
        if newer.is_gt() || (tool.is_none() && newer.is_ge()) {
            tool = Some(Source {
                package: native::read_package(&service.package.directory).map_err(invalid)?,
                _scratch: None,
                full: false,
            });
        }
        let tool = if let Some(tool) = tool {
            tool
        } else {
            match source(
                &cache,
                &Target {
                    id: None,
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                false,
                false,
                allow_full,
                window,
            )
            .await
            {
                Ok(source) => source,
                Err(PreparationError::DownloadRequired(size_bytes)) => {
                    return Ok(RepairOutcome::DownloadRequired { size_bytes })
                }
                Err(PreparationError::Failure(error)) => return Err(error),
            }
        };
        progress(window, "installing", 0, 0);
        apply(&owner, &service, &tool.package, &cache)?;
    }
    progress(window, "verifying", 0, 0);
    if !super::is_protected_runtime() {
        // An ordinary recovery copy must resume through the repaired activation.
        return Ok(RepairOutcome::Repaired {
            restart_required: true,
        });
    }
    if !healthy(&owner, &service) {
        return Err("APP_BOUND_REPAIR_SERVICE_UNAVAILABLE".into());
    }
    progress(window, "complete", 0, 0);
    Ok(RepairOutcome::Repaired {
        restart_required: changed && owner.full,
    })
}

pub(super) async fn run(
    window: &Window,
    package_path: Option<PathBuf>,
    allow_full: bool,
) -> Result<RepairOutcome, String> {
    let _guard = REPAIR_LOCK
        .try_lock()
        .map_err(|_| "APP_BOUND_REPAIR_BUSY")?;
    let first = run_once(window, package_path.clone(), allow_full).await;
    if first
        .as_ref()
        .is_err_and(|error| error == "APP_BOUND_REPAIR_VERSION_MISMATCH")
        && package_path.is_none()
    {
        // The elevated helper may have reconstructed a corrupt readable pointer
        // from the ledger. Retry once using that authoritative projection.
        run_once(window, None, allow_full).await
    } else {
        first
    }
}

#[cfg(test)]
mod tests;
