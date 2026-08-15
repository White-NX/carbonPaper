//! Plaintext reranking for OCR text search.
//!
//! The bitmap index can only say which bigrams a text block contains, never
//! whether they sat next to each other. Ranking on that alone is what let a
//! paragraph that happens to scatter all nine bigrams of `Separation` tie with
//! a block that actually says the word. Once a candidate's text is decrypted
//! there is no reason to keep guessing, so every ordering decision that
//! reaches the user is made here, against the real characters.
//!
//! Checking against real characters must not mean demanding them literally.
//! OCR misreads, and so do the people typing the query: `Separtion` and
//! `sepation` are both meant to find `Separation`, and `中华人共和国` is meant
//! to find `中华人民共和国`. So a needle that is not present verbatim is looked
//! for again with a small edit budget, and a match found that way is scored —
//! lower than a literal one, never zero. Rejection is reserved for text the
//! query does not resemble at all, which is what the scattered-bigram false
//! positives look like.

use super::search_plan::{is_indexable, QueryPlan};

/// The whole query appears, spacing and punctuation ignored. This is the
/// signal the index was being asked to approximate, and it outranks
/// everything else by a wide margin.
const W_PHRASE: f64 = 1000.0;
/// …and it appears as whole words rather than inside a longer one.
const W_PHRASE_WORD: f64 = 400.0;
/// Per keyword found anywhere in the block.
const W_KEYWORD: f64 = 200.0;
/// …and found on its own word boundaries.
const W_KEYWORD_WORD: f64 = 80.0;
/// Every keyword is present, even if scattered.
const W_ALL_KEYWORDS: f64 = 300.0;
/// How much of the block the match accounts for. A caption that is the phrase
/// says more than a page of prose mentioning it once.
const W_DENSITY: f64 = 120.0;
/// How early the first hit is. Weak on purpose: it only breaks ties between
/// blocks that matched equally well.
const W_EARLINESS: f64 = 60.0;

/// Ceiling for the evidence a candidate carries when its text could not be
/// checked at all.
///
/// Deliberately below [`W_KEYWORD`]: a block whose text bears the query out —
/// even approximately — always outranks one that merely shares most of its
/// bigrams.
pub(super) const W_FUZZY_PRIOR: f64 = 150.0;

/// Characters a needle must have per edit it is allowed to absorb.
///
/// One dropped character costs one edit, so this is the shortest word in which
/// a single typo is tolerated. Four is the loosest setting that still refuses
/// three-character words, where a single edit reaches so many other words that
/// the match would say nothing. It is also what the worked examples need:
/// `sepation` (eight characters, two edits away from `separation`) and
/// `中华人共和国` (six characters, one edit away from `中华人民共和国`) both
/// come out reachable.
const CHARS_PER_EDIT: usize = 4;

/// …and no needle absorbs more than this, however long it is. Past three edits
/// the match stops being a misspelling of the query and starts being a
/// different phrase.
const MAX_EDITS: usize = 3;

/// What the best possible approximate match is worth against a literal one.
///
/// Held well under 1.0 so that no amount of approximate evidence overtakes a
/// block the reader can see contains the word — the user asked for the near
/// misses to be found, and to be found *below* the exact hits.
const NEAR_QUALITY_CEILING: f64 = 0.55;

/// Matrix cells one approximate match may fill.
///
/// Approximate matching costs needle length times text length. An OCR text
/// block is a line or a short paragraph, so this is never reached in practice;
/// it exists so that one pathological row cannot stall a search.
const APPROX_CELL_BUDGET: usize = 1 << 20;

/// How many edits a needle of this length is allowed to absorb.
pub(super) fn edit_budget(len: usize) -> usize {
    (len / CHARS_PER_EDIT).min(MAX_EDITS)
}

/// Where a needle was found literally.
struct Located {
    /// Offset of the first occurrence, in folded characters.
    at: usize,
    /// Whether any occurrence stood on its own word boundaries.
    word_bounded: bool,
}

