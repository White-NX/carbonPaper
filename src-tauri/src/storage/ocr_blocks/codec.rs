//! Bounded, lossless blocks. Compression is applied before authenticated
//! encryption; the decrypted cache owns zeroizing buffers and a numeric directory.

use crate::ml_protocol::MlOcrTextLayout;
use std::ops::Range;
use zeroize::Zeroizing;

pub(crate) const TARGET_BYTES: usize = 128 * 1024;
pub(crate) const MAX_BYTES: usize = 512 * 1024;
pub(crate) const MAX_ROWS: usize = 512;
const MAGIC: &[u8; 8] = b"CPORBLK2";

fn invalid() -> String {
    "Invalid OCR block".into()
}

pub(crate) struct Record {
    pub id: i64,
    pub text: String,
    pub text_layout: Option<MlOcrTextLayout>,
    pub confidence: f64,
    pub box_coords: [[f64; 2]; 4],
}

pub(crate) struct Metadata {
    pub confidence: f64,
    pub box_coords: [[f64; 2]; 4],
}

struct Slot {
    id: i64,
    bytes: Range<usize>,
}

pub(crate) struct Decoded {
    data: Zeroizing<Vec<u8>>,
    source_size: [u32; 2],
    slots: Vec<Slot>,
}

pub(crate) fn varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        output.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    output.push(value as u8);
}

fn signed(output: &mut Vec<u8>, value: i64) {
    varint(output, ((value as u64) << 1) ^ ((value >> 63) as u64));
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(n).ok_or_else(invalid)?;
        let result = self.data.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        Ok(result)
    }
    fn integer(&mut self) -> Result<u64, String> {
        let mut result = 0u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.take(1)?[0];
            if shift == 63 && byte > 1 {
                return Err(invalid());
            }
            result |= ((byte & 127) as u64) << shift;
            if byte < 128 {
                return Ok(result);
            }
        }
        Err(invalid())
    }
    fn signed(&mut self) -> Result<i64, String> {
        let value = self.integer()?;
        Ok((value >> 1) as i64 ^ -((value & 1) as i64))
    }
    fn float(&mut self) -> Result<f64, String> {
        let value = f64::from_le_bytes(self.take(8)?.try_into().unwrap());
        if !value.is_finite() {
            return Err(invalid());
        }
        Ok(value)
    }
}

fn integer_coordinate(value: f64) -> Option<i64> {
    if value >= i64::MIN as f64 && value < i64::MAX as f64 {
        let integer = value as i64;
        if (integer as f64).to_bits() == value.to_bits() {
            return Some(integer);
        }
    }
    None
}

