//! Stable flat-vector format shared by the Tauri process and `carbonpaper-ml`.
//!
//! Version 4 separates variable-length subject keys from the fixed-stride f32
//! matrix. Readers can therefore mmap the file and reach candidate vectors
//! directly instead of walking the whole corpus during the first query.

use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"CPDVEC04";
pub const FORMAT_VERSION: u32 = 4;
pub const HEADER_BYTES: u64 = 512;
pub const VECTOR_ALIGNMENT: u64 = 4096;
pub const ANN_IMPLEMENTATION_VERSION: &str = "usearch-2.26.0";
pub const ANN_ALGORITHM: &str = "hnsw";
pub const ANN_METRIC: &str = "ip";
pub const ANN_QUANTIZATION: &str = "i8";
pub const ANN_CONNECTIVITY: u32 = 16;
pub const ANN_EXPANSION_ADD: u32 = 160;
pub const MAX_ROWS: u64 = 2_000_000;
pub const MAX_DIMENSIONS: u32 = 65_536;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const ANN_PROBE_MIN_COSINE: f32 = 0.95;

/// Validate the observable result of a persisted usearch query.
///
/// ANN search is approximate and the graph stores I8-quantized vectors, so a
/// probe is not required to return the exact ordinal from which that probe was
/// read. Duplicate or near-tied vectors can legitimately produce another
/// neighbor. What must hold after save/reopen is that the result is non-empty,
/// finite, and points at an ordinal present in the flat sidecar.
pub fn validate_ann_search_result(
    keys: &[u64],
    distances: &[f32],
    row_count: u64,
) -> Result<(), String> {
    if keys.is_empty() || keys.len() != distances.len() {
        return Err("ANN self-test returned an invalid result set".to_string());
    }
    if keys.iter().any(|key| {
        *key == 0
            || key
                .checked_sub(1)
                .is_none_or(|ordinal| ordinal >= row_count)
    }) {
        return Err("ANN self-test returned an out-of-range ordinal".to_string());
    }
    if distances.iter().any(|distance| !distance.is_finite()) {
        return Err("ANN self-test returned a non-finite distance".to_string());
    }
    Ok(())
}

/// Verify that a known key survived serialization and I8 dequantization.
/// This is the deterministic half of the ANN self-test; unlike approximate
/// search it has no nearest-neighbor tie-breaking behavior.
pub fn validate_ann_recovered_probe(
    expected: &[f32],
    recovered: &[f32],
    vectors_found: usize,
) -> Result<f32, String> {
    if vectors_found != 1 || expected.is_empty() || expected.len() != recovered.len() {
        return Err("ANN self-test could not recover the probe vector".to_string());
    }
    let mut dot = 0.0f64;
    let mut expected_norm = 0.0f64;
    let mut recovered_norm = 0.0f64;
    for (&left, &right) in expected.iter().zip(recovered) {
        if !left.is_finite() || !right.is_finite() {
            return Err("ANN self-test recovered a non-finite vector".to_string());
        }
        let left = f64::from(left);
        let right = f64::from(right);
        dot += left * right;
        expected_norm += left * left;
        recovered_norm += right * right;
    }
    if expected_norm <= f64::EPSILON || recovered_norm <= f64::EPSILON {
        return Err("ANN self-test recovered a zero-norm vector".to_string());
    }
    let cosine = dot / (expected_norm.sqrt() * recovered_norm.sqrt());
    if !cosine.is_finite() || cosine < f64::from(ANN_PROBE_MIN_COSINE) {
        return Err(format!(
            "ANN self-test recovered a mismatched probe vector: cosine={cosine:.6}"
        ));
    }
    Ok(cosine as f32)
}

