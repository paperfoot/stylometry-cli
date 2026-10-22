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

/// Names, numbers, single letters, and the placeholders links and @handles
/// become ("url", "user"): none of them is a word choice.
pub fn is_name_or_number(p: &Profile, w: &str) -> bool {
    let stem = w.strip_suffix("'s").unwrap_or(w);
    matches!(w, "url" | "user")
        || p.is_proper_noun(stem)
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
    // Multi-word tells, social-post phrases included, with their own text.
    static PHRASES: std::sync::LazyLock<Vec<(Vec<&'static str>, &'static str)>> = std::sync::LazyLock::new(|| {
        lexicon::llm_tells()
            .iter()
            .chain(lexicon::social_tells())
            .filter(|t| t.contains(' '))
            .map(|t| (t.split(' ').collect(), *t))
            .collect()
    });
    static SLOP: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
        std::sync::LazyLock::new(|| lexicon::slop_phrases().iter().map(|p| (*p, *p)).collect());
    let mut hits = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if let Some(w) = WORDS.get(t.as_str()) {
            hits.push(TellHit { at: i, len: 1, tell: w });
        }
        for (ph, tell) in PHRASES.iter() {
            if tokens.len() >= i + ph.len() && ph.iter().zip(&tokens[i..]).all(|(a, b)| a == b) {
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


/// Recurring formulas: word runs of 4+ words the author repeats across at
/// least 3 distinct items or passages ("new episode of the pod is out").
/// They are signature phrasing: the brief lists them, and the copy guard
/// exempts them. Runs are found Apriori-style (a 5-gram is counted only when
/// both its 4-word halves recur) within sentences, and a run absorbed by a
/// longer one that recurs almost as widely is dropped, so each formula is
/// listed once at its full length. Most widespread first.
pub fn signature_formulas(p: &Profile, limit: usize) -> Vec<Phrase> {
    const MIN_N: usize = 4;
    // Long enough for a whole repeated sentence (an email's stock line).
    const MAX_N: usize = 40;
    const MIN_UNITS: u32 = 3;
    let short = p.register.is_short_form();
    let streams: Vec<Vec<Vec<String>>> = p
        .chunks
        .iter()
        .map(|c| {
            c.paragraphs
                .iter()
                .flat_map(|para| text::sentences_for(para, short))
                .map(|s| text::word_tokens(&s))
                .filter(|t| t.len() >= MIN_N)
                .collect()
        })
        .collect();
    // phrase -> (n, distinct units, total occurrences)
    let mut kept: HashMap<String, (usize, u32, u32)> = HashMap::new();
    let mut survivors: HashSet<String> = HashSet::new();
    for n in MIN_N..=MAX_N {
        // key -> (last unit seen, distinct units, total)
        let mut seen: HashMap<String, (usize, u32, u32)> = HashMap::new();
        for (ci, sents) in streams.iter().enumerate() {
            for toks in sents {
                for w in toks.windows(n) {
                    if n > MIN_N
                        && (!survivors.contains(&w[..n - 1].join(" ")) || !survivors.contains(&w[1..].join(" ")))
                    {
                        continue;
                    }
                    let e = seen.entry(w.join(" ")).or_insert((usize::MAX, 0, 0));
                    e.2 += 1;
                    if e.0 != ci {
                        e.0 = ci;
                        e.1 += 1;
                    }
                }
            }
        }
        survivors = seen
            .iter()
            .filter(|(_, v)| v.1 >= MIN_UNITS)
            .map(|(k, _)| k.clone())
            .collect();
        if survivors.is_empty() {
            break;
        }
        for k in &survivors {
            let v = seen[k];
            kept.insert(k.clone(), (n, v.1, v.2));
        }
    }
    // A run is absorbed by the longer run around it when that one recurs in
    // at least 80% as many places.
    let mut absorbed: HashSet<String> = HashSet::new();
    for (k, (n, units, _)) in &kept {
        if *n == MIN_N {
            continue;
        }
        let toks: Vec<&str> = k.split(' ').collect();
        for sub in [toks[..toks.len() - 1].join(" "), toks[1..].join(" ")] {
            if let Some((_, su, _)) = kept.get(&sub) {
                if *units as f64 >= 0.8 * *su as f64 {
                    absorbed.insert(sub);
                }
            }
        }
    }
    let n_units = p.chunks.len().max(1) as f64;
    let mut out: Vec<(usize, u32, Phrase)> = kept
        .into_iter()
        .filter(|(k, _)| !absorbed.contains(k))
        .filter(|(k, _)| {
            k.split(' ')
                .filter(|w| !matches!(*w, "url" | "user") && !lexicon::is_function_word(w) && !style::is_contraction(w))
                .count()
                >= 1
        })
        .map(|(k, (n, units, total))| {
            (
                n,
                units,
                Phrase {
                    phrase: k,
                    count: total,
                    spread: r3(units as f64 / n_units),
                },
            )
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)).then_with(|| a.2.phrase.cmp(&b.2.phrase)));
    out.into_iter().take(limit).map(|(_, _, ph)| ph).collect()
}

/// A pictographic emoji (or symbol used as one: ☕, ✅, ⭐).
pub fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF)
}

