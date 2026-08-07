//! Read-only, local fixture exporter for the BGE migration benchmark.
//!
//! Plaintext is emitted only as one JSON document on stdout. Diagnostics use
//! stderr so callers can keep stdout in an anonymous pipe and avoid writing
//! decrypted screenshot data to disk.

#[allow(dead_code)]
#[path = "../src/credential_manager.rs"]
mod credential_manager;
#[allow(dead_code)]
#[path = "../src/registry_config.rs"]
mod registry_config;

use credential_manager::{
    decrypt_row_key_with_cng, decrypt_with_master_key, derive_db_key_from_public_key, CngKeySession,
};
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const DEFAULT_SAMPLE_SIZE: usize = 100;
const DEFAULT_SEED: &str = "carbonpaper-bge-v1";
const DEFAULT_MAX_OCR_CHARS: usize = 4096;

#[derive(Debug)]
struct Args {
    data_dir: PathBuf,
    sample_size: usize,
    seed: String,
    sample_ids: Option<Vec<i64>>,
    max_ocr_chars: usize,
}

#[derive(Serialize)]
struct ExportPayload {
    schema_version: u32,
    database: DatabaseSnapshot,
    selection: SelectionMetadata,
    corpus_sha256: String,
    samples: Vec<BenchmarkSample>,
}

#[derive(Serialize)]
struct DatabaseSnapshot {
    size_bytes: u64,
    modified_unix_ns: Option<u128>,
}

#[derive(Serialize)]
struct SelectionMetadata {
    seed: String,
    requested_sample_size: usize,
    eligible_rows: usize,
    selected_ids: Vec<i64>,
    skipped_rows: usize,
    max_ocr_chars: usize,
}

#[derive(Serialize)]
struct BenchmarkSample {
    screenshot_id: i64,
    window_title: String,
    process_name: String,
    ocr_text: String,
}

struct RawScreenshot {
    window_title_plain: Option<String>,
    process_name_plain: Option<String>,
    window_title_enc: Option<Vec<u8>>,
    process_name_enc: Option<Vec<u8>>,
    content_key_enc: Option<Vec<u8>>,
}

fn usage() -> &'static str {
    "Usage: cargo run --example bge_benchmark_export -- \
        --data-dir PATH [--sample-size N] [--seed TEXT] \
        [--sample-ids ID,ID,...] [--max-ocr-chars N]"
}

fn parse_args() -> Result<Args, String> {
    let mut data_dir = None;
    let mut sample_size = DEFAULT_SAMPLE_SIZE;
    let mut seed = DEFAULT_SEED.to_string();
    let mut sample_ids = None;
    let mut max_ocr_chars = DEFAULT_MAX_OCR_CHARS;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--help" | "-h" => return Err(usage().to_string()),
            "--data-dir" | "--sample-size" | "--seed" | "--sample-ids" | "--max-ocr-chars" => args
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?,
            _ => return Err(format!("unknown argument: {arg}\n{}", usage())),
        };
        match arg.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(value)),
            "--sample-size" => {
                sample_size = value
                    .parse::<usize>()
                    .map_err(|_| "--sample-size must be a positive integer".to_string())?;
            }
            "--seed" => seed = value,
            "--sample-ids" => sample_ids = Some(parse_sample_ids(&value)?),
            "--max-ocr-chars" => {
                max_ocr_chars = value
                    .parse::<usize>()
                    .map_err(|_| "--max-ocr-chars must be a positive integer".to_string())?;
            }
            _ => unreachable!(),
        }
    }

    if sample_size == 0 {
        return Err("--sample-size must be greater than zero".to_string());
    }
    if max_ocr_chars < 200 {
        return Err("--max-ocr-chars must be at least 200".to_string());
    }
    let data_dir = data_dir.ok_or_else(|| format!("--data-dir is required\n{}", usage()))?;
    Ok(Args {
        data_dir,
        sample_size,
        seed,
        sample_ids,
        max_ocr_chars,
    })
}

fn parse_sample_ids(raw: &str) -> Result<Vec<i64>, String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let id = value
            .parse::<i64>()
            .map_err(|_| format!("invalid screenshot id: {value}"))?;
        if id <= 0 {
            return Err(format!("screenshot ids must be positive: {id}"));
        }
        if seen.insert(id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err("--sample-ids did not contain any ids".to_string());
    }
    Ok(ids)
}

