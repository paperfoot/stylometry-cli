//! Countable surface habits of a passage: sentence rhythm, punctuation,
//! dialogue, contractions. Stored per chunk as plain counts so they can be
//! summed over any span (a chunk, a work, a whole profile) and turned into
//! rates or ranges later without re-reading the corpus.

use serde::{Deserialize, Serialize};

use crate::engine::text;

/// Contractions counted as such. "'s" is excluded except after pronouns and
/// wh-words, where it is a contraction rather than a possessive.
const S_CONTRACTIONS: [&str; 14] = [
    "it's", "that's", "there's", "here's", "he's", "she's", "what's", "who's", "where's",
    "how's", "let's", "when's", "why's", "one's",
];

/// Two-word forms that could have been contracted. Their rate against
/// contractions is the writer's contraction habit.
pub const EXPANDABLE: [&str; 40] = [
    "do not", "does not", "did not", "is not", "are not", "was not", "were not", "have not",
    "has not", "had not", "will not", "would not", "could not", "should not", "must not",
    "can not", "i am", "you are", "we are", "they are", "it is", "that is", "there is",
    "i have", "you have", "we have", "they have", "i will", "you will", "he will", "she will",
    "we will", "they will", "let us", "i would", "he is", "she is", "what is", "who is",
    "i had",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StyleCounts {
    pub words: u64,
    pub sentences: u64,
    pub paragraphs: u64,
    /// Sum of sentence lengths squared (words), for variance.
    pub sentence_words_sq: u64,
    /// Sentences of 8 words or fewer / of 30 words or more.
    pub short_sentences: u64,
    pub long_sentences: u64,
    pub letters: u64,
    pub commas: u64,
    pub semicolons: u64,
    pub colons: u64,
    /// — or a spaced/doubled hyphen used as a dash (" - ", "--").
    pub em_dashes: u64,
    pub en_dashes: u64,
    /// … or "...".
    pub ellipses: u64,
    pub exclamations: u64,
    pub questions: u64,
    pub parentheses: u64,
    pub dialogue_words: u64,
    pub contractions: u64,
    pub expanded_forms: u64,
}

impl StyleCounts {
    pub fn add(&mut self, o: &StyleCounts) {
        self.words += o.words;
        self.sentences += o.sentences;
        self.paragraphs += o.paragraphs;
        self.sentence_words_sq += o.sentence_words_sq;
        self.short_sentences += o.short_sentences;
        self.long_sentences += o.long_sentences;
        self.letters += o.letters;
        self.commas += o.commas;
        self.semicolons += o.semicolons;
        self.colons += o.colons;
        self.em_dashes += o.em_dashes;
        self.en_dashes += o.en_dashes;
        self.ellipses += o.ellipses;
        self.exclamations += o.exclamations;
        self.questions += o.questions;
        self.parentheses += o.parentheses;
        self.dialogue_words += o.dialogue_words;
        self.contractions += o.contractions;
        self.expanded_forms += o.expanded_forms;
    }

    /// Count the habits of a passage given as prose paragraphs.
    pub fn of_paragraphs(paragraphs: &[String]) -> StyleCounts {
        let mut c = StyleCounts::default();
        for p in paragraphs.iter().filter(|p| !text::is_heading(p)) {
            c.paragraphs += 1;
            c.dialogue_words += text::dialogue_words(p) as u64;
            for s in text::sentences(p) {
                let n = text::count_words(&s) as u64;
                if n == 0 {
                    continue;
                }
                c.sentences += 1;
                c.sentence_words_sq += n * n;
                if n <= 8 {
                    c.short_sentences += 1;
                }
                if n >= 30 {
                    c.long_sentences += 1;
                }
            }
            count_punctuation(p, &mut c);
            let toks = text::word_tokens(p);
            c.words += toks.len() as u64;
            for (i, w) in toks.iter().enumerate() {
                c.letters += w.chars().filter(|ch| ch.is_alphabetic()).count() as u64;
                if is_contraction(w) {
                    c.contractions += 1;
                }
                if let Some(next) = toks.get(i + 1) {
                    let pair = format!("{w} {next}");
                    if EXPANDABLE.contains(&pair.as_str()) {
                        c.expanded_forms += 1;
                    }
                }
            }
        }
        c
    }

    #[cfg(test)]
    fn per_1k(&self, n: u64) -> f64 {
        if self.words == 0 {
            0.0
        } else {
            n as f64 * 1000.0 / self.words as f64
        }
    }

    #[cfg(test)]
    /// Named rates for reports and comparisons. Punctuation is per 1,000
    /// words; shares are fractions in [0, 1].
    pub fn rates(&self) -> StyleRates {
        let sents = self.sentences.max(1) as f64;
        let mean = self.words as f64 / sents;
        let var = (self.sentence_words_sq as f64 / sents - mean * mean).max(0.0);
        let contractable = self.contractions + self.expanded_forms;
        StyleRates {
            words: self.words,
            mean_sentence_words: mean,
            sd_sentence_words: var.sqrt(),
            short_sentence_share: self.short_sentences as f64 / sents,
            long_sentence_share: self.long_sentences as f64 / sents,
            mean_paragraph_sentences: self.sentences as f64 / self.paragraphs.max(1) as f64,
            mean_word_letters: self.letters as f64 / self.words.max(1) as f64,
            commas_per_1k: self.per_1k(self.commas),
            semicolons_per_1k: self.per_1k(self.semicolons),
            colons_per_1k: self.per_1k(self.colons),
            em_dashes_per_1k: self.per_1k(self.em_dashes),
            en_dashes_per_1k: self.per_1k(self.en_dashes),
            ellipses_per_1k: self.per_1k(self.ellipses),
            exclamations_per_1k: self.per_1k(self.exclamations),
            questions_per_1k: self.per_1k(self.questions),
            parentheses_per_1k: self.per_1k(self.parentheses),
            dialogue_share: self.dialogue_words as f64 / self.words.max(1) as f64,
            contraction_share: if contractable == 0 {
                None
            } else {
                Some(self.contractions as f64 / contractable as f64)
            },
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleRates {
    pub words: u64,
    pub mean_sentence_words: f64,
    pub sd_sentence_words: f64,
    pub short_sentence_share: f64,
    pub long_sentence_share: f64,
    pub mean_paragraph_sentences: f64,
    pub mean_word_letters: f64,
    pub commas_per_1k: f64,
    pub semicolons_per_1k: f64,
    pub colons_per_1k: f64,
    pub em_dashes_per_1k: f64,
    pub en_dashes_per_1k: f64,
    pub ellipses_per_1k: f64,
    pub exclamations_per_1k: f64,
    pub questions_per_1k: f64,
    pub parentheses_per_1k: f64,
    pub dialogue_share: f64,
    /// contractions / (contractions + expandable two-word forms); None when
    /// the passage has neither.
    pub contraction_share: Option<f64>,
}

pub fn is_contraction(w: &str) -> bool {
    w.ends_with("n't")
        || w.ends_with("'re")
        || w.ends_with("'ve")
        || w.ends_with("'ll")
        || w.ends_with("'m")
        || w.ends_with("'d")
        || S_CONTRACTIONS.contains(&w)
}

fn count_punctuation(p: &str, c: &mut StyleCounts) {
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            ',' => c.commas += 1,
            ';' => c.semicolons += 1,
            ':' => c.colons += 1,
            '\u{2014}' => c.em_dashes += 1,
            '\u{2013}' => c.en_dashes += 1,
            '\u{2026}' => c.ellipses += 1,
            '!' => c.exclamations += 1,
            '?' => c.questions += 1,
            '(' => c.parentheses += 1,
            '.' if chars.get(i + 1) == Some(&'.') && chars.get(i + 2) == Some(&'.') => {
                c.ellipses += 1;
                while chars.get(i + 1) == Some(&'.') {
                    i += 1;
                }
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                // "--json" (no word before, one right after) is a flag.
                let word = |j: Option<&char>| j.is_some_and(|x| x.is_alphanumeric());
                let prev = i.checked_sub(1).and_then(|j| chars.get(j));
                while chars.get(i + 1) == Some(&'-') {
                    i += 1;
                }
                if !(word(chars.get(i + 1)) && !word(prev)) {
                    c.em_dashes += 1;
                }
            }
            '-' if i > 0
                && chars[i - 1] == ' '
                && chars.get(i + 1).is_some_and(|n| *n == ' ') =>
            {
                c.em_dashes += 1
            }
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_rhythm_punctuation_and_contractions() {
        let paras = vec![
            "It wasn't late. \"Come in,\" she said; he did not move — not yet...".to_string(),
        ];
        let c = StyleCounts::of_paragraphs(&paras);
        assert_eq!(c.paragraphs, 1);
        assert_eq!(c.sentences, 2);
        assert_eq!(c.semicolons, 1);
        assert_eq!(c.em_dashes, 1);
        assert_eq!(c.ellipses, 1);
        assert_eq!(c.contractions, 1);
        assert_eq!(c.expanded_forms, 1);
        assert_eq!(c.dialogue_words, 2);
        let r = c.rates();
        assert!((r.contraction_share.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn double_hyphen_counts_as_one_dash() {
        let c = StyleCounts::of_paragraphs(&["Wait--no. Stop - now.".to_string()]);
        assert_eq!(c.em_dashes, 2);
    }

    #[test]
    fn command_line_flags_are_not_dashes() {
        let c = StyleCounts::of_paragraphs(&["Run it with --json (or --limit=5) -- then wait.".to_string()]);
        assert_eq!(c.em_dashes, 1);
    }
}
