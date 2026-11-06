//! Isolated synthetic measurements of the actual envelope and flat-file code.
//! This executable has no Tauri/credential-manager code or application DB access.
extern crate self as carbonpaper_app_bound;

#[allow(dead_code)]
#[path = "../../../src-tauri/src/ann_format.rs"]
mod ann_format;
#[path = "../../../src-tauri/app-bound/src/payload.rs"]
pub mod payload;

use ann_format::{FlatFileWriter, Header, MappedFlatIndex};
use memmap2::{Mmap, MmapOptions};
use payload::CipherKey;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroizing;

struct Options {
    rows: usize,
    samples: usize,
    candidates: usize,
    graph_mib: usize,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        rows: 100_000,
        samples: 5,
        candidates: 512,
        graph_mib: 32,
    };
    let mut args = std::env::args().skip(1);
    while let Some(name) = args.next() {
        let value: usize = args
            .next()
            .ok_or_else(|| format!("Missing value for {name}"))?
            .parse()
            .map_err(|_| format!("Invalid value for {name}"))?;
        match name.as_str() {
            "--rows" => options.rows = value,
            "--samples" => options.samples = value,
            "--candidates" => options.candidates = value,
            "--graph-mib" => options.graph_mib = value,
            _ => return Err(format!("Unknown option {name}")),
        }
    }
    if !(1..=200_000).contains(&options.rows)
        || !(3..=25).contains(&options.samples)
        || options.candidates == 0
        || options.candidates > options.rows
        || !(1..=128).contains(&options.graph_mib)
    {
        return Err(
            "Expected 1..200000 rows, 3..25 samples, 1..rows candidates, 1..128 graph MiB".into(),
        );
    }
    Ok(options)
}

fn measure(
    samples: usize,
    mut prepare: impl FnMut(),
    mut operation: impl FnMut() -> Result<f64, String>,
) -> Result<Value, String> {
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        prepare();
        let started = Instant::now();
        black_box(operation()?);
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let mut sorted = timings.clone();
    sorted.sort_by(f64::total_cmp);
    Ok(json!({
        "median_ms": sorted[sorted.len() / 2],
        "min_ms": sorted[0],
        "max_ms": sorted[sorted.len() - 1],
        "samples_ms": timings,
    }))
}

fn dot(query: &[f32], vector: &[f32]) -> f64 {
    let mut sum = [0.0f32; 4];
    for (left, right) in query.chunks_exact(4).zip(vector.chunks_exact(4)) {
        for index in 0..4 {
            sum[index] += left[index] * right[index];
        }
    }
    f64::from(sum.into_iter().sum::<f32>())
}

fn synthetic_vector(row: usize, dimensions: usize) -> Zeroizing<Vec<u8>> {
    let mut random = row as u64 + 1;
    let mut vector = vec![0.0f32; dimensions];
    for value in &mut vector {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        *value = (random as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    Zeroizing::new(
        vector
            .iter()
            .flat_map(|value| (value / norm).to_le_bytes())
            .collect(),
    )
}

fn fixture(
    directory: &Path,
    rows: usize,
    dimensions: usize,
    key: Arc<CipherKey>,
) -> Result<(MappedFlatIndex, Mmap), Box<dyn std::error::Error>> {
    let flat_path = directory.join("synthetic-encrypted.cpdvec");
    let plain_path = directory.join("synthetic-plaintext-floats.bin");
    let header = Header::for_snapshot(
        1,
        1,
        rows as u64,
        dimensions as u32,
        "clip_image",
        "synthetic",
        "1",
        96,
        rows as u64 * 16,
        &key,
        "02".repeat(16).as_str(),
    )?;
    let mut writer = FlatFileWriter::create(&flat_path, header, key.clone())?;
    let mut plain = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&plain_path)?,
    );
    for start in (0..rows).step_by(256) {
        let end = (start + 256).min(rows);
        let keys = (start..end)
            .map(|row| format!("{row:016x}"))
            .collect::<Vec<_>>();
        let vectors = (start..end)
            .map(|row| synthetic_vector(row, dimensions))
            .collect::<Vec<_>>();
        writer.push_keys(&keys.iter().map(String::as_str).collect::<Vec<_>>())?;
        writer.push_vector_bytes(
            &vectors
                .iter()
                .map(|bytes| bytes.as_slice())
                .collect::<Vec<_>>(),
        )?;
        for vector in vectors {
            plain.write_all(&vector)?;
        }
    }
    writer.finish()?;
    plain.flush()?;
    plain.get_ref().sync_all()?;
    drop(plain);
    let file = File::open(plain_path)?;
    // SAFETY: this unique temporary fixture is complete and immutable while
    // the mapping is live; the tool never exposes it to another writer.
    let plain = unsafe { MmapOptions::new().map(&file) }?;
    Ok((MappedFlatIndex::open(&flat_path, key)?, plain))
}

