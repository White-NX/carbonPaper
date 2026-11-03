//! Native regressions use unnamed, unprotected RSA keys. They never open the
//! user's persisted CarbonPaper key or require an authentication dialog.

use super::*;
pub(crate) mod recap;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Security::Cryptography::{
    NCryptCreatePersistedKey, NCryptEncrypt, NCryptExportKey, NCryptFinalizeKey, NCryptFreeObject,
    NCryptOpenStorageProvider, CERT_KEY_SPEC, NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE,
    NCRYPT_PAD_PKCS1_FLAG, NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM, NCRYPT_SILENT_FLAG,
};

fn ephemeral_key() -> CngKeyHandle {
    let mut provider = NCRYPT_PROV_HANDLE::default();
    let name = HSTRING::from(CNG_PROVIDER_NAME);
    // SAFETY: name and output storage are live for this synchronous call.
    unsafe { NCryptOpenStorageProvider(&mut provider, &name, 0) }.unwrap();
    let mut raw_key = NCRYPT_KEY_HANDLE::default();
    // SAFETY: the provider is live. A null key name creates an ephemeral key,
    // without reading, creating, or replacing any persisted user key.
    let created = unsafe {
        NCryptCreatePersistedKey(
            provider,
            &mut raw_key,
            NCRYPT_RSA_ALGORITHM,
            PCWSTR::null(),
            CERT_KEY_SPEC(0),
            NCRYPT_FLAGS(0),
        )
    };
    // SAFETY: we own this provider; the key has its own lifetime.
    unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) }.unwrap();
    created.unwrap();
    let key = CngKeyHandle { key: raw_key };
    // SAFETY: this is a newly created, unfinalized test key with no UI policy.
    unsafe { NCryptFinalizeKey(key.key, NCRYPT_FLAGS(0)) }.unwrap();
    key
}

