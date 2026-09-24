//! Rule-based detection of personal information in OCR text.
//!
//! This replaces the Presidio/spaCy service that used to run in the Python
//! monitor. The same categories are now found by pattern and checksum rules
//! that run here, in process, and cannot time out. Names, and the entity types
//! that only the English built-in recognizers produced, are not detected; the
//! capability change is recorded in `docs/python-removal-roadmap.md`.
//!
//! Screen OCR changes numbers in characteristic ways. On PP-OCRv5 most errors
//! are dropped digits (CTC decoding merges repeated characters), swaps between
//! similar digits such as 0 and 8, and misread separators. A strict pattern
//! with a checksum misses exactly those strings, although a reader can still
//! recover most of the digits. The number rules therefore treat a checksum as
//! supporting evidence, tolerate one edit, or two when a label such as
//! "身份证号" is nearby, and callers replace every finding as a whole.
//! `fixtures/ocr_error_patterns.json` holds the OCR edits the tests replay.

mod numbers;
mod patterns;
#[cfg(test)]
mod tests;

/// A kind of personal information the rules can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PiiKind {
    PhoneNumber,
    CnIdCard,
    BankCard,
    EmailAddress,
    Address,
    Credential,
    IpAddress,
    /// A long digit string that matched no specific rule. It is only ever
    /// masked, and only when the settings ask for long numbers.
    LongNumber,
}

impl PiiKind {
    /// Kinds a user can switch on or off, in settings order.
    pub const SELECTABLE: [PiiKind; 7] = [
        PiiKind::PhoneNumber,
        PiiKind::CnIdCard,
        PiiKind::BankCard,
        PiiKind::EmailAddress,
        PiiKind::Address,
        PiiKind::Credential,
        PiiKind::IpAddress,
    ];

    /// Stable name used in settings and in masks such as `[PHONE_NUMBER]`.
    pub const fn name(self) -> &'static str {
        match self {
            PiiKind::PhoneNumber => "PHONE_NUMBER",
            PiiKind::CnIdCard => "CN_ID_CARD",
            PiiKind::BankCard => "CN_BANK_CARD",
            PiiKind::EmailAddress => "EMAIL_ADDRESS",
            PiiKind::Address => "ADDRESS",
            PiiKind::Credential => "CREDENTIAL",
            PiiKind::IpAddress => "IP_ADDRESS",
            PiiKind::LongNumber => "LONG_NUMBER",
        }
    }

    /// Parses a settings name, including `CREDIT_CARD` from the Presidio era.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "PHONE_NUMBER" => Some(PiiKind::PhoneNumber),
            "CN_ID_CARD" => Some(PiiKind::CnIdCard),
            "CN_BANK_CARD" | "CREDIT_CARD" => Some(PiiKind::BankCard),
            "EMAIL_ADDRESS" => Some(PiiKind::EmailAddress),
            "ADDRESS" => Some(PiiKind::Address),
            "CREDENTIAL" => Some(PiiKind::Credential),
            "IP_ADDRESS" => Some(PiiKind::IpAddress),
            _ => None,
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which kinds to look for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PiiSettings {
    kinds: u8,
    mask_long_numbers: bool,
}

impl PiiSettings {
    pub fn new(kinds: impl IntoIterator<Item = PiiKind>, mask_long_numbers: bool) -> Self {
        let kinds = kinds
            .into_iter()
            .filter(|kind| *kind != PiiKind::LongNumber)
            .fold(0, |bits, kind| bits | kind.bit());
        Self {
            kinds,
            mask_long_numbers,
        }
    }

    /// Settings that find nothing.
    pub const fn off() -> Self {
        Self {
            kinds: 0,
            mask_long_numbers: false,
        }
    }

    pub fn wants(self, kind: PiiKind) -> bool {
        match kind {
            PiiKind::LongNumber => self.mask_long_numbers,
            _ => self.kinds & kind.bit() != 0,
        }
    }

    pub fn is_off(self) -> bool {
        self.kinds == 0 && !self.mask_long_numbers
    }
}

/// Byte range of one finding in the inspected text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiiSpan {
    pub kind: PiiKind,
    pub start: usize,
    pub end: usize,
}

impl PiiSpan {
    /// Whether the finding is certain enough for the filter mode to apply.
    /// Long numbers are only ever masked in place.
    pub fn is_confident(&self) -> bool {
        self.kind != PiiKind::LongNumber
    }
}

/// Labels near a text that make a nearby number more likely to be personal:
/// "手机" before a phone number, "身份证号" before an ID number, and so on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    phone: bool,
    id: bool,
    card: bool,
}

// OCR often damages labels too ("份证号", "身证号"), so the Chinese entries
// are short fragments. English entries must stand as whole words.
const PHONE_WORDS: &[&str] = &[
    "手机", "机号", "电话", "话号", "号码", "联系", "致电", "来电", "座机", "tel", "phone",
    "mobile", "cell",
];
const ID_WORDS: &[&str] = &["身份", "份证", "证号", "证件", "身证", "id no", "id card"];
const CARD_WORDS: &[&str] = &[
    "卡号",
    "银行卡",
    "账号",
    "帐号",
    "开户",
    "储蓄卡",
    "信用卡",
    "借记卡",
    "card no",
    "card number",
    "account",
];

impl Context {
    /// Labels found in `text` itself.
    pub fn of(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        let lower = text.to_lowercase();
        let any = |words: &[&str]| words.iter().any(|word| contains_label(&lower, word));
        Self {
            phone: any(PHONE_WORDS),
            id: any(ID_WORDS),
            card: any(CARD_WORDS),
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            phone: self.phone || other.phone,
            id: self.id || other.id,
            card: self.card || other.card,
        }
    }
}

