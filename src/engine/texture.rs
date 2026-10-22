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
use std::time::{Duration, Instant};

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
    /// Either way is off; only too much fails the gate.
    Both,
}

pub struct TextureKey {
    /// Question id in `data/jev_texture.json`, or a counted measure.
    pub key: &'static str,
    pub label: &'static str,
    pub side: Side,
    /// Gate keys decide `texture.pass`; the rest are advice.
    pub gate: bool,
    /// Judged by JEV per passage; otherwise counted in code. What code can
    /// count never goes to JEV.
    pub jev: bool,
    /// What to do when the draft is off on this side.
    pub advice: &'static str,
}

/// The measured keys, gate keys first. Gate: the ones that separated
/// genuine from imitation and resisted gaming in rounds 3-4. JEV judges
/// only what code cannot; filler hedges are a word list and one-sentence
/// paragraphs a count, both held to the author's own rates.
pub const KEYS: [TextureKey; 9] = [
    TextureKey { key: "rough", label: "rough edges", side: Side::Low, gate: true, jev: true,
        advice: "loosen it in the author's own ways (the kinds below), not with filler" },
    TextureKey { key: "polish", label: "polish (0 plain to 3 highly crafted)", side: Side::High, gate: true, jev: true,
        advice: "write plainer: say what happens and what is thought, directly" },
    TextureKey { key: "button", label: "crafted closing lines", side: Side::High, gate: true, jev: true,
        advice: "end passages on ordinary sentences, not kickers, reveals or callbacks" },
    TextureKey { key: "figurative", label: "fresh figures of speech", side: Side::High, gate: true, jev: true,
        advice: "cut inventive metaphors and similes" },
    TextureKey { key: "hedge", label: "filler hedges per 1,000 words (counted)", side: Side::Both, gate: true, jev: false,
        advice: "use filler hedges (I guess, kind of) about as often as the author does: piling them on reads as imitation" },
    TextureKey { key: "overexplain", label: "over-explaining", side: Side::Low, gate: false, jev: true,
        advice: "spell out reasons, logistics and feelings a little more fully, as the author does" },
    TextureKey { key: "repeat", label: "repeating a noun instead of a pronoun", side: Side::Low, gate: false, jev: true,
        advice: "repeat the key noun now and then where the author would" },
    TextureKey { key: "splice", label: "loosely joined, run-on sentences", side: Side::Low, gate: false, jev: true,
        advice: "let a few sentences run on, joined loosely with commas or \"and\"" },
    TextureKey { key: "oneliner", label: "one-sentence paragraphs (counted)", side: Side::High, gate: false, jev: false,
        advice: "fold some standalone beats back into their paragraphs" },
];

/// The keys JEV judges.
pub fn jev_keys() -> impl Iterator<Item = &'static TextureKey> {
    KEYS.iter().filter(|k| k.jev)
}

/// The texture questions JEV is asked: the measured set minus what code
/// counts.
pub fn jev_questions() -> serde_json::Value {
    let all = jev::questions("texture");
    let map: serde_json::Map<String, serde_json::Value> =
        jev_keys().filter_map(|k| all.get(k.key).map(|q| (k.key.to_string(), q.clone()))).collect();
    serde_json::Value::Object(map)
}