fn open_database(data_dir: &Path) -> Result<(Connection, DatabaseSnapshot), Box<dyn Error>> {
    let db_path = data_dir.join("screenshots.db");
    let public_key_path = data_dir.join("credential_public_key.bin");
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()).into());
    }
    if !public_key_path.is_file() {
        return Err(format!("public key does not exist: {}", public_key_path.display()).into());
    }

    let public_key = fs::read(&public_key_path)?;
    let db_key = derive_db_key_from_public_key(&public_key);
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(db_key)))?;
    conn.execute_batch("SELECT count(*) FROM sqlite_master; PRAGMA query_only = ON; BEGIN;")?;

    let metadata = fs::metadata(db_path)?;
    let modified_unix_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos());
    Ok((
        conn,
        DatabaseSnapshot {
            size_bytes: metadata.len(),
            modified_unix_ns,
        },
    ))
}

fn stable_score(seed: &str, screenshot_id: i64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"CarbonPaper-BGE-benchmark-sample-v1\0");
    hasher.update(seed.as_bytes());
    hasher.update([0]);
    hasher.update(screenshot_id.to_le_bytes());
    hasher.finalize().into()
}

fn select_candidate_ids(
    conn: &Connection,
    seed: &str,
    sample_size: usize,
    explicit_ids: Option<&[i64]>,
) -> Result<(Vec<i64>, usize), Box<dyn Error>> {
    if let Some(ids) = explicit_ids {
        return Ok((ids.to_vec(), ids.len()));
    }

    let mut stmt = conn.prepare(
        "SELECT s.id FROM screenshots s \
         WHERE s.is_deleted = 0 AND (\
             s.window_title_enc IS NOT NULL OR length(COALESCE(s.window_title, '')) > 0 OR \
             EXISTS (SELECT 1 FROM ocr_results r \
                     WHERE r.screenshot_id = s.id AND r.is_deleted = 0)\
         ) ORDER BY s.id ASC",
    )?;
    let mut ranked = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|id| (stable_score(seed, id), id))
        .collect::<Vec<_>>();
    let eligible_rows = ranked.len();
    ranked.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

    // Decryption failures and empty legacy rows should not shrink a normal run.
    // Keep a deterministic reserve while avoiding unnecessary sensitive reads.
    let reserve = sample_size.saturating_mul(8).max(sample_size);
    ranked.truncate(reserve.min(ranked.len()));
    Ok((
        ranked.into_iter().map(|(_, id)| id).collect(),
        eligible_rows,
    ))
}

fn load_raw_screenshot(
    conn: &Connection,
    screenshot_id: i64,
) -> Result<Option<RawScreenshot>, Box<dyn Error>> {
    let mut stmt = conn.prepare_cached(
        "SELECT window_title, process_name, window_title_enc, process_name_enc, \
                content_key_encrypted \
         FROM screenshots WHERE id = ?1 AND is_deleted = 0",
    )?;
    let mut rows = stmt.query(params![screenshot_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(RawScreenshot {
        window_title_plain: row.get(0)?,
        process_name_plain: row.get(1)?,
        window_title_enc: row.get(2)?,
        process_name_enc: row.get(3)?,
        content_key_enc: row.get(4)?,
    }))
}

fn first_encrypted_key(
    conn: &Connection,
    candidate_ids: &[i64],
) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    for screenshot_id in candidate_ids {
        if let Some(raw) = load_raw_screenshot(conn, *screenshot_id)? {
            if let Some(key) = raw.content_key_enc {
                return Ok(Some(key));
            }
        }
        let key = conn
            .query_row(
                "SELECT text_key_encrypted FROM ocr_results \
                 WHERE screenshot_id = ?1 AND is_deleted = 0 \
                   AND text_key_encrypted IS NOT NULL ORDER BY id ASC LIMIT 1",
                params![screenshot_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok();
        if key.is_some() {
            return Ok(key);
        }
    }
    Ok(None)
}

fn decrypt_optional_text(
    encrypted: Option<&[u8]>,
    plaintext: Option<String>,
    row_key: Option<&[u8]>,
) -> Option<String> {
    match (encrypted, row_key) {
        (Some(data), Some(key)) => decrypt_with_master_key(key, data)
            .ok()
            .and_then(|value| String::from_utf8(value).ok()),
        _ => plaintext,
    }
}

fn load_ocr_text(
    conn: &Connection,
    cng: &CngKeySession,
    screenshot_id: i64,
    max_chars: usize,
) -> Result<String, Box<dyn Error>> {
    let mut stmt = conn.prepare_cached(
        "SELECT text, text_enc, text_key_encrypted FROM ocr_results \
         WHERE screenshot_id = ?1 AND is_deleted = 0 ORDER BY id ASC",
    )?;
    let mut rows = stmt.query(params![screenshot_id])?;
    let mut parts = Vec::new();
    let mut char_count = 0usize;
    while let Some(row) = rows.next()? {
        let plain: Option<String> = row.get(0)?;
        let encrypted: Option<Vec<u8>> = row.get(1)?;
        let encrypted_key: Option<Vec<u8>> = row.get(2)?;
        let text = match (encrypted.as_deref(), encrypted_key.as_deref()) {
            (Some(data), Some(key_data)) => {
                cng.unwrap_row_key(key_data).ok().and_then(|mut row_key| {
                    let value = decrypt_with_master_key(&row_key, data)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok());
                    row_key.fill(0);
                    value
                })
            }
            _ => plain,
        };
        let Some(text) = text.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        char_count =
            char_count.saturating_add(text.chars().count() + usize::from(!parts.is_empty()));
        parts.push(text);
        if char_count >= max_chars {
            break;
        }
    }
    Ok(parts.join(" ").chars().take(max_chars).collect())
}

fn load_sample(
    conn: &Connection,
    cng: &CngKeySession,
    screenshot_id: i64,
    max_ocr_chars: usize,
) -> Result<Option<BenchmarkSample>, Box<dyn Error>> {
    let Some(raw) = load_raw_screenshot(conn, screenshot_id)? else {
        return Ok(None);
    };
    let mut screenshot_key = raw
        .content_key_enc
        .as_deref()
        .and_then(|encrypted| cng.unwrap_row_key(encrypted).ok());
    let window_title = decrypt_optional_text(
        raw.window_title_enc.as_deref(),
        raw.window_title_plain,
        screenshot_key.as_deref(),
    )
    .unwrap_or_default();
    let process_name = decrypt_optional_text(
        raw.process_name_enc.as_deref(),
        raw.process_name_plain,
        screenshot_key.as_deref(),
    )
    .unwrap_or_default();
    if let Some(key) = screenshot_key.as_mut() {
        key.fill(0);
    }
    let ocr_text = load_ocr_text(conn, cng, screenshot_id, max_ocr_chars)?;

    if window_title.trim().is_empty() && ocr_text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(BenchmarkSample {
        screenshot_id,
        window_title,
        process_name,
        ocr_text,
    }))
}

