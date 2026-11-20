use std::{
    io::{self, Read, Write},
    ops::{Deref, DerefMut},
};
use zeroize::{Zeroize, Zeroizing};

/// A reusable sensitive byte buffer whose clearing and destruction are compiled
/// here, rather than in an unoptimized crate that instantiates generic Vec code.
#[derive(Default)]
pub struct ZeroizingBuffer(Vec<u8>);

impl ZeroizingBuffer {
    #[inline(never)]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    #[inline(never)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    #[inline(never)]
    pub fn reserve_capacity(&mut self, capacity: usize) -> io::Result<()> {
        self.0
            .try_reserve_exact(capacity.saturating_sub(self.0.len()))
            .map_err(io::Error::other)
    }

    #[inline(never)]
    pub fn extend_from_slice(&mut self, input: &[u8]) {
        self.0.extend_from_slice(input);
    }

    #[inline(never)]
    pub fn into_zeroizing(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }

    #[inline(never)]
    pub fn clear(&mut self) {
        // Reused buffers may have grown for an unusually large image. Erase the
        // active bytes here; Drop still erases the entire allocation once.
        self.0.as_mut_slice().zeroize();
        self.0.clear();
    }

    /// Reads through a concrete call boundary so allocation and copying are
    /// optimized. A false result means the input exceeded the caller's budget;
    /// that caller can rewind the file and process it without prefetching.
    #[inline(never)]
    pub fn read_from(&mut self, reader: &mut dyn Read, limit: Option<usize>) -> io::Result<bool> {
        self.clear();
        let mut block = Zeroizing::new([0u8; 64 * 1024]);
        loop {
            let remaining = limit.map_or(block.len(), |limit| {
                limit
                    .saturating_sub(self.0.len())
                    .saturating_add(1)
                    .min(block.len())
            });
            let read = match reader.read(&mut block[..remaining]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if read == 0 {
                return Ok(true);
            }
            if limit.is_some_and(|limit| read > limit.saturating_sub(self.0.len())) {
                self.clear();
                return Ok(false);
            }
            self.0.extend_from_slice(&block[..read]);
        }
    }
}

impl From<Vec<u8>> for ZeroizingBuffer {
    #[inline(never)]
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<Zeroizing<Vec<u8>>> for ZeroizingBuffer {
    #[inline(never)]
    fn from(mut bytes: Zeroizing<Vec<u8>>) -> Self {
        Self(std::mem::take(&mut *bytes))
    }
}

impl Deref for ZeroizingBuffer {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ZeroizingBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Write for ZeroizingBuffer {
    #[inline(never)]
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ZeroizingBuffer {
    #[inline(never)]
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_prefetch_limit_clears_partial_input_and_allows_a_later_read() {
        let mut buffer = ZeroizingBuffer::with_capacity(8);
        assert!(!buffer
            .read_from(&mut Cursor::new(b"oversized"), Some(8))
            .unwrap());
        assert!(buffer.is_empty());
        assert!(buffer
            .read_from(&mut Cursor::new(b"next"), Some(8))
            .unwrap());
        assert_eq!(&**buffer, b"next");
    }
}