/// The layout tells of machine-written posts, as `item_tells` names them.
pub const ITEM_TELLS: [&str; 4] = ["🧵👇", "closing \"thoughts?\"", "hashtag string at the end", "emoji bullets"];

/// Layout tells in one post: a thread opener "🧵👇", a closing "thoughts?"
/// line, two or more hashtags piled at the end, emoji used as bullets.
/// Author-relative like every tell: compare with `item_tell_shares`.
pub fn item_tells(item: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let compact: String = item.chars().filter(|c| !c.is_whitespace() && *c != '\u{FE0F}').collect();
    if compact.contains("🧵👇") || compact.contains("👇🧵") {
        out.push(ITEM_TELLS[0]);
    }
    let lines: Vec<&str> = item.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(last) = lines.last() {
        let l = last.to_lowercase();
        let l = l.trim_end_matches(|c: char| is_emoji(c) || c.is_whitespace() || c == '\u{FE0F}');
        if matches!(
            l,
            "thoughts?" | "your thoughts?" | "what are your thoughts?" | "what do you think?" | "agree?" | "am i wrong?"
        ) {
            out.push(ITEM_TELLS[1]);
        }
    }
    let trailing_tags = item
        .split_whitespace()
        .rev()
        .filter(|t| !t.starts_with("http"))
        .take_while(|t| t.starts_with('#') && t.len() > 1)
        .count();
    if trailing_tags >= 2 {
        out.push(ITEM_TELLS[2]);
    }
    let emoji_lines = lines
        .iter()
        .filter(|l| l.chars().next().is_some_and(is_emoji) && l.chars().count() > 3)
        .count();
    if emoji_lines >= 2 {
        out.push(ITEM_TELLS[3]);
    }
    out
}

/// Share of the author's items showing each layout tell.
pub fn item_tell_shares(p: &Profile) -> HashMap<&'static str, f64> {
    let mut counts: HashMap<&'static str, f64> = ITEM_TELLS.iter().map(|t| (*t, 0.0)).collect();
    for c in &p.chunks {
        for t in item_tells(&c.text()) {
            *counts.entry(t).or_insert(0.0) += 1.0;
        }
    }
    let n = p.chunks.len().max(1) as f64;
    counts.into_iter().map(|(k, v)| (k, v / n)).collect()
}

/// Social-post tells (phrases and layouts) the author never or almost never
/// uses: the ban list for a short-form brief.
pub fn unused_social_tells(rates: &HashMap<&'static str, f64>, shares: &HashMap<&'static str, f64>) -> Vec<&'static str> {
    lexicon::social_tells()
        .iter()
        .copied()
        .filter(|t| rates.get(t).copied().unwrap_or(0.0) < 0.005)
        .chain(ITEM_TELLS.iter().copied().filter(|t| shares.get(t).copied().unwrap_or(0.0) < 0.02))
        .collect()
}

/// Narration point of view and tense of a stretch of prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ModeLabel {
    /// first | third
    pub pov: &'static str,
    /// present | past | mixed
    pub tense: &'static str,
}

