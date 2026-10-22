//! Prose texture, judged by JEV passage by passage: how rough or polished
//! the writing is, whether passages end on crafted lines, how figurative it
//! is, and which kinds of roughness it has. Measured on the author's own
//! passages to give bands, then on a draft, so every judgment is relative
//! to the writer (a clean essayist and a loose poster differ a lot).
//!
//! Why: blind readers told imitations from the real writer by polish that
//! counts cannot see (aphorisms, kickers, no rough edges). On 2026-10-22
//! the `rough` question separated genuine from imitation at AUC 0.90-1.00
//! for novels, essays and X posts, where the stylometric score managed
//! 0.00-0.78. Used as a target alone it is gamed (agents add filler
//! hedges), so the kinds are bounded on both sides.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::engine::jev::{self, Answers, Jev, UsageReport};
use crate::engine::profile::{ItemKind, Profile, ProfileKind};
use crate::engine::{store, text};

/// Which side of the author's band is a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The draft has too little of it (below the band).
    Low,
    /// The draft has too much of it (above the band).
    High,
}

pub struct TextureKey {
    /// Question id in `data/jev_texture.json`.
    pub key: &'static str,
    pub label: &'static str,
    pub side: Side,
    /// Gate keys decide `texture.pass`; the rest are advice.
    pub gate: bool,
    /// What to do when the draft is off on this side.
    pub advice: &'static str,
}

/// The measured keys, gate keys first. Gate: the five that separated
/// genuine from imitation and resisted gaming in rounds 3-4.
pub const KEYS: [TextureKey; 9] = [
    TextureKey { key: "rough", label: "rough edges", side: Side::Low, gate: true,
        advice: "loosen it in the author's own ways (the kinds below), not with filler" },
    TextureKey { key: "polish", label: "polish (0 plain to 3 highly crafted)", side: Side::High, gate: true,
        advice: "write plainer: say what happens and what is thought, directly" },
    TextureKey { key: "button", label: "crafted closing lines", side: Side::High, gate: true,
        advice: "end passages on ordinary sentences, not kickers, reveals or callbacks" },
    TextureKey { key: "figurative", label: "fresh figures of speech", side: Side::High, gate: true,
        advice: "cut inventive metaphors and similes" },
    TextureKey { key: "hedge", label: "filler hedges (I guess, kind of)", side: Side::High, gate: true,
        advice: "cut filler hedges; the author rarely uses them" },
    TextureKey { key: "overexplain", label: "over-explaining", side: Side::Low, gate: false,
        advice: "spell out reasons, logistics and feelings a little more fully, as the author does" },
    TextureKey { key: "repeat", label: "repeating a noun instead of a pronoun", side: Side::Low, gate: false,
        advice: "repeat the key noun now and then where the author would" },
    TextureKey { key: "splice", label: "loosely joined, run-on sentences", side: Side::Low, gate: false,
        advice: "let a few sentences run on, joined loosely with commas or \"and\"" },
    TextureKey { key: "oneliner", label: "dramatic one-line paragraphs", side: Side::High, gate: false,
        advice: "fold some standalone beats back into their paragraphs" },
];

/// About this many words per judged passage (paragraph-aligned), as in the
/// eval that set the bands.
pub const PASSAGE_WORDS: usize = 150;
/// Shorter trailing groups are dropped; shorter whole texts are one passage.
pub const MIN_PASSAGE_WORDS: usize = 60;
/// Short-form batches are judged this many posts at a time: single posts
/// are too short to judge (rough AUC 0.79 per post vs 0.92 per batch).
pub const POSTS_PER_PASSAGE: usize = 4;

