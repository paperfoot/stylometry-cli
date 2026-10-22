//! A voice's vocabulary: the words and phrases an author leans on, the
//! common words they avoid, which member of each synonym group they reach
//! for, how they tag dialogue, and which LLM tells they genuinely use.
//!
//! Everything is relative: signature words are measured against general
//! English (wordfreq), tells against the author's own rate. A tell the
//! author uses is voice, not slop.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::engine::lexicon;
use crate::engine::profile::Profile;
use crate::engine::style;
use crate::engine::text;

/// The author's text as token streams, one per passage, tokenized once and
/// shared by every vocabulary analysis.
pub struct Corpus<'a> {
    pub profile: &'a Profile,
    pub passages: Vec<Vec<String>>,
}

impl<'a> Corpus<'a> {
    pub fn of(profile: &'a Profile) -> Corpus<'a> {
        let passages = profile
            .chunks
            .iter()
            .map(|c| c.paragraphs.iter().flat_map(|p| text::word_tokens(p)).collect())
            .collect();
        Corpus { profile, passages }
    }

    fn weight(&self, i: usize) -> f64 {
        self.profile.chunks[i].weight as f64
    }

    /// Weighted occurrences of a multi-word phrase (space-separated tokens).
    pub fn phrase_count(&self, phrase: &str) -> f64 {
        let parts: Vec<&str> = phrase.split(' ').collect();
        let mut total = 0.0;
        for (i, toks) in self.passages.iter().enumerate() {
            let hits = toks
                .windows(parts.len())
                .filter(|w| w.iter().zip(&parts).all(|(a, b)| a == b))
                .count();
            total += hits as f64 * self.weight(i);
        }
        total
    }
}

/// A count for one word: single words from the profile's vocabulary,
/// phrases from the passages.
fn count_of(c: &Corpus, w: &str) -> f64 {
    if w.contains(' ') {
        c.phrase_count(w)
    } else {
        c.profile.word_counts.get(w).copied().unwrap_or(0) as f64
    }
}

/// Share of passages that contain the word: a habit shows up everywhere,
/// a topic word clusters in a few scenes.
pub fn spread(p: &Profile, w: &str) -> f64 {
    let n = p.chunks.len().max(1) as f64;
    p.word_chunks.get(w).copied().unwrap_or(0) as f64 / n
}

pub fn is_name_or_number(p: &Profile, w: &str) -> bool {
    let stem = w.strip_suffix("'s").unwrap_or(w);
    p.is_proper_noun(stem)
        || w.chars().any(|c| c.is_ascii_digit())
        || (w.chars().count() == 1 && w != "i" && w != "a")
}

pub fn is_style_word(w: &str) -> bool {
    lexicon::is_function_word(w) || style::is_contraction(w)
}

#[derive(Debug, Clone, Serialize)]
pub struct SignatureWord {
    pub word: String,
    pub count: u64,
    pub per_1k: f64,
    /// General-English rate per 1k words (wordfreq).
    pub english_per_1k: f64,
    /// per_1k / english_per_1k.
    pub ratio: f64,
    /// Log-odds z-score against general English (Monroe et al. 2008).
    pub z: f64,
    /// Share of the author's passages that contain it.
    pub spread: f64,
}

/// Background pseudo-corpus size and prior strength for the log-odds test.
const BACKGROUND_TOKENS: f64 = 1e8;
const PRIOR_TOKENS: f64 = 1000.0;

/// Log-odds ratio with an informative Dirichlet prior (Monroe, Colaresi &
/// Quinn 2008), author vs general English, as a z-score: positive = the
/// author uses the word more than English at large, with the evidence the
/// counts give (frequent words need a big gap, rare ones a huge ratio).
pub fn log_odds_z(count: f64, n: f64, english_p: f64) -> f64 {
    let a = PRIOR_TOKENS * english_p;
    let yb = english_p * BACKGROUND_TOKENS;
    let la = ((count + a) / (n + PRIOR_TOKENS - count - a).max(1.0)).ln();
    let lb = ((yb + a) / (BACKGROUND_TOKENS + PRIOR_TOKENS - yb - a).max(1.0)).ln();
    (la - lb) / (1.0 / (count + a) + 1.0 / (yb + a)).sqrt()
}

/// Two-corpus log-odds z (a vs b) with the pooled counts as the prior.
pub fn log_odds_z2(ya: f64, na: f64, yb: f64, nb: f64, pooled_p: f64) -> f64 {
    let a = PRIOR_TOKENS * pooled_p;
    let la = ((ya + a) / (na + PRIOR_TOKENS - ya - a).max(1.0)).ln();
    let lb = ((yb + a) / (nb + PRIOR_TOKENS - yb - a).max(1.0)).ln();
    (la - lb) / (1.0 / (ya + a) + 1.0 / (yb + a)).sqrt()
}

/// Words the author uses distinctly more than general English, dispersed
/// across the corpus, names and numbers removed; split into style words
/// (function words, contractions) and content words.
pub fn signature_words(p: &Profile, limit: usize) -> (Vec<SignatureWord>, Vec<SignatureWord>) {
    let n = p.n_tokens.max(1) as f64;
    let min_count = 5u64.max((n / 20_000.0) as u64);
    let mut all: Vec<SignatureWord> = p
        .word_counts
        .iter()
        .filter(|(w, c)| **c >= min_count && !is_name_or_number(p, w))
        .filter_map(|(w, c)| {
            let sp = spread(p, w);
            if sp < 0.05 {
                return None;
            }
            let ep = lexicon::per_million(w) / 1e6;
            let z = log_odds_z(*c as f64, n, ep);
            if z <= 2.0 {
                return None;
            }
            let per_1k = *c as f64 * 1000.0 / n;
            Some(SignatureWord {
                word: w.clone(),
                count: *c,
                per_1k: r3(per_1k),
                english_per_1k: r3(ep * 1000.0),
                ratio: r3(per_1k / (ep * 1000.0)),
                z: r3(z),
                spread: r3(sp),
            })
        })
        .collect();
    all.sort_by(|a, b| b.z.total_cmp(&a.z));
    let (style, content): (Vec<_>, Vec<_>) = all.into_iter().partition(|s| is_style_word(&s.word));
    (
        style.into_iter().take(limit).collect(),
        content.into_iter().take(limit).collect(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct AvoidedWord {
    pub word: String,
    pub observed: u64,
    /// Occurrences general English would predict in a corpus this size.
    pub expected: f64,
    /// synonym | llm_tell | function_word
    pub reason: &'static str,
    /// The synonym the author uses instead, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instead: Option<String>,
}

/// Common words the author under-uses: general English predicts at least 8
/// in a corpus this size, the author has at most a fifth of that, in almost
/// no passages. Only voice-bearing candidates are considered (synonym-group
/// members, LLM tells, function words): an unused topic word ("government")
/// says nothing about style.
pub fn avoided_words(c: &Corpus, limit: usize) -> Vec<AvoidedWord> {
    let p = c.profile;
    let n = p.n_tokens.max(1) as f64;
    let tells: HashSet<&str> = lexicon::llm_tells()
        .iter()
        .chain(lexicon::slop_words())
        .copied()
        .filter(|t| !t.contains(' '))
        .collect();
    let mut cands: Vec<(&str, &'static str)> = Vec::new();
    for g in lexicon::synonym_groups() {
        cands.extend(g.iter().filter(|w| !w.contains(' ')).map(|w| (*w, "synonym")));
    }
    cands.extend(tells.iter().map(|w| (*w, "llm_tell")));
    cands.extend(
        p.word_counts
            .keys()
            .map(String::as_str)
            .chain(FUNCTION_EXTRA.iter().copied())
            .filter(|w| lexicon::is_function_word(w))
            .map(|w| (w, "function_word")),
    );
    let mut seen = HashSet::new();
    let mut out: Vec<(u8, AvoidedWord)> = Vec::new();
    for (w, reason) in cands {
        if !seen.insert(w) || p.is_proper_noun(w) {
            continue;
        }
        let expected = lexicon::per_million(w) * n / 1e6;
        let observed = p.word_counts.get(w).copied().unwrap_or(0);
        if expected < 8.0 || observed as f64 > 0.2 * expected || spread(p, w) > 0.02 {
            continue;
        }
        let instead = lexicon::group_of(w).and_then(|g| preferred_member(c, g, w));
        let rank = match (reason, &instead) {
            ("synonym", Some(_)) => 0,
            ("llm_tell", _) => 1,
            ("synonym", None) => 2,
            _ => 3,
        };
        out.push((
            rank,
            AvoidedWord {
                word: w.to_string(),
                observed,
                expected: r3(expected),
                reason,
                instead,
            },
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.expected.total_cmp(&a.1.expected)));
    out.into_iter().take(limit).map(|(_, a)| a).collect()
}

/// Function words worth checking even when the author never wrote them
/// (absent words are missing from `word_counts`).
const FUNCTION_EXTRA: [&str; 12] = [
    "however", "therefore", "thus", "whom", "whilst", "amongst", "upon", "hence", "moreover",
    "furthermore", "nevertheless", "whereas",
];

/// The group member the author uses most, if it is not `w` and is used.
fn preferred_member(c: &Corpus, g: usize, w: &str) -> Option<String> {
    let best = lexicon::synonym_groups()[g]
        .iter()
        .map(|m| (*m, count_of(c, m)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (best.0 != w && best.1 >= 1.0).then(|| best.0.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub word: String,
    pub count: f64,
    pub share: f64,
}

/// One synonym group as the author uses it.
#[derive(Debug, Clone, Serialize)]
pub struct Concept {
    /// Members the author uses, most used first, with shares of the group.
    pub members: Vec<Member>,
    pub preferred: String,
    pub preferred_share: f64,
    /// Members the author never uses.
    pub never_used: Vec<String>,
    pub total: f64,
    #[serde(skip)]
    pub group: usize,
}

/// The author's distribution over each synonym group with at least
/// `min_total` uses, most used groups first.
pub fn concepts(c: &Corpus, min_total: f64) -> Vec<Concept> {
    let mut out = Vec::new();
    for (gi, g) in lexicon::synonym_groups().iter().enumerate() {
        let counts: Vec<(&str, f64)> = g.iter().map(|m| (*m, count_of(c, m))).collect();
        let total: f64 = counts.iter().map(|x| x.1).sum();
        if total < min_total {
            continue;
        }
        let mut used: Vec<Member> = counts
            .iter()
            .filter(|x| x.1 > 0.0)
            .map(|(w, n)| Member {
                word: w.to_string(),
                count: n.round(),
                share: r3(n / total),
            })
            .collect();
        used.sort_by(|a, b| b.count.total_cmp(&a.count));
        let never_used = counts
            .iter()
            .filter(|x| x.1 == 0.0)
            .map(|x| x.0.to_string())
            .collect();
        out.push(Concept {
            preferred: used[0].word.clone(),
            preferred_share: used[0].share,
            members: used,
            never_used,
            total: total.round(),
            group: gi,
        });
    }
    out.sort_by(|a, b| b.total.total_cmp(&a.total));
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Phrase {
    pub phrase: String,
    pub count: u32,
    /// Share of passages that contain it.
    pub spread: f64,
}

/// Recurring 2–3-word phrases with at least one content word, no names,
/// dispersed across passages (and works, when there are several). A
/// three-word phrase absorbs the two-word phrases it mostly accounts for.
pub fn signature_phrases(c: &Corpus, limit: usize) -> Vec<Phrase> {
    let p = c.profile;
    let mut cands: Vec<(&String, u32)> = p
        .phrase_counts
        .iter()
        .filter(|(ph, n)| {
            **n >= 5
                && ph.split(' ').any(|w| !lexicon::is_function_word(w) && !style::is_contraction(w))
                && !ph.split(' ').any(|w| is_name_or_number(p, w))
        })
        .map(|(k, v)| (k, *v))
        .collect();
    cands.sort_by(|a, b| b.1.cmp(&a.1));
    cands.truncate(600);
    let wanted: HashSet<&str> = cands.iter().map(|(k, _)| k.as_str()).collect();
    let mut in_passages: HashMap<&str, usize> = HashMap::new();
    let mut in_sources: HashMap<&str, HashSet<u32>> = HashMap::new();
    for (i, toks) in c.passages.iter().enumerate() {
        let mut here: HashSet<&str> = HashSet::new();
        for n in 2..=3 {
            for w in toks.windows(n) {
                let key = w.join(" ");
                if let Some(k) = wanted.get(key.as_str()) {
                    here.insert(k);
                }
            }
        }
        for k in here {
            *in_passages.entry(k).or_insert(0) += 1;
            in_sources.entry(k).or_default().insert(p.chunks[i].source);
        }
    }
    let n_passages = p.chunks.len().max(1) as f64;
    let multi_work = p.sources.len() > 1;
    let mut kept: Vec<Phrase> = Vec::new();
    for (ph, n) in &cands {
        let sp = in_passages.get(ph.as_str()).copied().unwrap_or(0) as f64 / n_passages;
        let works = in_sources.get(ph.as_str()).map(|s| s.len()).unwrap_or(0);
        if sp < 0.01 || (multi_work && works < 2) {
            continue;
        }
        kept.push(Phrase {
            phrase: (*ph).clone(),
            count: *n,
            spread: r3(sp),
        });
    }
    let trigrams: Vec<(String, u32)> = kept
        .iter()
        .filter(|k| k.phrase.matches(' ').count() == 2)
        .map(|k| (k.phrase.clone(), k.count))
        .collect();
    kept.retain(|k| {
        k.phrase.matches(' ').count() == 2
            || !trigrams
                .iter()
                .any(|(t, n)| contains_words(t, &k.phrase) && *n as f64 >= 0.6 * k.count as f64)
    });
    kept.truncate(limit);
    kept
}

fn contains_words(long: &str, short: &str) -> bool {
    long.starts_with(&format!("{short} ")) || long.ends_with(&format!(" {short}"))
}

/// Verbs that tag speech, in the forms a narrator uses.
const SPEECH_VERBS: &[&str] = &[
    "say", "says", "said", "ask", "asks", "asked", "tell", "tells", "told", "reply", "replies",
    "replied", "whisper", "whispers", "whispered", "mutter", "mutters", "muttered", "murmur",
    "murmurs", "murmured", "shout", "shouts", "shouted", "yell", "yells", "yelled", "scream",
    "screams", "screamed", "cry", "cries", "cried", "call", "calls", "called", "answer", "answers",
    "answered", "add", "adds", "added", "continue", "continues", "continued", "admit", "admits",
    "admitted", "explain", "explains", "explained", "laugh", "laughs", "laughed", "sigh", "sighs",
    "sighed", "snap", "snaps", "snapped", "hiss", "hisses", "hissed", "growl", "growls", "growled",
    "breathe", "breathes", "breathed", "exclaim", "exclaims", "exclaimed", "respond", "responds",
    "responded", "state", "states", "stated", "demand", "demands", "demanded", "insist", "insists",
    "insisted", "beg", "begs", "begged", "plead", "pleads", "pleaded", "groan", "groans", "groaned",
    "mumble", "mumbles", "mumbled", "interrupt", "interrupts", "interrupted", "offer", "offers",
    "offered", "agree", "agrees", "agreed", "chuckle", "chuckles", "chuckled", "purr", "purrs",
    "purred", "drawl", "drawls", "drawled", "snarl", "snarls", "snarled",
];

#[derive(Debug, Clone, Serialize)]
pub struct TagVerb {
    pub verb: String,
    pub count: f64,
    pub share: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DialogueTags {
    /// Speech tags found next to quotes.
    pub tags: f64,
    pub verbs: Vec<TagVerb>,
}

/// The speech verb tagging a quote: within three words after a closing
/// quote ("…," she says) or right before an opening one (She says, "…").
pub fn tag_verbs_in(paragraph: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for span in text::quoted_spans(paragraph) {
        let Some(rel) = paragraph[cursor..].find(&span) else { continue };
        let start = cursor + rel;
        let end = start + span.len();
        let before = text::word_tokens(&paragraph[cursor..start]);
        cursor = end;
        // "She says, “…" or "He tells me, “…": the verb, maybe before an object.
        let lead = before.iter().rev().take(2).enumerate().find(|(i, t)| {
            SPEECH_VERBS.contains(&t.as_str())
                && (*i == 0 || matches!(before.last().map(String::as_str), Some("me" | "him" | "her" | "them" | "us")))
        });
        if let Some((_, v)) = lead {
            out.push(v.clone());
            continue;
        }
        let after_text: String = paragraph[end..]
            .chars()
            .skip_while(|ch| !ch.is_alphanumeric() && !matches!(ch, '.' | '!' | '?'))
            .take_while(|ch| !matches!(ch, '.' | '!' | '?' | '"' | '\u{201C}' | '\u{201D}'))
            .collect();
        let after = text::word_tokens(&after_text);
        if let Some(v) = after.iter().take(3).find(|t| SPEECH_VERBS.contains(&t.as_str())) {
            out.push(v.clone());
        }
    }
    out
}

pub fn dialogue_tags(c: &Corpus, limit: usize) -> DialogueTags {
    let mut counts: HashMap<String, f64> = HashMap::new();
    for (i, ch) in c.profile.chunks.iter().enumerate() {
        for para in &ch.paragraphs {
            for v in tag_verbs_in(para) {
                *counts.entry(v).or_insert(0.0) += c.weight(i);
            }
        }
    }
    let total: f64 = counts.values().sum();
    let mut verbs: Vec<TagVerb> = counts
        .into_iter()
        .map(|(verb, n)| TagVerb {
            verb,
            count: n.round(),
            share: if total > 0.0 { r3(n / total) } else { 0.0 },
        })
        .collect();
    verbs.sort_by(|a, b| b.count.total_cmp(&a.count).then_with(|| a.verb.cmp(&b.verb)));
    verbs.truncate(limit);
    DialogueTags {
        tags: total.round(),
        verbs,
    }
}

/// Where a tell occurs in a token stream.
#[derive(Debug, Clone)]
pub struct TellHit {
    /// Index of the tell's first token.
    pub at: usize,
    /// Tokens the tell spans in the original stream.
    pub len: usize,
    pub tell: &'static str,
}

/// LLM tells in a token stream: single words and phrases from the tell
/// list and slop-score's word list, plus slop-score's stopword-stripped
/// phrases matched on the content stream.
pub fn find_tells(tokens: &[String]) -> Vec<TellHit> {
    static WORDS: std::sync::LazyLock<HashSet<&'static str>> = std::sync::LazyLock::new(|| {
        lexicon::llm_tells()
            .iter()
            .chain(lexicon::slop_words())
            .copied()
            .filter(|t| !t.contains(' '))
            .collect()
    });
    static PHRASES: std::sync::LazyLock<Vec<Vec<&'static str>>> = std::sync::LazyLock::new(|| {
        lexicon::llm_tells()
            .iter()
            .filter(|t| t.contains(' '))
            .map(|t| t.split(' ').collect())
            .collect()
    });
    static SLOP: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
        std::sync::LazyLock::new(|| lexicon::slop_phrases().iter().map(|p| (*p, *p)).collect());
    let mut hits = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if let Some(w) = WORDS.get(t.as_str()) {
            hits.push(TellHit { at: i, len: 1, tell: w });
        }
        for ph in PHRASES.iter() {
            if tokens.len() >= i + ph.len() && ph.iter().zip(&tokens[i..]).all(|(a, b)| a == b) {
                let tell = lexicon::llm_tells()
                    .iter()
                    .find(|t| t.split(' ').eq(ph.iter().copied()))
                    .copied()
                    .unwrap_or("");
                hits.push(TellHit { at: i, len: ph.len(), tell });
            }
        }
    }
    let content = lexicon::content_stream(tokens);
    for n in [3usize, 2] {
        for w in content.windows(n) {
            let key = w.iter().map(|&i| tokens[i].as_str()).collect::<Vec<_>>().join(" ");
            if let Some(tell) = SLOP.get(key.as_str()) {
                let (a, b) = (w[0], w[n - 1]);
                // Skip a bigram already covered by a trigram hit.
                if n == 2 && hits.iter().any(|h| h.at <= a && h.at + h.len > b) {
                    continue;
                }
                hits.push(TellHit { at: a, len: b - a + 1, tell });
            }
        }
    }
    hits
}

/// The author's own rate (per 1k words) of each tell they use at all.
pub fn author_tell_rates(c: &Corpus) -> HashMap<&'static str, f64> {
    let mut counts: HashMap<&'static str, f64> = HashMap::new();
    let mut words = 0.0;
    for (i, toks) in c.passages.iter().enumerate() {
        let w = c.weight(i);
        words += toks.len() as f64 * w;
        for h in find_tells(toks) {
            *counts.entry(h.tell).or_insert(0.0) += w;
        }
    }
    let words = words.max(1.0);
    counts.into_iter().map(|(k, v)| (k, v * 1000.0 / words)).collect()
}

/// Tells the author never (or almost never) uses, most LLM-typical first:
/// the short author-relative ban list for a brief.
pub fn unused_tells(rates: &HashMap<&'static str, f64>, limit: usize) -> Vec<&'static str> {
    lexicon::llm_tells()
        .iter()
        .chain(lexicon::slop_words().iter().filter(|w| lexicon::zipf(w).is_some_and(|z| z >= 3.0)))
        .copied()
        .filter(|t| rates.get(t).copied().unwrap_or(0.0) < 0.005)
        .take(limit)
        .collect()
}

pub fn r3(v: f64) -> f64 {
    if v.abs() >= 100.0 {
        (v * 10.0).round() / 10.0
    } else if v.abs() >= 1.0 {
        (v * 100.0).round() / 100.0
    } else {
        (v * 1000.0).round() / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_odds_rewards_overuse() {
        let n = 100_000.0;
        let p = 1e-4; // 10 expected
        assert!(log_odds_z(60.0, n, p) > 3.0);
        assert!(log_odds_z(10.0, n, p).abs() < 1.0);
        assert!(log_odds_z(0.0, n, p) < 0.0);
    }

    #[test]
    fn tags_found_after_and_before_quotes() {
        let v = tag_verbs_in("“Come here,” she says. He tells me, “No.”");
        assert_eq!(v, vec!["says".to_string(), "tells".to_string()]);
    }

    #[test]
    fn tells_match_words_and_stripped_phrases() {
        let toks = text::word_tokens("She took a deep breath and her voice was barely a whisper in the tapestry");
        let hits = find_tells(&toks);
        let tells: Vec<&str> = hits.iter().map(|h| h.tell).collect();
        assert!(tells.contains(&"tapestry"), "{tells:?}");
        assert!(tells.contains(&"took deep breath"), "{tells:?}");
        assert!(!tells.contains(&"deep breath"), "bigram inside a trigram hit: {tells:?}");
    }
}

/// Recurring formulas: word runs of 4+ words the author repeats across at
/// least 3 distinct items or passages ("new episode of the pod is out").
/// They are signature phrasing: the brief lists them, and the copy guard
/// exempts them. FOUNDATION STUB: the GUIDE lane implements it.
pub fn signature_formulas(p: &Profile, limit: usize) -> Vec<Phrase> {
    let _ = (p, limit);
    Vec::new()
}
