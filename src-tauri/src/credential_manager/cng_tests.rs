//! Native regressions use an unnamed, unprotected RSA key. They never open the
//! user's persisted CarbonPaper key or require an authentication dialog.

use super::*;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Security::Cryptography::{
    NCryptCreatePersistedKey, NCryptEncrypt, NCryptFinalizeKey, NCryptFreeObject,
    NCryptOpenStorageProvider, CERT_KEY_SPEC, NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE,
    NCRYPT_PAD_PKCS1_FLAG, NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM,
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