/// One judged unit of text, with the 1-based line range it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// The text a passage is judged on: typography evened out (texture is not
/// about marks; `check` holds those to the author separately), links and
/// footnote markers dropped, hard wraps joined.
pub fn normalize(t: &str) -> String {
    let t = t
        .replace(['\u{201C}', '\u{201D}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace('\u{2026}', "...");
    let mut out = String::with_capacity(t.len());
    for word in t.split_inclusive(char::is_whitespace) {
        let w = word.trim_end();
        if w.starts_with("http://") || w.starts_with("https://") {
            out.push_str(&word[w.len()..]);
            continue;
        }
        out.push_str(word);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a draft or an author's text into judged passages: long-form text
/// into paragraph-aligned groups of about `PASSAGE_WORDS`, short-form text
/// (posts separated by `---` lines) into batches of `POSTS_PER_PASSAGE`.
pub fn passages(raw: &str, short_form: bool) -> Vec<Passage> {
    // Blocks: blank-line separated runs of lines, with their line ranges;
    // `---` lines separate posts and scene breaks, and belong to no block.
    let mut blocks: Vec<(String, usize, usize, bool)> = Vec::new(); // (text, start, end, starts_post)
    let mut cur: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut new_post = true;
    let lines: Vec<&str> = raw.lines().collect();
    let flush = |cur: &mut Vec<&str>, start: usize, end: usize, new_post: &mut bool, blocks: &mut Vec<(String, usize, usize, bool)>| {
        if !cur.is_empty() {
            blocks.push((cur.join("\n"), start, end, *new_post));
            cur.clear();
            *new_post = false;
        }
    };
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let is_break = t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | '*' | '•' | '#' | '~' | '=' | '_' | '·' | ' '));
        if t.is_empty() || is_break || text::is_heading(t) {
            flush(&mut cur, start, i, &mut new_post, &mut blocks);
            if is_break {
                new_post = true;
            }
            continue;
        }
        if cur.is_empty() {
            start = i + 1;
        }
        cur.push(line);
    }
    flush(&mut cur, start, lines.len(), &mut new_post, &mut blocks);

    let words = |s: &str| s.split_whitespace().count();
    let mut out = Vec::new();
    let mut group: Vec<&(String, usize, usize, bool)> = Vec::new();
    let emit = |group: &mut Vec<&(String, usize, usize, bool)>, out: &mut Vec<Passage>| {
        if let (Some(first), Some(last)) = (group.first(), group.last()) {
            let text = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
            out.push(Passage { text, start_line: first.1, end_line: last.2 });
        }
        group.clear();
    };
    if short_form {
        let mut posts = 0;
        for b in &blocks {
            if b.3 {
                if posts == POSTS_PER_PASSAGE {
                    emit(&mut group, &mut out);
                    posts = 0;
                }
                posts += 1;
            }
            group.push(b);
        }
        // A short trailing batch joins the previous one.
        if !group.is_empty() && posts < POSTS_PER_PASSAGE / 2 && !out.is_empty() {
            let last = out.pop().unwrap();
            let tail = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
            out.push(Passage { text: format!("{}\n\n{}", last.text, tail), start_line: last.start_line, end_line: group.last().unwrap().2 });
            group.clear();
        }
        emit(&mut group, &mut out);
        return out;
    }
    let mut n = 0;
    for b in &blocks {
        group.push(b);
        n += words(&b.0);
        if n >= PASSAGE_WORDS {
            emit(&mut group, &mut out);
            n = 0;
        }
    }
    if n >= MIN_PASSAGE_WORDS || out.is_empty() {
        emit(&mut group, &mut out);
    } else if let (Some(last), false) = (out.last_mut(), group.is_empty()) {
        // A short tail joins the passage before it.
        let tail = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
        last.text = format!("{}\n\n{}", last.text, tail);
        last.end_line = group.last().unwrap().2;
    }
    out
}

// ── The author's bands ───────────────────────────────────────────────────────

/// At most this many of the author's passages are judged (about $0.01 a
/// profile, cached): enough for stable p15-p85 bands, cheap enough to run
/// at every build.
pub const SAMPLE_PASSAGES: usize = 240;
/// Passages are sampled in runs of this many consecutive ones, so a draft
/// of several passages can be compared with stretches of the author's text
/// of the same length.
pub const RUN: usize = 8;
/// Short-form: replies and posts under this many words stay out of the
/// author's batches, as in the eval (one-liners say little about texture).
const MIN_POST_WORDS: usize = 15;
/// Fewer author stretches than this give no bands.
const MIN_WINDOWS: usize = 3;
/// JEV's price, USD per million input tokens (output tokens are free).
pub const PRICE_PER_M_INPUT: f64 = 0.042;
const SIDECAR_SCHEMA: u32 = 1;

/// One of the author's judged passages: its work, its place in the work,
/// its weight (a blend weighs each component by its share) and its values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorPassage {
    pub source: usize,
    pub index: usize,
    pub weight: f64,
    pub values: BTreeMap<String, f64>,
}