/// Version of what is asked (passage and sentence questions): stored with
/// the author's measurements, so a change of wording re-measures them.
pub fn questions_version() -> String {
    let h = Sha256::digest(format!("{}\n{}\n{}", jev::questions_version("texture"), jev_questions(), sentence_questions()).as_bytes());
    format!("texture-{}", h[..6].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Sentence-level drill-down: after a passage is flagged as polished, as
/// ending on a crafted line or as figurative, each of its candidate
/// sentences is asked these two narrow Nouls, so findings cite exact lines.
const SENTENCE: &str = include_str!("data/jev_sentence.json");

pub fn sentence_questions() -> serde_json::Value {
    serde_json::from_str(SENTENCE).expect("embedded sentence questions are valid JSON")
}

/// At most this many sentences are drilled into per check.
pub const MAX_SENTENCE_REQUESTS: usize = 40;
/// A sentence is cited when JEV gives at least this probability.
const SENTENCE_YES: f64 = 0.5;

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
    /// Never sent: JEV stopped answering or the time budget ran out.
    pub skipped: usize,
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
            skipped: after.skipped - before.skipped,
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
    jev_keys().map(|k| a.value(k.key).map(|v| (k.key.to_string(), v))).collect()
}

/// Judge passages in parallel (cached); `None` where JEV did not answer.
fn judge(jev: &Jev, texts: &[&str]) -> Vec<Option<BTreeMap<String, f64>>> {
    let q = jev_questions();
    let states: Vec<serde_json::Value> = texts.iter().map(|t| json!({ "text": t })).collect();
    jev.ask_all(&states, &q).iter().map(|a| a.as_ref().and_then(values_of)).collect()
}

/// The author's passages per work, and the (work, passage) pairs to judge.
fn author_plan(p: &Profile) -> (Vec<(usize, Vec<Passage>, f64)>, Vec<(usize, usize)>) {
    let short = p.register.is_short_form();
    let works: Vec<(usize, Vec<Passage>, f64)> =
        author_works(p).into_iter().map(|(s, t, w)| (s, passages(&t, short), w)).collect();
    let picks = sample(&works.iter().map(|w| w.1.len()).collect::<Vec<_>>());
    (works, picks)
}

/// How many of the author's sampled passages JEV would still be asked
/// about (the rest are in its cache).
pub fn author_pending(jev: &Jev, p: &Profile) -> usize {
    let (works, picks) = author_plan(p);
    let q = jev_questions();
    picks.iter().filter(|&&(w, i)| !jev.is_cached(&json!({ "text": works[w].1[i].text }), &q)).count()
}

/// Judge a sample of the author's passages and save the sidecar. When JEV
/// stops answering or the time budget runs out partway, nothing is saved
/// and the bands are not used: a partial sample would lean on the first
/// works. What was answered is cached, so the next run picks up where this
/// one stopped.
pub fn measure_author(jev: &Jev, p: &Profile) -> Result<(Sidecar, Usage), String> {
    let before = jev.usage();
    let short = p.register.is_short_form();
    let (works, picks) = author_plan(p);
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
    if usage.skipped > 0 {
        return Err(format!(
            "{}: judged {} of the author's {} sampled passages before it stopped. The answers are cached, so the next run continues; `stylometry profile rebuild {}` allows longer",
            jev.halted().unwrap_or_else(|| "JEV stopped".into()),
            usage.passages,
            texts.len(),
            p.name
        ));
    }
    if judged.is_empty() {
        return Err(format!("JEV did not answer for any of the author's {} sampled passages", picks.len()));
    }
    let sc = Sidecar {
        schema: SIDECAR_SCHEMA,
        profile: p.name.clone(),
        model: jev.model.clone(),
        questions_version: questions_version(),
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
        && sc.questions_version == questions_version()
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
    // A draft longer than the author's runs is compared with the longest
    // stretches that still give enough windows.
    let mut k = k.clamp(1, runs.iter().map(Vec::len).max()?);
    let windows = loop {
        let w = windows_of(&runs, k);
        if w.len() >= MIN_WINDOWS || k == 1 {
            break w;
        }
        k -= 1;
    };
    if windows.len() < MIN_WINDOWS {
        return None;
    }
    let by_key = jev_keys()
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

/// Every stretch of `k` consecutive passages within a run, half-overlapping
/// (the last one flush with the run's end), with its mean weight.
fn windows_of<'a>(runs: &'a [Vec<&'a AuthorPassage>], k: usize) -> Vec<(&'a [&'a AuthorPassage], f64)> {
    let step = (k / 2).max(1);
    let mut out = Vec::new();
    for r in runs.iter().filter(|r| r.len() >= k) {
        let mut starts: Vec<usize> = (0..=r.len() - k).step_by(step).collect();
        if starts.last() != Some(&(r.len() - k)) {
            starts.push(r.len() - k);
        }
        for s in starts {
            let w = &r[s..s + k];
            out.push((w, w.iter().map(|a| a.weight).sum::<f64>() / k as f64));
        }
    }
    out
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

// ── Counted keys ─────────────────────────────────────────────────────────────

const HEDGES: &str = include_str!("data/hedges.txt");
/// "kind of" and "sort of" hedge after these ("it was kind of fine", "I
/// sort of knew"), at a clause's edge ("Kind of, yes", "fine, sort of,").
/// After anything else they are nouns: "a certain kind of person", "what
/// kind of car", "an exacting sort of purity".
const HEDGE_BEFORE: [&str; 44] = [
    "i", "you", "he", "she", "it", "we", "they", "it's", "that's", "i'm", "you're", "we're", "they're", "i've", "i'd",
    "was", "is", "are", "were", "be", "been", "am", "feel", "feels", "felt", "seem", "seems", "seemed", "look", "looks",
    "looked", "sound", "sounds", "sounded", "get", "got", "just", "really", "also", "still", "and", "but", "so", "actually",
];
/// Clause punctuation, as one token: hedges never match across it.
const STOP: &str = ",";
/// One hedge is never "piling them on": too many needs at least this many.
const MIN_HEDGES_HIGH: usize = 2;

fn hedge_list() -> Vec<Vec<String>> {
    HEDGES
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

/// Lowercased words (apostrophes straightened, and dropped at the ends of
/// words, where they are quote marks), with clause punctuation as `STOP`.
fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let w = cur.trim_matches('\'');
        if !w.is_empty() {
            out.push(w.to_string());
        }
        cur.clear();
    };
    for c in text.to_lowercase().replace('\u{2019}', "'").chars() {
        if c.is_alphanumeric() || c == '\'' {
            cur.push(c);
            continue;
        }
        flush(&mut cur, &mut out);
        if ",.;:!?\u{2026}\u{2014}\u{2013}()\"\u{201C}\u{201D}\n".contains(c) && out.last().is_some_and(|t| t != STOP) {
            out.push(STOP.into());
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Filler hedges in a token run: (word index, hedge) per hit, and the
/// number of words (`STOP` tokens are not words).
fn hedges_at(ts: &[String], list: &[Vec<String>]) -> (Vec<(usize, String)>, usize) {
    let mut out = Vec::new();
    let mut word = 0;
    for i in 0..ts.len() {
        if ts[i] == STOP {
            continue;
        }
        for h in list.iter().filter(|h| ts[i..].starts_with(h)) {
            let edge = |t: Option<&String>| t.is_none_or(|t| t == STOP);
            let hedge = !(h[0] == "kind" || h[0] == "sort")
                || edge(i.checked_sub(1).and_then(|j| ts.get(j)))
                || edge(ts.get(i + h.len()))
                || HEDGE_BEFORE.contains(&ts[i - 1].as_str());
            if hedge {
                out.push((word, h.join(" ")));
            }
            break;
        }
        word += 1;
    }
    (out, word)
}

/// Filler hedges in a draft: (1-based line, the hedge, the line) per hit,
/// and the draft's words.
pub fn draft_hedges(draft: &str) -> (Vec<(usize, String, String)>, usize) {
    let list = hedge_list();
    let (mut hits, mut n) = (Vec::new(), 0);
    for (i, line) in draft.lines().enumerate() {
        let (found, words) = hedges_at(&tokens(line), &list);
        n += words;
        hits.extend(found.into_iter().map(|(_, h)| (i + 1, h, line.trim().to_string())));
    }
    (hits, n)
}

/// The author's filler hedges per 1,000 words over half-overlapping
/// windows of `words` words of one work: p15, p50, p85.
pub fn hedge_band(p: &Profile, words_per_window: usize) -> Option<[f64; 3]> {
    let list = hedge_list();
    let w = words_per_window.max(100);
    let mut rates = Vec::new();
    for (_, text, _) in author_works(p) {
        let (found, n) = hedges_at(&tokens(&text), &list);
        if n < w {
            continue;
        }
        let mut prefix = vec![0usize; n + 1];
        for (i, _) in found {
            prefix[i + 1] += 1;
        }
        for i in 0..n {
            prefix[i + 1] += prefix[i];
        }
        let mut starts: Vec<usize> = (0..=n - w).step_by(w / 2).collect();
        if starts.last() != Some(&(n - w)) {
            starts.push(n - w);
        }
        rates.extend(starts.into_iter().map(|s| ((prefix[s + w] - prefix[s]) as f64 * 1000.0 / w as f64, 1.0)));
    }
    (rates.len() >= MIN_WINDOWS).then(|| quantiles(rates))
}

// ── Sentence drill-down ──────────────────────────────────────────────────────

/// One sentence of a flagged passage, asked the sentence questions.
#[derive(Debug, Clone, Serialize)]
pub struct SentenceRow {
    pub line: usize,
    pub text: String,
    /// P(a fresh figure of speech).
    pub figure: f64,
    /// P(a crafted, quotable line: kicker, reveal, aphorism).
    pub crafted: f64,
    /// Ends its paragraph.
    pub closing: bool,
}

/// The sentences of a passage with their lines, and whether each ends its
/// paragraph.
fn sentences_of(lines: &[&str], p: &PassageRow, short: bool) -> Vec<(usize, String, bool)> {
    let mut out: Vec<(usize, String, bool)> = Vec::new();
    for l in p.lines[0]..=p.lines[1] {
        let Some(line) = lines.get(l - 1) else { continue };
        // Blank lines end paragraphs; `---` lines end posts and scenes.
        if !line.chars().any(char::is_alphanumeric) {
            if let Some(last) = out.last_mut() {
                last.2 = true;
            }
            continue;
        }
        out.extend(text::sentences_for(line.trim(), short).into_iter().map(|s| (l, s, false)));
    }
    if let Some(last) = out.last_mut() {
        last.2 = true;
    }
    out
}

/// The sentences that back a key's finding: fresh figures for
/// `figurative`, crafted lines ending a paragraph for `button`, either for
/// `polish`.
pub fn cited<'a>(p: &'a PassageRow, key: &str) -> Vec<&'a SentenceRow> {
    p.sentences
        .iter()
        .filter(|s| match key {
            "figurative" => s.figure >= SENTENCE_YES,
            "button" => s.closing && s.crafted >= SENTENCE_YES,
            "polish" => s.figure >= SENTENCE_YES || s.crafted >= SENTENCE_YES,
            _ => false,
        })
        .collect()
}

/// Ask the sentence questions of flagged passages, worst passages first,
/// at most `MAX_SENTENCE_REQUESTS` sentences: every sentence when the
/// passage is too polished or figurative, its paragraph endings when it
/// only ends on crafted lines.
fn drill(jev: &Jev, draft: &str, short: bool, off: &[&'static str], passages: &mut [PassageRow]) {
    let drillable = ["polish", "button", "figurative"];
    let keys: Vec<&str> = off.iter().copied().filter(|k| drillable.contains(k)).collect();
    if keys.is_empty() {
        return;
    }
    let lines: Vec<&str> = draft.lines().collect();
    let mut order: Vec<usize> = (0..passages.len()).filter(|&i| passages[i].flags.iter().any(|f| keys.contains(f))).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(passages[i].flags.iter().filter(|f| keys.contains(f)).count()));
    let mut asks: Vec<(usize, usize, String, bool)> = Vec::new(); // (passage, line, sentence, closing)
    for i in order {
        let only_endings = passages[i].flags.iter().filter(|f| keys.contains(f)).all(|f| *f == "button");
        let mut prev = String::new();
        for (line, s, closing) in sentences_of(&lines, &passages[i], short) {
            if (!only_endings || closing) && asks.len() < MAX_SENTENCE_REQUESTS {
                asks.push((i, line, format!("{prev}\u{0}{s}"), closing));
            }
            prev = s;
        }
    }
    let states: Vec<serde_json::Value> = asks
        .iter()
        .map(|(_, _, both, _)| {
            let (before, s) = both.split_once('\u{0}').unwrap_or(("", both));
            json!({ "sentence": normalize(s), "before": normalize(before) })
        })
        .collect();
    let answers = jev.ask_all(&states, &sentence_questions());
    for ((i, line, both, closing), a) in asks.into_iter().zip(answers) {
        let (Some(figure), Some(crafted)) = (a.as_ref().and_then(|a| a.noul("figure")), a.as_ref().and_then(|a| a.noul("crafted"))) else {
            continue;
        };
        let s = both.split_once('\u{0}').map(|x| x.1).unwrap_or(&both);
        passages[i].sentences.push(SentenceRow { line, text: excerpt(s, 160), figure: round3(figure), crafted: round3(crafted), closing });
    }
}

// ── A draft's texture ────────────────────────────────────────────────────────

/// One key: the draft's value (JEV keys: the mean over its passages;
/// counted keys: the rate) next to the author's band at the same length.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRow {
    pub key: &'static str,
    pub label: &'static str,
    pub side: Side,
    pub gate: bool,
    /// jev (judged per passage) | count (counted in code)
    pub source: &'static str,
    pub draft: f64,
    pub low: f64,
    pub median: f64,
    pub high: f64,
    /// ok | off (beyond the band on a side that is a fault)
    pub status: &'static str,
    /// When off: low (too little) or high (too much).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_side: Option<Side>,
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
    /// Flagged passages only: their sentences, asked about figures and
    /// crafted lines so findings cite exact lines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sentences: Vec<SentenceRow>,
    #[serde(skip)]
    pub excerpt: String,
}

/// The `texture` block of `check`: how the draft's prose texture compares
/// with the author's own. JEV judges what code cannot (rough edges, polish,
/// crafted lines, figures, kinds of roughness); hedges and one-sentence
/// paragraphs are counted. Advice: it never changes `pass`, `score` or
/// `--strict`.
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
    /// No gate key off (hedges: only too many fails).
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
    /// Everything this run asked JEV, author and sentences included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Filler hedges in the draft: (line, hedge, the line's text).
    #[serde(skip)]
    pub hedges: Vec<(usize, String, String)>,
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
            hedges: Vec::new(),
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

    /// Add a counted row measured elsewhere (one-sentence paragraphs come
    /// from `check`'s own metric) and settle `pass` again.
    pub fn add_counted(&mut self, key: &str, draft: f64, band: [f64; 3]) {
        let Some(def) = self::key(key) else { return };
        if !self.measured() {
            return;
        }
        self.keys.retain(|k| k.key != key);
        self.keys.push(row(def, round3(draft), band.map(round3), true));
        self.keys.sort_by_key(|k| KEYS.iter().position(|d| d.key == k.key));
        self.pass = Some(pass_of(&self.keys));
    }
}

pub fn key(id: &str) -> Option<&'static TextureKey> {
    KEYS.iter().find(|k| k.key == id)
}

fn is_off(side: Side, v: f64, band: [f64; 3]) -> bool {
    off_side(side, v, band, true).is_some()
}

/// Which side the value is off on, if a fault. `may_be_high` lets a
/// counted key need several hits before too many counts.
fn off_side(side: Side, v: f64, band: [f64; 3], may_be_high: bool) -> Option<Side> {
    let low = matches!(side, Side::Low | Side::Both) && v < band[0];
    let high = matches!(side, Side::High | Side::Both) && v > band[2] && may_be_high;
    if low {
        Some(Side::Low)
    } else if high {
        Some(Side::High)
    } else {
        None
    }
}

fn row(k: &'static TextureKey, draft: f64, band: [f64; 3], may_be_high: bool) -> KeyRow {
    let off = off_side(k.side, draft, band, may_be_high);
    KeyRow {
        key: k.key,
        label: k.label,
        side: k.side,
        gate: k.gate,
        source: if k.jev { "jev" } else { "count" },
        draft,
        low: band[0],
        median: band[1],
        high: band[2],
        status: if off.is_some() { "off" } else { "ok" },
        off_side: off,
    }
}

/// No gate key off; a two-sided gate key fails only on too much.
fn pass_of(keys: &[KeyRow]) -> bool {
    !keys.iter().any(|k| k.gate && k.status == "off" && (k.side != Side::Both || k.off_side == Some(Side::High)))
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

/// When the draft's own requests show JEV is slow, the reason to skip
/// measuring the author's passages now: it would overrun the budget.
fn too_slow_for_author(jev: &Jev, p: &Profile, sent: usize, took: Duration) -> Option<String> {
    if sent == 0 {
        return None;
    }
    let left = jev.time_left()?.as_secs_f64();
    let per_round = took.as_secs_f64() / sent.div_ceil(jev.workers()) as f64;
    let pending = author_pending(jev, p);
    let need = pending.div_ceil(jev.workers()) as f64 * per_round;
    (need > left).then(|| {
        format!(
            "JEV is slow (about {per_round:.1} s a request): measuring {pending} of the author's passages first would take about {need:.0} s, more than the {left:.0} s left of the budget (jev.budget_ms). Run `stylometry profile rebuild {}` once to measure them with more time, or check again later",
            p.name
        )
    })
}

pub fn assess_with(jev: &Jev, p: &Profile, draft: &str) -> Texture {
    jev.start_budget(jev.budget);
    let started = Instant::now();
    let short = p.register.is_short_form();
    let ps = passages(draft, short);
    if ps.is_empty() {
        return Texture::unavailable("the draft has no prose passages to judge");
    }
    // The draft first: when JEV does not answer, stop before the author.
    let before = jev.usage();
    let answers = judge(jev, &ps.iter().map(|x| x.text.as_str()).collect::<Vec<_>>());
    if answers.iter().all(Option::is_none) {
        return Texture::unavailable(format!(
            "JEV did not answer for any of the draft's {} passage(s) ({}); the rest of the check is unchanged",
            ps.len(),
            jev.halted().unwrap_or_else(|| "network, timeout or a bad response".into())
        ));
    }
    let sent = jev.usage().requests - before.requests;
    let (sc, author_measured) = match load_fresh(p, &jev.model) {
        Some(sc) => (sc, None),
        None => {
            if let Some(why) = too_slow_for_author(jev, p, sent, started.elapsed()) {
                return Texture::unavailable(why);
            }
            match measure_author(jev, p) {
                Ok((sc, u)) => (sc, Some(u)),
                Err(why) => return Texture::unavailable(why),
            }
        }
    };
    let judged: Vec<(&Passage, BTreeMap<String, f64>)> =
        ps.iter().zip(answers).filter_map(|(x, a)| a.map(|a| (x, a))).collect();
    let (Some(b), Some(pb)) = (bands(&sc, judged.len()), bands(&sc, 1)) else {
        return Texture::unavailable(format!(
            "the author's {} judged passage(s) are too few for texture bands",
            sc.passages.len()
        ));
    };
    let mut keys: Vec<KeyRow> = jev_keys()
        .map(|k| {
            let draft = round3(judged.iter().map(|(_, v)| v.get(k.key).copied().unwrap_or(0.0)).sum::<f64>() / judged.len() as f64);
            row(k, draft, b.get(k.key), true)
        })
        .collect();
    // Hedges are a word list, held to the author's rate at the draft's length.
    let (hedges, words) = draft_hedges(draft);
    if let (Some(def), Some(band)) = (key("hedge"), hedge_band(p, words)) {
        let rate = round3(hedges.len() as f64 * 1000.0 / words.max(1) as f64);
        keys.push(row(def, rate, band, hedges.len() >= MIN_HEDGES_HIGH));
    }
    keys.sort_by_key(|k| KEYS.iter().position(|d| d.key == k.key));
    let mut passages: Vec<PassageRow> = judged
        .iter()
        .enumerate()
        .map(|(i, (x, v))| PassageRow {
            index: i + 1,
            lines: [x.start_line, x.end_line],
            values: v.iter().map(|(k, x)| (k.clone(), round3(*x))).collect(),
            flags: jev_keys().filter(|k| is_off(k.side, v[k.key], pb.get(k.key))).map(|k| k.key).collect(),
            sentences: Vec::new(),
            excerpt: excerpt(&x.text, 100),
        })
        .collect();
    let off: Vec<&'static str> = keys.iter().filter(|k| k.status == "off").map(|k| k.key).collect();
    drill(jev, draft, short, &off, &mut passages);
    let u = jev.usage();
    Texture {
        status: "measured",
        reason: None,
        model: Some(jev.model.clone()),
        questions_version: Some(sc.questions_version.clone()),
        pass: Some(pass_of(&keys)),
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
            skipped: u.skipped,
            input_tokens: u.input_tokens,
            cost_usd: cost(u.input_tokens),
        }),
        hedges,
    }
}

/// The author's passage-level bands for a brief, measuring them first
/// when missing or stale. `Err` says why there are none.
pub fn author_bands(p: &Profile) -> Result<(Bands, usize, Option<Usage>), String> {
    let jev = Jev::load()?;
    jev.start_budget(jev.budget);
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

    fn sidecar(runs: &[(usize, usize)], value: impl Fn(usize, usize) -> f64) -> Sidecar {
        let passages = runs
            .iter()
            .flat_map(|&(source, n)| (0..n).map(move |index| (source, index)))
            .map(|(source, index)| AuthorPassage {
                source,
                index,
                weight: 1.0,
                values: KEYS.iter().map(|k| (k.key.to_string(), value(source, index))).collect(),
            })
            .collect();
        Sidecar {
            schema: SIDECAR_SCHEMA,
            profile: "t".into(),
            model: "m".into(),
            questions_version: "q".into(),
            fingerprint: "f".into(),
            register: "fiction".into(),
            short_form: false,
            total_passages: 0,
            failed: 0,
            measured_at: 0,
            passages,
        }
    }

    #[test]
    fn sampling_takes_everything_when_small_and_even_runs_when_large() {
        assert_eq!(sample(&[5, 3]).len(), 8);
        let big = sample(&[400, 100, 300]);
        assert!(big.len() <= SAMPLE_PASSAGES && big.len() > SAMPLE_PASSAGES - RUN, "{}", big.len());
        // Runs of consecutive passages, spread over every work in proportion.
        let per = |w: usize| big.iter().filter(|p| p.0 == w).count();
        assert!(per(0) > per(2) && per(2) > per(1) && per(1) > 0, "{} {} {}", per(0), per(1), per(2));
        assert!(big.windows(2).filter(|p| p[0].0 == p[1].0 && p[0].1 + 1 == p[1].1).count() >= big.len() * 3 / 4);
    }

    #[test]
    fn bands_average_k_consecutive_passages_of_one_work() {
        // Work 0 alternates 0/1, work 1 is all 0.5: 2-passage means are all 0.5.
        let sc = sidecar(&[(0, 8), (1, 8)], |s, i| if s == 0 { (i % 2) as f64 } else { 0.5 });
        let one = bands(&sc, 1).unwrap();
        assert_eq!(one.k, 1);
        assert_eq!(one.windows, 16);
        assert_eq!(one.get("rough"), [0.0, 0.5, 1.0]);
        let two = bands(&sc, 2).unwrap();
        assert_eq!(two.get("rough"), [0.5, 0.5, 0.5]);
        assert_eq!(two.windows, 14, "half-overlap: starts 0..6 in each run of 8");
        // Longer drafts: the longest stretches that still give 3+ windows.
        let long = bands(&sc, 50).unwrap();
        assert_eq!((long.k, long.windows), (7, 4));
        let tiny = sidecar(&[(0, 2)], |_, _| 0.3);
        assert!(bands(&tiny, 1).is_none(), "fewer than {MIN_WINDOWS} passages give no bands");
        // A gap in the indices breaks a run.
        let mut gap = sidecar(&[(0, 6)], |_, _| 0.3);
        gap.passages[3].index = 10;
        assert_eq!(bands(&gap, 6).unwrap().k, 2);
    }

    #[test]
    fn quantiles_match_the_eval_and_weigh_blends() {
        let v: Vec<(f64, f64)> = (0..11).map(|i| (i as f64 / 10.0, 1.0)).collect();
        assert_eq!(quantiles(v), [0.15, 0.5, 0.85]);
        // A component with three times the weight pulls the median its way.
        let w = vec![(0.0, 3.0), (0.0, 3.0), (1.0, 1.0), (1.0, 1.0)];
        assert_eq!(quantiles(w)[1], 0.0);
    }

    #[test]
    fn hedges_are_counted_by_line_and_kind_of_as_a_noun_is_not_one() {
        let (hits, words) = draft_hedges("I guess it was kind of fine.\nWhat kind of car?\nSort of, I dunno\u{2019}.\nA certain kind of person, an exacting sort of purity.");
        let found: Vec<(usize, &str)> = hits.iter().map(|h| (h.0, h.1.as_str())).collect();
        assert_eq!(found, [(1, "i guess"), (1, "kind of"), (3, "sort of"), (3, "i dunno")]);
        assert_eq!(words, 25);
        assert!(!jev_questions().as_object().unwrap().contains_key("hedge"), "counted keys never go to JEV");
        assert!(!jev_questions().as_object().unwrap().contains_key("oneliner"));
        assert_eq!(jev_keys().count(), 7);
    }

    #[test]
    fn normalize_evens_out_marks_and_drops_links() {
        let n = normalize("\u{201C}It\u{2019}s fine\u{2026}\u{201D} see https://t.co/x1 now\nand then");
        assert_eq!(n, "\"It's fine...\" see now and then");
    }
}