fn corpus_sha256(samples: &[BenchmarkSample]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"CarbonPaper-BGE-benchmark-corpus-v1\0");
    for sample in samples {
        hasher.update(sample.screenshot_id.to_le_bytes());
        for value in [&sample.window_title, &sample.process_name, &sample.ocr_text] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let (conn, database) = open_database(&args.data_dir)?;
    let (candidate_ids, eligible_rows) = select_candidate_ids(
        &conn,
        &args.seed,
        args.sample_size,
        args.sample_ids.as_deref(),
    )?;
    if candidate_ids.is_empty() {
        return Err("no eligible screenshots were found".into());
    }

    if let Some(encrypted_key) = first_encrypted_key(&conn, &candidate_ids)? {
        eprintln!("Authenticating with Windows CNG for local benchmark decryption...");
        let mut row_key = decrypt_row_key_with_cng(&encrypted_key)?;
        row_key.fill(0);
    }
    let cng = CngKeySession::open_silent()?;

    let wanted = args
        .sample_ids
        .as_ref()
        .map(Vec::len)
        .unwrap_or(args.sample_size);
    let mut samples = Vec::with_capacity(wanted);
    let mut skipped_rows = 0usize;
    for screenshot_id in candidate_ids {
        match load_sample(&conn, &cng, screenshot_id, args.max_ocr_chars) {
            Ok(Some(sample)) => samples.push(sample),
            Ok(None) => skipped_rows += 1,
            Err(error) => {
                skipped_rows += 1;
                eprintln!("Skipping screenshot id {screenshot_id}: {error}");
            }
        }
        if samples.len() >= wanted {
            break;
        }
    }
    if samples.is_empty() {
        return Err("selected rows could not be decrypted into benchmark inputs".into());
    }
    if samples.len() < wanted {
        eprintln!(
            "Warning: requested {wanted} samples but only {} were usable",
            samples.len()
        );
    }

    let selected_ids = samples.iter().map(|sample| sample.screenshot_id).collect();
    let payload = ExportPayload {
        schema_version: 1,
        database,
        selection: SelectionMetadata {
            seed: args.seed,
            requested_sample_size: wanted,
            eligible_rows,
            selected_ids,
            skipped_rows,
            max_ocr_chars: args.max_ocr_chars,
        },
        corpus_sha256: corpus_sha256(&samples),
        samples,
    };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, &payload)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn main() {
    match parse_args().and_then(|args| run(args).map_err(|error| error.to_string())) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("BGE benchmark export failed: {error}");
            std::process::exit(2);
        }
    }
}
