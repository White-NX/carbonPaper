//! Digit-run rules: Chinese mobile and landline numbers, resident ID numbers,
//! bank cards, and the optional long-number fallback.
//!
//! The text is split into groups of digit-like characters, and neighbouring
//! groups are joined across the separators people and OCR put between them
//! ("138 1234 5678", "181.4702 0889"). Every window of consecutive groups is
//! then tested against each rule. A group may contain a letter OCR produced
//! for a digit ("l52" for "152"); such letters are mapped back and counted.

use super::{Context, PiiKind, PiiSettings, PiiSpan};
use std::ops::Range;

/// Letters and symbols PP-OCR produces in place of a digit.
fn confusable_digit(c: char) -> Option<u8> {
    Some(match c {
        'O' | 'o' | 'D' | 'Q' | '〇' => b'0',
        'l' | 'I' | 'i' | '|' | '!' | '丨' => b'1',
        'Z' | 'z' => b'2',
        'S' | 's' => b'5',
        'b' | 'G' => b'6',
        'B' => b'8',
        'g' | 'q' => b'9',
        _ => return None,
    })
}

fn ascii_digit(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8),
        '０'..='９' => Some(b'0' + (c as u32 - '０' as u32) as u8),
        _ => None,
    }
}

/// The check character of an ID number, as written or as OCR misreads it.
fn is_check_letter(c: char) -> bool {
    matches!(c, 'X' | 'x' | 'K' | 'k')
}

/// Characters that may belong to a group before trimming.
fn is_member(c: char) -> bool {
    ascii_digit(c).is_some() || confusable_digit(c).is_some() || c.is_ascii_alphabetic()
}

enum Separator {
    /// Spaces and dashes, which people put between digit groups.
    Strong,
    /// Dots and similar marks, which OCR sometimes produces for a space.
    Weak,
}

fn separator(c: char) -> Option<Separator> {
    match c {
        ' ' | '\t' | '\u{a0}' | '\u{3000}' | '-' | '‐' | '‑' | '–' | '—' | '－' | '_' => {
            Some(Separator::Strong)
        }
        '.' | '·' | '・' | '•' | '~' | '～' => Some(Separator::Weak),
        _ => None,
    }
}

struct Group {
    /// Character indexes, end exclusive.
    start: usize,
    end: usize,
    /// ASCII digits, with `X` only as a trailing ID check character.
    canon: Vec<u8>,
    digits: usize,
    mapped: usize,
    noise: usize,
}

impl Group {
    /// Length as displayed, which is what digit grouping ("6222 0212 ...") is
    /// about, even when OCR turned one digit into a stray letter.
    fn shown_len(&self) -> usize {
        self.end - self.start
    }
}

#[derive(Default)]
struct Window {
    canon: Vec<u8>,
    digits: usize,
    mapped: usize,
    noise: usize,
    /// Displayed length of each group in the window.
    lens: Vec<usize>,
}

impl Window {
    fn push(&mut self, group: &Group) {
        self.canon.extend_from_slice(&group.canon);
        self.digits += group.digits;
        self.mapped += group.mapped;
        self.noise += group.noise;
        self.lens.push(group.shown_len());
    }
}

pub(super) fn find(
    text: &str,
    skip: &[Range<usize>],
    settings: PiiSettings,
    context: Context,
    found: &mut Vec<PiiSpan>,
) {
    let wanted = [
        PiiKind::PhoneNumber,
        PiiKind::CnIdCard,
        PiiKind::BankCard,
        PiiKind::LongNumber,
    ];
    if !wanted.iter().any(|kind| settings.wants(*kind)) {
        return;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let blocked: Vec<bool> = chars
        .iter()
        .map(|(at, _)| skip.iter().any(|range| range.contains(at)))
        .collect();

    // OCR sometimes emits a bracket for a digit ("135 8)98 0742"); between two
    // digits it is read as a misread digit rather than punctuation.
    let glitch = |k: usize| {
        matches!(chars[k].1, ')' | '(' | ']' | '[' | '}' | '{' | '>' | '<')
            && k > 0
            && k + 1 < chars.len()
            && ascii_digit(chars[k - 1].1).is_some()
            && ascii_digit(chars[k + 1].1).is_some()
    };
    let member = |k: usize| !blocked[k] && (is_member(chars[k].1) || glitch(k));
    let mut groups = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !member(i) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && member(j) {
            j += 1;
        }
        if let Some(group) = build_group(&chars, i, j) {
            groups.push(group);
        }
        i = j;
    }

    let mut runs: Vec<Vec<usize>> = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let joins = runs
            .last()
            .and_then(|run| run.last())
            .is_some_and(|&prev| linkable(&chars, &blocked, &groups[prev], group));
        match runs.last_mut() {
            Some(run) if joins => run.push(index),
            _ => runs.push(vec![index]),
        }
    }

    let byte_at = |index: usize| chars.get(index).map_or(text.len(), |(at, _)| *at);
    for run in &runs {
        for a in 0..run.len() {
            let mut window = Window::default();
            for b in a..run.len() {
                window.push(&groups[run[b]]);
                if window.canon.len() > 21 {
                    break;
                }
                if let Some(kind) = classify(&window, settings, context) {
                    found.push(PiiSpan {
                        kind,
                        start: byte_at(groups[run[a]].start),
                        end: byte_at(groups[run[b]].end),
                    });
                }
            }
        }
        if settings.wants(PiiKind::PhoneNumber) {
            if let Some(start) = glued_mobile_start(&chars, &groups, run) {
                let last = &groups[run[run.len() - 1]];
                found.push(PiiSpan {
                    kind: PiiKind::PhoneNumber,
                    start: byte_at(start),
                    end: byte_at(last.end),
                });
            }
        }
    }
}