/// The author's texture, kept next to the profile in
/// `profiles/<name>.texture.json` (the profile itself is never rewritten).
/// Stale when the profile's text, the model or the questions change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub schema: u32,
    pub profile: String,
    pub model: String,
    pub questions_version: String,
    pub fingerprint: String,
    pub register: String,
    pub short_form: bool,
    /// Passages the author's text splits into, before sampling.
    pub total_passages: usize,
    /// Sampled passages JEV did not answer: the next run retries them.
    pub failed: usize,
    pub measured_at: u64,
    pub passages: Vec<AuthorPassage>,
}

/// What a run of JEV calls cost.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    /// Passages judged: the author's in `author_measured`, the draft's in `usage`.
    pub passages: usize,
    pub requests: usize,
    pub cached: usize,
    pub failed: usize,
    pub input_tokens: u64,
    pub cost_usd: f64,
}

impl Usage {
    fn between(passages: usize, before: UsageReport, after: UsageReport) -> Usage {
        let input_tokens = after.input_tokens - before.input_tokens;
        Usage {
            passages,
            requests: after.requests - before.requests,
            cached: after.cached - before.cached,
            failed: after.failed - before.failed,
            input_tokens,
            cost_usd: cost(input_tokens),
        }
    }
}

/// USD for this many input tokens, to the micro-dollar.
pub fn cost(input_tokens: u64) -> f64 {
    (input_tokens as f64 * PRICE_PER_M_INPUT).round() / 1e6
}

pub fn sidecar_path(name: &str) -> PathBuf {
    store::profiles_dir().join(format!("{name}.texture.json"))
}

/// A hash of the profile's stored text, register and weights: a rebuild
/// from other text invalidates the bands; re-saving the same text does not.
pub fn fingerprint(p: &Profile) -> String {
    let mut h = Sha256::new();
    h.update(p.register.as_str().as_bytes());
    for c in &p.chunks {
        h.update(c.source.to_le_bytes());
        h.update(c.weight.to_le_bytes());
        h.update([c.meta.as_ref().map(|m| m.kind as u8 + 1).unwrap_or(0)]);
        for para in &c.paragraphs {
            h.update(para.as_bytes());
            h.update([0u8]);
        }
        h.update([1u8]);
    }
    h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect()
}

/// The author's text per work, ready for `passages`: long-form works as
/// paragraphs, short-form works as posts separated by `---` lines (replies
/// and very short posts left out unless that leaves too few). With each
/// work's total passage weight.
fn author_works(p: &Profile) -> Vec<(usize, String, f64)> {
    let short = p.register.is_short_form();
    let judged = |c: &crate::engine::profile::Chunk| {
        let kind = c.meta.as_ref().map(|m| m.kind).unwrap_or_default();
        matches!(kind, ItemKind::Post | ItemKind::Quote)
            && c.paragraphs.iter().map(|x| text::count_words(x)).sum::<usize>() >= MIN_POST_WORDS
    };
    let enough = p.chunks.iter().filter(|c| judged(c)).count() >= POSTS_PER_PASSAGE * MIN_WINDOWS * 2;
    let mut works: BTreeMap<usize, (Vec<String>, f64)> = BTreeMap::new();
    for c in &p.chunks {
        if short && enough && !judged(c) {
            continue;
        }
        let w = works.entry(c.source as usize).or_default();
        w.0.push(c.paragraphs.join("\n\n"));
        w.1 += c.weight as f64;
    }
    let sep = if short { "\n---\n" } else { "\n\n" };
    works.into_iter().map(|(s, (parts, w))| (s, parts.join(sep), w)).collect()
}

