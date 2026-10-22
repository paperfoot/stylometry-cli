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
//! Picks are diversified with MMR (one per thread, one per work first), and
//! when nothing shares the task's vocabulary the result says
//! `topic_match: none` and falls back to typical pieces.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::engine::metrics;
use crate::engine::profile::{ItemKind, Profile, Register};
use crate::engine::voice::{self, ModeLabel};
use crate::engine::{lexicon, style, text};

/// How strongly the best piece shares the task's vocabulary (TF-IDF cosine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicMatch {
    Strong,
    Weak,
    /// No task given, or nothing in the corpus is about it: typical pieces.
    None,
}

/// Cosine thresholds for `TopicMatch`, tuned on a blog, an essay book, a
/// novel blend and an X account: below 0.05 the "best" match is shared
/// stopword-like vocabulary.
const WEAK: f64 = 0.05;
const STRONG: f64 = 0.12;
/// MMR trade-off between relevance and novelty.
const LAMBDA: f64 = 0.7;

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
}

pub struct Retrieval {
    pub hits: Vec<Hit>,
    pub topic_match: TopicMatch,
    /// Pieces considered.
    pub candidates: usize,
    /// Content words of the task found anywhere in the corpus.
    pub query_terms: usize,
    pub notes: Vec<String>,
}

/// Content-word stem, or None for words that carry no topic.
fn term(p: &Profile, w: &str, names_ok: bool) -> Option<String> {
    let w = w.strip_suffix("'s").unwrap_or(w);
    if w.chars().count() < 3
        || w.chars().any(|c| c.is_ascii_digit())
        || matches!(w, "url" | "user")
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

/// Sublinear TF-IDF vector, L2-normalised.
fn tfidf(tf: &HashMap<String, f64>, idf: &HashMap<String, f64>) -> HashMap<String, f64> {
    let mut v: HashMap<String, f64> = tf
        .iter()
        .filter_map(|(t, c)| idf.get(t).map(|i| (t.clone(), (1.0 + c.ln()) * i)))
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
    let vecs: Vec<HashMap<String, f64>> = cands.iter().map(|c| tfidf(&c.tf, &idf)).collect();

    let q_tf = req.query.map(|q| terms_of(p, q, names_ok)).unwrap_or_default();
    let query_terms = q_tf.keys().filter(|t| idf.contains_key(*t)).count();
    let q_vec = tfidf(&q_tf, &idf);
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
    let topic_match = if best_topic >= STRONG {
        TopicMatch::Strong
    } else if best_topic >= WEAK {
        TopicMatch::Weak
    } else {
        TopicMatch::None
    };
    if req.query.is_some() && topic_match == TopicMatch::None {
        notes.push("nothing in the corpus shares the task's vocabulary: these are typical pieces instead".into());
    }
    let max_bm = bm25.iter().copied().fold(0.0, f64::max);
    let topic_n: Vec<f64> = if topic_match == TopicMatch::None || max_bm <= 0.0 {
        vec![0.0; n_cands]
    } else {
        bm25.iter().map(|b| b / max_bm).collect()
    };

    // Style nearness (long-form): to the draft's own shape when one was
    // given, else to the author's medians (typical passages).
    let shape_sim = if short { vec![None; n_cands] } else { shape_nearness(&cands, req.query, fiction) };
    let recency = recency_ranks(&cands);
    let length_typ = length_typicality(&cands);

    let score: Vec<f64> = (0..n_cands)
        .map(|i| {
            let t = topic_n[i];
            let base = if short {
                let eng = if cands[i].best_ok { cands[i].engagement.unwrap_or(0.5) } else { 0.0 };
                let has_eng = cands.iter().any(|c| c.engagement.is_some());
                match (topic_match, has_eng) {
                    (TopicMatch::None, true) => 0.6 * eng + 0.4 * length_typ[i],
                    (TopicMatch::None, false) => length_typ[i],
                    (_, true) => 0.75 * t + 0.25 * eng,
                    (_, false) => 0.8 * t + 0.2 * length_typ[i],
                }
            } else {
                let s = shape_sim[i].unwrap_or(0.5);
                match (fiction, topic_match) {
                    (true, TopicMatch::None) | (false, TopicMatch::None) => s,
                    (true, _) => 0.2 * t + 0.8 * s,
                    (false, _) => 0.5 * t + 0.5 * s,
                }
            };
            base + 0.02 * recency[i]
        })
        .collect();

    let picks = mmr(&cands, &vecs, &score, req.n, short, p.sources.len());
    let mut picks = picks;
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
        })
        .collect();
    Retrieval {
        hits,
        topic_match,
        candidates: n_cands,
        query_terms,
        notes,
    }
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
fn mmr(cands: &[Cand], vecs: &[HashMap<String, f64>], score: &[f64], n: usize, short: bool, n_src: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    // MMR over a shortlist keeps it cheap on big corpora.
    order.truncate((n * 20).max(60));
    let mut picked: Vec<usize> = Vec::new();
    let mut groups: HashSet<&str> = HashSet::new();
    while picked.len() < n {
        let mut best: Option<(f64, usize)> = None;
        for &i in &order {
            if picked.contains(&i) {
                continue;
            }
            let g = cands[i].group.as_str();
            let group_used = groups.contains(g);
            if group_used && (short || groups.len() < n_src) {
                continue;
            }
            let redundancy = picked.iter().map(|&j| cosine(&vecs[i], &vecs[j])).fold(0.0, f64::max);
            let m = LAMBDA * score[i] - (1.0 - LAMBDA) * redundancy;
            if best.is_none_or(|(b, _)| m > b) {
                best = Some((m, i));
            }
        }
        let Some((_, i)) = best else { break };
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
    fn engagement_ranks_within_groups() {
        let g = vec![(0, 1.0), (1, 5.0), (2, 3.0)];
        let mut out = HashMap::new();
        rank_into(&g, &mut out);
        assert_eq!(out[&1], 1.0);
        assert_eq!(out[&0], 0.0);
        assert_eq!(out[&2], 0.5);
    }
}