fn wrap_for_test(key: &CngKeyHandle, plaintext: &[u8]) -> Vec<u8> {
    let mut length = 0;
    // SAFETY: the test key and input are live; null output queries the size.
    unsafe {
        NCryptEncrypt(
            key.key,
            Some(plaintext),
            None,
            None,
            &mut length,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .unwrap();
    let mut encrypted = vec![0; length as usize];
    // SAFETY: output has the size returned by CNG and remains exclusively owned.
    unsafe {
        NCryptEncrypt(
            key.key,
            Some(plaintext),
            None,
            Some(&mut encrypted),
            &mut length,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .unwrap();
    encrypted.truncate(length as usize);
    encrypted
}

fn authorized_state() -> CredentialManagerState {
    let state = CredentialManagerState::new(PathBuf::new());
    state.set_session_timeout(-1);
    // Set only process-local test policy; leave the user's registry untouched.
    *state.background_processing_enabled.lock().unwrap() = true;
    state.cache_master_key_for_tests(vec![3; MASTER_KEY_LEN]);
    state.update_auth_time();
    state.grant_background_lease();
    state
}

fn native_fixture() -> (CredentialManagerState, Vec<u8>) {
    let state = authorized_state();
    let key = ephemeral_key();
    let ciphertext = wrap_for_test(&key, &[7; MASTER_KEY_LEN]);
    *state.cached_private_key.lock().unwrap() = Some(key);
    (state, ciphertext)
}

fn public_blob(key: &CngKeyHandle) -> Vec<u8> {
    let blob = HSTRING::from("RSAPUBLICBLOB");
    let mut length = 0;
    // SAFETY: the unnamed test key and blob type are live; this queries size only.
    unsafe {
        NCryptExportKey(
            key.key,
            NCRYPT_KEY_HANDLE::default(),
            &blob,
            None,
            None,
            &mut length,
            NCRYPT_SILENT_FLAG,
        )
    }
    .unwrap();
    let mut bytes = vec![0; length as usize];
    // SAFETY: the output is the queried size, and all inputs stay live.
    unsafe {
        NCryptExportKey(
            key.key,
            NCRYPT_KEY_HANDLE::default(),
            &blob,
            None,
            Some(&mut bytes),
            &mut length,
            NCRYPT_SILENT_FLAG,
        )
    }
    .unwrap();
    bytes.truncate(length as usize);
    bytes
}

pub(crate) fn storage_credentials(directory: &std::path::Path) -> Arc<CredentialManagerState> {
    let state = Arc::new(CredentialManagerState::new(directory.into()));
    state.set_session_timeout(-1);
    state.background_enabled_for_tests(false);
    let key = ephemeral_key();
    let public = public_blob(&key);
    state.fixture_public_key(&public);
    save_public_key_to_file(&state, &public).unwrap();
    std::fs::write(
        state.file_path(MASTER_KEY_FILE_NAME),
        encode_master_key_file(&wrap_for_test(&key, &[3; MASTER_KEY_LEN])),
    )
    .unwrap();
    *state.cached_private_key.lock().unwrap() = Some(key);
    state.cache_master_key_for_tests(vec![3; MASTER_KEY_LEN]);
    state.update_auth_time();
    state
}

#[test]
fn public_export_can_open_before_enforcing_silence_on_public_operations() {
    let key = ephemeral_key();
    let expected = public_blob(&key);
    let exported =
        windows_impl::export_public_key_with_open(CngKeyAccess::Existing, |access, flags| {
            assert!(matches!(access, CngKeyAccess::Existing));
            // Model the Software KSP denial observed on high-protection cold opens.
            // Do not create a protected persisted key, which prompts for a password.
            if flags.contains(NCRYPT_SILENT_FLAG) {
                return Err(CredentialError::AuthRequired);
            }
            Ok(key)
        })
        .unwrap();
    assert_eq!(exported, expected);
}

fn cold_storage_credentials(
    directory: &std::path::Path,
    authority: &[u8],
) -> Arc<CredentialManagerState> {
    let state = Arc::new(CredentialManagerState::new(directory.into()));
    state.background_enabled_for_tests(false);
    *state.test_public_key_authority.lock().unwrap() = Some(authority.to_vec());
    state
}

#[test]
fn cold_start_initializes_storage_and_captures_without_unlocking() {
    let directory = tempfile::tempdir().unwrap();
    let public = public_blob(&ephemeral_key());
    std::fs::write(directory.path().join(PUBLIC_KEY_FILE_NAME), &public).unwrap();
    let wrapped_master = encrypt_with_exported_public_key(&public, &[3; MASTER_KEY_LEN]).unwrap();
    std::fs::write(
        directory.path().join(MASTER_KEY_FILE_NAME),
        encode_master_key_file(&wrapped_master),
    )
    .unwrap();
    let mut image = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut image, image::ImageFormat::Png)
        .unwrap();

    for restart in 0..2 {
        let credentials = cold_storage_credentials(directory.path(), &public);
        assert!(get_cached_public_key(&credentials).is_none());
        assert_eq!(initialize_public_key(&credentials).unwrap(), public);
        ensure_master_key_created(&credentials).unwrap();
        let storage =
            crate::storage::StorageState::new(directory.path().into(), credentials.clone());
        storage.initialize().unwrap();
        assert!(storage.is_initialized());

        let request = crate::storage::SaveScreenshotRequest {
            image_data: String::new(),
            image_hash: format!("protected-cold-capture-{restart}"),
            width: 2,
            height: 2,
            window_title: Some("Synthetic protected capture".into()),
            process_name: Some("Editor.exe".into()),
            metadata: None,
            ocr_results: None,
            source: Some("capture".into()),
            page_url: None,
            page_icon: None,
            visible_links: None,
        };
        let saved = storage
            .save_screenshot_temp_bytes(&request, image.get_ref())
            .unwrap();
        assert!(saved.screenshot_id.is_some());
        let image_path = directory.path().join(saved.image_path.unwrap());
        assert_ne!(std::fs::read(image_path).unwrap(), *image.get_ref());
        assert!(storage.screenshot_exists(&request.image_hash).unwrap());
        assert!(matches!(
            decrypt_row_key_with_cng_silent(&credentials, &wrapped_master),
            Err(CredentialError::AuthRequired)
        ));
        assert!(!credentials.is_session_valid());
        assert!(!credentials.protected_read_authorized());
        assert!(get_cached_master_key(&credentials).is_none());
        assert!(credentials.cached_private_key.lock().unwrap().is_none());
        storage.shutdown().unwrap();
    }
}

#[test]
fn cold_public_validation_rejects_substitution_before_and_after_caching() {
    let directory = tempfile::tempdir().unwrap();
    let public = public_blob(&ephemeral_key());
    let substitute = public_blob(&ephemeral_key());
    let credentials = cold_storage_credentials(directory.path(), &public);
    let public_path = directory.path().join(PUBLIC_KEY_FILE_NAME);
    let master_path = directory.path().join(MASTER_KEY_FILE_NAME);
    let master = encode_master_key_file(
        &encrypt_with_exported_public_key(&public, &[3; MASTER_KEY_LEN]).unwrap(),
    );
    std::fs::write(&master_path, &master).unwrap();
    std::fs::write(&public_path, &substitute).unwrap();

    assert!(matches!(
        initialize_public_key(&credentials),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert!(get_cached_public_key(&credentials).is_none());
    let storage = crate::storage::StorageState::new(directory.path().into(), credentials.clone());
    assert!(storage.initialize().is_err());
    assert!(!storage.is_initialized());
    assert!(!directory.path().join("screenshots.db").exists());

    std::fs::write(&public_path, &public).unwrap();
    assert_eq!(initialize_public_key(&credentials).unwrap(), public);
    storage.initialize().unwrap();
    std::fs::write(&public_path, &substitute).unwrap();
    assert!(matches!(
        wrap_storage_domain_key(&credentials, &[7; MASTER_KEY_LEN]),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert_eq!(std::fs::read(master_path).unwrap(), master);
    assert_eq!(std::fs::read(public_path).unwrap(), substitute);
    assert!(!credentials.is_session_valid());
    assert!(credentials.cached_private_key.lock().unwrap().is_none());
    storage.shutdown().unwrap();
}

#[test]
fn public_key_substitution_is_rejected_even_with_an_unlocked_original_private_key() {
    let directory = tempfile::tempdir().unwrap();
    let state = storage_credentials(directory.path());
    let original = get_cached_public_key(&state).unwrap();
    let master = std::fs::read(state.file_path(MASTER_KEY_FILE_NAME)).unwrap();
    let handle = state
        .cached_private_key
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .key
        .0;
    let attacker = public_blob(&ephemeral_key());
    assert_ne!(attacker, original);
    // Prove this is a valid substitute, not a malformed-file test.
    encrypt_with_exported_public_key(&attacker, &[7; 32]).unwrap();
    std::fs::write(state.file_path(PUBLIC_KEY_FILE_NAME), &attacker).unwrap();
    assert!(matches!(
        load_public_key_from_file(&state),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert!(matches!(
        wrap_storage_domain_key(&state, &[7; 32]),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert!(matches!(
        initialize_public_key(&state),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert!(matches!(
        save_public_key_to_file(&state, &original),
        Err(CredentialError::PublicKeyMismatch)
    ));
    std::fs::write(
        directory.path().join("screenshots.db"),
        b"original database sentinel",
    )
    .unwrap();
    let storage = crate::storage::StorageState::new(directory.path().into(), state.clone());
    assert!(storage.initialize().is_err());
    assert_eq!(
        std::fs::read(directory.path().join("screenshots.db")).unwrap(),
        b"original database sentinel"
    );
    assert_eq!(
        std::fs::read(state.file_path(PUBLIC_KEY_FILE_NAME)).unwrap(),
        attacker
    );
    assert_eq!(
        std::fs::read(state.file_path(MASTER_KEY_FILE_NAME)).unwrap(),
        master
    );
    assert_eq!(
        state
            .cached_private_key
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .key
            .0,
        handle
    );
    assert!(state.is_session_valid());
}

#[test]
fn existing_dataset_with_missing_public_or_cng_key_never_enters_first_use_creation() {
    let directory = tempfile::tempdir().unwrap();
    let state = storage_credentials(directory.path());
    let original = get_cached_public_key(&state).unwrap();
    let master = std::fs::read(state.file_path(MASTER_KEY_FILE_NAME)).unwrap();
    let handle = state
        .cached_private_key
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .key
        .0;
    *state.cached_public_key.lock().unwrap() = None;
    state
        .test_existing_key_missing
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        initialize_public_key(&state),
        Err(CredentialError::KeyNotFound)
    ));
    assert_eq!(
        std::fs::read(state.file_path(PUBLIC_KEY_FILE_NAME)).unwrap(),
        original
    );
    state
        .test_existing_key_missing
        .store(false, Ordering::Relaxed);
    std::fs::remove_file(state.file_path(PUBLIC_KEY_FILE_NAME)).unwrap();
    assert!(matches!(
        initialize_public_key(&state),
        Err(CredentialError::KeyNotFound)
    ));
    assert!(!state.file_path(PUBLIC_KEY_FILE_NAME).exists());
    assert_eq!(
        std::fs::read(state.file_path(MASTER_KEY_FILE_NAME)).unwrap(),
        master
    );
    assert_eq!(
        state
            .cached_private_key
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .key
            .0,
        handle
    );
}

#[test]
fn first_use_master_wrap_uses_validated_public_material_without_replacing_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let state = CredentialManagerState::new(directory.path().into());
    let key = ephemeral_key();
    let public = public_blob(&key);
    state.fixture_public_key(&public);
    save_public_key_to_file(&state, &public).unwrap();
    assert!(!state.is_session_valid());
    assert!(state.cached_private_key.lock().unwrap().is_none());
    ensure_master_key_created(&state).unwrap();
    let original = std::fs::read(state.master_key_file_path()).unwrap();
    assert_eq!(
        key.unwrap_row_key(&decode_master_key_file(&original).unwrap())
            .unwrap(),
        get_cached_master_key(&state).unwrap()
    );
    assert!(!state.is_session_valid());
    assert!(state.cached_private_key.lock().unwrap().is_none());
    assert!(
        !persist_new_credential_file(&state.master_key_file_path(), b"replacement master").unwrap()
    );
    assert_eq!(
        std::fs::read(state.master_key_file_path()).unwrap(),
        original
    );
    *state.cached_master_key.lock().unwrap() = None;
    ensure_master_key_created(&state).unwrap();
    assert!(get_cached_master_key(&state).is_none());
    assert_eq!(
        std::fs::read(state.master_key_file_path()).unwrap(),
        original
    );

    std::fs::remove_file(state.master_key_file_path()).unwrap();
    let database = directory.path().join("screenshots.db");
    std::fs::write(&database, b"existing dataset sentinel").unwrap();
    assert!(matches!(
        ensure_master_key_created(&state),
        Err(CredentialError::KeyNotFound)
    ));
    assert!(!state.master_key_file_path().exists());
    std::fs::remove_file(database).unwrap();
    let substitute = public_blob(&ephemeral_key());
    std::fs::write(state.file_path(PUBLIC_KEY_FILE_NAME), &substitute).unwrap();
    assert!(matches!(
        ensure_master_key_created(&state),
        Err(CredentialError::PublicKeyMismatch)
    ));
    assert!(!state.master_key_file_path().exists());
    assert_eq!(
        std::fs::read(state.file_path(PUBLIC_KEY_FILE_NAME)).unwrap(),
        substitute
    );
}

#[test]
fn native_retained_handle_serves_single_and_batch_reads_across_threads() {
    let (state, ciphertext) = native_fixture();
    // This ciphertext belongs to an unnamed key. Reopening CarbonPaper's
    // persisted key (the original bug) cannot decrypt it.
    assert_eq!(
        decrypt_row_key_with_cng(&state, &ciphertext).unwrap(),
        vec![7; MASTER_KEY_LEN]
    );
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let session = CngKeySession::open_silent(&state).unwrap();
                    session.unwrap_row_key(&ciphertext).unwrap()
                })
            })
            .collect();
        for reader in readers {
            assert_eq!(reader.join().unwrap(), vec![7; MASTER_KEY_LEN]);
        }
    });
}

#[test]
fn native_measured_unwrap_reports_calls_and_honors_cancellation_after_lock_wait() {
    let (state, ciphertext) = native_fixture();
    let mut timings = RowKeyDecryptTimings::default();
    let key = decrypt_row_key_with_cng_measured(&state, &ciphertext, &mut timings, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(key, vec![7; MASTER_KEY_LEN]);
    assert_eq!(timings.calls, 1);
    assert!(!timings.decrypt.is_zero());
    let cancelled = AtomicBool::new(false);
    let (entered, waiting) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let held = state.cached_private_key.lock().unwrap();
        let reader = scope.spawn(|| {
            let first = std::cell::Cell::new(true);
            let mut timings = RowKeyDecryptTimings::default();
            let result =
                decrypt_row_key_with_cng_measured(&state, &ciphertext, &mut timings, &|| {
                    if first.replace(false) {
                        entered.send(()).unwrap();
                        false
                    } else {
                        cancelled.load(Ordering::Acquire)
                    }
                });
            assert!(result.unwrap().is_none());
            assert_eq!(timings.calls, 0);
        });
        waiting.recv().unwrap();
        cancelled.store(true, Ordering::Release);
        drop(held);
        reader.join().unwrap();
    });
}

#[test]
fn native_batch_reader_rechecks_authorization_after_admission() {
    let (state, ciphertext) = native_fixture();
    let reader = CngKeySession::open_silent(&state).unwrap();
    state.invalidate_session();
    assert_eq!(
        reader.unwrap_row_key(&ciphertext).unwrap(),
        vec![7; MASTER_KEY_LEN]
    );
    state.revoke_background_lease();
    assert!(matches!(
        reader.unwrap_row_key(&ciphertext),
        Err(CredentialError::AuthRequired)
    ));
    assert!(
        !state.silent_read_auth_required(),
        "policy revocation is not a CNG failure"
    );
}

#[test]
fn native_cache_reset_revokes_readers_that_already_started() {
    let (state, ciphertext) = native_fixture();
    let reader = CngKeySession::open_silent(&state).unwrap();
    state.clear_all_cached_keys();
    assert!(state.cached_private_key.lock().unwrap().is_none());
    assert!(matches!(
        reader.unwrap_row_key(&ciphertext),
        Err(CredentialError::AuthRequired)
    ));
}

#[test]
fn native_manual_reads_work_with_unattended_processing_disabled() {
    let (state, ciphertext) = native_fixture();
    *state.background_processing_enabled.lock().unwrap() = false;
    state.revoke_background_lease();
    let reader = CngKeySession::open_silent(&state).unwrap();
    assert_eq!(
        reader.unwrap_row_key(&ciphertext).unwrap(),
        vec![7; MASTER_KEY_LEN]
    );
    state.invalidate_session();
    assert!(matches!(
        reader.unwrap_row_key(&ciphertext),
        Err(CredentialError::AuthRequired)
    ));
}

#[test]
fn native_readers_cannot_borrow_another_credentials_private_key() {
    let (owner, ciphertext) = native_fixture();
    let other = authorized_state();
    assert!(matches!(
        decrypt_row_key_with_cng_silent(&other, &ciphertext),
        Err(CredentialError::AuthRequired)
    ));
    assert_eq!(
        decrypt_row_key_with_cng_silent(&owner, &ciphertext).unwrap(),
        vec![7; MASTER_KEY_LEN]
    );
}

#[test]
fn native_corrupt_ciphertext_does_not_revoke_valid_read_authorization() {
    let (state, ciphertext) = native_fixture();
    assert!(matches!(
        decrypt_row_key_with_cng_silent(&state, b"invalid"),
        Err(CredentialError::SystemError(_))
    ));
    assert!(!state.silent_read_auth_required());
    assert_eq!(
        decrypt_row_key_with_cng_silent(&state, &ciphertext).unwrap(),
        vec![7; MASTER_KEY_LEN]
    );
}

#[test]
fn native_initial_verification_retains_the_handle_used_for_its_silent_probe() {
    let key = ephemeral_key();
    let ciphertext = wrap_for_test(&key, &[7; MASTER_KEY_LEN]);
    let mut retained = None;
    let master_key = verify_with_retained_key(
        &mut retained,
        |handle: &CngKeyHandle| {
            let mut probe = handle.unwrap_row_key(&ciphertext)?;
            probe.zeroize();
            Ok(())
        },
        || {
            let master_key = key.decrypt_for_window(&ciphertext, None)?;
            Ok((key, master_key))
        },
        || panic!("the first unlock must happen in the parent"),
    )
    .unwrap();
    assert_eq!(master_key, vec![7; MASTER_KEY_LEN]);
    assert_eq!(
        retained.unwrap().unwrap_row_key(&ciphertext).unwrap(),
        master_key
    );
}