const FIELD_MAGIC: usize = 0;
const FIELD_VERSION: usize = 8;
const FIELD_HEADER_BYTES: usize = 12;
const FIELD_GENERATION: usize = 16;
const FIELD_COVERED_EPOCH: usize = 24;
const FIELD_ROW_COUNT: usize = 32;
const FIELD_DIMENSIONS: usize = 40;
const FIELD_KEY_OFFSETS_OFFSET: usize = 48;
const FIELD_KEYS_OFFSET: usize = 56;
const FIELD_KEYS_BYTES: usize = 64;
const FIELD_VECTORS_OFFSET: usize = 72;
const FIELD_VECTORS_BYTES: usize = 80;
const FIELD_EXPANSION_SEARCH: usize = 88;
const FIELD_CONNECTIVITY: usize = 92;
const FIELD_EXPANSION_ADD: usize = 96;
const FIELD_INDEX_KIND: usize = 104;
const FIELD_MODEL_ID: usize = 168;
const FIELD_MODEL_REVISION: usize = 296;
const FIELD_ANN_IMPLEMENTATION: usize = 424;
const INDEX_KIND_BYTES: usize = 64;
const MODEL_ID_BYTES: usize = 128;
const MODEL_REVISION_BYTES: usize = 128;
const ANN_IMPLEMENTATION_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub generation: u64,
    pub covered_epoch: u64,
    pub row_count: u64,
    pub dimensions: u32,
    pub key_offsets_offset: u64,
    pub keys_offset: u64,
    pub keys_bytes: u64,
    pub vectors_offset: u64,
    pub vectors_bytes: u64,
    pub expansion_search: u32,
    pub connectivity: u32,
    pub expansion_add: u32,
    pub index_kind: String,
    pub model_id: String,
    pub model_revision: String,
    pub ann_implementation: String,
}

impl Header {
    pub fn for_snapshot(
        generation: u64,
        covered_epoch: u64,
        row_count: u64,
        dimensions: u32,
        index_kind: &str,
        model_id: &str,
        model_revision: &str,
        expansion_search: u32,
        key_bytes: u64,
    ) -> Result<Self, String> {
        validate_text("index kind", index_kind, INDEX_KIND_BYTES)?;
        validate_text("model id", model_id, MODEL_ID_BYTES)?;
        validate_text("model revision", model_revision, MODEL_REVISION_BYTES)?;
        if row_count > MAX_ROWS {
            return Err(format!(
                "ANN sidecar row count {row_count} exceeds {MAX_ROWS}"
            ));
        }
        if dimensions == 0 || dimensions > MAX_DIMENSIONS {
            return Err(format!("Invalid ANN sidecar dimensions: {dimensions}"));
        }
        let offset_entries = row_count
            .checked_add(1)
            .ok_or("ANN key-offset count overflow")?;
        let key_offsets_bytes = offset_entries
            .checked_mul(8)
            .ok_or("ANN key-offset table overflow")?;
        let key_offsets_offset = HEADER_BYTES;
        let keys_offset = key_offsets_offset
            .checked_add(key_offsets_bytes)
            .ok_or("ANN keys offset overflow")?;
        let keys_end = keys_offset
            .checked_add(key_bytes)
            .ok_or("ANN key section overflow")?;
        let vectors_offset = align_up(keys_end, VECTOR_ALIGNMENT)?;
        let vector_stride = u64::from(dimensions)
            .checked_mul(4)
            .ok_or("ANN vector stride overflow")?;
        let vectors_bytes = row_count
            .checked_mul(vector_stride)
            .ok_or("ANN vector section overflow")?;
        Ok(Self {
            generation,
            covered_epoch,
            row_count,
            dimensions,
            key_offsets_offset,
            keys_offset,
            keys_bytes: key_bytes,
            vectors_offset,
            vectors_bytes,
            expansion_search,
            connectivity: ANN_CONNECTIVITY,
            expansion_add: ANN_EXPANSION_ADD,
            index_kind: index_kind.to_string(),
            model_id: model_id.to_string(),
            model_revision: model_revision.to_string(),
            ann_implementation: ANN_IMPLEMENTATION_VERSION.to_string(),
        })
    }

    pub fn file_len(&self) -> Result<u64, String> {
        self.vectors_offset
            .checked_add(self.vectors_bytes)
            .ok_or_else(|| "ANN sidecar file size overflow".to_string())
    }

