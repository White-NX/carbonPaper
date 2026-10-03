//! Local DeepSeek V3 estimate shared by request sizing and budget reservations.
use std::{io::Read, sync::OnceLock};

fn tokenizer() -> Option<&'static tokenizers::Tokenizer> {
    static TOKENIZER: OnceLock<Option<tokenizers::Tokenizer>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| {
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(&include_bytes!("deepseek-v3.json.gz")[..])
                .read_to_end(&mut bytes)
                .ok()?;
            let mut tokenizer = tokenizers::Tokenizer::from_bytes(bytes).ok()?;
            tokenizer.with_truncation(None).ok()?;
            tokenizer.with_padding(None);
            Some(tokenizer)
        })
        .as_ref()
}

/// One fixed 1.3 multiplier for all providers, plus protocol overhead. This is
/// a sizing estimate, not a promise about another model's tokenizer or bill.
pub(super) fn estimate(text: &str) -> u64 {
    if let Some(count) = tokenizer().and_then(|t| t.encode(text, false).ok()) {
        return 256 + (count.len() as u64 * 13).div_ceil(10);
    }
    // Fail conservatively if the bundled vocabulary cannot be decoded.
    let ascii = text.bytes().filter(u8::is_ascii).count() as u64;
    256 + ascii.div_ceil(3) + (text.len() as u64 - ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_vocabulary_encodes_multilingual_requests_without_truncation() {
        let tokenizer = tokenizer().expect("bundled DeepSeek vocabulary must load");
        // Counts cross-checked against Daydream's Kitoken.encode(text, true).
        for (text, expected) in [
            ("查看每日回顾，讨论代码修改。", 8),
            ("Review the code and run tests.", 7),
            ("{\"activities\":[{\"text\":\"阅读文档 🦀\"}]}", 14),
        ] {
            let count = tokenizer.encode(text, false).unwrap().len() as u64;
            assert_eq!(count, expected);
            assert_eq!(estimate(text), 256 + (count * 13).div_ceil(10));
        }
        let short = "今天阅读项目文档。";
        assert!(estimate(&short.repeat(5000)) > estimate(short) * 10);
        assert!(estimate(&short.repeat(100)) < 256 + short.repeat(100).len() as u64);
    }
}
