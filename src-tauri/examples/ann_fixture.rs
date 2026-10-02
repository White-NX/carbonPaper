//! Usage: cargo run --example ann_fixture -- <output.cpdvec>
#[allow(dead_code)]
#[path = "../src/ann_format.rs"]
mod ann_format;
use ann_format::{FlatFileWriter, Header};
fn main() {
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("output .cpdvec path is required"),
    );
    let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let vectors: Vec<Vec<f32>> = vec![
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
    let mut writer = FlatFileWriter::create(&path, header).unwrap();
    writer
        .push_keys(&keys.iter().map(String::as_str).collect::<Vec<_>>())
        .unwrap();
    let encoded: Vec<Vec<u8>> = vectors
        .iter()
        .map(|vector| {
            vector
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect()
        })
        .collect();
    writer
        .push_vector_bytes(&encoded.iter().map(Vec::as_slice).collect::<Vec<_>>())
        .unwrap();
    writer.finish().unwrap();
}