fn scenario(options: &Options, dimensions: usize) -> Result<Value, Box<dyn std::error::Error>> {
    let directory = tempfile::Builder::new()
        .prefix("carbonpaper-synthetic-crypto-")
        .tempdir()?;
    // An explicit, public fixture key. The tool never opens a Windows user key.
    let key = Arc::new(CipherKey::new(
        "01".repeat(16),
        "synthetic-benchmark".into(),
        "ann".into(),
        &[7; 32],
    )?);
    let (flat, plain) = fixture(directory.path(), options.rows, dimensions, key.clone())?;
    let floats = options.rows * dimensions;
    if plain.len() != floats * 4
        || plain.as_ptr().align_offset(std::mem::align_of::<f32>()) != 0
        || !cfg!(target_endian = "little")
    {
        return Err("Plain fixture does not match this platform's f32 layout".into());
    }
    // SAFETY: the read-only mapping is aligned, exactly `floats * 4` bytes long,
    // little endian, and was populated solely with valid synthetic f32 values.
    let plain_vectors = unsafe { std::slice::from_raw_parts(plain.as_ptr().cast::<f32>(), floats) };
    let query = vec![1.0 / (dimensions as f32).sqrt(); dimensions];
    let candidates = (0..options.candidates)
        .map(|index| index * options.rows / options.candidates)
        .collect::<Vec<_>>();
    for ordinal in [0, options.rows - 1] {
        let vector = flat.vector_uncached(ordinal)?;
        assert_eq!(
            &*vector,
            &plain_vectors[ordinal * dimensions..(ordinal + 1) * dimensions]
        );
    }
    let scan_plain = measure(
        options.samples,
        || {},
        || {
            Ok((0..options.rows)
                .map(|row| {
                    dot(
                        &query,
                        &plain_vectors[row * dimensions..(row + 1) * dimensions],
                    )
                })
                .sum())
        },
    )?;
    let scan_encrypted = measure(
        options.samples,
        || {},
        || {
            let mut score = 0.0;
            for row in 0..options.rows {
                score += dot(&query, &flat.vector_uncached(row)?);
            }
            Ok(score)
        },
    )?;
    let candidates_plain = measure(
        options.samples,
        || {},
        || {
            Ok(candidates
                .iter()
                .map(|&row| {
                    dot(
                        &query,
                        &plain_vectors[row * dimensions..(row + 1) * dimensions],
                    )
                })
                .sum())
        },
    )?;
    let candidates_cold = measure(
        options.samples,
        || flat.clear_vector_cache(),
        || {
            let mut score = 0.0;
            for &row in &candidates {
                score += dot(&query, &flat.vector(row)?);
            }
            Ok(score)
        },
    )?;
    let candidates_hot = measure(
        options.samples,
        || {},
        || {
            let mut score = 0.0;
            for _ in 0..100 {
                for &row in &candidates {
                    score += dot(&query, &flat.vector(row)?);
                }
            }
            Ok(score)
        },
    )?;
    let graph = Zeroizing::new(vec![0x5a; options.graph_mib * 1024 * 1024]);
    let graph_seal = measure(
        options.samples,
        || {},
        || Ok(black_box(key.seal("synthetic:graph", &graph)?).len() as f64),
    )?;
    let graph_encrypted = key.seal("synthetic:graph", &graph)?;
    let graph_open = measure(
        options.samples,
        || {},
        || {
            let decrypted = key.open("synthetic:graph", &graph_encrypted)?;
            Ok(black_box(&decrypted).len() as f64)
        },
    )?;
    Ok(json!({
        "dimensions": dimensions, "rows": options.rows,
        "encrypted_flat_bytes": flat.header.file_len()?,
        "float_payload_bytes": floats * 4,
        "envelope_overhead_bytes_per_vector": payload::OVERHEAD,
        "full_scan_plain_mmap": scan_plain, "full_scan_encrypted_uncached": scan_encrypted,
        "candidates": options.candidates, "candidates_plain_mmap": candidates_plain,
        "candidates_encrypted_cache_cold": candidates_cold,
        "candidates_encrypted_cache_hot_100_batches": candidates_hot,
        "graph_mib": options.graph_mib, "graph_encrypt": graph_seal, "graph_decrypt": graph_open,
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err("Use --release for this measurement".into());
    }
    let options = options()?;
    let results = [384, 512]
        .into_iter()
        .map(|dimensions| scenario(&options, dimensions))
        .collect::<Result<Vec<_>, _>>()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture": "synthetic normalized vectors; explicit public fixture key",
            "profile": "release; single thread; thin LTO; default target CPU",
            "scope": "actual CPDVEC05 and CipherKey; excludes CNG, SQLCipher, USearch, Tauri and app startup/query latency",
            "cache_conditions": "decryption cache cold/hot as labeled; OS file cache is not flushed",
            "hot_batch_repetitions": 100, "samples": options.samples, "results": results,
        }))?
    );
    Ok(())
}
