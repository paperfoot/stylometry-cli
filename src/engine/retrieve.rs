//! Exemplar retrieval: the author's own pieces most like what is being
//! written, as few-shot examples (`examples`) and brief exemplars (`guide`).
//!
//! Topic is BM25 over content words (light stemming; names excluded for
//! fiction, where matching names pulls the same characters and plot and
//! invites continuity leaks). What else counts depends on the register:
//! - fiction: scene shape (dialogue share, sentence and paragraph length,
//!   POV/tense mode) dominates, topic weighs at most 0.2;
//! - non-fiction and blogs: topic and style nearness, half each;
//! - short-form: topic, then engagement (percentile within kind and calendar
//!   quarter, so follower growth and replies' structurally lower counts
//!   don't decide), never a media post or a sub-6-word item as "best".
//! Picks are diversified with MMR (one per thread, one per work first).
//!
//! `topic_match` is judged against chance, not a fixed cosine: random tasks
//! of the same length, drawn from general English by frequency, set the
//! lines for this corpus (a shared word in a 20-word post scores far higher
//! than in a 500-word passage, and short tasks score higher than long ones).
//! Strong also needs two of the task's terms in one piece; the task's words
//! the corpus never uses count against the match. Weak and none come with a
//! note, and none falls back to typical pieces.
//!
//! With JEV (on by default when a key is found) and a task described in
//! words, essays, blogs and posts re-rank the top `JEV_POOL` candidates of
//! that ranking by how good a model each is for the piece (`fit`, 0-3: the
//! same kind of argument or post, not shared words), judged on a short
//! excerpt of each, with the word ranking as a quarter of the score;
//! `topic_match` then comes from the best fit (a strong word match drops
//! at most to weak). On 8 essay and post tasks labelled by hand before JEV
//! saw them, the top five went from NDCG@5 0.54 to 0.73 and precision@3
//! 0.38 to 0.67. Fiction keeps the word and shape ranking: on 4 scene tasks
//! short excerpts did no better (0.50 both). Any failure keeps the word
//! ranking.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::json;

use crate::engine::jev::Jev;
use crate::engine::metrics;
use crate::engine::profile::{ItemKind, Profile, Register};
use crate::engine::voice::{self, ModeLabel};
use crate::engine::{lexicon, style, text, texture};

/// How strongly the best piece shares the task's vocabulary (TF-IDF cosine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicMatch {
    Strong,
    Weak,
    /// No task given, or nothing in the corpus is about it: typical pieces.
    None,
}

/// Floors under the calibrated cosine lines for `TopicMatch` (see
/// `null_cosines`): below 0.05 the "best" match is shared stopword-like
/// vocabulary whatever random tasks reach.
const WEAK: f64 = 0.05;
const STRONG: f64 = 0.12;
/// Random tasks drawn per retrieval to place the strong/weak lines.
const NULL_TASKS: usize = 200;
/// MMR trade-off between relevance and novelty.
const LAMBDA: f64 = 0.7;
/// Candidates JEV judges: the top of the word ranking (the eval's pool).
const JEV_POOL: usize = 24;
/// Words of each candidate JEV reads: one short span, not the window.
const EXCERPT_WORDS: usize = 180;
/// Longer tasks are drafts, not descriptions: they keep the word and shape
/// ranking (fit was measured on descriptions of 20-90 words).
const JEV_TASK_WORDS: usize = 300;
/// Share of the word ranking in a JEV-ranked score (eval: 0.25 beat 0.1
/// and fit alone on NDCG@5 under all three question wordings tried).
const JEV_LEXICAL: f64 = 0.25;
/// Best-fit lines for `TopicMatch` (0-3 scale), set on the 180-word
/// excerpts JEV reads (they score lower than whole windows). Over 20 essay
/// and post tasks, labelled by the best passage in their pool: where one
/// was a similar or close piece (8), the best fit was 1.9 or more in 7;
/// where the nearest only shared the broad subject (5), 0.94 to 1.18; where
/// nothing was related (7), 0.71 or less. These lines get 19 of 20 right
/// (2.0 and 1.0 got 15: "strong" missed 3 of the 8); the miss is a task
/// whose two best passages were labelled similar but fit 0.98 and 0.71.
const FIT_STRONG: f64 = 1.5;
const FIT_WEAK: f64 = 0.8;

/// What ordered the picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankBasis {
    /// JEV's judgment of fit to the task, with the word ranking as a tie-break.
    Jev,
    /// Shared words, style nearness and engagement (no task, no JEV, or JEV failed).
    Lexical,
}

pub struct Request<'a> {
    /// The task: a draft/outline or a few words. None = typical pieces.
    pub query: Option<&'a str>,
    pub n: usize,
    /// Short-form: which kind of item to draw from.
    pub kind: ItemKind,
    /// Long-form: unit length in words.
    pub window_words: usize,
    /// Long-form: only each work's opening window (blog openings).
    pub openings_only: bool,
    /// Long-form: make sure picks include a dialogue-rich and a narration
    /// unit when the author writes dialogue.
    pub mix_dialogue: bool,
    /// Chunk indices that must not be used (already shown elsewhere).
    pub exclude: HashSet<usize>,
    /// Re-rank by JEV fit when a client is configured (default true).
    pub jev: bool,
}

