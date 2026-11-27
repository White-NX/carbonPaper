use super::*;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn test_cache(directory: &Path) -> Cache {
    let root = directory.canonicalize().unwrap();
    let mut locks = update::DirectoryLocks::default();
    locks.pin(&root).unwrap();
    Cache {
        root,
        _locks: locks,
    }
}

fn archive(entries: &[(&str, &[u8])]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    {
        let mut archive = ZipWriter::new(&mut file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in entries {
            archive.start_file(*name, options).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
    }
    file.rewind().unwrap();
    file
}

#[test]
fn component_extraction_and_failed_work_are_removed_without_removing_the_cache() {
    let directory = tempfile::tempdir().unwrap();
    let cache = test_cache(directory.path());
    let entries: &[(&str, &[u8])] = &[
        (repair::COMPONENT_FILES[0], b"service"),
        (repair::COMPONENT_FILES[1], b"helper"),
        (manifest::MANIFEST_NAME, b"manifest"),
        (manifest::SIGNATURE_NAME, b"signature"),
    ];
    let mut file = archive(entries);
    let mut scratch = cache.scratch().unwrap();
    let work = scratch.path.clone();
    let extracted = extract_archive(&mut file, &mut scratch, true).unwrap();
    for (name, bytes) in entries {
        assert_eq!(fs::read(extracted.join(name)).unwrap(), *bytes);
    }
    drop(scratch);
    assert!(!work.exists());
    assert!(cache.root.exists());

    let mut file = archive(&[
        (repair::COMPONENT_FILES[0], b"service"),
        ("extra.dll", b"extra"),
    ]);
    let mut scratch = cache.scratch().unwrap();
    let work = scratch.path.clone();
    assert!(extract_archive(&mut file, &mut scratch, true).is_err());
    drop(scratch);
    assert!(!work.exists());
}

#[test]
fn extraction_rejects_windows_aliases_links_and_unbounded_component_packages() {
    let directory = tempfile::tempdir().unwrap();
    let cache = test_cache(directory.path());
    for name in [
        "../outside.exe",
        "C:/outside.exe",
        "file:stream",
        "NUL.dll",
        "a/../file",
        "a./file",
    ] {
        let mut file = archive(&[(name, b"payload")]);
        let mut scratch = cache.scratch().unwrap();
        assert!(
            extract_archive(&mut file, &mut scratch, false).is_err(),
            "{name}"
        );
    }
    let mut file = archive(&[("worker.exe", b"one"), ("Worker.exe", b"two")]);
    assert!(extract_archive(&mut file, &mut cache.scratch().unwrap(), false).is_err());

    let mut file = tempfile::tempfile().unwrap();
    {
        let mut archive = ZipWriter::new(&mut file);
        archive
            .add_symlink("worker.exe", "../outside.exe", SimpleFileOptions::default())
            .unwrap();
        archive.finish().unwrap();
    }
    file.rewind().unwrap();
    assert!(extract_archive(&mut file, &mut cache.scratch().unwrap(), false).is_err());

    let mut file = archive(&[(repair::COMPONENT_FILES[0], b"service")]);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let central = bytes
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    let oversized = (repair::MAX_COMPONENT_EXPANDED_BYTES as u32 + 1).to_le_bytes();
    bytes[22..26].copy_from_slice(&oversized);
    bytes[central + 24..central + 28].copy_from_slice(&oversized);
    file.rewind().unwrap();
    file.write_all(&bytes).unwrap();
    file.rewind().unwrap();
    assert!(extract_archive(&mut file, &mut cache.scratch().unwrap(), true).is_err());
}

#[test]
fn archive_checksum_verification_rewinds_the_file_and_rejects_changed_bytes() {
    let mut file = archive(&[("worker.exe", b"worker")]);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let expected = format!("{:X}", Sha256::digest(&bytes));
    verify_archive(&mut file, &expected).unwrap();
    assert_eq!(file.stream_position().unwrap(), 0);
    file.write_all(b"BAD!").unwrap();
    assert!(verify_archive(&mut file, &expected).is_err());
}

#[test]
fn cache_retains_only_needed_component_versions_and_reclaims_interrupted_work() {
    let directory = tempfile::tempdir().unwrap();
    let cache = test_cache(directory.path());
    let owner = "1.2.3-0123456789abcdef";
    let service = "1.2.4-0123456789abcdef";
    let old = "1.2.2-0123456789abcdef";
    for id in [owner, service, old] {
        let path = cache.root.join(format!("bundle-{id}"));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("component"), b"cached").unwrap();
    }
    let interrupted = cache.root.join("work-interrupted");
    fs::create_dir(&interrupted).unwrap();
    fs::write(interrupted.join("download.zip"), b"partial").unwrap();
    fs::write(cache.root.join("unrelated.txt"), b"preserved").unwrap();
    cache
        .prune(&[
            Target {
                id: Some(owner.into()),
                version: "1.2.3".into(),
            },
            Target {
                id: Some(service.into()),
                version: "1.2.4".into(),
            },
        ])
        .unwrap();
    assert!(cache.root.join(format!("bundle-{owner}")).exists());
    assert!(cache.root.join(format!("bundle-{service}")).exists());
    assert!(!cache.root.join(format!("bundle-{old}")).exists());
    assert!(!interrupted.exists());
    assert_eq!(
        fs::read(cache.root.join("unrelated.txt")).unwrap(),
        b"preserved"
    );
}