/// A mobile number shown as "138 1234 5678" whose first group ran into the
/// characters before it ("40138 1234 5678" when OCR read a name as digits).
/// Returns the character index where the number starts.
fn glued_mobile_start(chars: &[(usize, char)], groups: &[Group], run: &[usize]) -> Option<usize> {
    let [.., first, second, third] = run else {
        return None;
    };
    let (first, second, third) = (&groups[*first], &groups[*second], &groups[*third]);
    if first.shown_len() <= 3 || second.shown_len() != 4 || third.shown_len() != 4 {
        return None;
    }
    if second.digits != 4 || third.digits != 4 {
        return None;
    }
    let head: Vec<char> = chars[first.end - 3..first.end]
        .iter()
        .map(|(_, c)| *c)
        .collect();
    let mobile = head[0] == '1' && ('3'..='9').contains(&head[1]) && head[2].is_ascii_digit();
    mobile.then_some(first.end - 3)
}

/// Trims `chars[i..j]` to its digit-like core and canonicalises it. Words
/// next to numbers ("build", "SF") are trimmed off; a group must be at least
/// half real digits.
fn build_group(chars: &[(usize, char)], i: usize, j: usize) -> Option<Group> {
    let digit_at = |k: usize| ascii_digit(chars[k].1).is_some();
    let (mut a, mut b) = (i, j);
    while a < b {
        let c = chars[a].1;
        if digit_at(a) || (confusable_digit(c).is_some() && a + 1 < b && digit_at(a + 1)) {
            break;
        }
        a += 1;
    }
    while b > a {
        let c = chars[b - 1].1;
        if digit_at(b - 1) {
            break;
        }
        let after_digit = b - 1 > a && digit_at(b - 2);
        if after_digit && (confusable_digit(c).is_some() || is_check_letter(c)) {
            break;
        }
        b -= 1;
    }
    if a == b {
        return None;
    }
    // A single letter between digit groups ("778e 5002") is a misread digit,
    // not the edge of a word; keep it so the grouping stays visible.
    let digits_inside = (a..b).filter(|&k| digit_at(k)).count();
    let separated_digit =
        |sep: usize, digit: usize| separator(chars[sep].1).is_some() && digit_at(digit);
    if b + 1 == j && digits_inside >= 3 && j + 1 < chars.len() && separated_digit(j, j + 1) {
        b = j;
    }
    if a == i + 1 && digits_inside >= 3 && i >= 2 && separated_digit(i - 1, i - 2) {
        a = i;
    }
    let mut group = Group {
        start: a,
        end: b,
        canon: Vec::with_capacity(b - a),
        digits: 0,
        mapped: 0,
        noise: 0,
    };
    for k in a..b {
        let c = chars[k].1;
        if let Some(d) = ascii_digit(c) {
            group.canon.push(d);
            group.digits += 1;
        } else if k == b - 1 && is_check_letter(c) {
            group.canon.push(b'X');
        } else if let Some(d) = confusable_digit(c) {
            group.canon.push(d);
            group.mapped += 1;
        } else {
            group.noise += 1;
        }
    }
    (group.digits * 2 >= b - a).then_some(group)
}

/// Whether two groups are parts of one number: at most three separator
/// characters between them, and weak separators only between longer groups.
fn linkable(chars: &[(usize, char)], blocked: &[bool], left: &Group, right: &Group) -> bool {
    let gap = left.end..right.start;
    if gap.is_empty() || gap.len() > 3 {
        return false;
    }
    let mut weak = false;
    for k in gap {
        if blocked[k] {
            return false;
        }
        match separator(chars[k].1) {
            Some(Separator::Strong) => {}
            Some(Separator::Weak) => weak = true,
            None => return false,
        }
    }
    !weak || (left.canon.len() >= 3 && right.canon.len() >= 3)
}