/// The narrator's own words: I and we (a first-person narrator is often one
/// of a party: "we got up early").
const FIRST_NARRATOR: [&str; 18] = [
    "i", "me", "my", "mine", "myself", "i'm", "i'd", "i've", "i'll", "we", "us", "our", "ours", "ourselves", "we're",
    "we'd", "we've", "we'll",
];
const THIRD_NARRATOR: [&str; 10] = ["he", "she", "him", "her", "his", "hers", "himself", "herself", "he's", "she's"];

/// POV from the narration (dialogue removed) and tense from the present
/// share of narration verbs: present at 0.6 or more, past at 0.3 or less,
/// else mixed. None when the stretch has too little narration to tell.
///
/// First person when at least 30% of the narration paragraphs (15+ words)
/// use I/me/my or we/us/our: a first-person narrator recounting what others
/// did says "he" far more than "I", but is present across the text, while
/// third-person narration keeps I and we to dialogue, so an unquoted speech
/// or a song leaks into a few paragraphs only. Stretches with fewer than six
/// such paragraphs (retrieval windows) fall back to the count: I/we at
/// least half as many as he/she.
pub fn mode_of(paragraphs: &[String]) -> Option<ModeLabel> {
    let (mut first, mut third, mut paras, mut with_first) = (0usize, 0usize, 0usize, 0usize);
    for para in paragraphs {
        let toks = text::word_tokens(&crate::engine::metrics::narration(para));
        let mine = toks.iter().filter(|t| FIRST_NARRATOR.contains(&t.as_str())).count();
        first += mine;
        third += toks.iter().filter(|t| THIRD_NARRATOR.contains(&t.as_str())).count();
        if toks.len() >= 15 {
            paras += 1;
            with_first += usize::from(mine > 0);
        }
    }
    if first + third < 5 {
        return None;
    }
    let first_person = if paras >= 6 { with_first * 10 >= paras * 3 } else { first * 2 >= third };
    let present = present_share(paragraphs)?;
    Some(ModeLabel {
        pov: if first_person { "first" } else { "third" },
        tense: if present >= 0.6 {
            "present"
        } else if present <= 0.3 {
            "past"
        } else {
            "mixed"
        },
    })
}

fn present_share(paragraphs: &[String]) -> Option<f64> {
    static IDX: std::sync::LazyLock<Option<usize>> = std::sync::LazyLock::new(|| {
        crate::engine::metrics::defs(&[]).iter().position(|d| d.key == "present_tense_share")
    });
    let i = (*IDX)?;
    crate::engine::metrics::measure(paragraphs, &[]).get(i).copied()
}