    pub fn encode(&self) -> Result<[u8; HEADER_BYTES as usize], String> {
        let mut bytes = [0u8; HEADER_BYTES as usize];
        bytes[FIELD_MAGIC..FIELD_MAGIC + MAGIC.len()].copy_from_slice(MAGIC);
        put_u32(&mut bytes, FIELD_VERSION, FORMAT_VERSION);
        put_u32(&mut bytes, FIELD_HEADER_BYTES, HEADER_BYTES as u32);
        put_u64(&mut bytes, FIELD_GENERATION, self.generation);
        put_u64(&mut bytes, FIELD_COVERED_EPOCH, self.covered_epoch);
        put_u64(&mut bytes, FIELD_ROW_COUNT, self.row_count);
        put_u32(&mut bytes, FIELD_DIMENSIONS, self.dimensions);
        put_u64(
            &mut bytes,
            FIELD_KEY_OFFSETS_OFFSET,
            self.key_offsets_offset,
        );
        put_u64(&mut bytes, FIELD_KEYS_OFFSET, self.keys_offset);
        put_u64(&mut bytes, FIELD_KEYS_BYTES, self.keys_bytes);
        put_u64(&mut bytes, FIELD_VECTORS_OFFSET, self.vectors_offset);
        put_u64(&mut bytes, FIELD_VECTORS_BYTES, self.vectors_bytes);
        put_u32(&mut bytes, FIELD_EXPANSION_SEARCH, self.expansion_search);
        put_u32(&mut bytes, FIELD_CONNECTIVITY, self.connectivity);
        put_u32(&mut bytes, FIELD_EXPANSION_ADD, self.expansion_add);
        put_text(
            &mut bytes,
            FIELD_INDEX_KIND,
            INDEX_KIND_BYTES,
            &self.index_kind,
        )?;
        put_text(&mut bytes, FIELD_MODEL_ID, MODEL_ID_BYTES, &self.model_id)?;
        put_text(
            &mut bytes,
            FIELD_MODEL_REVISION,
            MODEL_REVISION_BYTES,
            &self.model_revision,
        )?;
        put_text(
            &mut bytes,
            FIELD_ANN_IMPLEMENTATION,
            ANN_IMPLEMENTATION_BYTES,
            &self.ann_implementation,
        )?;
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES as usize || &bytes[FIELD_MAGIC..FIELD_MAGIC + 8] != MAGIC {
            return Err("ANN sidecar has an invalid header".to_string());
        }
        let version = get_u32(bytes, FIELD_VERSION)?;
        if version != FORMAT_VERSION {
            return Err(format!("Unsupported ANN sidecar format version: {version}"));
        }
        if get_u32(bytes, FIELD_HEADER_BYTES)? != HEADER_BYTES as u32 {
            return Err("ANN sidecar header size mismatch".to_string());
        }
        let header = Self {
            generation: get_u64(bytes, FIELD_GENERATION)?,
            covered_epoch: get_u64(bytes, FIELD_COVERED_EPOCH)?,
            row_count: get_u64(bytes, FIELD_ROW_COUNT)?,
            dimensions: get_u32(bytes, FIELD_DIMENSIONS)?,
            key_offsets_offset: get_u64(bytes, FIELD_KEY_OFFSETS_OFFSET)?,
            keys_offset: get_u64(bytes, FIELD_KEYS_OFFSET)?,
            keys_bytes: get_u64(bytes, FIELD_KEYS_BYTES)?,
            vectors_offset: get_u64(bytes, FIELD_VECTORS_OFFSET)?,
            vectors_bytes: get_u64(bytes, FIELD_VECTORS_BYTES)?,
            expansion_search: get_u32(bytes, FIELD_EXPANSION_SEARCH)?,
            connectivity: get_u32(bytes, FIELD_CONNECTIVITY)?,
            expansion_add: get_u32(bytes, FIELD_EXPANSION_ADD)?,
            index_kind: get_text(bytes, FIELD_INDEX_KIND, INDEX_KIND_BYTES)?,
            model_id: get_text(bytes, FIELD_MODEL_ID, MODEL_ID_BYTES)?,
            model_revision: get_text(bytes, FIELD_MODEL_REVISION, MODEL_REVISION_BYTES)?,
            ann_implementation: get_text(
                bytes,
                FIELD_ANN_IMPLEMENTATION,
                ANN_IMPLEMENTATION_BYTES,
            )?,
        };
        header.validate_layout(bytes.len() as u64)?;
        Ok(header)
    }

