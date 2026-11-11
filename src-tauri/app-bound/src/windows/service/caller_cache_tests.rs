use super::*;
#[cfg(feature = "development-runtime")]
use std::io::{Seek, SeekFrom};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::windows::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
};

struct WaitingProcess(Child);

impl WaitingProcess {
    fn start(image: &Path) -> Self {
        Self(
            Command::new(image)
                .args(["/D", "/Q", "/C", "set /p carbonpaper_cache_wait="])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
                .spawn()
                .expect("start isolated waiting process"),
        )
    }

    fn finish(&mut self) {
        drop(self.0.stdin.take());
        self.0.wait().expect("waiting process exits on EOF");
    }
}

impl Drop for WaitingProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn image_lock(path: &Path) -> File {
    #[cfg(feature = "development-runtime")]
    {
        identity::locked_development_image(path).unwrap()
    }
    #[cfg(not(feature = "development-runtime"))]
    {
        identity::locked_file(path).unwrap()
    }
}

fn verify_image(mut process: identity::ProcessIdentity, expected: &str) -> Result<VerifiedCaller> {
    let mut image = image_lock(&process.image);
    identity::verify_file(&mut image, expected)?;
    process.principal.runtime_id = "cache-test-runtime".into();
    Ok(VerifiedCaller::new(process, image))
}

#[test]
fn cache_hits_skip_hashing_but_still_check_kernel_sid_and_image_path() {
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let process = identity::inspect_process(std::process::id()).unwrap();
    let expected = crate::crypto::digest(&fs::read(&process.image).unwrap());
    let caller =
        cache_verified_caller(process, &cache, |process| verify_image(process, &expected)).unwrap();
    let hit = cache_verified_caller(
        identity::inspect_process(std::process::id()).unwrap(),
        &cache,
        |_| panic!("a live verified process must not be rehashed"),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&caller, &hit));

    let mut wrong_sid = identity::inspect_process(std::process::id()).unwrap();
    wrong_sid.principal.sid = "S-1-5-21-1-2-3-1234".into();
    assert!(matches!(
        cache_verified_caller(wrong_sid, &cache, |_| panic!("cache hit")),
        Err(BrokerError::AccessDenied)
    ));
    let mut wrong_path = identity::inspect_process(std::process::id()).unwrap();
    wrong_path.image.set_file_name("another.exe");
    assert!(matches!(
        cache_verified_caller(wrong_path, &cache, |_| panic!("cache hit")),
        Err(BrokerError::AccessDenied)
    ));
}

#[test]
fn exited_processes_release_cached_trust_and_replacement_images_need_a_fresh_hash() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("carbonpaper.exe");
    fs::copy(
        identity::system_directory().unwrap().join("cmd.exe"),
        &image,
    )
    .unwrap();
    let expected = crate::crypto::digest(&fs::read(&image).unwrap());
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let mut first = WaitingProcess::start(&image);
    let caller = cache_verified_caller(
        identity::inspect_process(first.0.id()).unwrap(),
        &cache,
        |process| verify_image(process, &expected),
    )
    .unwrap();
    assert!(caller.is_running());
    assert!(
        OpenOptions::new().write(true).open(&image).is_err(),
        "verified image bytes must stay immutable"
    );
    first.finish();
    assert!(!caller.is_running());
    assert!(
        OpenOptions::new().write(true).open(&image).is_err(),
        "cached image must deny writes after process exit too"
    );

    let previous = directory.path().join("previous.exe");
    #[cfg(feature = "development-runtime")]
    {
        // Cargo can replace the pathname immediately, even while an in-flight
        // operation still owns an Arc and the exit sweep has not run yet.
        fs::rename(&image, &previous).expect("development cache permits hot rebuild");
        fs::copy(&previous, &image).unwrap();
        let mut pinned = caller.image_lock.try_clone().unwrap();
        pinned.seek(SeekFrom::Start(0)).unwrap();
        identity::verify_file(&mut pinned, &expected).unwrap();
    }
    #[cfg(not(feature = "development-runtime"))]
    {
        assert!(
            fs::rename(&image, &previous).is_err(),
            "production image lock still denies replacement"
        );
    }
    drop(caller);
    prune_callers(&mut cache.lock().unwrap());
    assert!(cache.lock().unwrap().is_empty());

    // A harmless PE overlay changes the image hash while keeping the child
    // runnable. Reuse of the path cannot inherit the previous process's trust.
    OpenOptions::new()
        .append(true)
        .open(&image)
        .unwrap()
        .write_all(b"replacement image")
        .unwrap();
    let mut second = WaitingProcess::start(&image);
    assert!(matches!(
        cache_verified_caller(
            identity::inspect_process(second.0.id()).unwrap(),
            &cache,
            |process| verify_image(process, &expected)
        ),
        Err(BrokerError::Integrity)
    ));
    assert!(cache.lock().unwrap().is_empty());
    let replacement_hash = crate::crypto::digest(&fs::read(&image).unwrap());
    cache_verified_caller(
        identity::inspect_process(second.0.id()).unwrap(),
        &cache,
        |process| verify_image(process, &replacement_hash),
    )
    .unwrap();
    second.finish();
    prune_callers(&mut cache.lock().unwrap());
    assert!(cache.lock().unwrap().is_empty());
}
