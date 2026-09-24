//! Rules that match whole patterns instead of digit runs: e-mail addresses,
//! credentials, IP addresses, street addresses and phone numbers written with
//! a country code or in North American form. Also finds the "safe shapes"
//! (dates, times, versions, amounts) the digit rules must not read as numbers.

use super::{PiiKind, PiiSettings, PiiSpan};
use regex::Regex;
use std::net::Ipv6Addr;
use std::ops::Range;
use std::str::FromStr;
use std::sync::LazyLock;

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static PII pattern must compile")
}

static DATE: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?:19|20)\d{2}\s?[-/.年]\s?\d{1,2}\s?[-/.月]\s?\d{1,2}日?|\d{1,2}[-/.]\d{1,2}[-/.](?:19|20)\d{2}",
    )
});
static TIME: LazyLock<Regex> = LazyLock::new(|| compile(r"\d{1,2}:\d{2}(?::\d{2})?(?:\.\d{1,3})?"));
static DOTTED: LazyLock<Regex> = LazyLock::new(|| compile(r"[vV]?\d+(?:\.\d+){2,}"));
static MONEY: LazyLock<Regex> = LazyLock::new(|| compile(r"[¥$€£￥]\s?\d[\d,]*(?:\.\d+)?"));

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"[A-Za-z0-9][A-Za-z0-9._%+\-]*[@＠][A-Za-z0-9](?:[A-Za-z0-9\-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9\-]{0,61}[A-Za-z0-9])?)*\.[A-Za-z]{2,24}",
    )
});
/// File extensions that follow "@" in names such as `icon@2x.png`.
const FILE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "tif", "tiff", "js", "mjs", "cjs",
    "ts", "tsx", "jsx", "css", "scss", "less", "json", "html", "htm", "txt", "log", "pdf", "zip",
    "rar", "exe", "dll", "lock", "yaml", "yml", "toml", "xml", "wasm", "map", "vue", "mp3", "mp4",
    "mov", "avi",
];

/// Credential patterns; the second field is the capture group to hide, 0 for
/// the whole match.
static CREDENTIALS: LazyLock<Vec<(Regex, usize)>> = LazyLock::new(|| {
    [
        (
            r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----[A-Za-z0-9+/=\s]*(?:-----END (?:[A-Z0-9]+ )*PRIVATE KEY-----)?",
            0,
        ),
        (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", 0),
        (r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b", 0),
        (r"\bgithub_pat_[A-Za-z0-9_]{22,255}\b", 0),
        (r"\bsk-(?:ant-|proj-|live-)?[A-Za-z0-9_\-]{20,}", 0),
        (r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}\b", 0),
        (r"\bxox[abposr]-[A-Za-z0-9\-]{10,}", 0),
        (r"\bAIza[0-9A-Za-z_\-]{35}\b", 0),
        (
            r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
            0,
        ),
        (
            r"\b[a-zA-Z][a-zA-Z0-9+.\-]{1,15}://[^\s/:@]{1,64}:([^\s/@]{1,128})@",
            1,
        ),
        (
            r"(?i)(?:password|passwd|pwd|passcode|secret|api[_\- ]?key|access[_\- ]?token|auth[_\- ]?token|密码|口令|密钥)\s*[:=：]\s*([^\s，,;；]{6,128})",
            1,
        ),
    ]
    .into_iter()
    .map(|(pattern, group)| (compile(pattern), group))
    .collect()
});

static IPV4: LazyLock<Regex> = LazyLock::new(|| compile(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}"));
static IPV6: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)(?:[0-9a-f]{1,4}:|:){2,7}(?:[0-9a-f]{1,4}|:)"));

const ADMIN: &str = r"(?:\p{Han}{2,8}?(?:省|自治区))?(?:\p{Han}{2,8}?(?:市|自治州|地区|盟))?(?:\p{Han}{1,8}?(?:区|县|旗|镇|乡|街道))?";
const UNIT: &str =
    r"\s?[0-9０-９一二三四五六七八九十]{1,4}\s?(?:号楼|栋|幢|座|单元|层|楼|室|号|组)";
/// A street and house number ("文三路477号"), optionally led by the province,
/// city and district.
static STREET_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"{ADMIN}\p{{Han}}{{1,6}}?(?:路|街|大街|大道|巷|弄|胡同)\s?[0-9０-９]{{1,5}}\s?(?:号|弄|巷)院?(?:{UNIT})*"
    ))
});
/// A residential compound with a building, unit or room ("阳光小区3栋2单元").
static COMPOUND_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"{ADMIN}\p{{Han}}{{2,10}}?(?:小区|花园|公寓|大厦|家园|苑|村)(?:{UNIT})+"
    ))
});

static INTL_PHONE: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"\+\d{1,3}[ \-.]?(?:\(\d{1,4}\)[ \-.]?)?\d{1,4}(?:[ \-.]?\d{1,4}){1,5}")
});
static NANP_PHONE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\(\d{3}\)\s?\d{3}[\-.\s]\d{4}|\d{3}[\-.]\d{3}[\-.]\d{4}"));

fn prev_char(text: &str, at: usize) -> Option<char> {
    text[..at].chars().next_back()
}

fn next_char(text: &str, at: usize) -> Option<char> {
    text[at..].chars().next()
}

fn is_digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}

fn overlaps(range: &Range<usize>, others: &[Range<usize>]) -> bool {
    others
        .iter()
        .any(|other| range.start < other.end && other.start < range.end)
}

