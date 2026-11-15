//! Matching a spoken phrase against the text the library stores.
//!
//! Alexa hands over what it heard, not what was typed: no punctuation, spacing
//! by its own rules, and casing that means nothing. So neither side is compared
//! as written — both are folded down to the characters that carry the name, and
//! the score says how much of one the other accounts for.

/// The score of a field that is the query and nothing else. Named because
/// callers short-circuit on it: nothing can beat an exact match in its tier.
pub(super) const EXACT: u32 = 100;

/// How well `field` answers a query already reduced by [`fold`]: the whole
/// name, the start of it, or a mention inside. `None` when unrelated.
pub(super) fn match_score(field: &str, folded_query: &str) -> Option<u32> {
    let field = fold(field);
    // An empty query would otherwise be a prefix of everything.
    if folded_query.is_empty() || field.is_empty() {
        return None;
    }
    if field == folded_query {
        Some(EXACT)
    } else if field.starts_with(folded_query) {
        Some(70)
    } else if field.contains(folded_query) {
        Some(50)
    } else {
        None
    }
}

/// Reduce a string to its lowercase letters and digits: punctuation and
/// spacing never agree between a written title and speech recognition.
pub(super) fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The highest-scoring item, or None if `score` answers for none. Ties go to
/// the earliest, so a phrase always resolves to the same thing in the caller's
/// order.
pub(super) fn best_scored<T>(
    items: impl IntoIterator<Item = T>,
    score: impl Fn(&T) -> Option<u32>,
) -> Option<T> {
    let mut best: Option<(u32, T)> = None;
    for item in items {
        let Some(score) = score(&item) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, item));
        }
    }
    best.map(|(_, item)| item)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Score a field against an unfolded query, as the callers' own folding does.
    fn score(field: &str, query: &str) -> Option<u32> {
        match_score(field, &fold(query))
    }

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let exact = score("Never Gonna Give You Up", "never gonna give you up").unwrap();
        let prefix = score("Never Gonna Give You Up", "never gonna").unwrap();
        let inside = score("Never Gonna Give You Up", "give you").unwrap();
        assert_eq!(exact, EXACT);
        assert!(exact > prefix && prefix > inside);
    }

    #[test]
    fn separators_and_case_do_not_decide_a_match() {
        // Punctuation the speaker never uttered, and spacing neither side agrees on
        assert_eq!(
            score("【MV】Hello, World! - Official", "hello world"),
            Some(50)
        );
        assert_eq!(score("けもの フレンズ", "けものフレンズ"), Some(EXACT));
        assert_eq!(score("YOASOBI", "yoasobi"), Some(EXACT));
    }

    #[test]
    fn unrelated_text_scores_nothing() {
        assert_eq!(score("Never Gonna Give You Up", "bohemian"), None);
        // An empty or punctuation-only query must not match everything
        assert_eq!(score("anything", ""), None);
        assert_eq!(score("anything", "?!"), None);
        // Nor may an empty field be matched by a real query
        assert_eq!(score("", "anything"), None);
    }

    #[test]
    fn the_best_score_wins_and_ties_go_to_the_earliest() {
        let items = ["a", "bb", "cc", "d"];
        assert_eq!(
            best_scored(items, |s| match s.len() {
                1 => Some(1),
                _ => Some(2),
            }),
            Some("bb")
        );
        // Items the scorer declines are passed over entirely
        assert_eq!(best_scored(items, |s| (*s == "d").then_some(1)), Some("d"));
        assert_eq!(best_scored(items, |_| None), None);
        assert_eq!(best_scored(Vec::<&str>::new(), |_| Some(1)), None);
    }
}