/// Where a needle was found, and how literally.
struct Match {
    /// Offset of the match in folded characters. Feeds the earliness signal
    /// only, so for an approximate match an estimate is good enough.
    at: usize,
    /// Whether the match stood on its own word boundaries. Only a literal
    /// match claims this: an approximate one has no exact extent to test, and
    /// its reduced quality already keeps it below the literal hits that would
    /// have earned the bonus.
    word_bounded: bool,
    /// 1.0 for a literal match, less for one that needed edits.
    quality: f64,
}

/// Decrypted text reduced to the characters the index kept, remembering where
/// the dropped ones were.
///
/// Folding both sides the same way is what makes a match here mean what a
/// bitmap hit meant: `Six Degrees of Separation` and `six-degrees-of-
/// separation` fold to the same characters. The dropped positions are still
/// worth knowing, because they are exactly the word boundaries.
pub(super) struct FoldedText {
    chars: Vec<char>,
    /// `breaks[i]` is true when position `i` begins a word — the character
    /// before it in the original text was dropped by folding, or there was no
    /// character before it.
    breaks: Vec<bool>,
}

impl FoldedText {
    pub(super) fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut breaks = Vec::with_capacity(text.len());
        let mut at_break = true;
        for ch in text.chars() {
            if !is_indexable(ch) {
                at_break = true;
                continue;
            }
            for lowered in ch.to_lowercase() {
                chars.push(lowered);
                breaks.push(at_break);
                // Only the first character of an expanded case mapping starts
                // the word.
                at_break = false;
            }
        }
        Self { chars, breaks }
    }

    pub(super) fn len(&self) -> usize {
        self.chars.len()
    }

    fn bounded_at(&self, at: usize, len: usize) -> bool {
        self.breaks[at] && (at + len == self.chars.len() || self.breaks[at + len])
    }

    fn locate(&self, needle: &[char]) -> Option<Located> {
        if needle.is_empty() || needle.len() > self.chars.len() {
            return None;
        }
        let mut found: Option<Located> = None;
        for at in 0..=self.chars.len() - needle.len() {
            if &self.chars[at..at + needle.len()] != needle {
                continue;
            }
            let bounded = self.bounded_at(at, needle.len());
            match found.as_mut() {
                // Keep the earliest offset, but let a later occurrence prove
                // the word-boundary property.
                Some(existing) => existing.word_bounded |= bounded,
                None => {
                    found = Some(Located {
                        at,
                        word_bounded: bounded,
                    })
                }
            }
            if found.as_ref().is_some_and(|hit| hit.word_bounded) {
                break;
            }
        }
        found
    }

    /// The fewest edits that turn `needle` into some substring of this text,
    /// with the position that substring ends at — provided the count stays
    /// within `max_edits`.
    ///
    /// This is Sellers' variant of edit distance: the first row of the matrix
    /// is all zeroes, which lets a match begin anywhere in the text, and the
    /// answer is the smallest value in the last row, which lets it end
    /// anywhere. Insertions, deletions and substitutions all cost one, so a
    /// dropped character (`sepation` for `separation`), a doubled one and a
    /// misread one (`5eparation`) are treated alike — which is what makes the
    /// same budget cover both a typing slip and an OCR error.
    ///
    /// Only two rows are ever live, so the memory cost is the text length
    /// rather than the whole matrix.
    fn locate_approx(&self, needle: &[char], max_edits: usize) -> Option<(usize, usize)> {
        let (rows, columns) = (needle.len(), self.chars.len());
        if rows == 0 || columns == 0 || max_edits == 0 {
            return None;
        }
        if rows.saturating_mul(columns) > APPROX_CELL_BUDGET {
            return None;
        }

        let mut previous: Vec<usize> = vec![0; columns + 1];
        let mut current: Vec<usize> = vec![0; columns + 1];
        for row in 1..=rows {
            // Matching a non-empty needle against nothing costs one deletion
            // per character.
            current[0] = row;
            for column in 1..=columns {
                let substitute =
                    previous[column - 1] + usize::from(needle[row - 1] != self.chars[column - 1]);
                let delete = previous[column] + 1;
                let insert = current[column - 1] + 1;
                current[column] = substitute.min(delete).min(insert);
            }
            std::mem::swap(&mut previous, &mut current);
        }

        // Ties go to the earliest end position, which keeps the earliness
        // signal stable when a word repeats.
        let (end, distance) = previous
            .iter()
            .enumerate()
            .skip(1)
            .min_by_key(|(column, cost)| (**cost, *column))?;
        (*distance <= max_edits).then_some((end, *distance))
    }

    /// Looks for `needle` literally, then — failing that — within the edit
    /// budget its length earns.
    fn find(&self, needle: &[char]) -> Option<Match> {
        if let Some(hit) = self.locate(needle) {
            return Some(Match {
                at: hit.at,
                word_bounded: hit.word_bounded,
                quality: 1.0,
            });
        }

        let (end, distance) = self.locate_approx(needle, edit_budget(needle.len()))?;
        Some(Match {
            // The match spans roughly the needle's length back from where it
            // ended; being off by the edit count does not matter to a signal
            // that only breaks ties.
            at: end.saturating_sub(needle.len()),
            word_bounded: false,
            quality: NEAR_QUALITY_CEILING * (1.0 - distance as f64 / needle.len() as f64),
        })
    }
}