/// Which passages to judge, as (work, passage) pairs: all of them when
/// there are few, else `SAMPLE_PASSAGES` in runs of `RUN` consecutive
/// passages spread evenly over the works (longer works give more runs).
fn sample(sizes: &[usize]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    for (w, &n) in sizes.iter().enumerate() {
        for start in (0..n).step_by(RUN) {
            runs.push((w, start, RUN.min(n - start)));
        }
    }
    let total: usize = sizes.iter().sum();
    let chosen: Vec<(usize, usize, usize)> = if total <= SAMPLE_PASSAGES {
        runs
    } else {
        let m = (SAMPLE_PASSAGES / RUN).min(runs.len());
        let mut idx: Vec<usize> = (0..m).map(|j| ((j as f64 + 0.5) * runs.len() as f64 / m as f64) as usize).collect();
        idx.dedup();
        idx.into_iter().map(|i| runs[i]).collect()
    };
    chosen.into_iter().flat_map(|(w, start, len)| (start..start + len).map(move |i| (w, i))).collect()
}

/// Every key's value from one answer set, or `None` if any is missing.
fn values_of(a: &Answers) -> Option<BTreeMap<String, f64>> {
    KEYS.iter().map(|k| a.value(k.key).map(|v| (k.key.to_string(), v))).collect()
}

/// Judge passages in parallel (cached); `None` where JEV did not answer.
fn judge(jev: &Jev, texts: &[&str]) -> Vec<Option<BTreeMap<String, f64>>> {
    let q = jev::questions("texture");
    let states: Vec<serde_json::Value> = texts.iter().map(|t| json!({ "text": t })).collect();
    jev.ask_all(&states, &q).iter().map(|a| a.as_ref().and_then(values_of)).collect()
}

/// Judge a sample of the author's passages and save the sidecar.
pub fn measure_author(jev: &Jev, p: &Profile) -> Result<(Sidecar, Usage), String> {
    let before = jev.usage();
    let short = p.register.is_short_form();
    let works: Vec<(usize, Vec<Passage>, f64)> =
        author_works(p).into_iter().map(|(s, t, w)| (s, passages(&t, short), w)).collect();
    let picks = sample(&works.iter().map(|w| w.1.len()).collect::<Vec<_>>());
    if picks.is_empty() {
        return Err("the profile has no passages to judge".into());
    }
    let texts: Vec<&str> = picks.iter().map(|&(w, i)| works[w].1[i].text.as_str()).collect();
    let answers = judge(jev, &texts);
    let mut per_work = vec![0usize; works.len()];
    for &(w, _) in &picks {
        per_work[w] += 1;
    }
    let blend = p.kind == ProfileKind::Blend;
    let mut judged = Vec::new();
    for (&(w, i), a) in picks.iter().zip(answers) {
        if let Some(values) = a {
            let weight = if blend { works[w].2 / per_work[w] as f64 } else { 1.0 };
            judged.push(AuthorPassage { source: works[w].0, index: i, weight, values });
        }
    }
    let usage = Usage::between(judged.len(), before, jev.usage());
    if judged.is_empty() {
        return Err(format!("JEV did not answer for any of the author's {} sampled passages", picks.len()));
    }
    let sc = Sidecar {
        schema: SIDECAR_SCHEMA,
        profile: p.name.clone(),
        model: jev.model.clone(),
        questions_version: jev::questions_version("texture"),
        fingerprint: fingerprint(p),
        register: p.register.as_str().to_string(),
        short_form: short,
        total_passages: works.iter().map(|w| w.1.len()).sum(),
        failed: picks.len() - judged.len(),
        measured_at: crate::engine::profile::now_unix(),
        passages: judged,
    };
    // Not fatal: the bands still serve this run, and the next one re-saves.
    let _ = save(&sc);
    Ok((sc, usage))
}

fn save(sc: &Sidecar) -> std::io::Result<()> {
    let path = sidecar_path(&sc.profile);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(sc).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, &path)
}

/// The saved measurements, if they still describe this profile, model and
/// question set, and every sampled passage was answered.
pub fn load_fresh(p: &Profile, model: &str) -> Option<Sidecar> {
    let raw = std::fs::read(sidecar_path(&p.name)).ok()?;
    let sc: Sidecar = serde_json::from_slice(&raw).ok()?;
    let fresh = sc.schema == SIDECAR_SCHEMA
        && sc.model == model
        && sc.questions_version == jev::questions_version("texture")
        && sc.failed == 0
        && sc.fingerprint == fingerprint(p);
    fresh.then_some(sc)
}

