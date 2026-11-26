//! Character coverage of text actually supplied during one authorized run.
//! Ranges are transient; the encrypted source records remain unchanged.
use std::collections::HashSet;

use super::types::Evidence;

pub(super) fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.retain(|(start, end)| start < end);
    ranges.sort_unstable();
    let mut result: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = result.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            result.push((start, end));
        }
    }
    result
}

pub(super) fn supplied(evidence: &Evidence) -> Vec<(usize, usize)> {
    if evidence.supplied_ranges.is_empty() {
        // Legacy callers passing an unexcerpted record supplied its whole body.
        merge(vec![(0, evidence.text.chars().count())])
    } else {
        evidence.supplied_ranges.clone()
    }
}

pub(super) fn append(existing: &mut Evidence, added: &Evidence) {
    debug_assert_eq!(existing.id, added.id);
    let mut ranges = supplied(existing);
    ranges.extend(supplied(added));
    existing.supplied_ranges = merge(ranges);
    if !added.text.is_empty() && existing.text != added.text {
        existing.text =
            super::types::limited(&format!("{}\n{}", added.text, existing.text), 12_000);
    }
}

pub(super) fn subtract(wanted: &[(usize, usize)], known: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    for &(start, end) in wanted {
        let mut cursor = start;
        for &(old_start, old_end) in known {
            if old_end <= cursor {
                continue;
            }
            if old_start >= end {
                break;
            }
            if old_start > cursor {
                result.push((cursor, old_start.min(end)));
            }
            cursor = cursor.max(old_end);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            result.push((cursor, end));
        }
    }
    result
}

/// Keep the existing normalized excerpt, but map its disjoint fragments back
/// to the original filtered OCR. Trimming, CRLFs and duplicate lines must not
/// accidentally grant coverage of characters the model never received.
pub(super) fn excerpt(
    text: &str,
    previous: Option<&str>,
    limit: usize,
) -> (String, Vec<(usize, usize)>) {
    struct Line<'a> {
        text: &'a str,
        joined_start: usize,
        source_start: usize,
        len: usize,
    }
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    let (mut source_offset, mut joined_offset) = (0, 0);
    for piece in text.split_inclusive('\n') {
        let trimmed = piece.trim();
        if !trimmed.is_empty() && seen.insert(trimmed) {
            let leading_bytes = piece.len() - piece.trim_start().len();
            let len = trimmed.chars().count();
            lines.push(Line {
                text: trimmed,
                joined_start: joined_offset,
                source_start: source_offset + piece[..leading_bytes].chars().count(),
                len,
            });
            joined_offset += len + 1;
        }
        source_offset += piece.chars().count();
    }
    let joined = lines
        .iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n");
    let count = joined.chars().count();
    let wanted = if count <= limit {
        vec![(0, count)]
    } else if limit < 16 {
        vec![(0, limit)]
    } else {
        let budget = limit - 10;
        let edge = budget / 4;
        let middle_len = budget - edge * 2;
        let prior = previous
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .collect::<HashSet<_>>();
        let changed = lines
            .iter()
            .find(|line| {
                previous.is_some()
                    && !prior.contains(line.text)
                    && line.joined_start + line.len + 1 > edge
            })
            .map(|line| line.joined_start);
        let middle = changed
            .unwrap_or((count - middle_len) / 2)
            .clamp(edge, count - edge - middle_len);
        vec![
            (0, edge),
            (middle, middle + middle_len),
            (count - edge, count),
        ]
    };
    let body = if count <= limit {
        joined
    } else {
        // Copy only the supplied characters, without allocating a character
        // array for the entire OCR document.
        let mut chars = joined.chars();
        let mut cursor = 0;
        wanted
            .iter()
            .map(|&(start, end)| {
                for _ in cursor..start {
                    chars.next();
                }
                let text = chars.by_ref().take(end - start).collect::<String>();
                cursor = end;
                text
            })
            .collect::<Vec<_>>()
            .join("\n[…]\n")
    };
    let mut ranges = Vec::new();
    for &(start, end) in &wanted {
        for line in &lines {
            let a = start.max(line.joined_start);
            let b = end.min(line.joined_start + line.len);
            if a < b {
                ranges.push((
                    line.source_start + a - line.joined_start,
                    line.source_start + b - line.joined_start,
                ));
            }
        }
    }
    (body, merge(ranges))
}

pub(super) fn sample(evidence: &Evidence, previous: Option<&str>, cap: usize) -> Evidence {
    let (text, ranges) = excerpt(&evidence.text, previous, cap);
    with_text(evidence, text, ranges)
}

fn with_text(evidence: &Evidence, text: String, supplied_ranges: Vec<(usize, usize)>) -> Evidence {
    // A repeated read should not first clone the large original body merely
    // to replace it with a small fragment or an empty supplied-content stub.
    Evidence {
        id: evidence.id,
        timestamp_ms: evidence.timestamp_ms,
        process_name: evidence.process_name.clone(),
        window_title: evidence.window_title.clone(),
        page_url: evidence.page_url.clone(),
        text,
        supplied_ranges,
        segment: evidence.segment.clone(),
        context: evidence.context.clone(),
        novelty: evidence.novelty,
        span_ms: evidence.span_ms,
        screening: evidence.screening.clone(),
    }
}

pub(super) fn unread(
    evidence: &Evidence,
    wanted: &[(usize, usize)],
    seen: Option<&Evidence>,
) -> Evidence {
    let known = seen.map(supplied).unwrap_or_default();
    let count = evidence.text.chars().count();
    let wanted = merge(
        wanted
            .iter()
            .map(|&(start, end)| (start.min(count), end.min(count)))
            .collect(),
    );
    let ranges = subtract(&wanted, &known);
    let mut chars = evidence.text.chars();
    let mut cursor = 0;
    let mut fragments = Vec::new();
    let mut supplied_ranges = Vec::new();
    for (start, end) in ranges {
        for _ in cursor..start {
            chars.next();
        }
        let fragment = chars.by_ref().take(end - start).collect::<String>();
        cursor = end;
        if !fragment.trim().is_empty() {
            fragments.push(fragment);
            supplied_ranges.push((start, end));
        }
    }
    with_text(evidence, fragments.join("\n[…]\n"), supplied_ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_unicode_excerpts_cover_only_the_original_positions_they_supply() {
        let text = "  甲😀乙  \r\n\r\n  丙丁  \r\n甲😀乙\n尾部";
        let (body, ranges) = excerpt(text, None, 900);
        assert_eq!(body, "甲😀乙\n丙丁\n尾部");
        let chars = text.chars().collect::<Vec<_>>();
        let supplied = ranges
            .iter()
            .map(|&(start, end)| chars[start..end].iter().collect::<String>())
            .collect::<Vec<_>>();
        assert_eq!(supplied, ["甲😀乙", "丙丁", "尾部"]);
        assert_eq!(
            subtract(&[(0, chars.len())], &ranges)
                .iter()
                .map(|&(start, end)| end - start)
                .sum::<usize>(),
            chars.len() - 7
        );
    }
}