#[derive(Debug, Clone, Serialize)]
pub struct Mode {
    pub pov: &'static str,
    pub tense: &'static str,
    /// Share of the author's words written in this mode.
    pub share: f64,
    pub sections: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Modes {
    /// "sections" (chapters) or "2000-word blocks" when the corpus has no
    /// chapter breaks.
    pub basis: &'static str,
    pub sections: usize,
    pub modes: Vec<Mode>,
}

/// The narration modes of a long-form voice, per chapter: a dual narrative
/// (present-tense first person plus a past-tense manuscript) has two modes,
/// and a brief must say "pick one per chapter", never "57% present".
/// Sections come from chapter breaks; without them, 2,000-word blocks stand
/// in. None for short-form profiles.
pub fn section_modes(p: &Profile) -> Option<Modes> {
    if p.register.is_short_form() || p.chunks.is_empty() {
        return None;
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut key = (u32::MAX, u32::MAX);
    for (i, c) in p.chunks.iter().enumerate() {
        if (c.source, c.section) != key {
            key = (c.source, c.section);
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(i);
    }
    let mut basis = "sections";
    if groups.len() <= p.sources.len() {
        basis = "2000-word blocks";
        groups = groups.into_iter().flat_map(|g| g.chunks(8).map(<[usize]>::to_vec).collect::<Vec<_>>()).collect();
    }
    let mut tally: HashMap<ModeLabel, (f64, usize)> = HashMap::new();
    let mut total = 0.0;
    for g in &groups {
        let paras: Vec<String> = g.iter().flat_map(|&i| p.chunks[i].paragraphs.iter().cloned()).collect();
        let Some(m) = mode_of(&paras) else { continue };
        let w: f64 = g.iter().map(|&i| p.chunks[i].n_words as f64 * p.chunks[i].weight as f64).sum();
        let e = tally.entry(m).or_insert((0.0, 0));
        e.0 += w;
        e.1 += 1;
        total += w;
    }
    let mut modes: Vec<Mode> = tally
        .into_iter()
        .map(|(m, (w, n))| Mode {
            pov: m.pov,
            tense: m.tense,
            share: r3(w / total.max(1.0)),
            sections: n,
        })
        .filter(|m| m.share >= 0.05)
        .collect();
    modes.sort_by(|a, b| b.share.total_cmp(&a.share));
    Some(Modes {
        basis,
        sections: groups.len(),
        modes,
    })
}

/// Content words that are habits, not subjects: in at least 30% of the
/// author's passages, and (with several works) not among any single work's
/// 50 most characteristic words (TF-IDF over works), so "galaxy" or
/// "captain" in a space novel never reads as voice.
pub fn habit_words(p: &Profile, words: &[SignatureWord]) -> Vec<SignatureWord> {
    let topical = topical_words(p, 50);
    words
        .iter()
        .filter(|w| w.spread >= 0.3 && !topical.contains(w.word.as_str()) && !matches!(w.word.as_str(), "url" | "user"))
        .cloned()
        .collect()
}

/// Each work's `per_source` most characteristic words (TF-IDF over works).
/// Empty for a single work: nothing to contrast it with.
pub fn topical_words(p: &Profile, per_source: usize) -> HashSet<String> {
    let n_src = p.sources.len();
    let mut out = HashSet::new();
    if n_src < 2 {
        return out;
    }
    let mut per: Vec<HashMap<&str, u64>> = vec![HashMap::new(); n_src];
    let mut totals = vec![0u64; n_src];
    for c in &p.chunks {
        let s = c.source as usize;
        totals[s] += c.n_words as u64;
        for (w, n) in &c.words {
            *per[s].entry(w.as_str()).or_insert(0) += *n as u64;
        }
    }
    let mut df: HashMap<&str, usize> = HashMap::new();
    for m in &per {
        for w in m.keys() {
            *df.entry(w).or_insert(0) += 1;
        }
    }
    for (s, m) in per.iter().enumerate() {
        let mut scored: Vec<(&str, f64)> = m
            .iter()
            .filter(|(w, _)| !lexicon::is_function_word(w))
            .map(|(w, n)| {
                let idf = (n_src as f64 / df[w] as f64).ln();
                (*w, *n as f64 / totals[s].max(1) as f64 * idf)
            })
            .filter(|x| x.1 > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        out.extend(scored.into_iter().take(per_source).map(|(w, _)| w.to_string()));
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct WordShare {
    pub word: String,
    /// Share of the items that use it.
    pub share: f64,
}

/// Informal words (lol, tbh, gonna, ok) in short-form items: the ones the
/// author uses, with the share of items using each, and the common ones
/// they never use. Agents either drop all of them or add the wrong ones.
pub fn informal_use(p: &Profile, items: &[usize]) -> (Vec<WordShare>, Vec<&'static str>) {
    let mut hits: HashMap<&'static str, usize> = HashMap::new();
    for &i in items {
        let toks: HashSet<String> = text::word_tokens(&p.chunks[i].text()).into_iter().collect();
        for w in lexicon::informal_words() {
            if toks.contains(*w) {
                *hits.entry(w).or_insert(0) += 1;
            }
        }
    }
    let n = items.len().max(1) as f64;
    let mut used: Vec<WordShare> = hits
        .iter()
        .filter(|(_, c)| **c >= 2)
        .map(|(w, c)| WordShare {
            word: w.to_string(),
            share: r3(*c as f64 / n),
        })
        .collect();
    used.sort_by(|a, b| b.share.total_cmp(&a.share).then_with(|| a.word.cmp(&b.word)));
    let never = lexicon::informal_words()
        .iter()
        .copied()
        .filter(|w| !hits.contains_key(w) && lexicon::zipf(w).is_some_and(|z| z >= 4.0))
        .collect();
    (used, never)
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

    fn social_profile(posts: &[&str]) -> Profile {
        let text = posts.join("\n---\n");
        let doc = text::Doc {
            name: "posts.txt".into(),
            text,
            items: Vec::new(),
        };
        let reg = Some((crate::engine::profile::Register::Social, "test".to_string()));
        Profile::build_with("t", &[doc], crate::engine::profile::ProfileKind::Author, reg, 1).unwrap()
    }

    #[test]
    fn formulas_recur_across_items_at_full_length() {
        let p = social_profile(&[
            "new episode of the pod is out today, we talk sleep",
            "new episode of the pod is out today, we talk rapamycin",
            "new episode of the pod is out today, we talk zone two",
            "coffee first then code",
            "shipped the thing at last",
        ]);
        let f = signature_formulas(&p, 10);
        let phrases: Vec<&str> = f.iter().map(|x| x.phrase.as_str()).collect();
        assert!(phrases.contains(&"new episode of the pod is out today we talk"), "{phrases:?}");
        assert!(!phrases.contains(&"new episode of the"), "sub-run kept: {phrases:?}");
        assert!(f.iter().all(|x| x.spread >= 0.59), "{f:?}");
    }

    #[test]
    fn item_tells_find_layout_tells() {
        assert_eq!(item_tells("Big news today 🧵👇"), vec!["🧵👇"]);
        assert_eq!(item_tells("We shipped it.\n\nThoughts?"), vec!["closing \"thoughts?\""]);
        assert_eq!(item_tells("Shipped. #buildinpublic #ai #saas"), vec!["hashtag string at the end"]);
        assert_eq!(item_tells("Why:\n✅ fast setup\n✅ no keys in chat"), vec!["emoji bullets"]);
        assert!(item_tells("i think so. tbh it works").is_empty());
    }

    #[test]
    fn find_tells_matches_social_phrases() {
        let toks = text::word_tokens("Unpopular opinion: this is a game-changer. Let that sink in.");
        let tells: Vec<&str> = find_tells(&toks).iter().map(|h| h.tell).collect();
        assert!(tells.contains(&"unpopular opinion"), "{tells:?}");
        assert!(tells.contains(&"game changer"), "{tells:?}");
        assert!(tells.contains(&"let that sink in"), "{tells:?}");
    }

    #[test]
    fn modes_tell_first_present_from_third_past() {
        let first = vec!["I walk to the door. I am tired and my hands shake. She is there and I look at her.".to_string(); 3];
        let third = vec!["He walked to the door. He was tired and his hands shook. She had gone and he looked away.".to_string(); 3];
        assert_eq!(mode_of(&first), Some(ModeLabel { pov: "first", tense: "present" }));
        assert_eq!(mode_of(&third), Some(ModeLabel { pov: "third", tense: "past" }));
    }

    /// Three Men in a Boat: an "I" narrator telling anecdotes about George
    /// says "he" seven times for each "I", and is still first person.
    #[test]
    fn an_i_narrator_recounting_others_is_first_person() {
        let anecdote = "George got up early that morning. He said he would make the tea, and he went down to the river with his towel, and he stood there for a long time while his breakfast went cold. I watched him from the boat.";
        let paras = vec![anecdote.to_string(); 8];
        assert_eq!(mode_of(&paras).map(|m| m.pov), Some("first"));
        // A party narrator: "we" is the narrator too.
        let party = "We got up at six and pulled the boat out. Harris said he would cook, and he burnt the eggs, and he blamed the stove for it, and he sulked until noon while his coffee went cold.";
        assert_eq!(mode_of(&vec![party.to_string(); 8]).map(|m| m.pov), Some("first"));
        // Third person with one unquoted line of speech in eight paragraphs.
        let told = "He walked to the door and looked out at the rain. He was tired, and his hands shook as he turned the key, and she watched him from the stairs without a word.";
        let mut paras = vec![told.to_string(); 8];
        paras[3] = "I will not go back, he thought, and he said it again to himself as he walked, and his voice was low and she did not hear him.".into();
        assert_eq!(mode_of(&paras).map(|m| m.pov), Some("third"));
    }
}