/// The author's measurements: the saved ones when fresh, else measured now
/// (with what that cost).
pub fn author(jev: &Jev, p: &Profile) -> Result<(Sidecar, Option<Usage>), String> {
    if let Some(sc) = load_fresh(p, &jev.model) {
        return Ok((sc, None));
    }
    measure_author(jev, p).map(|(sc, u)| (sc, Some(u)))
}

/// The author's p15/p50/p85 per key over stretches of `k` consecutive
/// passages of one work (half-overlapping), the unit a draft of `k`
/// passages is compared with; `k = 1` gives passage-level bands.
#[derive(Debug, Clone)]
pub struct Bands {
    /// Passages per stretch (the draft's count, capped at the longest run).
    pub k: usize,
    pub windows: usize,
    pub by_key: BTreeMap<&'static str, [f64; 3]>,
}

impl Bands {
    pub fn get(&self, key: &str) -> [f64; 3] {
        self.by_key.get(key).copied().unwrap_or([0.0; 3])
    }
}

pub fn bands(sc: &Sidecar, k: usize) -> Option<Bands> {
    let mut ps: Vec<&AuthorPassage> = sc.passages.iter().collect();
    ps.sort_by_key(|a| (a.source, a.index));
    let mut runs: Vec<Vec<&AuthorPassage>> = Vec::new();
    for a in ps {
        match runs.last_mut() {
            Some(r) if r.last().is_some_and(|b| b.source == a.source && b.index + 1 == a.index) => r.push(a),
            _ => runs.push(vec![a]),
        }
    }
    let k = k.clamp(1, runs.iter().map(Vec::len).max()?);
    let step = (k / 2).max(1);
    let mut windows: Vec<(&[&AuthorPassage], f64)> = Vec::new();
    for r in &runs {
        if r.len() < k {
            continue;
        }
        let mut starts: Vec<usize> = (0..=r.len() - k).step_by(step).collect();
        if starts.last() != Some(&(r.len() - k)) {
            starts.push(r.len() - k);
        }
        for s in starts {
            let w = &r[s..s + k];
            windows.push((w, w.iter().map(|a| a.weight).sum::<f64>() / k as f64));
        }
    }
    if windows.len() < MIN_WINDOWS {
        return None;
    }
    let by_key = KEYS
        .iter()
        .map(|key| {
            let v: Vec<(f64, f64)> = windows
                .iter()
                .map(|(w, weight)| (w.iter().map(|a| a.values.get(key.key).copied().unwrap_or(0.0)).sum::<f64>() / k as f64, *weight))
                .collect();
            (key.key, quantiles(v))
        })
        .collect();
    Some(Bands { k, windows: windows.len(), by_key })
}

/// p15, p50, p85: linear interpolation between order statistics, as the
/// eval did; weighted (by cumulative midpoints) when weights differ.
fn quantiles(mut v: Vec<(f64, f64)>) -> [f64; 3] {
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let equal = v.windows(2).all(|p| (p[0].1 - p[1].1).abs() < 1e-12);
    [0.15, 0.5, 0.85].map(|q| round3(if equal { pct(&v, q) } else { weighted_pct(&v, q) }))
}

fn pct(v: &[(f64, f64)], q: f64) -> f64 {
    let i = q * (v.len() - 1) as f64;
    let lo = i.floor() as usize;
    let hi = (lo + 1).min(v.len() - 1);
    v[lo].0 + (v[hi].0 - v[lo].0) * (i - lo as f64)
}

