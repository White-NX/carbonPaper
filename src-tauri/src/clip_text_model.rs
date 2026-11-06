//! Deterministic, checksum-pinned extraction of the installed CLIP text graph.
//! Original weights and vector-space provenance remain unchanged.
use crate::ml_protocol::{MlError, MlErrorKind, MlSemanticModel};
use crate::semantic_models::{expected_model_fingerprint, ResolvedSemanticModel};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const TEXT_MODEL_BYTES: u64 = 120_650_049;
const TEXT_MODEL_SHA256: &str = "fb8cf65dfe01a28ca95d375ab7591d0b53c3270aaec55df17239044bd1fa84ac";
pub(crate) const CACHE_DIRECTORY: &str = ".derived-onnx";
const CACHE_FILENAME: &str = "carbonpaper-clip-text-q4-r1.onnx";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub fn prepare(resolved: &ResolvedSemanticModel) -> Result<PathBuf, MlError> {
    if resolved.descriptor.model != MlSemanticModel::ChineseClip {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            "text graph requires Chinese-CLIP",
        ));
    }
    if resolved.model_fingerprint != expected_model_fingerprint(MlSemanticModel::ChineseClip) {
        return Err(MlError::new(
            MlErrorKind::ModelMismatch,
            "CLIP text graph requires the pinned source model",
        ));
    }
    let directory = resolved.base_dir.join(CACHE_DIRECTORY);
    let target = directory.join(CACHE_FILENAME);
    if verify_file(&target, TEXT_MODEL_BYTES, TEXT_MODEL_SHA256) {
        return Ok(target);
    }

    let source = fs::read(&resolved.model_path).map_err(|error| {
        MlError::new(
            MlErrorKind::ModelMissing,
            format!("CLIP source read: {error}"),
        )
    })?;
    if hex::encode(Sha256::digest(&source)) != resolved.model_fingerprint {
        return Err(MlError::new(
            MlErrorKind::ModelMismatch,
            "CLIP source changed after verification",
        ));
    }
    let derived = extract_output(&source, "text_embeds").map_err(|error| {
        MlError::new(
            MlErrorKind::ModelMismatch,
            format!("CLIP text graph: {error}"),
        )
    })?;
    if derived.len() as u64 != TEXT_MODEL_BYTES
        || hex::encode(Sha256::digest(&derived)) != TEXT_MODEL_SHA256
    {
        return Err(MlError::new(
            MlErrorKind::ModelMismatch,
            "CLIP text graph does not match its pinned recipe",
        ));
    }
    install_cache(&directory, &target, &derived).map_err(|error| {
        MlError::new(MlErrorKind::Inference, format!("CLIP text cache: {error}"))
    })?;
    Ok(target)
}

fn verify_file(path: &Path, size: u64, expected: &str) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|metadata| metadata.len() == size) {
        return false;
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => hash.update(&buffer[..size]),
            Err(_) => return false,
        }
    }
    hex::encode(hash.finalize()) == expected
}

fn install_cache(directory: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".{CACHE_FILENAME}.{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        // Another worker may have atomically installed the same verified graph.
        if verify_file(target, TEXT_MODEL_BYTES, TEXT_MODEL_SHA256) {
            return Ok(());
        }
    }
    result
}

#[derive(Clone, Copy)]
struct Field<'a> {
    number: u32,
    value: &'a [u8],
    raw: &'a [u8],
}

fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, &'static str> {
    let mut value = 0;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or("truncated varint")?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err("varint overflow");
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err("varint overflow")
}

/// Retains unknown fields byte-for-byte, including every tensor's raw weights.
fn fields(bytes: &[u8]) -> Result<Vec<Field<'_>>, &'static str> {
    let mut cursor = 0;
    let mut result = Vec::new();
    while cursor < bytes.len() {
        let start = cursor;
        let tag = varint(bytes, &mut cursor)?;
        let number = u32::try_from(tag >> 3).map_err(|_| "field number overflow")?;
        if number == 0 {
            return Err("zero field number");
        }
        let value_start;
        let size = match tag & 7 {
            0 => {
                value_start = cursor;
                varint(bytes, &mut cursor)?;
                cursor - value_start
            }
            1 => {
                value_start = cursor;
                8
            }
            2 => {
                let length = varint(bytes, &mut cursor)?;
                value_start = cursor;
                usize::try_from(length).map_err(|_| "field length overflow")?
            }
            5 => {
                value_start = cursor;
                4
            }
            _ => return Err("unsupported wire type"),
        };
        let end = value_start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("truncated field")?;
        cursor = end;
        result.push(Field {
            number,
            value: &bytes[value_start..end],
            raw: &bytes[start..end],
        });
    }
    Ok(result)
}