    fn validate_layout(&self, file_len: u64) -> Result<(), String> {
        if self.row_count > MAX_ROWS || self.dimensions == 0 || self.dimensions > MAX_DIMENSIONS {
            return Err("ANN sidecar shape is outside supported bounds".to_string());
        }
        if self.connectivity != ANN_CONNECTIVITY
            || self.expansion_add != ANN_EXPANSION_ADD
            || self.ann_implementation != ANN_IMPLEMENTATION_VERSION
        {
            return Err("ANN sidecar implementation contract mismatch".to_string());
        }
        let offset_bytes = self
            .row_count
            .checked_add(1)
            .and_then(|value| value.checked_mul(8))
            .ok_or("ANN sidecar offset table overflow")?;
        if self.key_offsets_offset != HEADER_BYTES
            || self.keys_offset != self.key_offsets_offset.saturating_add(offset_bytes)
            || self.vectors_offset % VECTOR_ALIGNMENT != 0
        {
            return Err("ANN sidecar section layout is invalid".to_string());
        }
        let expected_vectors = self
            .row_count
            .checked_mul(u64::from(self.dimensions).saturating_mul(4))
            .ok_or("ANN sidecar vector section overflow")?;
        if self.vectors_bytes != expected_vectors || self.file_len()? != file_len {
            return Err("ANN sidecar file length mismatch".to_string());
        }
        Ok(())
    }
}

pub struct MappedFlatIndex {
    _file: File,
    map: Mmap,
    pub header: Header,
}

impl MappedFlatIndex {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path)
            .map_err(|error| format!("Failed to open ANN sidecar {}: {error}", path.display()))?;
        let map = unsafe { MmapOptions::new().map(&file) }
            .map_err(|error| format!("Failed to mmap ANN sidecar {}: {error}", path.display()))?;
        let header = Header::decode(&map)?;
        let last = read_u64_at(&map, header.key_offsets_offset + header.row_count * 8)?;
        if last != header.keys_bytes {
            return Err("ANN sidecar key-offset terminator mismatch".to_string());
        }
        Ok(Self {
            _file: file,
            map,
            header,
        })
    }

    pub fn key(&self, ordinal: usize) -> Result<&str, String> {
        if ordinal >= self.header.row_count as usize {
            return Err(format!("ANN ordinal {ordinal} is out of range"));
        }
        let base = self.header.key_offsets_offset;
        let start = read_u64_at(&self.map, base + ordinal as u64 * 8)?;
        let end = read_u64_at(&self.map, base + (ordinal as u64 + 1) * 8)?;
        if start > end || end > self.header.keys_bytes {
            return Err("ANN sidecar key offsets are invalid".to_string());
        }
        let absolute_start = self.header.keys_offset.saturating_add(start);
        let absolute_end = self.header.keys_offset.saturating_add(end);
        let bytes = slice_at(&self.map, absolute_start, absolute_end - absolute_start)?;
        std::str::from_utf8(bytes).map_err(|_| "ANN sidecar key is not UTF-8".to_string())
    }

    pub fn vector(&self, ordinal: usize) -> Result<&[f32], String> {
        if ordinal >= self.header.row_count as usize {
            return Err(format!("ANN ordinal {ordinal} is out of range"));
        }
        let stride = u64::from(self.header.dimensions) * 4;
        let offset = self.header.vectors_offset + ordinal as u64 * stride;
        let bytes = slice_at(&self.map, offset, stride)?;
        let (prefix, floats, suffix) = unsafe { bytes.align_to::<f32>() };
        if !prefix.is_empty()
            || !suffix.is_empty()
            || floats.len() != self.header.dimensions as usize
        {
            return Err("ANN sidecar vector alignment is invalid".to_string());
        }
        Ok(floats)
    }

    pub fn rows(&self) -> usize {
        self.header.row_count as usize
    }
}

