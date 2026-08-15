//! Query planning for OCR text search.
//!
//! Turns a query string into the exact index lookups it needs and the needles
//! the reranker matches decrypted text against. Nothing here touches the
//! database, so every decision the planner makes is checkable by a unit test —
//! which is what the previous implementation lacked when it quietly swapped
//! bigram intersection for a union.

use std::collections::HashSet;

use super::StorageState;

/// Case forms tried per character.
///
/// The bitmap index stores bigrams exactly as they were captured, so text
/// reading `Separation` contributes `Se` while a query typed as `separation`
/// asks for `se` and misses. Rebuilding a multi-gigabyte index to normalise
/// case is not on the table, so the *query* expands each position instead:
/// the spelling as typed plus the other case, which for an ordinary letter
/// covers both spellings the index could hold.
const MAX_CASE_FORMS_PER_CHAR: usize = 2;

/// Characters the index keeps.
///
/// Deliberately identical to the filter in
/// [`StorageState::bigram_tokenize`]: whitespace and punctuation are dropped
/// before bigrams are cut, on the write path as much as here. That is why the
/// index holds cross-word bigrams — `Six Degrees` contributes `xD` — and why a
/// phrase query is allowed to ask for them.
pub(super) fn is_indexable(ch: char) -> bool {
    ch.is_alphanumeric() || StorageState::is_cjk(ch)
}

/// The case spellings of one character, as typed first.
///
/// A character whose case mapping expands to several characters (`ß` uppercases
/// to `SS`) would change the bigram's length, so it is left as typed. A
/// titlecase character has three forms and is truncated to two, which costs a
/// little recall on `ǅ` and keeps the lookup count bounded everywhere else.
fn case_forms(ch: char) -> Vec<char> {
    fn single(mut mapped: impl Iterator<Item = char>) -> Option<char> {
        let first = mapped.next()?;
        mapped.next().is_none().then_some(first)
    }

    let mut forms = vec![ch];
    for mapped in [single(ch.to_lowercase()), single(ch.to_uppercase())]
        .into_iter()
        .flatten()
    {
        if !forms.contains(&mapped) {
            forms.push(mapped);
        }
    }
    forms.truncate(MAX_CASE_FORMS_PER_CHAR);
    forms
}

/// One bigram of the query together with every spelling worth looking up.
///
/// The spelling as typed comes first, so a caller that only wants one lookup
/// still gets the most likely hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BigramGroup {
    pub bigram: String,
    pub variants: Vec<String>,
}

/// Every case spelling of one bigram, at most `MAX_CASE_FORMS_PER_CHAR`
/// squared of them.
fn case_variants(first: char, second: char) -> Vec<String> {
    let mut variants = Vec::with_capacity(MAX_CASE_FORMS_PER_CHAR * MAX_CASE_FORMS_PER_CHAR);
    for a in case_forms(first) {
        for b in case_forms(second) {
            let variant: String = [a, b].iter().collect();
            if !variants.contains(&variant) {
                variants.push(variant);
            }
        }
    }
    variants
}

