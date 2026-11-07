//! Password-protected, seekable backup streams. Only ciphertext reaches disk.
//!
//! RustCrypto STREAM authenticates the block counter and final-block marker.
//! The complete header is authenticated too. Readers verify every block before
//! exposing the ZIP and authenticate blocks again when ZIP seeks back to them.

use aead::stream::{EncryptorBE32, NewStream, StreamBE32, StreamPrimitive};
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use zeroize::{Zeroize, Zeroizing};

pub(super) const MAGIC: &[u8; 8] = b"CPBACK01";
const CHUNK_BYTES: usize = 1024 * 1024;
const TAG_BYTES: usize = 16;
// Version 1 accepts exactly these parameters, bounding unauthenticated KDF work.
const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const LANES: u32 = 1;
const PREFIX_BYTES: usize = 8 + 16 + 16 + 12 + 7;
const HEADER_BYTES: usize = PREFIX_BYTES + 48;
const FRAME_BYTES: u64 = (5 + CHUNK_BYTES + TAG_BYTES) as u64;

pub(super) fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        // `Interrupted` would make io::copy retry indefinitely.
        Err(io::Error::other("BACKUP_CANCELLED"))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn password_key(password: &str, salt: &[u8]) -> io::Result<Zeroizing<[u8; 32]>> {
    if password.is_empty() {
        return Err(invalid("BACKUP_PASSWORD_EMPTY"));
    }
    let parameters = Params::new(MEMORY_KIB, ITERATIONS, LANES, Some(32))
        .map_err(|_| invalid("Invalid backup KDF parameters"))?;
    let mut key = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters)
        .hash_password_into(password.as_bytes(), salt, &mut *key)
        .map_err(|_| invalid("Backup password derivation failed"))?;
    Ok(key)
}

pub(super) struct EncryptedWriter<W: Write> {
    inner: W,
    header: Vec<u8>,
    cipher: Option<EncryptorBE32<Aes256Gcm>>,
    buffer: Zeroizing<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
}

impl<W: Write> EncryptedWriter<W> {
    pub(super) fn new(
        mut inner: W,
        password: &str,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        check_cancelled(&cancelled)?;
        let mut header = Vec::with_capacity(HEADER_BYTES);
        header.extend_from_slice(MAGIC);
        for parameter in [MEMORY_KIB, ITERATIONS, LANES, CHUNK_BYTES as u32] {
            header.extend_from_slice(&parameter.to_le_bytes());
        }
        let mut randomness = [0; 35]; // salt, wrapping nonce, STREAM nonce
        rand::thread_rng().fill_bytes(&mut randomness);
        header.extend_from_slice(&randomness);
        let wrapping_key = password_key(password, &header[24..40])?;
        check_cancelled(&cancelled)?;
        let mut backup_key = Zeroizing::new([0; 32]);
        rand::thread_rng().fill_bytes(&mut *backup_key);
        let wrapped = Aes256Gcm::new_from_slice(&*wrapping_key)
            .unwrap()
            .encrypt(
                Nonce::from_slice(&header[40..52]),
                Payload {
                    msg: &*backup_key,
                    aad: &header,
                },
            )
            .map_err(|_| invalid("Backup key encryption failed"))?;
        let cipher = EncryptorBE32::new((&*backup_key).into(), header[52..59].into());
        header.extend_from_slice(&wrapped);
        inner.write_all(&header)?;
        Ok(Self {
            inner,
            header,
            cipher: Some(cipher),
            buffer: Zeroizing::new(Vec::with_capacity(CHUNK_BYTES)),
            cancelled,
        })
    }