/// What one candidate's decrypted text is worth for this query.
pub(super) struct RankedText {
    pub score: f64,
    /// Whether the text bears the query out — containing one of its words
    /// either literally or within the edit budget. A candidate that fails
    /// this is a bitmap false positive: it holds the right bigrams in the
    /// wrong order, and reads as nothing like the query.
    pub matched: bool,
    /// Whether at least one of those matches was literal. The difference is
    /// only reported, never ranked on: the quality multiplier has already
    /// placed the approximate matches below the exact ones.
    pub literal: bool,
}

impl RankedText {
    const NOTHING: Self = Self {
        score: 0.0,
        matched: false,
        literal: false,
    };
}

/// Scores decrypted text against the query.
pub(super) fn score_text(plan: &QueryPlan, text: &str) -> RankedText {
    let folded = FoldedText::new(text);
    score_folded(plan, &folded)
}

pub(super) fn score_folded(plan: &QueryPlan, folded: &FoldedText) -> RankedText {
    let length = folded.len();
    if length == 0 || plan.phrase.is_empty() {
        return RankedText::NOTHING;
    }

    let phrase_match = folded.find(&plan.phrase);
    let mut score = 0.0;
    if let Some(hit) = phrase_match.as_ref() {
        score += W_PHRASE * hit.quality;
        if hit.word_bounded {
            score += W_PHRASE_WORD;
        }
    }

    let mut hits = 0usize;
    let mut literal = false;
    let mut quality_total = 0.0;
    let mut matched_chars = 0usize;
    let mut earliest = usize::MAX;
    for keyword in &plan.keywords {
        // A one-word query has the phrase for its only keyword; searching for
        // it twice would run the approximate matcher twice for one answer.
        let found = if *keyword == plan.phrase {
            phrase_match.as_ref()
        } else {
            None
        };
        let owned;
        let hit = match found {
            Some(hit) => hit,
            None => match folded.find(keyword) {
                Some(hit) => {
                    owned = hit;
                    &owned
                }
                None => continue,
            },
        };

        hits += 1;
        literal |= hit.quality >= 1.0;
        quality_total += hit.quality;
        matched_chars += keyword.len();
        earliest = earliest.min(hit.at);
        score += W_KEYWORD * hit.quality;
        if hit.word_bounded {
            score += W_KEYWORD_WORD;
        }
    }

    if hits == 0 {
        return RankedText::NOTHING;
    }
    if hits == plan.keywords.len() {
        // Scaled by how literal the matches were, so a block holding every
        // word outright beats one that only approximates them.
        score += W_ALL_KEYWORDS * (quality_total / hits as f64);
    }

    let density = (matched_chars as f64 / length as f64).min(1.0);
    score += W_DENSITY * density;
    score += W_EARLINESS * (1.0 - earliest as f64 / length as f64);

    RankedText {
        score,
        matched: true,
        literal,
    }
}

