//! Authentication and encryption shared by the archive and its text reader.
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

#[derive(Debug)]
pub(crate) struct InvalidBinding;

impl std::fmt::Display for InvalidBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid OCR key or payload binding")
    }
}

impl std::error::Error for InvalidBinding {}

pub(crate) fn scope(dataset: &str, screenshot: i64, group: i64) -> Result<Vec<u8>, InvalidBinding> {
    if !carbonpaper_app_bound::protocol::valid_id(dataset) || screenshot <= 0 || group <= 0 {
        return Err(InvalidBinding);
    }
    let mut bytes = b"carbonpaper/ocr-key/v1\0".to_vec();
    bytes.extend_from_slice(dataset.as_bytes());
    bytes.extend_from_slice(&screenshot.to_le_bytes());
    bytes.extend_from_slice(&group.to_le_bytes());
    Ok(bytes)
}

pub(crate) fn scope_tag(
    key: &[u8],
    dataset: &str,
    screenshot: i64,
    group: i64,
) -> Result<Vec<u8>, InvalidBinding> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| InvalidBinding)?;
    mac.update(&scope(dataset, screenshot, group)?);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(crate) fn verify_scope(
    key: &[u8],
    dataset: &str,
    screenshot: i64,
    group: i64,
    tag: &[u8],
) -> Result<(), InvalidBinding> {
    if key.len() != 32 {
        return Err(InvalidBinding);
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| InvalidBinding)?;
    mac.update(&scope(dataset, screenshot, group)?);
    mac.verify_slice(tag).map_err(|_| InvalidBinding)
}

pub(crate) fn aad(
    dataset: &str,
    screenshot: i64,
    group: i64,
    row: i64,
) -> Result<Vec<u8>, InvalidBinding> {
    if row <= 0 {
        return Err(InvalidBinding);
    }
    let mut data = scope(dataset, screenshot, group)?;
    data.extend_from_slice(b"/text/");
    data.extend_from_slice(&row.to_le_bytes());
    Ok(data)
}

pub(crate) fn block_aad(
    dataset: &str,
    screenshot: i64,
    group: i64,
) -> Result<Vec<u8>, InvalidBinding> {
    let mut data = scope(dataset, screenshot, group)?;
    data.extend_from_slice(b"/block/v2");
    Ok(data)
}

pub(crate) fn seal(key: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, InvalidBinding> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| InvalidBinding)?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut result = nonce.to_vec();
    result.extend(
        cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| InvalidBinding)?,
    );
    Ok(result)
}

pub(crate) fn open(key: &[u8], aad: &[u8], encrypted: &[u8]) -> Result<Vec<u8>, InvalidBinding> {
    if encrypted.len() < 28 {
        return Err(InvalidBinding);
    }
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| InvalidBinding)?
        .decrypt(
            Nonce::from_slice(&encrypted[..12]),
            Payload {
                msg: &encrypted[12..],
                aad,
            },
        )
        .map_err(|_| InvalidBinding)
}