impl<'a> Request<'a> {
    pub fn new(query: Option<&'a str>, n: usize, kind: ItemKind) -> Request<'a> {
        Request {
            query,
            n,
            kind,
            window_words: 500,
            openings_only: false,
            mix_dialogue: false,
            exclude: HashSet::new(),
            jev: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunks: Vec<usize>,
    /// The blended rank score, 0–1.
    pub similarity: f64,
    /// TF-IDF cosine with the task's content words (0 without a task).
    pub topic: f64,
    /// Long-form: nearness of style to the target shape, 0–1.
    pub shape: Option<f64>,
    /// Short-form: percentile of engagement within kind and quarter.
    pub engagement_pct: Option<f64>,
    pub dialogue_share: Option<f64>,
    pub mode: Option<ModeLabel>,
    /// The first window of its work.
    pub opening: bool,
    /// JEV's fit to the task, 0 (unrelated) to 3 (the same kind of piece in
    /// a similar situation); None unless JEV ranked.
    pub fit: Option<f64>,
}

pub struct Retrieval {
    pub hits: Vec<Hit>,
    pub topic_match: TopicMatch,
    pub rank_basis: RankBasis,
    /// Pieces considered.
    pub candidates: usize,
    /// Content words of the task found anywhere in the corpus.
    pub query_terms: usize,
    pub notes: Vec<String>,
    /// Why JEV was configured but did not rank (for `examples`; a brief
    /// has no use for it).
    pub rank_note: Option<String>,
}

/// Content-word stem, or None for words that carry no topic.
fn term(p: &Profile, w: &str, names_ok: bool) -> Option<String> {
    let w = w.strip_suffix("'s").unwrap_or(w);
    if w.chars().count() < 3
        || w.chars().any(|c| c.is_ascii_digit())
        || crate::engine::text::is_placeholder(w)
        || lexicon::is_function_word(w)
        || lexicon::is_stopword(w)
        || style::is_contraction(w)
        || (!names_ok && p.is_proper_noun(w))
    {
        return None;
    }
    Some(stem(w))
}

/// A light suffix stripper: enough for "launching"/"launched"/"launch" and
/// "keys"/"key" to meet, not a linguistic stemmer.
fn stem(w: &str) -> String {
    let n = w.len();
    if n > 5 && w.ends_with("ies") {
        return format!("{}y", &w[..n - 3]);
    }
    for (suf, keep) in [("ing", 4), ("ed", 3), ("es", 4), ("s", 3)] {
        if n >= suf.len() + keep && w.ends_with(suf) && !w.ends_with("ss") {
            return w[..n - suf.len()].to_string();
        }
    }
    w.to_string()
}

fn terms_of(p: &Profile, txt: &str, names_ok: bool) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for t in text::word_tokens(txt) {
        if let Some(s) = term(p, &t, names_ok) {
            *out.entry(s).or_insert(0.0) += 1.0;
        }
    }
    out
}

/// Sublinear TF-IDF vector, L2-normalised. With `unseen`, terms the corpus
/// never uses stay in the vector at that IDF (they can match nothing but
/// count in the norm): used for the task, so a task whose words the corpus
/// mostly lacks cannot read as a perfect match on the one word it shares.
fn tfidf(tf: &HashMap<String, f64>, idf: &HashMap<String, f64>, unseen: Option<f64>) -> HashMap<String, f64> {
    let mut v: HashMap<String, f64> = tf
        .iter()
        .filter_map(|(t, c)| idf.get(t).copied().or(unseen).map(|i| (t.clone(), (1.0 + c.ln()) * i)))
        .collect();
    let norm = v.values().map(|x| x * x).sum::<f64>().sqrt();
    if norm > 0.0 {
        v.values_mut().for_each(|x| *x /= norm);
    }
    v
}

fn cosine(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small.iter().filter_map(|(t, x)| big.get(t).map(|y| x * y)).sum()
}

/// Content words of general English with their frequency, cumulative, for
/// drawing random tasks: the same filters as `term` bar the profile's names.
static NULL_WORDS: std::sync::LazyLock<(Vec<&'static str>, Vec<f64>)> = std::sync::LazyLock::new(|| {
    let mut words = Vec::new();
    let mut cum = Vec::new();
    let mut total = 0.0;
    for &(w, z) in lexicon::zipf_table() {
        if w.len() < 3
            || !w.bytes().all(|b| b.is_ascii_lowercase())
            || crate::engine::text::is_placeholder(w)
            || lexicon::is_function_word(w)
            || lexicon::is_stopword(w)
        {
            continue;
        }
        total += 10f64.powf(f64::from(z));
        words.push(w);
        cum.push(total);
    }
    (words, cum)
});

/// SplitMix64: a fixed-seed generator, so the same task against the same
/// profile always gets the same verdict.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Best cosine reached by random tasks of `tokens` content words drawn from
/// general English by frequency, sorted. A task beats chance when its own
/// best cosine clears these: what a cosine means depends on the corpus
/// (one shared word in a 20-word post scores far higher than in a
/// 500-word passage) and on the task's length, and a fixed line can't
/// follow either.
fn null_cosines(
    p: &Profile,
    vecs: &[HashMap<String, f64>],
    tfs: &[&HashMap<String, f64>],
    idf: &HashMap<String, f64>,
    idf_unseen: f64,
    tokens: usize,
    names_ok: bool,
) -> (Vec<f64>, Vec<f64>) {
    let (words, cum) = &*NULL_WORDS;
    let Some(&total) = cum.last() else { return (Vec::new(), Vec::new()) };
    let mut postings: HashMap<&str, Vec<(usize, f64)>> = HashMap::new();
    for (ci, v) in vecs.iter().enumerate() {
        for (t, w) in v {
            postings.entry(t.as_str()).or_default().push((ci, *w));
        }
    }
    let tokens = tokens.clamp(1, 400);
    let mut rng = Rng(0x5EED ^ tokens as u64);
    let mut scores = vec![(0.0, 0usize); vecs.len()];
    let (mut any, mut two) = (Vec::with_capacity(NULL_TASKS), Vec::with_capacity(NULL_TASKS));
    for _ in 0..NULL_TASKS {
        let mut tf: HashMap<String, f64> = HashMap::new();
        let mut drawn = 0;
        for _ in 0..tokens * 4 {
            if drawn == tokens {
                break;
            }
            let x = rng.unit() * total;
            let w = words[cum.partition_point(|&c| c <= x).min(words.len() - 1)];
            if let Some(t) = term(p, w, names_ok) {
                *tf.entry(t).or_insert(0.0) += 1.0;
                drawn += 1;
            }
        }
        let q = tfidf(&tf, idf, Some(idf_unseen));
        let single = tf.len() == 1;
        let mut touched = Vec::new();
        for (t, qw) in &q {
            for &(ci, dw) in postings.get(t.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                if scores[ci].1 == 0 {
                    touched.push(ci);
                }
                scores[ci].0 += qw * dw;
                scores[ci].1 += 1;
            }
        }
        any.push(touched.iter().map(|&ci| scores[ci].0).fold(0.0, f64::max));
        two.push(
            touched
                .iter()
                .filter(|&&ci| evidence(&tf, tfs[ci], single) >= 2)
                .map(|&ci| scores[ci].0)
                .fold(0.0, f64::max),
        );
        for ci in touched {
            scores[ci] = (0.0, 0);
        }
    }
    any.sort_by(|a, b| a.total_cmp(b));
    two.sort_by(|a, b| a.total_cmp(b));
    (any, two)
}

/// How many pieces of topic evidence a candidate holds for a task: the
/// task's distinct terms it uses, or for a one-word task its uses of the
/// word (capped at 2).
fn evidence(q_tf: &HashMap<String, f64>, d_tf: &HashMap<String, f64>, single: bool) -> usize {
    q_tf.keys()
        .filter_map(|t| d_tf.get(t))
        .map(|&c| if single { (c as usize).min(2) } else { 1 })
        .sum()
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// Short-form kinds a request draws from: posts include quote posts.
fn kind_matches(want: ItemKind, have: ItemKind) -> bool {
    want == have || (want == ItemKind::Post && have == ItemKind::Quote)
}

/// Calendar quarter of an ISO date ("2026-10-19..." -> 2026*4 + 3).
fn quarter(date: &str) -> Option<i32> {
    let y: i32 = date.get(0..4)?.parse().ok()?;
    let m: i32 = date.get(5..7)?.parse().ok()?;
    (1..=12).contains(&m).then_some(y * 4 + (m - 1) / 3)
}

/// Words once links and handles are gone: a "lol https://t.co/x" post earns
/// its likes from the image, not the words.
pub fn bare_words(item: &str) -> usize {
    item.split_whitespace()
        .filter(|t| !t.starts_with("http") && !t.starts_with('@'))
        .map(text::count_words)
        .sum()
}

/// Percentile rank of each item's engagement among items of the same kind
/// and quarter (likes per impression when impressions are known, else
/// likes + 2 reposts). Groups under 8 items pool across quarters.
pub fn engagement_percentiles(p: &Profile, items: &[usize]) -> HashMap<usize, f64> {
    let mut values: Vec<(usize, ItemKind, Option<i32>, bool, f64)> = Vec::new();
    for &i in items {
        let Some(m) = &p.chunks[i].meta else { continue };
        let (has_imp, v) = match (m.impressions, m.likes) {
            (Some(imp), Some(l)) if imp > 0.0 => (true, l / imp),
            (_, Some(l)) => (false, l + 2.0 * m.reposts.unwrap_or(0.0)),
            _ => continue,
        };
        values.push((i, m.kind, m.date.as_deref().and_then(quarter), has_imp, v));
    }
    let mut groups: HashMap<(ItemKind, Option<i32>, bool), Vec<(usize, f64)>> = HashMap::new();
    for (i, k, q, imp, v) in &values {
        groups.entry((*k, *q, *imp)).or_default().push((*i, *v));
    }
    let mut pooled: HashMap<(ItemKind, bool), Vec<(usize, f64)>> = HashMap::new();
    let mut out = HashMap::new();
    for ((k, _, imp), g) in groups {
        if g.len() < 8 {
            pooled.entry((k, imp)).or_default().extend(g);
        } else {
            rank_into(&g, &mut out);
        }
    }
    for g in pooled.values() {
        rank_into(g, &mut out);
    }
    out
}

fn rank_into(g: &[(usize, f64)], out: &mut HashMap<usize, f64>) {
    let n = g.len();
    for (i, v) in g {
        let below = g.iter().filter(|(_, x)| x < v).count() as f64;
        let equal = g.iter().filter(|(_, x)| x == v).count() as f64;
        let pct = if n <= 1 { 0.5 } else { (below + 0.5 * (equal - 1.0)) / (n - 1) as f64 };
        out.insert(*i, pct.clamp(0.0, 1.0));
    }
}

/// The root of each item's thread (following parent ids within the corpus),
/// so at most one item per thread is picked.
fn thread_roots(p: &Profile, items: &[usize]) -> HashMap<usize, String> {
    let parent: HashMap<&str, &str> = p
        .chunks
        .iter()
        .filter_map(|c| c.meta.as_ref())
        .filter_map(|m| Some((m.id.as_deref()?, m.parent_id.as_deref()?)))
        .collect();
    items
        .iter()
        .map(|&i| {
            let id = p.chunks[i].meta.as_ref().and_then(|m| m.id.as_deref());
            let mut cur = match id {
                Some(id) => id,
                None => return (i, format!("#{i}")),
            };
            for _ in 0..64 {
                match parent.get(cur) {
                    Some(up) => cur = up,
                    None => break,
                }
            }
            (i, cur.to_string())
        })
        .collect()
}

/// Shape keys for long-form nearness; fiction adds nothing here (its mode
/// is matched separately).
const SHAPE_KEYS: [&str; 5] = [
    "dialogue_word_share",
    "sentence_words_mean",
    "short_sentence_share",
    "paragraph_words_mean",
    "one_sentence_paragraph_share",
];

struct Cand {
    chunks: Vec<usize>,
    tf: HashMap<String, f64>,
    opening: bool,
    shape: Vec<f64>,
    dialogue: Option<f64>,
    mode: Option<ModeLabel>,
    engagement: Option<f64>,
    best_ok: bool,
    words: usize,
    date: Option<String>,
    group: String,
}

pub fn retrieve(p: &Profile, req: &Request) -> Retrieval {
    let short = p.register.is_short_form();
    let fiction = p.register == Register::Fiction;
    let names_ok = !fiction;
    let mut notes = Vec::new();
    let mut cands = if short {
        short_candidates(p, req, &mut notes)
    } else {
        long_candidates(p, req)
    };
    cands.retain(|c| !c.chunks.iter().any(|i| req.exclude.contains(i)));
    let n_cands = cands.len();

    // Document frequencies over candidates; BM25 and TF-IDF share them.
    let mut df: HashMap<&str, f64> = HashMap::new();
    for c in &cands {
        for t in c.tf.keys() {
            *df.entry(t.as_str()).or_insert(0.0) += 1.0;
        }
    }
    let n = n_cands.max(1) as f64;
    let idf: HashMap<String, f64> = df
        .iter()
        .map(|(t, d)| (t.to_string(), (1.0 + (n - d + 0.5) / (d + 0.5)).ln()))
        .collect();
    let vecs: Vec<HashMap<String, f64>> = cands.iter().map(|c| tfidf(&c.tf, &idf, None)).collect();

    let q_tf = req.query.map(|q| terms_of(p, q, names_ok)).unwrap_or_default();
    let query_terms = q_tf.keys().filter(|t| idf.contains_key(*t)).count();
    // A term no candidate uses gets the IDF of document frequency zero.
    let idf_unseen = (1.0 + (n + 0.5) / 0.5).ln();
    let q_vec = tfidf(&q_tf, &idf, Some(idf_unseen));
    let avg_len = cands.iter().map(|c| c.tf.values().sum::<f64>()).sum::<f64>() / n;
    let bm25: Vec<f64> = cands
        .iter()
        .map(|c| {
            let len = c.tf.values().sum::<f64>();
            q_tf.keys()
                .filter_map(|t| {
                    let f = *c.tf.get(t)?;
                    let i = idf.get(t)?;
                    Some(i * f * 2.2 / (f + 1.2 * (0.25 + 0.75 * len / avg_len.max(1.0))))
                })
                .sum()
        })
        .collect();
    let topic: Vec<f64> = vecs.iter().map(|v| cosine(&q_vec, v)).collect();
    let best_topic = topic.iter().copied().fold(0.0, f64::max);
    // Strong needs a piece with two pieces of evidence (two of the task's
    // terms, or a one-word task's word twice: a single shared word, "bread"
    // in a novel or "team" in a post, is never a topic) and a cosine beyond
    // what 95% of random tasks reach under the same rule; weak, a best
    // cosine beyond the median random task's.
    let single = q_tf.len() == 1;
    let tfs: Vec<&HashMap<String, f64>> = cands.iter().map(|c| &c.tf).collect();
    let best_need = (0..n_cands)
        .filter(|&i| evidence(&q_tf, tfs[i], single) >= 2)
        .map(|i| topic[i])
        .fold(0.0, f64::max);
    let (weak_line, strong_line) = if q_tf.is_empty() {
        (WEAK, STRONG)
    } else {
        let (any, two) = null_cosines(p, &vecs, &tfs, &idf, idf_unseen, q_tf.values().sum::<f64>() as usize, names_ok);
        (WEAK.max(quantile(&any, 0.5)), STRONG.max(quantile(&two, 0.95)))
    };
    let mut topic_match = if best_need >= strong_line {
        TopicMatch::Strong
    } else if best_topic >= weak_line {
        TopicMatch::Weak
    } else {
        TopicMatch::None
    };
    let mut lexical_note = None;
    if req.query.is_some() {
        // A word in a quarter of the pieces or more is the corpus's own
        // subject, so no single piece stands out on it.
        let everywhere = q_tf.keys().any(|t| df.get(t.as_str()).is_some_and(|d| *d >= 0.25 * n));
        lexical_note = match (topic_match, single, everywhere) {
            (TopicMatch::Strong, ..) => None,
            (TopicMatch::None, _, true) => Some(
                "the task's words run through much of the corpus, so no piece stands out on them: these are typical pieces instead",
            ),
            (TopicMatch::None, _, false) => Some("nothing in the corpus is about the task: these are typical pieces instead"),
            (TopicMatch::Weak, true, _) => Some(
                "one word is thin evidence of a topic: these pieces use it, but describe the task in a few words for a firmer match",
            ),
            (TopicMatch::Weak, false, _) => Some(
                "no piece is clearly about the task (its words are rare in the corpus or spread thinly across it): take these pieces' voice, not their subject",
            ),
        };
    }
    // BM25 relative to the best candidate, scaled by how far the evidence
    // goes: 1 for a strong match, at most 0.5 for a weak one, so a loosely
    // related piece cannot read as a 0.95 match.
    let max_bm = bm25.iter().copied().fold(0.0, f64::max);
    let confidence = match topic_match {
        TopicMatch::Strong => 1.0,
        TopicMatch::Weak => 0.5 * (best_topic / strong_line).min(1.0),
        TopicMatch::None => 0.0,
    };
    let topic_n: Vec<f64> = if confidence <= 0.0 || max_bm <= 0.0 {
        vec![0.0; n_cands]
    } else {
        bm25.iter().map(|b| confidence * b / max_bm).collect()
    };

    // Style nearness (long-form): to the draft's own shape when one was
    // given, else to the author's medians (typical passages).
    let shape_sim = if short { vec![None; n_cands] } else { shape_nearness(&cands, req.query, fiction) };
    let recency = recency_ranks(&cands);
    let length_typ = length_typicality(&cands);

    // Topic's share of the score by register; the rest is engagement and
    // length (short-form) or style nearness (long-form). With a task, the
    // topic share stays in even when nothing matches, so a typical piece
    // offered for an unmatched task never reads as a near-perfect match.
    let has_eng = cands.iter().any(|c| c.engagement.is_some());
    let w_topic = match (short, has_eng, fiction) {
        (true, true, _) => 0.75,
        (true, false, _) => 0.8,
        (false, _, true) => 0.2,
        (false, _, false) => 0.5,
    };
    let mut score: Vec<f64> = (0..n_cands)
        .map(|i| {
            let rest = if short {
                let eng = if cands[i].best_ok { cands[i].engagement.unwrap_or(0.5) } else { 0.0 };
                match (topic_match, has_eng) {
                    (TopicMatch::None, true) => 0.6 * eng + 0.4 * length_typ[i],
                    (_, true) => eng,
                    (_, false) => length_typ[i],
                }
            } else {
                shape_sim[i].unwrap_or(0.5)
            };
            let base = if req.query.is_some() { w_topic * topic_n[i] + (1.0 - w_topic) * rest } else { rest };
            base + 0.02 * recency[i]
        })
        .collect();

    // JEV re-ranks the top of that ranking by fit to a described task
    // (not fiction, where it measured no better than scene shape).
    let task = req
        .query
        .filter(|q| req.jev && !fiction && !q.trim().is_empty() && text::count_words(q) <= JEV_TASK_WORDS);
    let judged = match task {
        Some(t) => {
            // The pool is what `examples -n 24` would list: diversified
            // like the final picks, so one work's many windows can't fill
            // it and leave the one-per-work picks to unjudged passages.
            let pool = mmr(&cands, &vecs, &score, JEV_POOL, short, topic_match != TopicMatch::None);
            let excerpts: Vec<String> = pool.iter().map(|&i| excerpt(p, &cands[i], &q_tf, &idf, names_ok)).collect();
            judge_fit(n_cands, &pool, &excerpts, t).map(|fits| (pool, fits))
        }
        None => Err(None),
    };
    let (rank_basis, fits, rank_note) = match judged {
        Ok((pool, fits)) => {
            score = fit_scores(&score, &pool, &fits);
            let best = fits.iter().flatten().copied().fold(0.0, f64::max);
            topic_match = fit_match(best, topic_match);
            lexical_note = match topic_match {
                TopicMatch::Strong => None,
                TopicMatch::Weak => Some(
                    "no passage is a close model for the task (the nearest share its broad subject, not its situation or kind of piece): take their voice, not their subject",
                ),
                TopicMatch::None => Some(
                    "nothing in the corpus is like the task: these passages are the least far off, so take their voice, not their subject",
                ),
            };
            (RankBasis::Jev, fits, None)
        }
        Err(why) => (RankBasis::Lexical, vec![None; n_cands], why),
    };
    notes.extend(lexical_note.map(String::from));

    let mut picks = mmr(&cands, &vecs, &score, req.n, short, topic_match != TopicMatch::None);
    if req.mix_dialogue && !short {
        mix_dialogue(&cands, &score, &mut picks);
    }
    let hits = picks
        .into_iter()
        .map(|i| Hit {
            chunks: cands[i].chunks.clone(),
            similarity: voice::r3(score[i].clamp(0.0, 1.0)),
            topic: if topic[i] > 0.0 { voice::r3(topic[i]) } else { 0.0 },
            shape: shape_sim[i].map(voice::r3),
            engagement_pct: cands[i].engagement.map(voice::r3),
            dialogue_share: cands[i].dialogue.map(voice::r3),
            mode: cands[i].mode,
            opening: cands[i].opening,
            fit: fits[i].map(voice::r3),
        })
        .collect();
    Retrieval {
        hits,
        topic_match,
        rank_basis,
        candidates: n_cands,
        query_terms,
        notes,
        rank_note,
    }
}

/// The span JEV judges for a candidate (a narrow question wants a short
/// span, never a whole window): the run of whole paragraphs, up to
/// `EXCERPT_WORDS`, holding the most IDF weight of the task's terms; the
/// opening when none of them appears. A longer paragraph is cut.
fn excerpt(p: &Profile, c: &Cand, q_tf: &HashMap<String, f64>, idf: &HashMap<String, f64>, names_ok: bool) -> String {
    let paras: Vec<&str> = c.chunks.iter().flat_map(|&i| p.chunks[i].paragraphs.iter().map(String::as_str)).collect();
    let mut best: Option<(f64, String)> = None;
    for start in 0..paras.len() {
        let (mut parts, mut words) = (Vec::new(), 0);
        for para in &paras[start..] {
            let n = text::count_words(para);
            if words > 0 && words + n > EXCERPT_WORDS {
                break;
            }
            parts.push(*para);
            words += n;
        }
        let span = parts.join("\n\n");
        let tf = terms_of(p, &span, names_ok);
        let weight: f64 = q_tf.keys().filter(|t| tf.contains_key(*t)).filter_map(|t| idf.get(t)).sum();
        if best.as_ref().is_none_or(|(b, _)| weight > *b) {
            best = Some((weight, span));
        }
    }
    let span = texture::normalize(&best.map(|b| b.1).unwrap_or_default());
    span.split(' ').take(EXCERPT_WORDS).collect::<Vec<_>>().join(" ")
}

/// JEV's fit to the task for the candidates in `pool`, judged on their
/// `excerpts` (None for the rest). Err carries why not: None when JEV is
/// off or has no key (the word ranking is then the normal path, not a
/// failure), a note when it was asked and did not answer for every one.
fn judge_fit(n_cands: usize, pool: &[usize], excerpts: &[String], task: &str) -> Result<Vec<Option<f64>>, Option<String>> {
    if pool.is_empty() {
        return Err(None);
    }
    let jev = Jev::load().map_err(|_| None)?;
    let task = texture::normalize(task);
    let states: Vec<serde_json::Value> = excerpts
        .iter()
        .map(|x| json!({ "piece_to_write": task, "author_passage": x }))
        .collect();
    let answers = jev.ask_all(&states, &crate::engine::jev::questions("relevance"));
    let got: Vec<Option<f64>> = answers.iter().map(|a| a.as_ref().and_then(|a| a.score("fit"))).collect();
    let failed = got.iter().filter(|f| f.is_none()).count();
    if failed > 0 {
        return Err(Some(format!(
            "JEV did not judge {failed} of {} passages (timeout, bad answer or no connection): ranked by shared words instead",
            pool.len()
        )));
    }
    let mut fits = vec![None; n_cands];
    for (&i, f) in pool.iter().zip(got) {
        fits[i] = f.map(|f| f.min(3.0));
    }
    Ok(fits)
}

/// `TopicMatch` from the best fit in the pool. A strong word match (two of
/// the task's terms in one piece, beyond what random tasks reach) means the
/// corpus does take up the subject, so JEV can lower it to weak but never
/// to none: "nothing in the corpus is like the task" would be false.
fn fit_match(best: f64, lexical: TopicMatch) -> TopicMatch {
    if best >= FIT_STRONG {
        TopicMatch::Strong
    } else if best >= FIT_WEAK || lexical == TopicMatch::Strong {
        TopicMatch::Weak
    } else {
        TopicMatch::None
    }
}

/// Scores once JEV has judged the pool: fit (0-3) for three quarters and
/// the pool's own order (the word ranking) for a quarter, mapped above
/// every unjudged candidate (those keep their order, below 0.1), so MMR
/// and the dialogue mix see the new order.
fn fit_scores(score: &[f64], pool: &[usize], fits: &[Option<f64>]) -> Vec<f64> {
    let m = pool.len().max(1) as f64;
    let mut out: Vec<f64> = score.iter().map(|s| 0.1 * s.clamp(0.0, 1.0)).collect();
    for (r, &i) in pool.iter().enumerate() {
        let f = fits[i].unwrap_or(0.0);
        out[i] = 0.1 + 0.9 * ((1.0 - JEV_LEXICAL) * f / 3.0 + JEV_LEXICAL * (1.0 - r as f64 / m));
    }
    out
}

fn short_candidates(p: &Profile, req: &Request, notes: &mut Vec<String>) -> Vec<Cand> {
    let all: Vec<usize> = (0..p.chunks.len()).collect();
    let kind_of = |i: usize| p.chunks[i].meta.as_ref().map(|m| m.kind).unwrap_or_default();
    let mut items: Vec<usize> = all.iter().copied().filter(|&i| kind_matches(req.kind, kind_of(i))).collect();
    if items.is_empty() {
        notes.push(format!(
            "no items of kind '{}' in this profile: drawing from all items",
            kind_label(req.kind)
        ));
        items = all;
    }
    let eng = engagement_percentiles(p, &items);
    let roots = thread_roots(p, &items);
    items
        .into_iter()
        .map(|i| {
            let c = &p.chunks[i];
            let t = c.text();
            let words = bare_words(&t);
            Cand {
                chunks: vec![i],
                tf: terms_of(p, &t, true),
                opening: false,
                shape: Vec::new(),
                dialogue: None,
                mode: None,
                engagement: eng.get(&i).copied(),
                best_ok: words >= 6 && !c.meta.as_ref().is_some_and(|m| m.media),
                words,
                date: c.meta.as_ref().and_then(|m| m.date.clone()),
                group: roots.get(&i).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

pub fn kind_label(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Post => "post",
        ItemKind::Reply => "reply",
        ItemKind::ThreadPart => "thread",
        ItemKind::Quote => "quote",
    }
}

fn long_candidates(p: &Profile, req: &Request) -> Vec<Cand> {
    let fiction = p.register == Register::Fiction;
    let blog = p.register == Register::Blog;
    let windows = units(p, req.window_words);
    let defs = metrics::defs(&[]);
    let idx: Vec<usize> = SHAPE_KEYS
        .iter()
        .filter_map(|k| defs.iter().position(|d| d.key == *k))
        .collect();
    // Position of each window within its work, to skip front and back matter.
    let mut per_src: HashMap<u32, Vec<usize>> = HashMap::new();
    for (wi, w) in windows.iter().enumerate() {
        per_src.entry(p.chunks[w[0]].source).or_default().push(wi);
    }
    let mut out = Vec::new();
    for (wi, w) in windows.iter().enumerate() {
        let src = p.chunks[w[0]].source;
        let same = &per_src[&src];
        let pos = same.iter().position(|&o| o == wi).unwrap_or(0);
        let opening = pos == 0;
        if req.openings_only && !opening {
            continue;
        }
        // Blogs keep their openings (what agents imitate best); books skip
        // the first and last two windows of a long work.
        if !req.openings_only && !blog && same.len() > 4 && (pos < 2 || pos + 2 >= same.len()) {
            continue;
        }
        let paras: Vec<String> = w.iter().flat_map(|&i| p.chunks[i].paragraphs.iter().cloned()).collect();
        let vals = metrics::measure(&paras, &[]);
        let shape: Vec<f64> = idx.iter().map(|&j| vals[j]).collect();
        let joined = paras.join("\n\n");
        out.push(Cand {
            chunks: w.clone(),
            tf: terms_of(p, &joined, !fiction),
            opening,
            dialogue: shape.first().copied(),
            mode: if fiction { voice::mode_of(&paras) } else { None },
            shape,
            engagement: None,
            best_ok: true,
            words: text::count_words(&joined),
            date: None,
            group: format!("src{src}"),
        });
    }
    out
}

/// Exemplar units of about `words` words that never cross a work (an
/// exemplar straddling the end of one post and the start of the next
/// teaches a seam, not a voice): consecutive passages of one source until
/// the length is reached; a short remainder joins the unit before it, and a
/// work shorter than the length is one unit on its own.
fn units(p: &Profile, words: usize) -> Vec<Vec<usize>> {
    let words = words.max(1) as u32;
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut i = 0;
    while i < p.chunks.len() {
        let src = p.chunks[i].source;
        let mut j = i;
        while j < p.chunks.len() && p.chunks[j].source == src {
            j += 1;
        }
        let first = out.len();
        let (mut cur, mut n) = (Vec::new(), 0u32);
        for k in i..j {
            cur.push(k);
            n += p.chunks[k].n_words;
            if n >= words {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
        }
        if !cur.is_empty() {
            match out.get_mut(first..).and_then(|w| w.last_mut()) {
                Some(last) if n * 2 < words => last.extend(cur),
                _ => out.push(cur),
            }
        }
        i = j;
    }
    out
}

/// Nearness of each candidate's shape to the target: the query draft's own
/// shape when it is long enough to measure (150+ words), else the median
/// candidate. Distances are in units of the candidates' p15–p85 width; for
/// fiction a different POV/tense mode halves the nearness.
fn shape_nearness(cands: &[Cand], query: Option<&str>, fiction: bool) -> Vec<Option<f64>> {
    if cands.is_empty() {
        return Vec::new();
    }
    let k = cands[0].shape.len();
    let mut bands: Vec<(f64, f64)> = Vec::with_capacity(k);
    for j in 0..k {
        let mut v: Vec<f64> = cands.iter().map(|c| c.shape[j]).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
        bands.push((q(0.5), (q(0.85) - q(0.15)).abs().max(1e-6)));
    }
    let query_paras = query
        .filter(|q| text::count_words(q) >= 150)
        .map(text::paragraphs)
        .filter(|ps| !ps.is_empty());
    let (target, q_mode): (Vec<f64>, Option<ModeLabel>) = match &query_paras {
        Some(ps) => {
            let defs = metrics::defs(&[]);
            let vals = metrics::measure(ps, &[]);
            let t = SHAPE_KEYS
                .iter()
                .filter_map(|key| defs.iter().position(|d| d.key == *key))
                .map(|j| vals[j])
                .collect();
            (t, if fiction { voice::mode_of(ps) } else { None })
        }
        None => (bands.iter().map(|b| b.0).collect(), None),
    };
    cands
        .iter()
        .map(|c| {
            let d2: f64 = (0..k)
                .map(|j| ((c.shape[j] - target[j]) / bands[j].1).powi(2).min(9.0))
                .sum::<f64>()
                / k.max(1) as f64;
            let mut s = (-0.5 * d2).exp();
            if let (Some(q), Some(m)) = (q_mode, c.mode) {
                if q != m {
                    s *= 0.5;
                }
            }
            Some(s)
        })
        .collect()
}

/// 0 (oldest) to 1 (newest) by date; 0 for undated pieces.
fn recency_ranks(cands: &[Cand]) -> Vec<f64> {
    let mut dated: Vec<(usize, &str)> = cands
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.date.as_deref().map(|d| (i, d)))
        .collect();
    dated.sort_by(|a, b| a.1.cmp(b.1));
    let mut out = vec![0.0; cands.len()];
    let n = dated.len();
    for (r, (i, _)) in dated.into_iter().enumerate() {
        out[i] = if n <= 1 { 1.0 } else { r as f64 / (n - 1) as f64 };
    }
    out
}

/// 1 at the median length, falling to 0 at the shortest and longest pieces.
fn length_typicality(cands: &[Cand]) -> Vec<f64> {
    let mut lens: Vec<usize> = cands.iter().map(|c| c.words).collect();
    lens.sort_unstable();
    let n = lens.len().max(1) as f64;
    cands
        .iter()
        .map(|c| {
            // Mid-rank, so equal lengths all sit at the median.
            let below = lens.partition_point(|&l| l < c.words) as f64;
            let equal = (lens.partition_point(|&l| l <= c.words) as f64) - below;
            1.0 - (((below + equal / 2.0) / n) - 0.5).abs() * 2.0
        })
        .collect()
}

/// Maximal marginal relevance: relevance minus similarity to what is
/// already picked, at most one piece per thread (short-form) and one per
/// work until every work has given one (long-form).
///
/// With a topic in play, "every work" means every work in the shortlist:
/// a dominant work can fill it, and the rest are off topic, so waiting for
/// them would stop at one pick or pad the list with unrelated passages.
/// Without one, it means every work, found past the shortlist if need be.
/// A shortlist with nothing admissible left widens to every candidate, so
/// `n` picks come back whenever `n` threads exist.
fn mmr(cands: &[Cand], vecs: &[HashMap<String, f64>], score: &[f64], n: usize, short: bool, topical: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    // MMR over a shortlist keeps it cheap on big corpora.
    let shortlist = &order[..order.len().min((n * 20).max(60))];
    let pool = if topical { shortlist } else { &order[..] };
    let works: HashSet<&str> = pool.iter().map(|&i| cands[i].group.as_str()).collect();
    let mut picked: Vec<usize> = Vec::new();
    let mut groups: HashSet<&str> = HashSet::new();
    let best_in = |pool: &[usize], picked: &[usize], groups: &HashSet<&str>| {
        let mut best: Option<(f64, usize)> = None;
        for &i in pool {
            if picked.contains(&i) {
                continue;
            }
            let group_used = groups.contains(cands[i].group.as_str());
            if group_used && (short || groups.len() < works.len()) {
                continue;
            }
            let redundancy = picked.iter().map(|&j| cosine(&vecs[i], &vecs[j])).fold(0.0, f64::max);
            let m = LAMBDA * score[i] - (1.0 - LAMBDA) * redundancy;
            if best.is_none_or(|(b, _)| m > b) {
                best = Some((m, i));
            }
        }
        best.map(|(_, i)| i)
    };
    while picked.len() < n {
        let next = best_in(shortlist, &picked, &groups).or_else(|| best_in(&order, &picked, &groups));
        let Some(i) = next else { break };
        groups.insert(cands[i].group.as_str());
        picked.push(i);
    }
    picked
}

/// Keep at least one dialogue-rich and one narration pick when the author
/// writes dialogue: swap the weakest pick for the best missing kind.
fn mix_dialogue(cands: &[Cand], score: &[f64], picks: &mut [usize]) {
    let mut ds: Vec<f64> = cands.iter().filter_map(|c| c.dialogue).collect();
    if ds.is_empty() || picks.len() < 2 {
        return;
    }
    ds.sort_by(|a, b| a.total_cmp(b));
    let median = ds[ds.len() / 2];
    if median < 0.1 {
        return;
    }
    let rich = |i: usize| cands[i].dialogue.unwrap_or(0.0) > median.max(0.15);
    for want_rich in [true, false] {
        if picks.iter().any(|&i| rich(i) == want_rich) {
            continue;
        }
        let best = (0..cands.len())
            .filter(|&i| rich(i) == want_rich && !picks.contains(&i))
            .max_by(|a, b| score[*a].total_cmp(&score[*b]));
        if let Some(b) = best {
            let last = picks.len() - 1;
            picks[last] = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_never_cross_works() {
        let doc = |name: &str, n: usize| crate::engine::text::Doc {
            name: name.into(),
            text: (0..n)
                .map(|i| format!("Paragraph {i} is a plain sentence with some words in it, and then another clause follows here."))
                .collect::<Vec<_>>()
                .join("\n\n"),
            items: Vec::new(),
        };
        let reg = Some((Register::Blog, "test".to_string()));
        let p = Profile::build_with(
            "t",
            &[doc("a.md", 60), doc("b.md", 14), doc("c.md", 60)],
            crate::engine::profile::ProfileKind::Author,
            reg,
            1,
        )
        .unwrap();
        let us = units(&p, 500);
        assert!(us.iter().all(|u| u.iter().all(|&i| p.chunks[i].source == p.chunks[u[0]].source)));
        // The short work is one unit of its own.
        assert!(us.iter().any(|u| p.chunks[u[0]].source == 1));
    }

    #[test]
    fn unseen_task_words_dilute_the_match() {
        let tf = |ws: &[&str]| ws.iter().map(|w| (w.to_string(), 1.0)).collect::<HashMap<_, _>>();
        let idf: HashMap<String, f64> = [("bread", 3.0), ("door", 1.0)].iter().map(|(w, i)| (w.to_string(), *i)).collect();
        let doc = tfidf(&tf(&["bread", "door"]), &idf, None);
        let task = tf(&["recipe", "sourdough", "bread"]);
        // Dropping the corpus's unknown words turns the task into "bread".
        let blind = cosine(&tfidf(&task, &idf, None), &doc);
        let seen = cosine(&tfidf(&task, &idf, Some(6.0)), &doc);
        assert!(blind > 0.9 && seen < 0.4, "blind {blind} seen {seen}");
    }

    #[test]
    fn one_word_tasks_need_the_word_twice() {
        let tf = |ws: &[(&str, f64)]| ws.iter().map(|(w, c)| (w.to_string(), *c)).collect::<HashMap<_, _>>();
        let task = tf(&[("hiring", 1.0)]);
        assert_eq!(evidence(&task, &tf(&[("hiring", 1.0)]), true), 1);
        assert_eq!(evidence(&task, &tf(&[("hiring", 5.0)]), true), 2);
        let task = tf(&[("recipe", 1.0), ("bread", 1.0)]);
        assert_eq!(evidence(&task, &tf(&[("bread", 4.0)]), false), 1);
        assert_eq!(evidence(&task, &tf(&[("bread", 1.0), ("recipe", 1.0)]), false), 2);
    }

    #[test]
    fn stems_meet() {
        assert_eq!(stem("launching"), stem("launch"));
        assert_eq!(stem("launched"), "launch");
        assert_eq!(stem("keys"), "key");
        assert_eq!(stem("stories"), "story");
        assert_eq!(stem("class"), "class");
    }

    #[test]
    fn quarters_and_bare_words() {
        assert_eq!(quarter("2026-10-19T21:05:16.000Z"), Some(2026 * 4 + 3));
        assert_eq!(quarter("Wed Oct"), None);
        assert_eq!(bare_words("lol https://t.co/x"), 1);
        assert_eq!(bare_words("@alice thanks a lot for this"), 5);
    }

    #[test]
    fn fit_lines_and_the_word_match_floor() {
        // Excerpt fits from the eval: a close essay task, the same-subject
        // and unrelated tasks either side of the weak line.
        assert_eq!(fit_match(1.92, TopicMatch::Strong), TopicMatch::Strong);
        assert_eq!(fit_match(1.99, TopicMatch::Weak), TopicMatch::Strong);
        assert_eq!(fit_match(0.94, TopicMatch::None), TopicMatch::Weak);
        assert_eq!(fit_match(0.71, TopicMatch::Weak), TopicMatch::None);
        // Shared words beyond chance: JEV lowers strong to weak, not none.
        assert_eq!(fit_match(0.4, TopicMatch::Strong), TopicMatch::Weak);
    }

    #[test]
    fn fit_orders_the_pool_above_the_rest() {
        // Candidates 0-2 judged (pool order 1, 0, 2), 3 not.
        let score = [0.9, 0.95, 0.5, 0.99];
        let fits = [Some(0.5), Some(1.0), Some(3.0), None];
        let s = fit_scores(&score, &[1, 0, 2], &fits);
        assert!(s[2] > s[1] && s[1] > s[0], "{s:?}");
        assert!(s[3] < s[0] && s[3] <= 0.1, "unjudged stays below the pool: {s:?}");
        // Equal fits: the pool's order (the word ranking) decides.
        let s = fit_scores(&score, &[1, 0, 2], &[Some(1.0), Some(1.0), Some(1.0), None]);
        assert!(s[1] > s[0] && s[0] > s[2], "{s:?}");
    }

    #[test]
    fn engagement_ranks_within_groups() {
        let g = vec![(0, 1.0), (1, 5.0), (2, 3.0)];
        let mut out = HashMap::new();
        rank_into(&g, &mut out);
        assert_eq!(out[&1], 1.0);
        assert_eq!(out[&0], 0.0);
        assert_eq!(out[&2], 0.5);
    }
}