#[cfg(test)]
fn write_flat_file(
    path: &Path,
    header: &Header,
    keys: &[String],
    vectors: &[Vec<f32>],
) -> Result<(), String> {
    if keys.len() != vectors.len() || keys.len() != header.row_count as usize {
        return Err("ANN flat writer row count mismatch".to_string());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Failed to create ANN sidecar {}: {error}", path.display()))?;
    file.write_all(&header.encode()?)
        .map_err(|error| format!("Failed to write ANN header: {error}"))?;
    let mut key_offset = 0u64;
    for key in keys {
        file.write_all(&key_offset.to_le_bytes())
            .map_err(|error| format!("Failed to write ANN key offsets: {error}"))?;
        key_offset = key_offset
            .checked_add(key.as_bytes().len() as u64)
            .ok_or("ANN key bytes overflow")?;
    }
    file.write_all(&key_offset.to_le_bytes())
        .map_err(|error| format!("Failed to write ANN key terminator: {error}"))?;
    if key_offset != header.keys_bytes {
        return Err("ANN flat writer key byte count mismatch".to_string());
    }
    for key in keys {
        file.write_all(key.as_bytes())
            .map_err(|error| format!("Failed to write ANN keys: {error}"))?;
    }
    file.seek(SeekFrom::Start(header.vectors_offset))
        .map_err(|error| format!("Failed to align ANN vector section: {error}"))?;
    for vector in vectors {
        if vector.len() != header.dimensions as usize {
            return Err("ANN flat writer vector dimension mismatch".to_string());
        }
        for value in vector {
            file.write_all(&value.to_le_bytes())
                .map_err(|error| format!("Failed to write ANN vector: {error}"))?;
        }
    }
    file.sync_all()
        .map_err(|error| format!("Failed to sync ANN sidecar: {error}"))
}

pub struct FlatFileWriter {
    file: File,
    header: Header,
    next_key_offset: u64,
    keys_written: u64,
    vectors_written: u64,
    vector_bytes: Vec<u8>,
}

impl FlatFileWriter {
    pub fn create(path: &Path, header: Header) -> Result<Self, String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Failed to create ANN sidecar {}: {error}", path.display()))?;
        file.set_len(header.file_len()?)
            .map_err(|error| format!("Failed to size ANN sidecar: {error}"))?;
        file.write_all(&header.encode()?)
            .map_err(|error| format!("Failed to write ANN header: {error}"))?;
        Ok(Self {
            file,
            header,
            next_key_offset: 0,
            keys_written: 0,
            vectors_written: 0,
            vector_bytes: Vec::new(),
        })
    }

    #[cfg(test)]
    pub fn push_key(&mut self, key: &str) -> Result<(), String> {
        self.push_keys(&[key])
    }

    /// Append a batch of keys. Keys are written in one offset-table write and
    /// one contiguous key-payload write, which avoids a seek/write pair for
    /// every row while building a large snapshot.
    pub fn push_keys(&mut self, keys: &[&str]) -> Result<(), String> {
        let batch_len = u64::try_from(keys.len())
            .map_err(|_| "ANN flat writer key batch length overflow".to_string())?;
        if self.keys_written.saturating_add(batch_len) > self.header.row_count {
            return Err("ANN flat writer received too many keys".to_string());
        }
        let mut offsets = Vec::with_capacity(keys.len() * 8);
        let mut payload = Vec::new();
        let payload_bytes = keys
            .iter()
            .map(|key| key.as_bytes().len())
            .try_fold(0usize, |total, len| total.checked_add(len))
            .ok_or("ANN key bytes overflow")?;
        payload.reserve(payload_bytes);
        let mut next_offset = self.next_key_offset;
        for key in keys {
            offsets.extend_from_slice(&next_offset.to_le_bytes());
            next_offset = next_offset
                .checked_add(key.as_bytes().len() as u64)
                .ok_or("ANN key bytes overflow")?;
            payload.extend_from_slice(key.as_bytes());
        }
        self.file
            .seek(SeekFrom::Start(
                self.header.key_offsets_offset + self.keys_written * 8,
            ))
            .and_then(|_| self.file.write_all(&offsets))
            .map_err(|error| format!("Failed to write ANN key offsets: {error}"))?;
        self.file
            .seek(SeekFrom::Start(
                self.header.keys_offset + self.next_key_offset,
            ))
            .and_then(|_| self.file.write_all(&payload))
            .map_err(|error| format!("Failed to write ANN keys: {error}"))?;
        self.next_key_offset = next_offset;
        self.keys_written = self.keys_written.saturating_add(batch_len);
        Ok(())
    }

    #[cfg(test)]
    pub fn push_vector(&mut self, vector: &[f32]) -> Result<(), String> {
        self.push_vectors(&[vector])
    }

    /// Append a batch of already-little-endian vectors in one write. SQLite
    /// stores `derived_embeddings.vector_f32` in exactly this representation,
    /// so a bootstrap pays one page-sized copy instead of a conversion per
    /// scalar.
    pub fn push_vector_bytes(&mut self, vectors: &[&[u8]]) -> Result<(), String> {
        let batch_len = u64::try_from(vectors.len())
            .map_err(|_| "ANN flat writer vector batch length overflow".to_string())?;
        if self.vectors_written.saturating_add(batch_len) > self.header.row_count {
            return Err("ANN flat writer received too many vectors".to_string());
        }
        let dimensions = self.header.dimensions as usize;
        let bytes_per_vector = dimensions
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or("ANN flat writer vector byte length overflow")?;
        let total_bytes = bytes_per_vector
            .checked_mul(vectors.len())
            .ok_or("ANN flat writer vector batch byte length overflow")?;
        self.vector_bytes.clear();
        self.vector_bytes.reserve(total_bytes);
        for vector in vectors {
            if vector.len() != bytes_per_vector {
                return Err("ANN flat writer vector dimension mismatch".to_string());
            }
            self.vector_bytes.extend_from_slice(vector);
        }
        let stride = u64::from(self.header.dimensions) * 4;
        self.file
            .seek(SeekFrom::Start(
                self.header.vectors_offset + self.vectors_written * stride,
            ))
            .and_then(|_| self.file.write_all(&self.vector_bytes))
            .map_err(|error| format!("Failed to write ANN vectors: {error}"))?;
        self.vectors_written = self.vectors_written.saturating_add(batch_len);
        Ok(())
    }

    #[cfg(test)]
    pub fn push_vectors(&mut self, vectors: &[&[f32]]) -> Result<(), String> {
        let encoded = vectors
            .iter()
            .map(|vector| {
                vector
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let encoded = encoded.iter().map(Vec::as_slice).collect::<Vec<_>>();
        self.push_vector_bytes(&encoded)
    }

    pub fn finish(mut self) -> Result<(), String> {
        if self.keys_written != self.header.row_count
            || self.vectors_written != self.header.row_count
            || self.next_key_offset != self.header.keys_bytes
        {
            return Err(format!(
                "ANN flat writer snapshot changed: keys={} vectors={} key_bytes={} expected_rows={} expected_key_bytes={}",
                self.keys_written,
                self.vectors_written,
                self.next_key_offset,
                self.header.row_count,
                self.header.keys_bytes
            ));
        }
        self.file
            .seek(SeekFrom::Start(
                self.header.key_offsets_offset + self.header.row_count * 8,
            ))
            .and_then(|_| self.file.write_all(&self.next_key_offset.to_le_bytes()))
            .map_err(|error| format!("Failed to write ANN key terminator: {error}"))?;
        self.file
            .sync_all()
            .map_err(|error| format!("Failed to sync ANN sidecar: {error}"))
    }
}

fn align_up(value: u64, alignment: u64) -> Result<u64, String> {
    value
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
        .ok_or_else(|| "ANN sidecar alignment overflow".to_string())
}

fn validate_text(name: &str, value: &str, field_bytes: usize) -> Result<(), String> {
    if value.as_bytes().len() > field_bytes.saturating_sub(2)
        || value.as_bytes().len() > MAX_TEXT_BYTES
    {
        return Err(format!("ANN {name} is too long"));
    }
    Ok(())
}

fn put_text(bytes: &mut [u8], offset: usize, width: usize, value: &str) -> Result<(), String> {
    validate_text("header text", value, width)?;
    let len = value.as_bytes().len();
    bytes[offset..offset + 2].copy_from_slice(&(len as u16).to_le_bytes());
    bytes[offset + 2..offset + 2 + len].copy_from_slice(value.as_bytes());
    Ok(())
}

fn get_text(bytes: &[u8], offset: usize, width: usize) -> Result<String, String> {
    let len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    if len > width.saturating_sub(2) {
        return Err("ANN header text length is invalid".to_string());
    }
    std::str::from_utf8(&bytes[offset + 2..offset + 2 + len])
        .map(str::to_string)
        .map_err(|_| "ANN header text is not UTF-8".to_string())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "ANN header is truncated".to_string())?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| "ANN header is truncated".to_string())?;
    Ok(u64::from_le_bytes(raw.try_into().unwrap()))
}

