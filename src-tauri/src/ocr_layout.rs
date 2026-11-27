//! Versioned, compact OCR records inside the existing authenticated text payload.
//! Legacy UTF-8 stays unchanged. Layout-bearing records share the text's nonce,
//! key, row binding, backup handling, and deletion lifecycle.

use crate::ml_protocol::MlOcrTextLayout;

// Invalid UTF-8 makes this prefix unambiguous for legacy OCR strings.
const MAGIC: &[u8] = b"\xffCPOT";
const VERSION: u8 = 2;
const MAX_TOKENS: usize = 16_384;
const SCALAR_LENGTHS: u8 = 8;
const SHARED_SIZE: u8 = 16;

fn invalid() -> String {
    "Invalid OCR text layout".into()
}

pub(crate) fn validate(text: &str, layout: &MlOcrTextLayout) -> Result<(), String> {
    let count = layout.centers.len();
    if count == 0
        || count > MAX_TOKENS
        || count != layout.text_lengths.len()
        || layout.source_size.contains(&0)
        || layout.valid_width == 0
        || layout.valid_width > layout.input_width
        || layout.time_steps == 0
        || layout.time_steps > u32::MAX / 2
        || layout.matrix.iter().flatten().any(|v| !v.is_finite())
    {
        return Err(invalid());
    }
    let m = layout.matrix.map(|row| row.map(f64::from));
    let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let denominators = [
        m[2][2],
        m[2][0] + m[2][2],
        m[2][1] + m[2][2],
        m[2][0] + m[2][1] + m[2][2],
    ];
    if determinant.abs() < 1e-12
        || !(denominators.iter().all(|v| *v > 1e-12) || denominators.iter().all(|v| *v < -1e-12))
    {
        return Err(invalid());
    }
    let mut previous = 0;
    let mut offset = 0usize;
    for (&center, &length) in layout.centers.iter().zip(&layout.text_lengths) {
        if center <= previous || center > layout.time_steps * 2 || length == 0 {
            return Err(invalid());
        }
        previous = center;
        offset = offset.checked_add(length as usize).ok_or_else(invalid)?;
        if !text.is_char_boundary(offset) {
            return Err(invalid());
        }
    }
    if offset != text.len() {
        return Err(invalid());
    }
    Ok(())
}

