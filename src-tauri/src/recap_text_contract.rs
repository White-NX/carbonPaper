//! Content-addressed recap chunks shared by interactive and unattended readers.
use crate::semantic_models::MINILM_VECTOR_SPACE_REVISION;
use sha2::{Digest, Sha256};

pub const INDEX_VERSION: u32 = 2;

pub struct Chunk {
    pub key: String,
    pub text: String,
    pub detail: bool,
}

fn windows(text: &str, bytes: usize) -> Vec<String> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        let mut size = 0;
        while end < chars.len() && size + chars[end].len_utf8() <= bytes {
            size += chars[end].len_utf8();
            end += 1;
        }
        if end == start {
            end += 1;
        }
        out.push(chars[start..end].iter().collect());
        if end == chars.len() {
            break;
        }
        start = end.saturating_sub(12).max(start + 1);
    }
    out
}

pub fn chunks<'a>(title: &str, text: &str, details: impl Iterator<Item = &'a str>) -> Vec<Chunk> {
    let context = windows(title, 72).into_iter().next().unwrap_or_default();
    [(title, false), (text, false)]
        .into_iter()
        .chain(details.map(|text| (text, true)))
        .flat_map(|(text, detail)| {
            windows(text, 160)
                .into_iter()
                .map(move |text| (text, detail))
        })
        .map(|(text, detail)| {
            let text = format!("{context}\n{text}");
            let key = hex::encode(Sha256::digest(
                format!("recap-search-{INDEX_VERSION}:{MINILM_VECTOR_SPACE_REVISION}:{text}")
                    .as_bytes(),
            ));
            Chunk { key, text, detail }
        })
        .collect()
}