fn read_u64_at(bytes: &[u8], offset: u64) -> Result<u64, String> {
    let offset = usize::try_from(offset).map_err(|_| "ANN offset exceeds address space")?;
    get_u64(bytes, offset)
}

fn slice_at(bytes: &[u8], offset: u64, len: u64) -> Result<&[u8], String> {
    let start = usize::try_from(offset).map_err(|_| "ANN offset exceeds address space")?;
    let len = usize::try_from(len).map_err(|_| "ANN length exceeds address space")?;
    let end = start.checked_add(len).ok_or("ANN slice overflow")?;
    bytes
        .get(start..end)
        .ok_or_else(|| "ANN sidecar is truncated".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_supports_unicode_paths_and_keys() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("中文 索引");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.cpdvec");
        let keys = vec!["图像-a".to_string(), "hash-b".to_string()];
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let key_bytes = keys.iter().map(|key| key.as_bytes().len() as u64).sum();
        let header =
            Header::for_snapshot(7, 11, 2, 2, "clip_image", "clip", "rev", 256, key_bytes).unwrap();
        write_flat_file(&path, &header, &keys, &vectors).unwrap();
        let mapped = MappedFlatIndex::open(&path).unwrap();
        assert_eq!(mapped.key(0).unwrap(), "图像-a");
        assert_eq!(mapped.vector(1).unwrap(), &[0.0, 1.0]);
        assert_eq!(mapped.header.covered_epoch, 11);
    }

    #[test]
    fn batched_writer_preserves_key_and_vector_order_across_pages() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batched.cpdvec");
        let header =
            Header::for_snapshot(8, 12, 3, 2, "clip_image", "clip", "rev", 256, 3).unwrap();
        let mut writer = FlatFileWriter::create(&path, header).unwrap();
        writer.push_keys(&["a", "b"]).unwrap();
        writer.push_vectors(&[&[1.0, 0.0], &[0.0, 1.0]]).unwrap();
        writer.push_keys(&["c"]).unwrap();
        writer.push_vectors(&[&[0.5, 0.5]]).unwrap();
        writer.finish().unwrap();

        let mapped = MappedFlatIndex::open(&path).unwrap();
        assert_eq!(mapped.key(0).unwrap(), "a");
        assert_eq!(mapped.key(1).unwrap(), "b");
        assert_eq!(mapped.key(2).unwrap(), "c");
        assert_eq!(mapped.vector(0).unwrap(), &[1.0, 0.0]);
        assert_eq!(mapped.vector(1).unwrap(), &[0.0, 1.0]);
        assert_eq!(mapped.vector(2).unwrap(), &[0.5, 0.5]);
    }

    #[test]
    fn rejects_a_truncated_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.cpdvec");
        std::fs::write(&path, MAGIC).unwrap();
        assert!(MappedFlatIndex::open(&path).is_err());
    }

    #[test]
    fn ann_probe_accepts_a_different_valid_neighbor() {
        assert!(validate_ann_search_result(&[2], &[0.99], 3).is_ok());
    }

    #[test]
    fn ann_probe_rejects_invalid_results() {
        assert!(validate_ann_search_result(&[], &[], 3).is_err());
        assert!(validate_ann_search_result(&[0], &[0.99], 3).is_err());
        assert!(validate_ann_search_result(&[4], &[0.99], 3).is_err());
        assert!(validate_ann_search_result(&[1], &[f32::NAN], 3).is_err());
        assert!(validate_ann_search_result(&[1], &[], 3).is_err());
    }

    #[test]
    fn ann_recovered_probe_allows_quantization_but_rejects_wrong_vectors() {
        let expected = [0.6, 0.8];
        let recovered = [76.0 / 127.0, 101.0 / 127.0];
        assert!(validate_ann_recovered_probe(&expected, &recovered, 1).unwrap() > 0.99);
        assert!(validate_ann_recovered_probe(&expected, &[0.8, -0.6], 1).is_err());
        assert!(validate_ann_recovered_probe(&expected, &recovered, 0).is_err());
    }

    #[test]
    fn creates_a_sidecar_for_builder_smoke_test() {
        let Some(path) = std::env::var_os("CARBONPAPER_ANN_TEST_SIDECAR") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let vectors = vec![
            // These first two normalized vectors deliberately reproduce the
            // production failure: after I8 quantization, raw inner product can
            // rank key 2 above the key-1 probe even though they are near-equal.
            // The builder self-test must accept that ANN tie break while still
            // proving key 1 itself survived serialization via `Index::get`.
            vec![0.18817376, -0.6422276, 0.4634306, 0.58083254],
            vec![0.1680381, -0.6528201, 0.46298614, 0.57552844],
            vec![0.0, 0.0, 1.0, 0.0],
        ];
        let header = Header::for_snapshot(
            99,
            7,
            keys.len() as u64,
            4,
            "clip_image",
            "clip",
            "rev",
            96,
            3,
        )
        .unwrap();
        write_flat_file(&path, &header, &keys, &vectors).unwrap();
    }
}