/// Finds `word` in `haystack`; an ASCII word must not touch other letters, so
/// "tel" matches "Tel:" but not "Intel".
fn contains_label(haystack: &str, word: &str) -> bool {
    if !word.is_ascii() {
        return haystack.contains(word);
    }
    haystack.match_indices(word).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphabetic())
            && !after.is_some_and(|c| c.is_ascii_alphabetic())
    })
}

/// Axis-aligned bounds of an OCR block in screenshot pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bounds {
    /// Bounds of an OCR quadrilateral stored as `[[x, y], ...]`.
    pub fn from_points(points: &[Vec<f64>]) -> Option<Self> {
        let mut bounds: Option<Bounds> = None;
        for point in points {
            let (x, y) = match point.as_slice() {
                [x, y, ..] if x.is_finite() && y.is_finite() => (*x, *y),
                _ => continue,
            };
            bounds = Some(match bounds {
                None => Bounds {
                    left: x,
                    top: y,
                    right: x,
                    bottom: y,
                },
                Some(b) => Bounds {
                    left: b.left.min(x),
                    top: b.top.min(y),
                    right: b.right.max(x),
                    bottom: b.bottom.max(y),
                },
            });
        }
        bounds.filter(|b| b.right > b.left && b.bottom > b.top)
    }

    fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// Context for each OCR block of one screenshot: its own labels plus those of
/// the nearest block to its left on the same line and the nearest block
/// directly above. Forms usually place "身份证号" and the number in separate
/// blocks, so a per-block check alone loses the label.
pub fn block_contexts(blocks: &[(&str, Option<Bounds>)]) -> Vec<Context> {
    let own: Vec<Context> = blocks.iter().map(|(text, _)| Context::of(text)).collect();
    (0..blocks.len())
        .map(|i| {
            let mut context = own[i];
            let Some(bounds) = blocks[i].1 else {
                return context;
            };
            for neighbor in [
                nearest_left(blocks, i, bounds),
                nearest_above(blocks, i, bounds),
            ]
            .into_iter()
            .flatten()
            {
                context = context.union(own[neighbor]);
            }
            context
        })
        .collect()
}

fn nearest_left(blocks: &[(&str, Option<Bounds>)], i: usize, b: Bounds) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (j, (_, other)) in blocks.iter().enumerate() {
        let Some(o) = other.filter(|_| j != i) else {
            continue;
        };
        let scale = b.height().max(o.height());
        let overlap = b.bottom.min(o.bottom) - b.top.max(o.top);
        let same_line = overlap >= 0.5 * b.height().min(o.height());
        let gap = b.left - o.right;
        if same_line && gap >= -0.5 * scale && gap <= 30.0 * scale {
            if best.is_none_or(|(_, right)| o.right > right) {
                best = Some((j, o.right));
            }
        }
    }
    best.map(|(j, _)| j)
}

fn nearest_above(blocks: &[(&str, Option<Bounds>)], i: usize, b: Bounds) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (j, (_, other)) in blocks.iter().enumerate() {
        let Some(o) = other.filter(|_| j != i) else {
            continue;
        };
        let scale = b.height().max(o.height());
        let gap = b.top - o.bottom;
        let overlaps = o.left < b.right && b.left < o.right;
        let aligned = (o.left - b.left).abs() <= 2.0 * scale;
        if gap >= -0.3 * scale && gap <= 1.5 * scale && (overlaps || aligned) {
            if best.is_none_or(|(_, best_gap)| gap < best_gap) {
                best = Some((j, gap));
            }
        }
    }
    best.map(|(j, _)| j)
}

/// Finds personal information in `text`. `context` carries labels from
/// neighbouring blocks; labels inside `text` are added automatically.
/// Returns non-overlapping spans in text order.
pub fn detect(text: &str, settings: PiiSettings, context: Context) -> Vec<PiiSpan> {
    if settings.is_off() || text.is_empty() {
        return Vec::new();
    }
    let context = context.union(Context::of(text));
    let mut found = Vec::new();
    let blanked = patterns::find(text, settings, &mut found);
    numbers::find(text, &blanked, settings, context, &mut found);
    select(found)
}

/// Keeps the strongest non-overlapping findings: confident kinds first, then
/// the longer span.
fn select(mut found: Vec<PiiSpan>) -> Vec<PiiSpan> {
    found.sort_by(|a, b| {
        b.is_confident()
            .cmp(&a.is_confident())
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
    let mut kept: Vec<PiiSpan> = Vec::with_capacity(found.len());
    for span in found {
        if kept
            .iter()
            .all(|k| span.end <= k.start || k.end <= span.start)
        {
            kept.push(span);
        }
    }
    kept.sort_by_key(|span| span.start);
    kept
}

/// Replaces each span with its label, e.g. `[CN_ID_CARD]`.
pub fn mask(text: &str, spans: &[PiiSpan]) -> String {
    if spans.is_empty() {
        return text.to_string();
    }
    let mut ordered: Vec<&PiiSpan> = spans.iter().collect();
    ordered.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for span in ordered {
        let start = span.start.clamp(pos, text.len());
        let end = span.end.min(text.len());
        if end <= start || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        out.push_str(&text[pos..start]);
        out.push('[');
        out.push_str(span.kind.name());
        out.push(']');
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}