/// Runs the pattern rules and returns the byte ranges the digit rules skip:
/// safe shapes, IPv4 addresses and every span found here.
pub(super) fn find(
    text: &str,
    settings: PiiSettings,
    found: &mut Vec<PiiSpan>,
) -> Vec<Range<usize>> {
    let mut skip = safe_shapes(text);
    let mut add = |kind: PiiKind, range: Range<usize>, skip: &mut Vec<Range<usize>>| {
        if range.start < range.end {
            skip.push(range.clone());
            found.push(PiiSpan {
                kind,
                start: range.start,
                end: range.end,
            });
        }
    };

    for m in IPV4.find_iter(text) {
        if !is_ipv4(text, m.range()) {
            continue;
        }
        if settings.wants(PiiKind::IpAddress) {
            add(PiiKind::IpAddress, m.range(), &mut skip);
        } else {
            skip.push(m.range());
        }
    }
    if settings.wants(PiiKind::IpAddress) {
        for m in IPV6.find_iter(text) {
            let before = prev_char(text, m.start());
            let after = next_char(text, m.end());
            let hexish = |c: Option<char>| c.is_some_and(|c| c.is_ascii_hexdigit() || c == ':');
            if !hexish(before) && !hexish(after) && Ipv6Addr::from_str(m.as_str()).is_ok() {
                add(PiiKind::IpAddress, m.range(), &mut skip);
            }
        }
    }

    if settings.wants(PiiKind::Credential) {
        for (pattern, group) in CREDENTIALS.iter() {
            for caps in pattern.captures_iter(text) {
                let Some(m) = caps.get(*group) else { continue };
                if m.as_str()
                    .chars()
                    .all(|c| matches!(c, '*' | '•' | '●' | '·'))
                {
                    continue;
                }
                add(PiiKind::Credential, m.range(), &mut skip);
            }
        }
    }

    if settings.wants(PiiKind::EmailAddress) {
        for m in EMAIL.find_iter(text) {
            let tld = m.as_str().rsplit('.').next().unwrap_or_default();
            if FILE_EXTENSIONS.contains(&tld.to_ascii_lowercase().as_str()) {
                continue;
            }
            // "user:secret@host" in a URL is a credential, already hidden.
            if next_char(text, m.end()).is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
                || overlaps(&m.range(), &skip)
            {
                continue;
            }
            add(PiiKind::EmailAddress, m.range(), &mut skip);
        }
    }

    if settings.wants(PiiKind::PhoneNumber) {
        for m in INTL_PHONE.find_iter(text) {
            let digits = m.as_str().chars().filter(char::is_ascii_digit).count();
            let before = prev_char(text, m.start());
            if (8..=15).contains(&digits)
                && !before.is_some_and(|c| c.is_ascii_alphanumeric())
                && !is_digit(next_char(text, m.end()))
            {
                add(PiiKind::PhoneNumber, m.range(), &mut skip);
            }
        }
        for m in NANP_PHONE.find_iter(text) {
            let digits: Vec<u8> = m.as_str().bytes().filter(u8::is_ascii_digit).collect();
            let before = prev_char(text, m.start());
            if digits[0] >= b'2'
                && digits[3] >= b'2'
                && !is_digit(before)
                && before != Some('+')
                && !is_digit(next_char(text, m.end()))
                && !overlaps(&m.range(), &skip)
            {
                add(PiiKind::PhoneNumber, m.range(), &mut skip);
            }
        }
    }

    if settings.wants(PiiKind::Address) {
        for pattern in [&*STREET_ADDRESS, &*COMPOUND_ADDRESS] {
            for m in pattern.find_iter(text) {
                add(PiiKind::Address, m.range(), &mut skip);
            }
        }
    }

    skip
}

/// Dates, times, versions and amounts. A dotted run whose parts all have
/// three or more digits is left alone, because OCR sometimes reads the spaces
/// in "181 4702 0889" as dots.
fn safe_shapes(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for m in DATE.find_iter(text) {
        if !is_digit(prev_char(text, m.start())) && !is_digit(next_char(text, m.end())) {
            ranges.push(m.range());
        }
    }
    for m in TIME.find_iter(text) {
        let before = prev_char(text, m.start());
        if !is_digit(before) && before != Some(':') && !is_digit(next_char(text, m.end())) {
            ranges.push(m.range());
        }
    }
    for m in DOTTED.find_iter(text) {
        let before = prev_char(text, m.start());
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.') {
            continue;
        }
        let parts: Vec<&str> = m
            .as_str()
            .trim_start_matches(|c| c == 'v' || c == 'V')
            .split('.')
            .collect();
        let digits: usize = parts.iter().map(|part| part.len()).sum();
        let grouped_number = parts.iter().all(|part| part.len() >= 3) && digits >= 10;
        if !grouped_number || is_ipv4(text, m.range()) {
            ranges.push(m.range());
        }
    }
    for m in MONEY.find_iter(text) {
        ranges.push(m.range());
    }
    ranges
}

fn is_ipv4(text: &str, range: Range<usize>) -> bool {
    let candidate = &text[range.clone()];
    let parts: Vec<&str> = candidate.split('.').collect();
    if parts.len() != 4 || !parts.iter().all(|p| !p.is_empty() && p.len() <= 3) {
        return false;
    }
    if !parts
        .iter()
        .all(|p| p.parse::<u16>().is_ok_and(|v| v <= 255))
    {
        return false;
    }
    let before = prev_char(text, range.start);
    let rest = &text[range.end..];
    let continues = rest.starts_with('.') && rest[1..].starts_with(|c: char| c.is_ascii_digit());
    !is_digit(before) && before != Some('.') && !is_digit(next_char(text, range.end)) && !continues
}