fn varint(output: &mut Vec<u8>, mut value: u32) {
    while value >= 128 {
        output.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    output.push(value as u8);
}

#[cfg(test)]
pub(crate) fn encode(text: &str, layout: Option<&MlOcrTextLayout>) -> Result<Vec<u8>, String> {
    let Some(layout) = layout else {
        return Ok(text.as_bytes().to_vec());
    };
    let mut output = Vec::with_capacity(text.len() + 80 + layout.centers.len() * 2);
    output.extend_from_slice(MAGIC);
    output.push(VERSION);
    write_layout_body(&mut output, text, layout, None)?;
    Ok(output)
}

fn matrix_encoding(matrix: &[[f32; 3]; 3]) -> (u8, &'static [usize]) {
    let values = matrix.as_flattened();
    let same = |i: usize, value: f32| values[i].to_bits() == value.to_bits();
    let affine = same(6, 0.0) && same(7, 0.0) && same(8, 1.0);
    if affine && same(1, 0.0) && same(3, 0.0) {
        (0, &[0, 2, 4, 5])
    } else if affine && same(0, 0.0) && same(4, 0.0) {
        (1, &[1, 2, 3, 5])
    } else if affine {
        (2, &[0, 1, 2, 3, 4, 5])
    } else if same(8, 1.0) {
        (3, &[0, 1, 2, 3, 4, 5, 6, 7])
    } else {
        (4, &[0, 1, 2, 3, 4, 5, 6, 7, 8])
    }
}

fn write_layout_body(
    output: &mut Vec<u8>,
    text: &str,
    layout: &MlOcrTextLayout,
    source_size: Option<[u32; 2]>,
) -> Result<(), String> {
    validate(text, layout)?;
    varint(output, text.len().try_into().map_err(|_| invalid())?);
    output.extend_from_slice(text.as_bytes());
    let scalar_lengths = text
        .chars()
        .map(|ch| ch.len_utf8() as u32)
        .eq(layout.text_lengths.iter().copied());
    let shared_size = source_size == Some(layout.source_size);
    let (kind, indices) = matrix_encoding(&layout.matrix);
    output.push(
        kind | if scalar_lengths { SCALAR_LENGTHS } else { 0 }
            | if shared_size { SHARED_SIZE } else { 0 },
    );
    for &index in indices {
        output.extend_from_slice(&layout.matrix.as_flattened()[index].to_le_bytes());
    }
    if !shared_size {
        varint(output, layout.source_size[0]);
        varint(output, layout.source_size[1]);
    }
    for value in [layout.input_width, layout.valid_width, layout.time_steps] {
        varint(output, value);
    }
    if !scalar_lengths {
        varint(output, layout.centers.len() as u32);
    }
    let mut previous = 0;
    for &center in &layout.centers {
        varint(output, center - previous);
        previous = center;
    }
    if !scalar_lengths {
        for &length in &layout.text_lengths {
            varint(output, length);
        }
    }
    Ok(())
}

/// Blocks already identify their format, so layout records omit the repeated
/// magic/version and share the screenshot dimensions. Plain text has no layout.
pub(crate) fn encode_block_record(
    text: &str,
    layout: Option<&MlOcrTextLayout>,
    source_size: [u32; 2],
) -> Result<Vec<u8>, String> {
    let mut output =
        Vec::with_capacity(text.len() + 80 + layout.map_or(0, |layout| layout.centers.len() * 2));
    output.push(u8::from(layout.is_some()));
    if let Some(layout) = layout {
        write_layout_body(&mut output, text, layout, Some(source_size))?;
    } else {
        output.extend_from_slice(text.as_bytes());
    }
    Ok(output)
}

pub(crate) fn decode_block_text(bytes: &[u8]) -> Result<&str, String> {
    match bytes.first() {
        Some(0) => std::str::from_utf8(&bytes[1..]).map_err(|_| invalid()),
        Some(1) => {
            let mut reader = Reader { bytes, offset: 1 };
            let length = reader.integer()? as usize;
            std::str::from_utf8(reader.take(length)?).map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }
}

pub(crate) fn decode_block_record(
    bytes: &[u8],
    source_size: [u32; 2],
) -> Result<(String, Option<MlOcrTextLayout>), String> {
    match bytes.first() {
        Some(0) => Ok((decode_block_text(bytes)?.into(), None)),
        Some(1) => decode_v2(&mut Reader { bytes, offset: 1 }, Some(source_size)),
        _ => Err(invalid()),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(count).ok_or_else(invalid)?;
        let bytes = self.bytes.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        Ok(bytes)
    }

    fn integer(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for shift in (0..=28).step_by(7) {
            let byte = self.take(1)?[0];
            if shift == 28 && byte > 15 {
                return Err(invalid());
            }
            value |= ((byte & 127) as u32) << shift;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(invalid())
    }
}

/// Text-only readers avoid parsing and allocating the per-token layout.
pub(crate) fn decode_text(bytes: &[u8]) -> Result<&str, String> {
    if !bytes.starts_with(MAGIC) {
        return std::str::from_utf8(bytes).map_err(|_| invalid());
    }
    let mut reader = Reader {
        bytes,
        offset: MAGIC.len(),
    };
    if reader.take(1)?[0] != VERSION {
        return Err(invalid());
    }
    let length = reader.integer()? as usize;
    std::str::from_utf8(reader.take(length)?).map_err(|_| invalid())
}

pub(crate) fn decode(bytes: &[u8]) -> Result<(String, Option<MlOcrTextLayout>), String> {
    if !bytes.starts_with(MAGIC) {
        return String::from_utf8(bytes.to_vec())
            .map(|text| (text, None))
            .map_err(|_| invalid());
    }
    let mut reader = Reader {
        bytes,
        offset: MAGIC.len(),
    };
    match reader.take(1)?[0] {
        VERSION => decode_v2(&mut reader, None),
        _ => Err(invalid()),
    }
}

fn decode_v2(
    reader: &mut Reader<'_>,
    shared_size: Option<[u32; 2]>,
) -> Result<(String, Option<MlOcrTextLayout>), String> {
    let length = reader.integer()? as usize;
    let text = std::str::from_utf8(reader.take(length)?)
        .map_err(|_| invalid())?
        .to_owned();
    let flags = reader.take(1)?[0];
    if flags & !31 != 0 {
        return Err(invalid());
    }
    let kind = flags & 7;
    let indices: &[usize] = match kind {
        0 => &[0, 2, 4, 5],
        1 => &[1, 2, 3, 5],
        2 => &[0, 1, 2, 3, 4, 5],
        3 => &[0, 1, 2, 3, 4, 5, 6, 7],
        4 => &[0, 1, 2, 3, 4, 5, 6, 7, 8],
        _ => return Err(invalid()),
    };
    let mut matrix = [[0.0; 3]; 3];
    matrix[2][2] = 1.0;
    for &index in indices {
        matrix.as_flattened_mut()[index] = f32::from_le_bytes(reader.take(4)?.try_into().unwrap());
    }
    let source_size = if flags & SHARED_SIZE != 0 {
        shared_size.ok_or_else(invalid)?
    } else {
        [reader.integer()?, reader.integer()?]
    };
    let input_width = reader.integer()?;
    let valid_width = reader.integer()?;
    let time_steps = reader.integer()?;
    let scalar_lengths = flags & SCALAR_LENGTHS != 0;
    let count = if scalar_lengths {
        text.chars().count()
    } else {
        reader.integer()? as usize
    };
    if count == 0 || count > MAX_TOKENS || count > text.len() {
        return Err(invalid());
    }
    let mut centers = Vec::with_capacity(count);
    let mut center = 0u32;
    for _ in 0..count {
        center = center.checked_add(reader.integer()?).ok_or_else(invalid)?;
        centers.push(center);
    }
    let text_lengths = if scalar_lengths {
        text.chars().map(|ch| ch.len_utf8() as u32).collect()
    } else {
        (0..count)
            .map(|_| reader.integer())
            .collect::<Result<_, _>>()?
    };
    if reader.offset != reader.bytes.len() {
        return Err(invalid());
    }
    let layout = MlOcrTextLayout {
        matrix,
        source_size,
        input_width,
        valid_width,
        time_steps,
        centers,
        text_lengths,
    };
    validate(&text, &layout)?;
    Ok((text, Some(layout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> MlOcrTextLayout {
        MlOcrTextLayout {
            matrix: [[90.0, 4.0, 5.0], [3.0, 18.0, 2.0], [0.05, 0.02, 1.0]],
            source_size: [120, 50],
            input_width: 20,
            valid_width: 18,
            time_steps: 5,
            centers: vec![1, 3, 6, 8],
            text_lengths: vec![3, 1, 1, 8],
        }
    }

    #[test]
    fn mixed_utf8_and_perspective_layout_round_trip_without_quantization() {
        let text = "中 a🇨🇳";
        let expected = layout();
        let bytes = encode(text, Some(&expected)).unwrap();
        assert_eq!(decode_text(&bytes).unwrap(), text);
        assert_eq!(decode(&bytes).unwrap(), (text.into(), Some(expected)));
        assert_eq!(encode(text, None).unwrap(), text.as_bytes());
        assert_eq!(decode(text.as_bytes()).unwrap(), (text.into(), None));
    }

    #[test]
    fn a_thousand_tokens_add_less_than_2200_bytes_to_the_existing_text_payload() {
        let text = "a".repeat(1000);
        let layout = MlOcrTextLayout {
            matrix: [[3999.0, 0.0, 0.0], [0.0, 19.0, 0.0], [0.0, 0.0, 1.0]],
            source_size: [4000, 20],
            input_width: 4000,
            valid_width: 4000,
            time_steps: 1000,
            centers: (0..1000).map(|i| 2 * i + 1).collect(),
            text_lengths: vec![1; 1000],
        };
        let bytes = encode(&text, Some(&layout)).unwrap();
        assert!(bytes.len() - text.len() < 2200);
        assert_eq!(decode(&bytes).unwrap(), (text, Some(layout)));
    }

    #[test]
    fn rejects_split_utf8_ranges_unsorted_centers_and_invalid_transforms() {
        let text = "中 a🇨🇳";
        let mut invalid_range = layout();
        invalid_range.text_lengths = vec![1, 3, 1, 8];
        assert!(encode(text, Some(&invalid_range)).is_err());
        let mut invalid_centers = layout();
        invalid_centers.centers[2] = 3;
        assert!(encode(text, Some(&invalid_centers)).is_err());
        let mut singular = layout();
        singular.matrix[1] = singular.matrix[0];
        assert!(encode(text, Some(&singular)).is_err());
        let mut pole = layout();
        pole.matrix[2] = [-2.0, 0.0, 1.0];
        assert!(encode(text, Some(&pole)).is_err());
    }

    #[test]
    fn rejects_truncated_unknown_and_oversized_records() {
        let bytes = encode("中 a🇨🇳", Some(&layout())).unwrap();
        for length in MAGIC.len()..bytes.len() {
            assert!(decode(&bytes[..length]).is_err());
        }
        let mut unknown = bytes.clone();
        unknown[MAGIC.len()] = VERSION + 1;
        assert!(decode(&unknown).is_err());
        let mut overflow = MAGIC.to_vec();
        overflow.extend_from_slice(&[VERSION, 255, 255, 255, 255, 31]);
        assert!(decode(&overflow).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }

    #[test]
    fn compact_matrix_modes_preserve_float_bits_and_shared_dimensions() {
        let matrices = [
            [[90.0, 0.0, 5.0], [0.0, 18.0, 2.0], [0.0, 0.0, 1.0]],
            [[0.0, -18.0, 23.0], [90.0, 0.0, 2.0], [0.0, 0.0, 1.0]],
            [[90.0, 4.0, 5.0], [3.0, 18.0, 2.0], [0.0, 0.0, 1.0]],
            layout().matrix,
            [[90.0, 4.0, 5.0], [3.0, 18.0, 2.0], [0.05, 0.02, 2.0]],
            [[90.0, -0.0, 5.0], [0.0, 18.0, 2.0], [-0.0, 0.0, 1.0]],
        ];
        for matrix in matrices {
            let expected = MlOcrTextLayout { matrix, ..layout() };
            let encoded =
                encode_block_record("中 a🇨🇳", Some(&expected), expected.source_size).unwrap();
            assert_eq!(decode_block_text(&encoded).unwrap(), "中 a🇨🇳");
            let (_, actual) = decode_block_record(&encoded, expected.source_size).unwrap();
            let actual = actual.unwrap();
            assert_eq!(
                actual.matrix.map(|r| r.map(f32::to_bits)),
                matrix.map(|r| r.map(f32::to_bits))
            );
            assert_eq!(actual.text_lengths, expected.text_lengths);
            assert_eq!(actual.centers, expected.centers);
        }
    }

    #[test]
    fn common_scalar_tokens_use_one_position_byte_and_keep_size_overrides() {
        let text = "中a ".repeat(300);
        let expected = MlOcrTextLayout {
            matrix: [[3000.0, 0.0, 5.0], [0.0, 18.0, 2.0], [0.0, 0.0, 1.0]],
            source_size: [4000, 1000],
            input_width: 4000,
            valid_width: 4000,
            time_steps: 1000,
            centers: (0..900).map(|i| i * 2 + 1).collect(),
            text_lengths: text.chars().map(|ch| ch.len_utf8() as u32).collect(),
        };
        let bytes = encode(&text, Some(&expected)).unwrap();
        assert!(bytes.len() - text.len() < 950);
        let block = encode_block_record(&text, Some(&expected), [1920, 1080]).unwrap();
        assert_eq!(
            decode_block_record(&block, [1920, 1080]).unwrap(),
            (text, Some(expected))
        );
    }

    #[test]
    fn unpublished_v1_requires_offline_conversion() {
        let expected = layout();
        let text = "中 a🇨🇳";
        let mut legacy = MAGIC.to_vec();
        legacy.push(1);
        varint(&mut legacy, text.len() as u32);
        legacy.extend_from_slice(text.as_bytes());
        for value in expected.matrix.as_flattened() {
            legacy.extend_from_slice(&value.to_le_bytes());
        }
        for value in [120, 50, 20, 18, 5, 4] {
            varint(&mut legacy, value);
        }
        let mut previous = 0;
        for (&center, &length) in expected.centers.iter().zip(&expected.text_lengths) {
            varint(&mut legacy, center - previous);
            varint(&mut legacy, length);
            previous = center;
        }
        assert!(decode(&legacy).is_err());
        assert!(decode_text(&legacy).is_err());
    }
}