/// The score a candidate carries when its text says nothing — either the row
/// failed to decrypt, or the typo-tolerant pass proposed it and the query does
/// not resemble its text closely enough to be scored.
///
/// `hits` of `total` bigram groups matched. Used only for the tail of a search
/// that found too little to fill a page; see `search.rs::rank_and_page`.
pub(super) fn fuzzy_prior(hits: u32, total: u32) -> f64 {
    if total == 0 {
        return 0.0;
    }
    W_FUZZY_PRIOR * (hits as f64 / total as f64).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::search_plan::fold;

    fn needle(text: &str) -> Vec<char> {
        fold(text)
    }

    fn score(query: &str, text: &str) -> f64 {
        score_text(&QueryPlan::build(query), text).score
    }

    fn matched(query: &str, text: &str) -> bool {
        score_text(&QueryPlan::build(query), text).matched
    }

    #[test]
    fn scattered_bigrams_without_the_word_score_nothing() {
        // Every bigram of `separation` — se ep pa ar ra at ti io on — appears
        // in this sentence, and the word does not. Under the old ranking it
        // scored a perfect nine and, being newer, sorted above real hits. It
        // is also far enough from the word that the edit budget does not
        // rescue it, which is the line between a false positive and a typo.
        let noise = "onset deep pass art ratio";
        let plan = QueryPlan::build("separation");
        for group in &plan.phrase_groups {
            let folded: String = fold(noise).into_iter().collect();
            assert!(
                folded.contains(&group.bigram.to_lowercase()),
                "fixture is supposed to contain every bigram, missing {}",
                group.bigram
            );
        }

        assert_eq!(score("separation", noise), 0.0);
        assert!(!matched("separation", noise));
        assert!(score("separation", "Six Degrees of Separation") > 0.0);
    }

    #[test]
    fn a_misspelled_query_still_finds_the_word() {
        // The two spellings the user asked for by name. `Separtion` drops one
        // character, `sepation` drops two.
        let text = "Six Degrees of Separation";
        for typo in ["Separtion", "sepation", "seperation", "Separatlon"] {
            assert!(
                matched(typo, text),
                "{typo} should still bear out against {text}"
            );
            assert!(score(typo, text) > 0.0, "{typo} scored nothing");
        }
    }

    #[test]
    fn a_missing_character_still_finds_the_chinese_phrase() {
        let text = "中华人民共和国宪法";
        assert!(matched("中华人共和国", text));
        assert!(score("中华人共和国", text) > 0.0);
        // A phrase that merely shares characters is not a misspelling of it.
        assert_eq!(score("中华人共和国", "华人共同体的国民"), 0.0);
    }

    #[test]
    fn one_ocr_substitution_still_finds_a_four_character_phrase() {
        assert!(matched("卫戍协议", "卫成协议"));
        assert!(score("卫戍协议", "卫成协议") > 0.0);
    }

    #[test]
    fn an_exact_spelling_outranks_every_misspelling_of_it() {
        let text = "Six Degrees of Separation";
        let exact = score("Separation", text);
        for typo in ["Separtion", "sepation", "seperation"] {
            let near = score(typo, text);
            assert!(near > 0.0, "{typo} scored nothing");
            assert!(
                exact > near,
                "exact {exact} should outrank {typo} at {near}"
            );
        }

        let exact_cjk = score("中华人民共和国", "中华人民共和国宪法");
        let near_cjk = score("中华人共和国", "中华人民共和国宪法");
        assert!(near_cjk > 0.0);
        assert!(exact_cjk > near_cjk);
    }

    #[test]
    fn a_closer_misspelling_outranks_a_looser_one() {
        let text = "Six Degrees of Separation";
        assert!(score("Separtion", text) > score("sepation", text));
    }

    #[test]
    fn short_words_tolerate_nothing() {
        // One edit on a three-character word reaches most other three-letter
        // words, so the match would carry no information.
        assert_eq!(edit_budget(3), 0);
        assert_eq!(edit_budget(4), 1);
        assert_eq!(edit_budget(8), 2);
        assert_eq!(edit_budget(400), MAX_EDITS);
        assert_eq!(score("cat", "the car is red"), 0.0);
    }

    #[test]
    fn approximate_matching_treats_every_kind_of_slip_alike() {
        let folded = FoldedText::new("Six Degrees of Separation");
        for (typo, edits) in [
            ("separtion", 1),   // dropped a character
            ("separaation", 1), // doubled one
            ("sepqration", 1),  // misread one
            ("sepation", 2),    // dropped two
        ] {
            let (_, distance) = folded
                .locate_approx(&needle(typo), MAX_EDITS)
                .unwrap_or_else(|| panic!("{typo} should be within {MAX_EDITS} edits"));
            assert_eq!(distance, edits, "unexpected distance for {typo}");
        }
    }

    #[test]
    fn a_whole_word_outranks_the_same_word_inside_another() {
        let whole = score("separation", "the separation of powers");
        let inside = score("separation", "inseparationism as a doctrine");
        assert!(
            whole > inside,
            "whole word {whole} should outrank embedded {inside}"
        );
    }

    #[test]
    fn the_phrase_outranks_its_words_scattered() {
        let phrase = score("six degrees", "Six Degrees of Separation");
        let scattered = score("six degrees", "six of them, ninety degrees apart");
        assert!(
            phrase > scattered,
            "phrase {phrase} should outrank scattered {scattered}"
        );
    }

    #[test]
    fn every_keyword_present_outranks_only_some() {
        let all = score("six degrees", "six of them, ninety degrees apart");
        let one = score("six degrees", "six of them, ninety apart");
        assert!(all > one);
        assert!(one > 0.0);
    }

    #[test]
    fn ranking_ignores_case_and_separators() {
        let plain = score("six degrees", "Six Degrees");
        let punctuated = score("six degrees", "SIX-DEGREES");
        assert_eq!(plain, punctuated);
    }

    #[test]
    fn a_denser_block_outranks_a_paragraph_mentioning_it_once() {
        let caption = score("separation", "Separation");
        let paragraph = score(
            "separation",
            "The separation of concerns is a design principle that keeps each \
             part of a program responsible for one thing and nothing else.",
        );
        assert!(
            caption > paragraph,
            "caption {caption} should outrank paragraph {paragraph}"
        );
    }

    #[test]
    fn cjk_matches_without_word_boundaries() {
        let plan = QueryPlan::build("图片");
        let hit = score_text(&plan, "这张图片很清楚");
        assert!(hit.matched);
        assert!(hit.score > 0.0);
        // Two characters earn no edit budget, so a different word stays a
        // different word.
        assert_eq!(score_text(&plan, "这张照片很清楚").score, 0.0);
    }

    #[test]
    fn empty_and_undecryptable_text_scores_nothing() {
        let plan = QueryPlan::build("separation");
        assert_eq!(score_text(&plan, "").score, 0.0);
        assert!(!score_text(&plan, "").matched);
    }

    #[test]
    fn word_boundaries_survive_folding_expansions() {
        // Folding lowercases, and a few characters expand while doing so. The
        // expansion must not invent a word boundary in the middle of a word.
        let folded = FoldedText::new("İstanbul separation");
        let hit = folded
            .locate(&needle("separation"))
            .expect("needle is present");
        assert!(hit.word_bounded);
    }

    #[test]
    fn approximate_matching_gives_up_on_a_pathological_row() {
        let folded = FoldedText::new(&"a".repeat(APPROX_CELL_BUDGET));
        assert!(folded.locate_approx(&needle("separation"), 2).is_none());
    }

    #[test]
    fn fuzzy_prior_never_reaches_a_plain_keyword_hit() {
        assert!(fuzzy_prior(9, 9) < W_KEYWORD);
        assert_eq!(fuzzy_prior(0, 9), 0.0);
        assert_eq!(fuzzy_prior(1, 0), 0.0);
    }
}