/// Cuts `text` into bigram groups, in order of first appearance and without
/// repeats.
///
/// Order matters only for readability — every caller re-sorts by posting-list
/// size before doing any work — but a stable order keeps tests legible.
pub(super) fn bigram_groups(text: &str) -> Vec<BigramGroup> {
    let chars: Vec<char> = text.chars().filter(|ch| is_indexable(*ch)).collect();
    if chars.len() < 2 {
        return Vec::new();
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut groups = Vec::with_capacity(chars.len() - 1);
    for window in chars.windows(2) {
        let bigram: String = window.iter().collect();
        if !seen.insert(bigram.clone()) {
            continue;
        }
        groups.push(BigramGroup {
            variants: case_variants(window[0], window[1]),
            bigram,
        });
    }
    groups
}

/// Lowercases and strips `text` down to the characters the index keeps.
///
/// Both the query needles and the decrypted text run through this, so a match
/// found here means the same thing the bitmap index meant: the characters are
/// adjacent once spacing and punctuation are removed.
pub(super) fn fold(text: &str) -> Vec<char> {
    text.chars()
        .filter(|ch| is_indexable(*ch))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// A query broken into what the index can answer and what the reranker checks.
pub(super) struct QueryPlan {
    /// The whole query folded to a needle, used both as the phrase to look for
    /// in decrypted text and — for a one-word query — as the only keyword.
    pub phrase: Vec<char>,
    /// One folded needle per whitespace-separated word.
    pub keywords: Vec<Vec<char>>,
    /// Bigrams of the whole query, cross-word ones included. This is the
    /// narrowest thing the index can be asked, because a text block containing
    /// the phrase verbatim contains every one of them.
    pub phrase_groups: Vec<BigramGroup>,
    /// Bigrams per keyword. Asking for their union instead of `phrase_groups`
    /// drops the cross-word bigrams, which is what makes reordered or reworded
    /// hits reachable.
    pub keyword_groups: Vec<Vec<BigramGroup>>,
}

impl QueryPlan {
    pub(super) fn build(query: &str) -> Self {
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut keywords = Vec::with_capacity(words.len());
        let mut keyword_groups = Vec::with_capacity(words.len());
        for word in words {
            let groups = bigram_groups(word);
            if groups.is_empty() {
                // Too short to have produced an index entry; it can still be
                // scored against decrypted text, so keep the needle.
                let folded = fold(word);
                if !folded.is_empty() {
                    keywords.push(folded);
                }
                continue;
            }
            keywords.push(fold(word));
            keyword_groups.push(groups);
        }

        Self {
            phrase: fold(query),
            keywords,
            phrase_groups: bigram_groups(query),
            keyword_groups,
        }
    }

    /// Whether the index can be asked anything at all. A one-character query
    /// produces no bigram, and the index stores nothing else.
    pub(super) fn has_index_terms(&self) -> bool {
        !self.phrase_groups.is_empty()
    }

    /// Whether the keyword pass says anything the phrase pass did not.
    ///
    /// For a single-word query the two group sets are identical, so running
    /// both would be the same intersection twice.
    pub(super) fn has_distinct_keyword_pass(&self) -> bool {
        self.keyword_groups.len() > 1
    }

    /// The keyword groups as one list, deduplicated.
    ///
    /// Intersecting this flat list is what lets the rarest bigram of *any*
    /// keyword narrow the candidate set first, instead of each keyword being
    /// resolved on its own and only then combined.
    pub(super) fn flat_keyword_groups(&self) -> Vec<&BigramGroup> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut flat = Vec::new();
        for groups in &self.keyword_groups {
            for group in groups {
                if seen.insert(group.bigram.as_str()) {
                    flat.push(group);
                }
            }
        }
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bigrams_of(text: &str) -> Vec<String> {
        bigram_groups(text)
            .into_iter()
            .map(|group| group.bigram)
            .collect()
    }

    #[test]
    fn case_variants_cover_both_spellings_of_each_letter() {
        let groups = bigram_groups("Se");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].bigram, "Se");
        // As typed first, then the spellings the index could hold instead.
        assert_eq!(groups[0].variants, vec!["Se", "SE", "se", "sE"]);
    }

    #[test]
    fn non_letters_expand_to_a_single_variant() {
        assert_eq!(bigram_groups("12")[0].variants, vec!["12"]);
        assert_eq!(bigram_groups("搜索")[0].variants, vec!["搜索"]);
    }

    #[test]
    fn a_query_and_its_lowercase_ask_for_the_same_postings() {
        // The defect this expansion exists for: `Separation` was indexed, so
        // the index holds `Se`; a query typed in lowercase asks for `se` and
        // used to miss. Both spellings now reach the same lookup set, which is
        // what makes `separation` and `Separation` return the same results.
        let upper: HashSet<String> = bigram_groups("Separation")
            .into_iter()
            .flat_map(|group| group.variants)
            .collect();
        let lower: HashSet<String> = bigram_groups("separation")
            .into_iter()
            .flat_map(|group| group.variants)
            .collect();
        assert_eq!(upper, lower);
        assert!(lower.contains("Se"));
        assert!(lower.contains("se"));
    }

    #[test]
    fn phrase_bigrams_span_word_boundaries() {
        // The write path filters spaces out before cutting bigrams, so the
        // index really does hold `xD`. The old query path split on whitespace
        // first and threw away the phrase's most selective signal.
        let phrase = bigrams_of("Six Degrees of Separation");
        assert!(phrase.contains(&"xD".to_string()));
        assert!(phrase.contains(&"so".to_string()));
        assert!(phrase.contains(&"fS".to_string()));

        let plan = QueryPlan::build("Six Degrees of Separation");
        let per_keyword: HashSet<&str> = plan
            .flat_keyword_groups()
            .iter()
            .map(|group| group.bigram.as_str())
            .collect();
        assert!(!per_keyword.contains("xD"));
        // Every keyword bigram is also a phrase bigram, which is why the
        // phrase pass is the narrower of the two.
        let phrase_set: HashSet<&str> = phrase.iter().map(String::as_str).collect();
        assert!(per_keyword.is_subset(&phrase_set));
    }

    #[test]
    fn repeated_bigrams_are_looked_up_once() {
        // `banana` cuts to ba, an, na, an, na.
        assert_eq!(bigrams_of("banana"), vec!["ba", "an", "na"]);
    }

    #[test]
    fn folding_matches_what_the_index_kept() {
        assert_eq!(
            fold("Six Degrees, of Separation!"),
            fold("sixdegreesofseparation")
        );
        assert_eq!(fold("  "), Vec::<char>::new());
    }

    #[test]
    fn single_character_queries_are_not_indexable() {
        let plan = QueryPlan::build("图");
        assert!(!plan.has_index_terms());
        // The needle survives even though no posting list can be asked for it.
        assert_eq!(plan.phrase, vec!['图']);

        let two = QueryPlan::build("图片");
        assert!(two.has_index_terms());
    }

    #[test]
    fn a_single_word_query_has_no_separate_keyword_pass() {
        let one = QueryPlan::build("separation");
        assert!(!one.has_distinct_keyword_pass());
        assert_eq!(one.keywords, vec![fold("separation")]);

        let many = QueryPlan::build("six degrees");
        assert!(many.has_distinct_keyword_pass());
    }

    #[test]
    fn short_words_keep_their_needle_without_a_posting_list() {
        // `a` is too short to produce a bigram, but dropping it from the
        // needles would let a hit for `of` alone claim to match `a of`.
        let plan = QueryPlan::build("a separation");
        assert_eq!(plan.keywords.len(), 2);
        assert_eq!(plan.keyword_groups.len(), 1);
        assert!(!plan.has_distinct_keyword_pass());
    }
}