pub(crate) fn record_bytes(
    record: &Record,
    source_size: [u32; 2],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if record.id <= 0
        || !record.confidence.is_finite()
        || record.box_coords.iter().flatten().any(|v| !v.is_finite())
        || record.text.chars().count() > 16_384
    {
        return Err(invalid());
    }
    let text = Zeroizing::new(crate::ocr_layout::encode_block_record(
        &record.text,
        record.text_layout.as_ref(),
        source_size,
    )?);
    let mut output = Zeroizing::new(Vec::with_capacity(text.len() + 80));
    varint(&mut output, text.len() as u64);
    output.extend_from_slice(&text);
    output.extend_from_slice(&record.confidence.to_le_bytes());
    let ints = record.box_coords.map(|r| r.map(integer_coordinate));
    if ints.iter().flatten().all(Option::is_some) {
        let q = ints.map(|r| r.map(Option::unwrap));
        let rectangle =
            q[0][1] == q[1][1] && q[2][1] == q[3][1] && q[0][0] == q[3][0] && q[1][0] == q[2][0];
        let extent = q[1][0]
            .checked_sub(q[0][0])
            .zip(q[3][1].checked_sub(q[0][1]));
        if let Some((width, height)) = extent.filter(|_| rectangle) {
            output.push(0);
            for value in [q[0][0], q[0][1], width, height] {
                signed(&mut output, value);
            }
        } else {
            output.push(1);
            for value in q.as_flattened() {
                signed(&mut output, *value);
            }
        }
    } else {
        output.push(2);
        for value in record.box_coords.as_flattened() {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    if output.len() > MAX_BYTES - 64 {
        return Err("OCR record exceeds the block limit".into());
    }
    Ok(output)
}

pub(crate) fn encode(
    records: &[(i64, Zeroizing<Vec<u8>>)],
    source_size: [u32; 2],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if records.is_empty() || records.len() > MAX_ROWS {
        return Err(invalid());
    }
    let mut data = Zeroizing::new(MAGIC.to_vec());
    for value in source_size {
        varint(&mut data, value.into());
    }
    varint(&mut data, records.len() as u64);
    let mut previous = 0i64;
    for (id, bytes) in records {
        let delta = id
            .checked_sub(previous)
            .filter(|n| *n > 0)
            .ok_or_else(invalid)?;
        varint(&mut data, delta as u64);
        varint(&mut data, bytes.len() as u64);
        data.extend_from_slice(bytes);
        previous = *id;
    }
    if data.len() > MAX_BYTES {
        return Err("OCR block exceeds the storage limit".into());
    }
    let compressed =
        Zeroizing::new(zstd::bulk::compress(&data, 3).map_err(|_| "Failed to compress OCR block")?);
    let use_compressed = compressed.len() < data.len();
    let mut output = Zeroizing::new(Vec::with_capacity(
        5 + if use_compressed {
            compressed.len()
        } else {
            data.len()
        },
    ));
    output.push(u8::from(use_compressed));
    output.extend_from_slice(&(data.len() as u32).to_le_bytes());
    output.extend_from_slice(if use_compressed { &compressed } else { &data });
    Ok(output)
}

impl Decoded {
    pub(crate) fn open(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 5 || bytes.len() > MAX_BYTES + 5 {
            return Err(invalid());
        }
        let length = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        if !(MAGIC.len()..=MAX_BYTES).contains(&length) {
            return Err(invalid());
        }
        let data = Zeroizing::new(match bytes[0] {
            0 => {
                if bytes.len() != length + 5 {
                    return Err(invalid());
                }
                bytes[5..].to_vec()
            }
            1 => zstd::bulk::decompress(&bytes[5..], length).map_err(|_| invalid())?,
            _ => return Err(invalid()),
        });
        if data.len() != length {
            return Err(invalid());
        }
        let mut reader = Reader {
            data: &data,
            offset: 0,
        };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(invalid());
        }
        let source_size = [
            u32::try_from(reader.integer()?).map_err(|_| invalid())?,
            u32::try_from(reader.integer()?).map_err(|_| invalid())?,
        ];
        let count = usize::try_from(reader.integer()?).map_err(|_| invalid())?;
        if count == 0 || count > MAX_ROWS {
            return Err(invalid());
        }
        let mut slots = Vec::with_capacity(count);
        let mut previous = 0i64;
        for _ in 0..count {
            let delta = i64::try_from(reader.integer()?).map_err(|_| invalid())?;
            if delta <= 0 {
                return Err(invalid());
            }
            let id = previous.checked_add(delta).ok_or_else(invalid)?;
            let length = usize::try_from(reader.integer()?).map_err(|_| invalid())?;
            let start = reader.offset;
            reader.take(length)?;
            slots.push(Slot {
                id,
                bytes: start..reader.offset,
            });
            previous = id;
        }
        if reader.offset != data.len() {
            return Err(invalid());
        }
        Ok(Self {
            data,
            source_size,
            slots,
        })
    }
    pub(crate) fn size(&self) -> usize {
        self.data.len()
    }
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
    pub(crate) fn bounds(&self) -> (i64, i64) {
        (
            self.slots.first().unwrap().id,
            self.slots.last().unwrap().id,
        )
    }
    fn body(&self, slot: usize, id: i64) -> Result<Reader<'_>, String> {
        let slot = self
            .slots
            .get(slot)
            .filter(|s| s.id == id)
            .ok_or_else(invalid)?;
        Ok(Reader {
            data: &self.data[slot.bytes.clone()],
            offset: 0,
        })
    }
    fn text_bytes<'a>(&'a self, reader: &mut Reader<'a>) -> Result<&'a [u8], String> {
        let length = usize::try_from(reader.integer()?).map_err(|_| invalid())?;
        reader.take(length)
    }
    pub(crate) fn text(&self, slot: usize, id: i64) -> Result<&str, String> {
        let mut reader = self.body(slot, id)?;
        let text = crate::ocr_layout::decode_block_text(self.text_bytes(&mut reader)?)?;
        if text.chars().count() > 16_384 {
            return Err(invalid());
        }
        Ok(text)
    }
    pub(crate) fn record(&self, slot: usize, id: i64) -> Result<Record, String> {
        let mut reader = self.body(slot, id)?;
        let (text, text_layout) = crate::ocr_layout::decode_block_record(
            self.text_bytes(&mut reader)?,
            self.source_size,
        )?;
        if text.chars().count() > 16_384 {
            return Err(invalid());
        }
        let metadata = Self::metadata(&mut reader)?;
        Ok(Record {
            id,
            text,
            text_layout,
            confidence: metadata.confidence,
            box_coords: metadata.box_coords,
        })
    }
    pub(crate) fn metadata_for(&self, slot: usize, id: i64) -> Result<Metadata, String> {
        let mut reader = self.body(slot, id)?;
        self.text_bytes(&mut reader)?;
        Self::metadata(&mut reader)
    }
    fn metadata(reader: &mut Reader<'_>) -> Result<Metadata, String> {
        let confidence = reader.float()?;
        let mut box_coords = [[0.0; 2]; 4];
        match reader.take(1)?[0] {
            0 => {
                let x = reader.signed()?;
                let y = reader.signed()?;
                let width = reader.signed()?;
                let height = reader.signed()?;
                let x2 = x.checked_add(width).ok_or_else(invalid)?;
                let y2 = y.checked_add(height).ok_or_else(invalid)?;
                box_coords = [
                    [x as f64, y as f64],
                    [x2 as f64, y as f64],
                    [x2 as f64, y2 as f64],
                    [x as f64, y2 as f64],
                ];
            }
            1 => {
                for value in box_coords.as_flattened_mut() {
                    *value = reader.signed()? as f64;
                }
            }
            2 => {
                for value in box_coords.as_flattened_mut() {
                    *value = reader.float()?;
                }
            }
            _ => return Err(invalid()),
        }
        if reader.offset != reader.data.len() {
            return Err(invalid());
        }
        Ok(Metadata {
            confidence,
            box_coords,
        })
    }
}
