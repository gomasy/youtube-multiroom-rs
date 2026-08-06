//! Matching a spoken phrase against the text the library stores.
//!
//! Alexa hands over what it heard, not what was typed: no punctuation, spacing
//! by its own rules, and casing that means nothing. So neither side is compared
//! as written — both are folded down to the characters that carry the name, and
//! the score says how much of one the other accounts for.
//!
//! Nothing here touches Redis. The lookups themselves live with their subject
//! (`find_track` in [`super::track`], `find_playlist` in [`super::playlist`]);
//! this is only how candidates are ranked against each other.

/// The score of a field that is the query and nothing else. Named because
/// callers short-circuit on it: nothing can beat an exact match in its tier.
pub(super) const EXACT: u32 = 100;

/// How well `field` answers a query already reduced by [`fold`], higher being
/// better. `None` when the two are unrelated, which is what keeps an
/// unrecognized phrase from playing something arbitrary.
///
/// The three tiers are what a listener would rank the same way: the whole name,
/// the start of it, and a mention somewhere inside.
///
/// The query arrives folded rather than being folded here, so a search over a
/// few hundred candidates folds it once instead of once per comparison.
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

/// Reduce a string to the characters that name it: letters and digits, in
/// lowercase. Everything else only separates words, and the two sides never
/// agree on that — a title writes "Hello, World!" where speech recognition
/// hands over "hello world", and a Japanese title spaces its words wherever it
/// likes or not at all.
pub(super) fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The highest-scoring item, or None if `score` answers for none of them.
///
/// Ties go to the earliest, which is what makes a phrase always resolve to the
/// same thing: the caller orders the candidates the way the user sees them —
/// library order, playlist creation order — and the answer follows that rather
/// than whatever the iteration happened to reach last.
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