fn classify(window: &Window, settings: PiiSettings, context: Context) -> Option<PiiKind> {
    let canon = &window.canon;
    let len = canon.len();
    if len < 7 {
        return None;
    }
    let check_letter = canon.iter().position(|&d| d == b'X');
    let all_digits = check_letter.is_none();
    if all_digits
        && settings.wants(PiiKind::PhoneNumber)
        && (is_mobile(window, context) || is_landline(window, context))
    {
        return Some(PiiKind::PhoneNumber);
    }
    if (all_digits || check_letter == Some(len - 1))
        && settings.wants(PiiKind::CnIdCard)
        && is_id_card(window, context)
    {
        return Some(PiiKind::CnIdCard);
    }
    if all_digits && settings.wants(PiiKind::BankCard) && is_bank_card(window, context) {
        return Some(PiiKind::BankCard);
    }
    if settings.wants(PiiKind::LongNumber)
        && len >= 11
        && 4 * window.digits >= 3 * (len + window.noise)
    {
        return Some(PiiKind::LongNumber);
    }
    None
}

fn is_mobile(w: &Window, context: Context) -> bool {
    let c = &w.canon;
    let len = c.len();
    let prefix = c[0] == b'1' && (b'3'..=b'9').contains(&c[1]);
    // A stray character that replaced a digit still occupies its place.
    let slots = len + w.noise;
    if slots == 11 && prefix && w.noise <= 1 && w.mapped + w.noise <= 2 {
        return true;
    }
    // "138 1234 5678" with one digit lost or added in one of its groups.
    let slipped = match (len, w.lens.as_slice()) {
        (10, [2, 4, 4] | [3, 3, 4] | [3, 4, 3]) => true,
        (12, [3, 5, 4] | [3, 4, 5]) => true,
        _ => false,
    };
    if slipped && prefix && w.noise == 0 && w.mapped <= 1 {
        return true;
    }
    context.phone && (10..=12).contains(&slots) && c[0] == b'1' && w.digits >= 8 && w.noise <= 2
}

/// Length of a Chinese area code at the start of `c`: 010 and 02x have three
/// digits, the rest four.
fn area_code_len(c: &[u8]) -> Option<usize> {
    match c {
        [b'0', b'1', b'0', ..] | [b'0', b'2', _, ..] => Some(3),
        [b'0', b'3'..=b'9', _, _, ..] => Some(4),
        _ => None,
    }
}

fn is_landline(w: &Window, context: Context) -> bool {
    let c = &w.canon;
    let len = c.len();
    if let Some(area) = area_code_len(c) {
        let subscriber = len - area;
        let plausible =
            (area == 3 && subscriber == 8) || (area == 4 && (7..=8).contains(&subscriber));
        // Without a label the area code must stand apart, as in "010-62345678".
        if plausible && w.noise == 0 && w.mapped == 0 && w.lens.len() >= 2 && w.lens[0] == area {
            return true;
        }
        if plausible && context.phone && w.noise <= 1 {
            return true;
        }
    }
    context.phone && (7..=8).contains(&len) && c[0] != b'0' && w.noise <= 1 && w.mapped <= 1
}

fn is_province(a: u8, b: u8) -> bool {
    matches!(
        (a, b),
        (b'1', b'1'..=b'5')
            | (b'2', b'1'..=b'3')
            | (b'3', b'1'..=b'7')
            | (b'4', b'1'..=b'6')
            | (b'5', b'0'..=b'4')
            | (b'6', b'1'..=b'5')
            | (b'7', b'1')
            | (b'8', b'1'..=b'2')
    )
}

fn two_digits(c: &[u8]) -> Option<u32> {
    match c {
        [a @ b'0'..=b'9', b @ b'0'..=b'9'] => Some(u32::from(a - b'0') * 10 + u32::from(b - b'0')),
        _ => None,
    }
}

fn month_ok(c: &[u8]) -> bool {
    two_digits(c).is_some_and(|m| (1..=12).contains(&m))
}

fn day_ok(c: &[u8]) -> bool {
    two_digits(c).is_some_and(|d| (1..=31).contains(&d))
}

fn year_prefix_ok(c: &[u8]) -> bool {
    c == b"19" || c == b"20"
}

/// A plausible birth date written as YYYYMMDD.
fn date_ok(c: &[u8]) -> bool {
    c.len() == 8
        && year_prefix_ok(&c[..2])
        && two_digits(&c[2..4]).is_some()
        && &c[..4] <= b"2039".as_slice()
        && month_ok(&c[4..6])
        && day_ok(&c[6..8])
}