fn weighted_pct(v: &[(f64, f64)], q: f64) -> f64 {
    let total: f64 = v.iter().map(|x| x.1).sum::<f64>().max(1e-12);
    let mut cum = 0.0;
    let pos: Vec<f64> = v
        .iter()
        .map(|x| {
            let p = (cum + x.1 / 2.0) / total;
            cum += x.1;
            p
        })
        .collect();
    if q <= pos[0] {
        return v[0].0;
    }
    for i in 1..v.len() {
        if q <= pos[i] {
            let t = (q - pos[i - 1]) / (pos[i] - pos[i - 1]).max(1e-12);
            return v[i - 1].0 + (v[i].0 - v[i - 1].0) * t;
        }
    }
    v[v.len() - 1].0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// ── A draft's texture ────────────────────────────────────────────────────────

/// One key: the draft's mean over its passages next to the author's band
/// for stretches of the same length.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRow {
    pub key: &'static str,
    pub label: &'static str,
    pub side: Side,
    pub gate: bool,
    pub draft: f64,
    pub low: f64,
    pub median: f64,
    pub high: f64,
    /// ok | off (beyond the band on the side that is a fault)
    pub status: &'static str,
}

/// One judged passage of the draft.
#[derive(Debug, Clone, Serialize)]
pub struct PassageRow {
    pub index: usize,
    /// First and last line (1-based).
    pub lines: [usize; 2],
    pub values: BTreeMap<String, f64>,
    /// Keys beyond the author's passage-level band, on their fault side.
    pub flags: Vec<&'static str>,
    #[serde(skip)]
    pub excerpt: String,
}

/// The `texture` block of `check`: how the draft's prose texture compares
/// with the author's own, judged by JEV. Advice: it never changes `pass`,
/// `score` or `--strict`.
#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    /// measured | unavailable
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions_version: Option<String>,
    /// No gate key off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pass: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeyRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub passages: Vec<PassageRow>,
    /// The author's judged passages behind the bands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_passages: Option<usize>,
    /// Passages per author stretch the draft was compared with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub band_passages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_windows: Option<usize>,
    /// Set when the author's passages were measured during this run (the
    /// first check after a build without JEV, or after the profile changed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_measured: Option<Usage>,
    /// Everything this run asked JEV, author included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl Texture {
    pub fn unavailable(reason: impl Into<String>) -> Texture {
        Texture {
            status: "unavailable",
            reason: Some(reason.into()),
            model: None,
            questions_version: None,
            pass: None,
            keys: Vec::new(),
            passages: Vec::new(),
            author_passages: None,
            band_passages: None,
            author_windows: None,
            author_measured: None,
            usage: None,
        }
    }

    pub fn measured(&self) -> bool {
        self.status == "measured"
    }

    /// Keys off, gate keys first.
    pub fn off(&self) -> Vec<&KeyRow> {
        let mut v: Vec<&KeyRow> = self.keys.iter().filter(|k| k.status == "off").collect();
        v.sort_by_key(|k| !k.gate);
        v
    }
}

pub fn key(id: &str) -> Option<&'static TextureKey> {
    KEYS.iter().find(|k| k.key == id)
}

fn is_off(side: Side, v: f64, band: [f64; 3]) -> bool {
    match side {
        Side::Low => v < band[0],
        Side::High => v > band[2],
    }
}

fn excerpt(t: &str, chars: usize) -> String {
    let flat = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= chars {
        return flat;
    }
    let cut: String = flat.chars().take(chars).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a.to_string()).unwrap_or(cut);
    format!("{cut}…")
}

/// Judge a draft's texture against the author's, when JEV is available.
pub fn assess(p: &Profile, draft: &str) -> Texture {
    match Jev::load() {
        Ok(jev) => assess_with(&jev, p, draft),
        Err(why) => Texture::unavailable(why),
    }
}