    fn write_frame(&mut self, last: bool) -> io::Result<()> {
        check_cancelled(&self.cancelled)?;
        let payload = Payload {
            msg: &self.buffer,
            aad: &self.header,
        };
        let encrypted = if last {
            self.cipher
                .take()
                .ok_or_else(|| invalid("Backup writer already finished"))?
                .encrypt_last(payload)
        } else {
            self.cipher
                .as_mut()
                .ok_or_else(|| invalid("Backup writer already finished"))?
                .encrypt_next(payload)
        }
        .map_err(|_| invalid("Backup block encryption failed"))?;
        self.buffer.zeroize();
        self.buffer.clear();
        self.inner
            .write_all(&(encrypted.len() as u32).to_le_bytes())?;
        self.inner.write_all(&[u8::from(last)])?;
        self.inner.write_all(&encrypted)
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        self.write_frame(true)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for EncryptedWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        check_cancelled(&self.cancelled)?;
        if self.cipher.is_none() {
            return Err(invalid("Backup writer already finished"));
        }
        let count = input.len().min(CHUNK_BYTES - self.buffer.len());
        self.buffer.extend_from_slice(&input[..count]);
        if self.buffer.len() == CHUNK_BYTES {
            self.write_frame(false)?;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) struct EncryptedReader<R: Read + Seek> {
    inner: R,
    header: Vec<u8>,
    cipher: StreamBE32<Aes256Gcm>,
    full_chunks: u32,
    final_bytes: usize,
    length: u64,
    position: u64,
    cached_chunk: Option<u32>,
    buffer: Zeroizing<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
}

fn read_frame<R: Read>(reader: &mut R) -> io::Result<(bool, Vec<u8>)> {
    let mut frame = [0; 5];
    reader.read_exact(&mut frame)?;
    let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    let last = match frame[4] {
        0 => false,
        1 => true,
        _ => return Err(invalid("Invalid backup block marker")),
    };
    if !(TAG_BYTES..=CHUNK_BYTES + TAG_BYTES).contains(&length)
        || (!last && length != CHUNK_BYTES + TAG_BYTES)
        || (last && length == CHUNK_BYTES + TAG_BYTES)
    {
        return Err(invalid("Invalid backup block length"));
    }
    let mut encrypted = vec![0; length];
    reader.read_exact(&mut encrypted)?;
    Ok((last, encrypted))
}

impl<R: Read + Seek> EncryptedReader<R> {
    pub(super) fn open(
        mut inner: R,
        password: &str,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        check_cancelled(&cancelled)?;
        inner.seek(SeekFrom::Start(0))?;
        let mut header = vec![0; HEADER_BYTES];
        inner.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(invalid("BACKUP_UNSUPPORTED_FORMAT"));
        }
        for (index, expected) in [MEMORY_KIB, ITERATIONS, LANES, CHUNK_BYTES as u32]
            .into_iter()
            .enumerate()
        {
            let offset = 8 + index * 4;
            if u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap()) != expected {
                return Err(invalid("BACKUP_UNSUPPORTED_FORMAT"));
            }
        }
        let wrapping_key = password_key(password, &header[24..40])?;
        check_cancelled(&cancelled)?;
        let backup_key = Zeroizing::new(
            Aes256Gcm::new_from_slice(&*wrapping_key)
                .unwrap()
                .decrypt(
                    Nonce::from_slice(&header[40..52]),
                    Payload {
                        msg: &header[PREFIX_BYTES..],
                        aad: &header[..PREFIX_BYTES],
                    },
                )
                .map_err(|_| invalid("BACKUP_INCORRECT_PASSWORD_OR_CORRUPT"))?,
        );
        let cipher = StreamBE32::from_aead(
            Aes256Gcm::new_from_slice(&backup_key).unwrap(),
            header[52..59].into(),
        );
        let mut full_chunks = 0u32;
        let final_bytes;
        loop {
            check_cancelled(&cancelled)?;
            let (last, encrypted) = read_frame(&mut inner)?;
            let plaintext = Zeroizing::new(
                cipher
                    .decrypt(
                        full_chunks,
                        last,
                        Payload {
                            msg: &encrypted,
                            aad: &header,
                        },
                    )
                    .map_err(|_| invalid("BACKUP_INCORRECT_PASSWORD_OR_CORRUPT"))?,
            );
            if last {
                final_bytes = plaintext.len();
                let mut trailing = [0];
                if inner.read(&mut trailing)? != 0 {
                    return Err(invalid("Unexpected data after backup final block"));
                }
                break;
            }
            full_chunks = full_chunks
                .checked_add(1)
                .filter(|value| *value < u32::MAX)
                .ok_or_else(|| invalid("Backup stream is too large"))?;
        }
        let length = u64::from(full_chunks) * CHUNK_BYTES as u64 + final_bytes as u64;
        Ok(Self {
            inner,
            header,
            cipher,
            full_chunks,
            final_bytes,
            length,
            position: 0,
            cached_chunk: None,
            buffer: Zeroizing::new(Vec::new()),
            cancelled,
        })
    }