/// ISO 7064 MOD 11-2 check character of the first 17 digits.
fn id_check_char(first17: &[u8]) -> u8 {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let sum: u32 = first17
        .iter()
        .zip(WEIGHTS)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    b"10X98765432"[(sum % 11) as usize]
}

/// An 18-character ID number whose province code, birth date and check
/// character all agree.
fn id_valid(c: &[u8]) -> bool {
    c.len() == 18
        && c[..17].iter().all(u8::is_ascii_digit)
        && is_province(c[0], c[1])
        && date_ok(&c[6..14])
        && id_check_char(&c[..17]) == c[17]
}

/// Whether one inserted, deleted or replaced character turns `c` into a valid
/// ID number. This is the repair a reader would attempt: a single slip leaves
/// the true number among a handful of candidates, so it must still be caught.
/// For random digit strings the chance of a repair is under half a percent.
fn id_repairable(c: &[u8]) -> bool {
    let mut candidate = Vec::with_capacity(19);
    let alphabet = b"0123456789X";
    match c.len() {
        17 => (0..=17).any(|at| {
            alphabet.iter().any(|&d| {
                candidate.clear();
                candidate.extend_from_slice(&c[..at]);
                candidate.push(d);
                candidate.extend_from_slice(&c[at..]);
                id_valid(&candidate)
            })
        }),
        18 => (0..18).any(|at| {
            alphabet.iter().any(|&d| {
                if d == c[at] {
                    return false;
                }
                candidate.clear();
                candidate.extend_from_slice(c);
                candidate[at] = d;
                id_valid(&candidate)
            })
        }),
        19 => (0..19).any(|at| {
            candidate.clear();
            candidate.extend_from_slice(&c[..at]);
            candidate.extend_from_slice(&c[at + 1..]);
            id_valid(&candidate)
        }),
        _ => false,
    }
}

/// Resident ID numbers. A failed checksum does not rule a string out: one
/// misread digit breaks it, yet the province code, birth date and checksum
/// together still narrow the true number to a handful of candidates.
fn is_id_card(w: &Window, context: Context) -> bool {
    let c = &w.canon;
    let len = c.len();
    if context.id && (15..=19).contains(&len) && w.digits >= 13 && w.noise <= 2 {
        return true;
    }
    if w.noise > 1 || !(17..=19).contains(&len) {
        return false;
    }
    if len == 18 && is_province(c[0], c[1]) && date_ok(&c[6..14]) {
        return true;
    }
    w.digits >= 15 && id_repairable(c)
}

fn luhn_ok(c: &[u8]) -> bool {
    let mut sum = 0;
    for (i, d) in c.iter().rev().enumerate() {
        let mut v = u32::from(d - b'0');
        if i % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum % 10 == 0
}

/// Issuer prefixes of the card schemes seen in China.
fn card_prefix_ok(c: &[u8]) -> bool {
    matches!(
        c,
        [b'6', b'2', ..] | [b'4', ..] | [b'5', b'1'..=b'5', ..] | [b'3', b'4' | b'5' | b'7', ..]
    )
}

/// Prefix and length combinations the schemes actually issue.
fn card_scheme_ok(c: &[u8]) -> bool {
    let len = c.len();
    match c {
        [b'6', b'2', ..] => (16..=19).contains(&len),
        [b'4', ..] => matches!(len, 16 | 19),
        [b'5', b'1'..=b'5', ..] | [b'3', b'5', ..] => len == 16,
        [b'2', b'2'..=b'7', ..] => len == 16,
        [b'3', b'4' | b'7', ..] => len == 15,
        _ => false,
    }
}

fn is_bank_card(w: &Window, context: Context) -> bool {
    let c = &w.canon;
    let len = c.len();
    if context.card && (13..=20).contains(&len) && w.digits >= 12 && w.noise <= 2 {
        return true;
    }
    if w.noise == 0 && w.mapped <= 1 && card_scheme_ok(c) && luhn_ok(c) {
        return true;
    }
    // "6222 0212 3456 7890 003" keeps its four-digit grouping when OCR breaks
    // the check digit. One group may be a digit short or long.
    let lens = &w.lens;
    let Some((_, leading)) = lens.split_last() else {
        return false;
    };
    let off = leading.iter().filter(|&&l| l != 4).count();
    let grouped = leading.len() >= 2
        && (off == 0
            || (off == 1 && leading.len() >= 3 && leading.iter().all(|&l| (3..=5).contains(&l))));
    grouped && card_prefix_ok(c) && (15..=19).contains(&len) && w.digits >= 13 && w.noise <= 1
}
