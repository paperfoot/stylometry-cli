//! Embedded English lexicons: function words, a general-English frequency
//! baseline, synonym groups where the choice is a matter of voice, and words
//! over-represented in LLM-generated prose. All parsed once, on first use.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const FUNCTION_WORDS: &str = include_str!("data/function_words.txt");
const ENGLISH_ZIPF: &str = include_str!("data/english_zipf.tsv");
const SYNONYMS: &str = include_str!("data/synonyms.txt");
const LLM_TELLS: &str = include_str!("data/llm_tells.txt");

fn lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

static FUNCTION_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| lines(FUNCTION_WORDS).collect());

static ZIPF: LazyLock<HashMap<&'static str, f32>> = LazyLock::new(|| {
    lines(ENGLISH_ZIPF)
        .filter_map(|l| {
            let (w, z) = l.split_once('\t')?;
            Some((w, z.parse().ok()?))
        })
        .collect()
});

static GROUPS: LazyLock<Vec<Vec<&'static str>>> = LazyLock::new(|| {
    lines(SYNONYMS)
        .map(|l| {
            l.split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .collect()
        })
        .filter(|g: &Vec<&str>| g.len() >= 2)
        .collect()
});

static GROUP_OF: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for (i, g) in GROUPS.iter().enumerate() {
        for w in g {
            m.entry(*w).or_insert(i);
        }
    }
    m
});

static TELLS: LazyLock<Vec<&'static str>> = LazyLock::new(|| lines(LLM_TELLS).collect());

/// Articles, pronouns, prepositions, conjunctions, auxiliaries and the like.
pub fn is_function_word(w: &str) -> bool {
    FUNCTION_SET.contains(w)
}

/// Zipf frequency of a word in general English (log10 per billion words;
/// "the" ≈ 7.7, "however" ≈ 5.6, "delve" ≈ 3.1). None if outside the
/// 50k-word table, i.e. rarer than about Zipf 2.5.
pub fn zipf(w: &str) -> Option<f32> {
    ZIPF.get(w).copied()
}

/// Expected occurrences per million words in general English. Words outside
/// the table get a floor just below the table's rarest entries.
pub fn per_million(w: &str) -> f64 {
    let z = zipf(w).map(f64::from).unwrap_or(2.0);
    10f64.powf(z - 3.0)
}

/// All synonym groups. Members are lowercase; multi-word members are phrases.
pub fn synonym_groups() -> &'static [Vec<&'static str>] {
    &GROUPS
}

/// Index of the synonym group containing `w`, if any.
pub fn group_of(w: &str) -> Option<usize> {
    GROUP_OF.get(w).copied()
}

/// LLM-tell words and phrases (lowercase; phrases contain spaces).
pub fn llm_tells() -> &'static [&'static str] {
    &TELLS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexicons_load() {
        assert!(is_function_word("the"));
        assert!(!is_function_word("whale"));
        assert!(zipf("the").unwrap() > 7.0);
        assert!(zipf("don't").is_some());
        assert!(per_million("the") > per_million("however"));
        let g = group_of("however").unwrap();
        assert!(synonym_groups()[g].contains(&"but"));
        assert!(llm_tells().contains(&"delve"));
    }

    #[test]
    fn every_synonym_member_maps_to_one_group() {
        let mut seen = HashMap::new();
        for (i, g) in synonym_groups().iter().enumerate() {
            for w in g {
                if let Some(prev) = seen.insert(*w, i) {
                    panic!("'{w}' is in groups {prev} and {i}");
                }
            }
        }
    }
}