pub fn assess_with(jev: &Jev, p: &Profile, draft: &str) -> Texture {
    let ps = passages(draft, p.register.is_short_form());
    if ps.is_empty() {
        return Texture::unavailable("the draft has no prose passages to judge");
    }
    // The draft first: when JEV does not answer, stop before the author.
    let answers = judge(jev, &ps.iter().map(|x| x.text.as_str()).collect::<Vec<_>>());
    if answers.iter().all(Option::is_none) {
        return Texture::unavailable(format!(
            "JEV did not answer for any of the draft's {} passage(s) (network, timeout or a bad response); the rest of the check is unchanged",
            ps.len()
        ));
    }
    let (sc, author_measured) = match author(jev, p) {
        Ok(x) => x,
        Err(why) => return Texture::unavailable(why),
    };
    let judged: Vec<(&Passage, BTreeMap<String, f64>)> =
        ps.iter().zip(answers).filter_map(|(x, a)| a.map(|a| (x, a))).collect();
    let (Some(b), Some(pb)) = (bands(&sc, judged.len()), bands(&sc, 1)) else {
        return Texture::unavailable(format!(
            "the author's {} judged passage(s) are too few for texture bands",
            sc.passages.len()
        ));
    };
    let keys: Vec<KeyRow> = KEYS
        .iter()
        .map(|k| {
            let draft = round3(judged.iter().map(|(_, v)| v.get(k.key).copied().unwrap_or(0.0)).sum::<f64>() / judged.len() as f64);
            let band = b.get(k.key);
            KeyRow {
                key: k.key,
                label: k.label,
                side: k.side,
                gate: k.gate,
                draft,
                low: band[0],
                median: band[1],
                high: band[2],
                status: if is_off(k.side, draft, band) { "off" } else { "ok" },
            }
        })
        .collect();
    let passages = judged
        .iter()
        .enumerate()
        .map(|(i, (x, v))| PassageRow {
            index: i + 1,
            lines: [x.start_line, x.end_line],
            values: v.iter().map(|(k, x)| (k.clone(), round3(*x))).collect(),
            flags: KEYS.iter().filter(|k| is_off(k.side, v[k.key], pb.get(k.key))).map(|k| k.key).collect(),
            excerpt: excerpt(&x.text, 100),
        })
        .collect();
    let u = jev.usage();
    Texture {
        status: "measured",
        reason: None,
        model: Some(jev.model.clone()),
        questions_version: Some(sc.questions_version.clone()),
        pass: Some(!keys.iter().any(|k| k.gate && k.status == "off")),
        keys,
        passages,
        author_passages: Some(sc.passages.len()),
        band_passages: Some(b.k),
        author_windows: Some(b.windows),
        author_measured,
        usage: Some(Usage {
            passages: judged.len(),
            requests: u.requests,
            cached: u.cached,
            failed: u.failed,
            input_tokens: u.input_tokens,
            cost_usd: cost(u.input_tokens),
        }),
    }
}

/// The author's passage-level bands for a brief, measuring them first
/// when missing or stale. `Err` says why there are none.
pub fn author_bands(p: &Profile) -> Result<(Bands, usize, Option<Usage>), String> {
    let jev = Jev::load()?;
    let (sc, measured) = author(&jev, p)?;
    let b = bands(&sc, 1)
        .ok_or_else(|| format!("the author's {} judged passage(s) are too few for texture bands", sc.passages.len()))?;
    Ok((b, sc.passages.len(), measured))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(n: usize, w: &str) -> String {
        vec![w; n].join(" ") + "."
    }

    #[test]
    fn long_form_groups_paragraphs_to_about_150_words_with_line_ranges() {
        let text = [para(80, "a"), para(80, "b"), para(80, "c"), para(30, "d")].join("\n\n");
        let p = passages(&text, false);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!((p[0].start_line, p[0].end_line), (1, 3));
        assert_eq!((p[1].start_line, p[1].end_line), (5, 7), "the 30-word tail joins the last passage");
        assert!(p[1].text.contains("d d"));
    }

    #[test]
    fn short_form_judges_posts_four_at_a_time_and_scene_breaks_split_nothing_else() {
        let posts: Vec<String> = (0..9).map(|i| format!("post {i} says something plain here")).collect();
        let p = passages(&posts.join("\n---\n"), true);
        assert_eq!(p.len(), 2, "9 posts: 4 + 5 (a lone trailing post joins the batch before)");
        assert!(p[0].text.contains("post 0") && p[0].text.contains("post 3") && !p[0].text.contains("post 4"));
        assert!(p[1].text.contains("post 8"));
    }

    #[test]
    fn normalize_evens_out_marks_and_drops_links() {
        let n = normalize("\u{201C}It\u{2019}s fine\u{2026}\u{201D} see https://t.co/x1 now\nand then");
        assert_eq!(n, "\"It's fine...\" see now and then");
    }
}