    fn load_chunk(&mut self, index: u32) -> io::Result<()> {
        check_cancelled(&self.cancelled)?;
        if self.cached_chunk == Some(index) {
            return Ok(());
        }
        self.buffer.zeroize();
        self.inner.seek(SeekFrom::Start(
            HEADER_BYTES as u64 + u64::from(index) * FRAME_BYTES,
        ))?;
        let (last, encrypted) = read_frame(&mut self.inner)?;
        if last != (index == self.full_chunks)
            || (last && encrypted.len() != self.final_bytes + TAG_BYTES)
        {
            return Err(invalid("Backup block changed after validation"));
        }
        self.buffer = Zeroizing::new(
            self.cipher
                .decrypt(
                    index,
                    last,
                    Payload {
                        msg: &encrypted,
                        aad: &self.header,
                    },
                )
                .map_err(|_| invalid("BACKUP_INCORRECT_PASSWORD_OR_CORRUPT"))?,
        );
        self.cached_chunk = Some(index);
        Ok(())
    }
}

impl<R: Read + Seek> Read for EncryptedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_cancelled(&self.cancelled)?;
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let index = (self.position / CHUNK_BYTES as u64) as u32;
        self.load_chunk(index)?;
        let offset = (self.position % CHUNK_BYTES as u64) as usize;
        let count = output.len().min(self.buffer.len() - offset);
        output[..count].copy_from_slice(&self.buffer[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for EncryptedReader<R> {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        check_cancelled(&self.cancelled)?;
        let position = match offset {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.length) + i128::from(value),
        };
        self.position = u64::try_from(position).map_err(|_| invalid("Invalid backup seek"))?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    fn archive() -> (Vec<u8>, Vec<u8>) {
        let plaintext = (0..CHUNK_BYTES * 2 + 123)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut writer = EncryptedWriter::new(Vec::new(), "backup-password", flag()).unwrap();
        writer.write_all(&plaintext).unwrap();
        (writer.finish().unwrap(), plaintext)
    }

    #[test]
    fn stream_round_trips_and_seeks_without_a_plaintext_file() {
        let (encrypted, plaintext) = archive();
        assert!(!encrypted
            .windows(plaintext[..128].len())
            .any(|window| window == &plaintext[..128]));
        let mut reader =
            EncryptedReader::open(Cursor::new(encrypted), "backup-password", flag()).unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, plaintext);
        reader
            .seek(SeekFrom::Start((CHUNK_BYTES - 7) as u64))
            .unwrap();
        let mut across = [0; 30];
        reader.read_exact(&mut across).unwrap();
        assert_eq!(across, plaintext[CHUNK_BYTES - 7..CHUNK_BYTES + 23]);
        reader.seek(SeekFrom::End(-20)).unwrap();
        reader.read_exact(&mut across[..20]).unwrap();
        assert_eq!(&across[..20], &plaintext[plaintext.len() - 20..]);
    }

    #[test]
    fn password_tampering_reordering_truncation_and_trailing_data_are_rejected() {
        let (encrypted, _) = archive();
        assert!(
            EncryptedReader::open(Cursor::new(encrypted.clone()), "wrong-password", flag())
                .is_err()
        );
        let mut altered = encrypted.clone();
        altered[HEADER_BYTES + 50] ^= 1;
        assert!(EncryptedReader::open(Cursor::new(altered), "backup-password", flag()).is_err());
        let mut reordered = encrypted.clone();
        let first = reordered[HEADER_BYTES..HEADER_BYTES + FRAME_BYTES as usize].to_vec();
        let second = reordered
            [HEADER_BYTES + FRAME_BYTES as usize..HEADER_BYTES + 2 * FRAME_BYTES as usize]
            .to_vec();
        reordered[HEADER_BYTES..HEADER_BYTES + FRAME_BYTES as usize].copy_from_slice(&second);
        reordered[HEADER_BYTES + FRAME_BYTES as usize..HEADER_BYTES + 2 * FRAME_BYTES as usize]
            .copy_from_slice(&first);
        assert!(EncryptedReader::open(Cursor::new(reordered), "backup-password", flag()).is_err());
        for length in [HEADER_BYTES + FRAME_BYTES as usize, encrypted.len() - 1] {
            assert!(EncryptedReader::open(
                Cursor::new(&encrypted[..length]),
                "backup-password",
                flag()
            )
            .is_err());
        }
        let mut trailing = encrypted.clone();
        trailing.push(0);
        assert!(EncryptedReader::open(Cursor::new(trailing), "backup-password", flag()).is_err());
        let mut excessive_kdf = encrypted;
        excessive_kdf[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            EncryptedReader::open(Cursor::new(excessive_kdf), "backup-password", flag()).is_err()
        );
    }
}
