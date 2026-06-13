//! Feature counting: turn a text into word-frequency and char-trigram-frequency
//! maps. Selection of the *shared* feature set (the most-frequent-word list and
//! the most-frequent-trigram list) lives in `model`, because it must be computed
//! across the whole reference set, not a single text.

use std::collections::HashMap;

use crate::engine::text;

/// Raw integer counts of word tokens.
pub fn word_counts(s: &str) -> HashMap<String, u64> {
    let mut m: HashMap<String, u64> = HashMap::new();
    for w in text::word_tokens(s) {
        *m.entry(w).or_insert(0) += 1;
    }
    m
}

/// Raw integer counts of character trigrams.
pub fn trigram_counts(s: &str) -> HashMap<String, u64> {
    let mut m: HashMap<String, u64> = HashMap::new();
    for g in text::char_trigrams(s) {
        *m.entry(g).or_insert(0) += 1;
    }
    m
}

/// Sum of all values in a count map.
pub fn total(counts: &HashMap<String, u64>) -> u64 {
    counts.values().sum()
}

/// Keep only the `keep` highest-count entries (ties broken lexicographically
/// for determinism). Bounds stored profile size without losing the signal,
/// since stylometry lives in the frequent end of the distribution.
pub fn prune_top(counts: HashMap<String, u64>, keep: usize) -> HashMap<String, u64> {
    if counts.len() <= keep {
        return counts;
    }
    let mut v: Vec<(String, u64)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(keep);
    v.into_iter().collect()
}

/// Relative frequency of a single feature given its count and a total.
#[inline]
pub fn rel_freq(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}