fn name<'a>(bytes: &'a [u8], number: u32) -> Result<&'a str, &'static str> {
    let parts = fields(bytes)?;
    let field = parts
        .iter()
        .find(|field| field.number == number)
        .ok_or("missing name")?;
    std::str::from_utf8(field.value).map_err(|_| "invalid tensor name")
}

fn encode_field(number: u32, value: &[u8], target: &mut Vec<u8>) {
    fn put(mut value: u64, target: &mut Vec<u8>) {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            target.push(byte | if value == 0 { 0 } else { 128 });
            if value == 0 {
                break;
            }
        }
    }
    put(u64::from(number) * 8 + 2, target);
    put(value.len() as u64, target);
    target.extend_from_slice(value);
}

pub(crate) fn extract_output(source: &[u8], output: &str) -> Result<Vec<u8>, &'static str> {
    let model = fields(source)?;
    let graphs: Vec<_> = model.iter().filter(|field| field.number == 7).collect();
    if graphs.len() != 1 {
        return Err("expected one graph");
    }
    let graph = fields(graphs[0].value)?;
    if graph
        .iter()
        .filter(|field| field.number == 12)
        .filter_map(|field| name(field.value, 1).ok())
        .filter(|name| *name == output)
        .count()
        != 1
    {
        return Err("requested output is missing or duplicated");
    }
    let nodes: Vec<_> = graph.iter().filter(|field| field.number == 1).collect();
    let mut inputs = Vec::new();
    let mut producers = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let parts = fields(node.value)?;
        let mut node_inputs = Vec::new();
        for field in parts.iter().filter(|field| matches!(field.number, 1 | 2)) {
            let tensor = std::str::from_utf8(field.value).map_err(|_| "invalid node tensor")?;
            if field.number == 1 {
                node_inputs.push(tensor);
            } else if producers.insert(tensor, index).is_some() {
                return Err("duplicate tensor producer");
            }
        }
        inputs.push(node_inputs);
    }
    let mut tensors = HashSet::new();
    let mut selected = HashSet::new();
    let mut pending = vec![output];
    while let Some(tensor) = pending.pop() {
        if tensor.is_empty() || !tensors.insert(tensor) {
            continue;
        }
        if let Some(&index) = producers.get(tensor) {
            selected.insert(index);
            pending.extend(inputs[index].iter().copied());
        }
    }
    let roots: HashSet<_> = tensors
        .iter()
        .copied()
        .filter(|tensor| !producers.contains_key(tensor))
        .collect();
    let graph_inputs: HashSet<_> = graph
        .iter()
        .filter(|field| field.number == 11)
        .map(|field| name(field.value, 1))
        .collect::<Result<_, _>>()?;
    let initializers: HashSet<_> = graph
        .iter()
        .filter(|field| field.number == 5)
        .map(|field| name(field.value, 8))
        .collect::<Result<_, _>>()?;
    if roots
        .iter()
        .any(|tensor| !graph_inputs.contains(tensor) && !initializers.contains(tensor))
    {
        return Err("unresolved graph input");
    }
    let mut reduced = Vec::new();
    let mut node_index = 0;
    for field in &graph {
        let retain = match field.number {
            1 => {
                let keep = selected.contains(&node_index);
                node_index += 1;
                keep
            }
            5 => roots.contains(name(field.value, 8)?),
            11 => roots.contains(name(field.value, 1)?),
            12 => name(field.value, 1)? == output,
            13 => tensors.contains(name(field.value, 1)?),
            _ => true,
        };
        if retain {
            reduced.extend_from_slice(field.raw);
        }
    }
    let mut result = Vec::new();
    for field in model {
        if field.number == 7 {
            encode_field(7, &reduced, &mut result);
        } else {
            result.extend_from_slice(field.raw);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(number: u32, value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_field(number, value.as_bytes(), &mut bytes);
        bytes
    }
    fn node(input: &str, weight: &str, output: &str) -> Vec<u8> {
        [
            string(1, input),
            string(1, weight),
            string(2, output),
            string(4, "Add"),
        ]
        .concat()
    }
    fn fixture() -> Vec<u8> {
        let mut graph = Vec::new();
        for payload in [
            node("text", "shared", "text_embeds"),
            node("pixels", "vision", "image_embeds"),
        ] {
            encode_field(1, &payload, &mut graph);
        }
        for tensor in ["shared", "vision"] {
            encode_field(
                5,
                &[string(8, tensor), string(9, "pinned raw bytes")].concat(),
                &mut graph,
            );
        }
        for input in ["text", "pixels"] {
            encode_field(11, &string(1, input), &mut graph);
        }
        for output in ["text_embeds", "image_embeds"] {
            encode_field(12, &string(1, output), &mut graph);
        }
        let mut model = string(2, "preserved producer metadata");
        encode_field(7, &graph, &mut model);
        model
    }

    #[test]
    fn extraction_preserves_exact_text_weights_and_removes_vision_dependencies() {
        let source = fixture();
        let derived = extract_output(&source, "text_embeds").unwrap();
        let model = fields(&derived).unwrap();
        assert_eq!(model[0].raw, fields(&source).unwrap()[0].raw);
        let graph = fields(model.iter().find(|field| field.number == 7).unwrap().value).unwrap();
        assert_eq!(graph.iter().filter(|field| field.number == 1).count(), 1);
        assert_eq!(
            graph
                .iter()
                .filter(|field| field.number == 5)
                .map(|field| name(field.value, 8).unwrap())
                .collect::<Vec<_>>(),
            vec!["shared"]
        );
        assert_eq!(
            graph
                .iter()
                .filter(|field| field.number == 11)
                .map(|field| name(field.value, 1).unwrap())
                .collect::<Vec<_>>(),
            vec!["text"]
        );
        assert_eq!(
            graph
                .iter()
                .filter(|field| field.number == 12)
                .map(|field| name(field.value, 1).unwrap())
                .collect::<Vec<_>>(),
            vec!["text_embeds"]
        );
        let original_graph = fields(
            fields(&source)
                .unwrap()
                .iter()
                .find(|field| field.number == 7)
                .unwrap()
                .value,
        )
        .unwrap();
        assert_eq!(
            graph.iter().find(|field| field.number == 5).unwrap().raw,
            original_graph
                .iter()
                .find(|field| field.number == 5)
                .unwrap()
                .raw
        );
    }

    #[test]
    fn malformed_or_unresolved_graphs_are_rejected() {
        for bytes in [
            &[0][..],
            &[10, 255][..],
            &[10, 5, 0][..],
            &[15][..],
            &[128; 12][..],
        ] {
            assert!(fields(bytes).is_err());
        }
        assert!(extract_output(&fixture(), "unknown").is_err());
        let mut graph = Vec::new();
        encode_field(
            1,
            &node("missing", "also_missing", "text_embeds"),
            &mut graph,
        );
        encode_field(12, &string(1, "text_embeds"), &mut graph);
        let mut model = Vec::new();
        encode_field(7, &graph, &mut model);
        assert_eq!(
            extract_output(&model, "text_embeds").unwrap_err(),
            "unresolved graph input"
        );
    }

    #[test]
    fn partial_or_corrupt_cached_files_do_not_pass_verification() {
        let directory = std::env::temp_dir().join(format!(
            "carbonpaper-clip-cache-test-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(CACHE_FILENAME);
        assert!(!verify_file(&path, 3, "invalid"));
        fs::write(&path, b"abc").unwrap();
        assert!(!verify_file(&path, 4, "invalid"));
        assert!(!verify_file(&path, 3, "invalid"));
        assert!(verify_file(&path, 3, &hex::encode(Sha256::digest(b"abc"))));
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cache_installation_replaces_a_corrupt_file_and_cleans_up_temporary_files() {
        let directory = std::env::temp_dir().join(format!(
            "carbonpaper-clip-repair-test-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(CACHE_FILENAME);
        fs::write(&path, b"corrupt").unwrap();
        install_cache(&directory, &path, b"replacement").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn preparation_rejects_an_unpinned_source_before_using_its_cache() {
        let resolved = ResolvedSemanticModel {
            descriptor: crate::semantic_models::descriptor(MlSemanticModel::ChineseClip),
            base_dir: PathBuf::new(),
            model_path: PathBuf::new(),
            tokenizer_path: PathBuf::new(),
            preprocessor_path: None,
            model_fingerprint: "unverified".to_string(),
        };
        assert_eq!(
            prepare(&resolved).unwrap_err().kind,
            MlErrorKind::ModelMismatch
        );
    }
}
