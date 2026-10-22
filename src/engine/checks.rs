//! Draft checking: one voice score, a short revision plan, and grouped
//! findings with line lists, against a target voice.
//!
//! Long-form drafts (fiction, non-fiction, blog) are measured as a window
//! of the author's text at the draft's own length; the score is the
//! draft's percentile among the author's own windows, each of which is
//! measured against bands rebuilt without its own source, so an unseen
//! piece by the author lands near 50 rather than being penalised for not
//! being in-sample. Short-form drafts (posts, emails, messages) are scored
//! item by item against the author's items of the same kind. Hard failures
//! (copying the author's text, banned words, habits the author never shows,
//! X's length limit) always fail.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use serde::Serialize;

use crate::engine::lexicon;
use crate::engine::metrics::{self, Band, ItemMeasure, ItemModel, Mode, Status, Unit, WindowRow};
use crate::engine::profile::{ItemKind, Profile, Register};
use crate::engine::text;
use crate::engine::voice::{self, Concept, Corpus};

/// Shared n-gram length that counts as copying long-form text.
pub const COPY_NGRAM: usize = 8;
/// Posts are short: a 6-word run is already a lifted line.
pub const COPY_NGRAM_SHORT: usize = 6;
/// Below this a short-form draft gets item measures only (no window rates).
pub const SHORT_DRAFT_WORDS: usize = 150;
/// Score below which a draft fails: less typical than 90% of the author's own.
pub const PASS_SCORE: f64 = 10.0;
/// Long drafts are also measured in segments of about this many words.
pub const SEGMENT_WORDS: usize = 1000;
/// X's limit in weighted characters.
pub const X_LIMIT: usize = 280;
/// Word-choice evidence. An unseen book by the same author holds a dozen
/// words they rarely use (topic shifts: "kids", "shop"), so a word is
/// off-voice only when the author's synonym group is well attested (15+
/// uses) and the word takes under 3% of it, or when general English would
/// put it 12+ times in a corpus this size and the author barely has it.
const MIN_CONCEPT_USES: f64 = 15.0;
const RARE_SHARE: f64 = 0.03;
const MIN_AVOIDED_EXPECTED: f64 = 12.0;
const MAX_EXCERPTS: usize = 3;
const MAX_PLAN: usize = 8;

/// One finding: every occurrence of one problem (a word, a construction, a
/// metric), with the lines it occurs on. Never one finding per sentence.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// copy_overlap | avoid_list | never | length | typography |
    /// metric_out_of_range | habit | llm_tell | off_voice_word |
    /// synonym_variation | structure | tense_drift | batch
    pub kind: &'static str,
    /// What the finding is about: a metric key, a word, a habit.
    pub key: String,
    /// Occurrences in the draft.
    pub count: usize,
    /// 1-based lines, ascending; empty for whole-draft findings.
    pub lines: Vec<usize>,
    /// Up to three excerpts.
    pub excerpts: Vec<String>,
    pub message: String,
    pub suggestion: String,
    /// Hard findings fail the check on their own.
    pub hard: bool,
    #[serde(skip)]
    pub severity: f64,
    /// The author's word to use instead, for the plan.
    #[serde(skip)]
    pub alt: Option<String>,
}

/// One occurrence, before grouping.
struct Hit {
    kind: &'static str,
    key: String,
    line: Option<usize>,
    excerpt: String,
    message: String,
    suggestion: String,
    hard: bool,
    severity: f64,
    alt: Option<String>,
    /// Strong author evidence on its own (short drafts need 2 hits otherwise).
    strong: bool,
}

impl Hit {
    fn new(kind: &'static str, key: impl Into<String>, line: Option<usize>, message: String, suggestion: String) -> Hit {
        Hit {
            kind,
            key: key.into(),
            line,
            excerpt: String::new(),
            message,
            suggestion,
            hard: false,
            severity: 0.0,
            alt: None,
            strong: true,
        }
    }
}

/// Group hits by (kind, key): one finding with the count, sorted lines and
/// up to three excerpts.
fn group(hits: Vec<Hit>) -> Vec<Finding> {
    let mut order: Vec<(&'static str, String)> = Vec::new();
    let mut by: HashMap<(&'static str, String), Finding> = HashMap::new();
    for h in hits {
        let k = (h.kind, h.key.clone());
        match by.get_mut(&k) {
            Some(f) => {
                f.count += 1;
                if let Some(l) = h.line {
                    f.lines.push(l);
                }
                if !h.excerpt.is_empty() && f.excerpts.len() < MAX_EXCERPTS && !f.excerpts.contains(&h.excerpt) {
                    f.excerpts.push(h.excerpt);
                }
                f.severity = f.severity.max(h.severity);
                f.hard |= h.hard;
            }
            None => {
                order.push(k.clone());
                by.insert(
                    k,
                    Finding {
                        kind: h.kind,
                        key: h.key,
                        count: 1,
                        lines: h.line.into_iter().collect(),
                        excerpts: if h.excerpt.is_empty() { Vec::new() } else { vec![h.excerpt] },
                        message: h.message,
                        suggestion: h.suggestion,
                        hard: h.hard,
                        severity: h.severity,
                        alt: h.alt,
                    },
                );
            }
        }
    }
    let mut out: Vec<Finding> = order.into_iter().filter_map(|k| by.remove(&k)).collect();
    for f in &mut out {
        f.lines.sort_unstable();
        f.lines.dedup();
        // Repeats are stronger evidence than a lone hit.
        f.severity += (f.count as f64 - 1.0).min(10.0);
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricRow {
    pub key: String,
    pub label: String,
    pub family: &'static str,
    pub value: f64,
    pub low: f64,
    pub median: f64,
    pub high: f64,
    pub status: Status,
}

/// One revision step: what to do, why, how much, where.
#[derive(Debug, Clone, Serialize)]
pub struct PlanStep {
    pub action: String,
    pub why: String,
    /// "9 of 31", "+3", "14 → 18 words"...
    pub quantity: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<usize>,
    pub hard: bool,
    #[serde(skip)]
    order: f64,
}

/// A ~1,000-word stretch of a long draft and what drifts in it.
#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    /// First and last line (1-based).
    pub lines: [usize; 2],
    pub words: usize,
    pub out_of_range: Vec<String>,
}

/// A short-form habit of the draft next to how often the author shows it.
#[derive(Debug, Clone, Serialize)]
pub struct HabitRow {
    /// 1-based item, for drafts of several items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<usize>,
    pub key: &'static str,
    pub draft_value: serde_json::Value,
    /// Share of the author's items with the same value (count bin).
    pub author_share: f64,
}

/// One item of a short-form draft.
#[derive(Debug, Clone, Serialize)]
pub struct ItemScore {
    pub item: usize,
    pub lines: [usize; 2],
    pub words: usize,
    pub score: f64,
}

/// How a batch of posts spreads compared with the author's posts.
#[derive(Debug, Clone, Serialize)]
pub struct Batch {
    pub items: usize,
    /// p25–p75 of words per item.
    pub length_spread: [f64; 2],
    pub author_length_spread: [f64; 2],
    pub layouts: Vec<(String, f64)>,
    pub author_layouts: Vec<(String, f64)>,
    pub endings: Vec<(String, f64)>,
    pub author_endings: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreBasis {
    /// windows (long-form) | items (short-form)
    pub unit: &'static str,
    /// Author windows or items the draft was ranked among.
    pub compared: usize,
    /// Window length in words (long-form, and short-form drafts of 150+
    /// words, whose items are also measured pooled), else 0.
    pub window_words: usize,
    /// source (each reference window judged against bands without its own
    /// work) | block (one or two works, or a blend: without its own half of its work) | item
    /// (leave-one-out over items)
    pub holdout: &'static str,
    /// Item kind compared (short-form).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// Narrative mode the bands were restricted to (fiction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// Measures in the score, and their families (each family weighs the same).
    pub metrics: usize,
    pub families: Vec<&'static str>,
    /// Short-form drafts of 150+ words: the two scores behind `score`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<ScoreParts>,
    pub note: String,
}

/// A short-form draft long enough for rates (an email, an X Article, a
/// batch of posts) is scored twice and the two percentiles are combined.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreParts {
    /// Item habits: percentile among the author's items (mean over items).
    pub items: Option<f64>,
    /// The items pooled: percentile of their rhythm, punctuation, lexis and
    /// function-word rates among the author's own windows of that length.
    pub windows: Option<f64>,
    pub windows_compared: usize,
    /// Distance behind `windows`, as `distance` is for long-form drafts.
    pub window_distance: f64,
}

/// Everything `check` reports about one draft.
pub struct Checked {
    pub register: Register,
    pub score: Option<f64>,
    pub distance: f64,
    pub basis: ScoreBasis,
    /// high | medium | low
    pub reliability: &'static str,
    pub findings: Vec<Finding>,
    pub plan: Vec<PlanStep>,
    pub rows: Vec<MetricRow>,
    pub segments: Vec<Segment>,
    pub habits: Vec<HabitRow>,
    pub items: Vec<ItemScore>,
    pub batch: Option<Batch>,
    pub warnings: Vec<String>,
}

impl Checked {
    pub fn hard_failures(&self) -> usize {
        self.findings.iter().filter(|f| f.hard).count()
    }

    /// No hard failure and at least as typical as the author's 10th
    /// percentile (no score: hard failures alone decide).
    pub fn pass(&self) -> bool {
        self.hard_failures() == 0 && self.score.is_none_or(|s| s >= PASS_SCORE)
    }
}

// ── The draft ────────────────────────────────────────────────────────────────

/// The draft as lines, word tokens anchored to lines, and paragraphs
/// anchored to their first line. Headings, code and link targets are not
/// prose: they stay out of the word stream (no "https" or heading words in
/// the off-voice check), but headings and fences stay as paragraphs so
/// structure measures can count them.
pub struct Draft<'a> {
    pub lines: Vec<&'a str>,
    pub tokens: Vec<String>,
    pub cased: Vec<String>,
    pub token_line: Vec<usize>,
    /// (paragraph, its lines trimmed, first line index)
    pub paragraphs: Vec<(String, Vec<&'a str>, usize)>,
    /// Split sentences the short-form way (every line break and terminal
    /// mark ends one), as the register's measures do.
    pub short_form: bool,
}

fn is_heading_line(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with('#') && t.trim_start_matches('#').starts_with(' ')
}

fn is_fence_line(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// A line without markdown link targets and bare URLs.
fn prose_of_line(l: &str) -> String {
    metrics::strip_links(l)
}

impl<'a> Draft<'a> {
    pub fn of(text: &'a str) -> Draft<'a> {
        let lines: Vec<&str> = text.lines().collect();
        let (mut tokens, mut cased, mut token_line) = (Vec::new(), Vec::new(), Vec::new());
        let mut in_code = false;
        for (i, l) in lines.iter().enumerate() {
            if is_fence_line(l) {
                in_code = !in_code;
                continue;
            }
            if in_code || is_heading_line(l) {
                continue;
            }
            let c = text::word_tokens_cased(&text::normalize_web(&prose_of_line(l)));
            for t in &c {
                if text::is_placeholder(t) {
                    continue;
                }
                tokens.push(t.to_lowercase());
                token_line.push(i);
                cased.push(t.clone());
            }
        }
        let mut paragraphs = Vec::new();
        let mut block: Vec<&str> = Vec::new();
        let mut start = 0;
        let flush = |block: &mut Vec<&'a str>, start: usize, out: &mut Vec<(String, Vec<&'a str>, usize)>| {
            if block.is_empty() {
                return;
            }
            if let Some(p) = text::paragraphs(&block.join("\n")).into_iter().next() {
                out.push((p, std::mem::take(block), start));
            }
            block.clear();
        };
        for (i, l) in lines.iter().enumerate() {
            // A separator (`---` between posts, a scene break) ends a
            // paragraph even without blank lines around it.
            if l.trim().is_empty() || is_separator(l) {
                flush(&mut block, start, &mut paragraphs);
            } else if is_heading_line(l) || is_fence_line(l) {
                flush(&mut block, start, &mut paragraphs);
                paragraphs.push((l.trim().to_string(), vec![l.trim()], i));
            } else {
                if block.is_empty() {
                    start = i;
                }
                block.push(l.trim());
            }
        }
        flush(&mut block, start, &mut paragraphs);
        Draft { lines, tokens, cased, token_line, paragraphs, short_form: false }
    }

    pub fn prose(&self) -> Vec<String> {
        self.paragraphs.iter().map(|p| p.0.clone()).collect()
    }

    /// 1-based line of a byte offset inside paragraph `pi`'s joined text.
    fn line_of(&self, pi: usize, offset: usize) -> usize {
        let (_, lines, start) = &self.paragraphs[pi];
        let mut acc = 0;
        for (j, l) in lines.iter().enumerate() {
            acc += l.len() + 1;
            if offset < acc {
                return start + j + 1;
            }
        }
        start + lines.len()
    }

    fn excerpt_at(&self, tok: usize, len: usize) -> String {
        let line = self.lines[self.token_line[tok]].trim();
        let words: Vec<&str> = self.cased[tok..(tok + len).min(self.cased.len())].iter().map(String::as_str).collect();
        excerpt(line, &words.join(" "))
    }

    /// Words capitalized at every occurrence: the draft's own names.
    fn names(&self) -> HashSet<String> {
        let mut caps: HashMap<&str, (usize, usize)> = HashMap::new();
        for (t, c) in self.tokens.iter().zip(&self.cased) {
            let e = caps.entry(t.as_str()).or_insert((0, 0));
            e.0 += 1;
            if c.chars().next().is_some_and(char::is_uppercase) {
                e.1 += 1;
            }
        }
        caps.into_iter()
            .filter(|(t, (n, up))| *t != "i" && n == up)
            .map(|(t, _)| t.to_string())
            .collect()
    }

    /// Every sentence of the prose: (1-based line, words).
    fn sentences(&self) -> Vec<(usize, usize, String)> {
        self.sentences_in(false)
    }

    /// `sentences`, split the way the register's measures split them.
    fn sentences_in(&self, short_form: bool) -> Vec<(usize, usize, String)> {
        (0..self.paragraphs.len()).flat_map(|pi| self.paragraph_sentences(pi, short_form)).collect()
    }

    /// Paragraph `pi`'s sentences, as the measures split them (ellipses
    /// unified), each as written: (1-based line, words, text). Headings and
    /// code have none.
    fn paragraph_sentences(&self, pi: usize, short_form: bool) -> Vec<(usize, usize, String)> {
        let para = &self.paragraphs[pi].0;
        if matches!(metrics::block_kind(para), metrics::Block::Heading | metrics::Block::Fence) {
            return Vec::new();
        }
        metrics::sentences_at(para, short_form)
            .into_iter()
            .map(|(off, s)| (self.line_of(pi, off), text::count_words(&metrics::strip_links(&s)), s))
            .collect()
    }

    /// Lines (1-based) whose prose contains any of `needles`, outside
    /// headings (the colon in "https:" is not a colon to cut).
    fn lines_with(&self, needles: &[&str]) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| !is_heading_line(l) && needles.iter().any(|n| prose_of_line(l).contains(n)))
            .map(|(i, _)| i + 1)
            .collect()
    }

    /// Prose lines (1-based) with their text, links stripped: outside code
    /// blocks and headings.
    fn prose_lines(&self) -> Vec<(usize, String)> {
        let mut in_code = false;
        let mut out = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            if is_fence_line(l) {
                in_code = !in_code;
                continue;
            }
            if !in_code && !is_heading_line(l) {
                out.push((i + 1, prose_of_line(l)));
            }
        }
        out
    }

    /// The polish runs of the draft's sentences, each as (first line, the
    /// run's sentences joined), for one of the polish keys. Paragraphs are
    /// boundaries, as in `metrics::measure_named`, so the runs cited are the
    /// runs counted.
    fn polish_runs(&self, key: &str) -> Vec<(usize, String)> {
        let mut sents: Vec<(usize, Vec<String>, String)> = Vec::new();
        for pi in 0..self.paragraphs.len() {
            let mut para: Vec<(usize, Vec<String>, String)> = self
                .paragraph_sentences(pi, self.short_form)
                .into_iter()
                .map(|(line, _, s)| (line, text::word_tokens(&metrics::strip_links(&s)), s))
                .filter(|s| !s.1.is_empty())
                .collect();
            if !para.is_empty() && !sents.is_empty() {
                sents.push((0, Vec::new(), String::new()));
            }
            sents.append(&mut para);
        }
        let toks: Vec<Vec<String>> = sents.iter().map(|s| s.1.clone()).collect();
        let p = metrics::polish(&toks);
        let runs = match key {
            "short_triplets_per_1k" => p.triplets,
            "anaphora_runs_per_1k" => p.anaphora,
            "mirror_pairs_per_1k" => p.mirrors,
            _ => Vec::new(),
        };
        runs.into_iter()
            .map(|(i, n)| (sents[i].0, sents[i..i + n].iter().map(|s| s.2.as_str()).collect::<Vec<_>>().join(" ")))
            .collect()
    }

    fn token_lines(&self, f: impl Fn(&[String], usize) -> bool) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.tokens.len()).filter(|&i| f(&self.tokens, i)).map(|i| self.token_line[i] + 1).collect();
        v.dedup();
        v
    }
}

/// The line, cut to about 140 characters around `needle`.
fn excerpt(line: &str, needle: &str) -> String {
    const MAX: usize = 140;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let lower = line.to_lowercase();
    let at = lower.find(&needle.to_lowercase()).unwrap_or(0);
    let start = line[..at.min(line.len())].char_indices().rev().nth(50).map(|(i, _)| i).unwrap_or(0);
    let s: String = line[start..].chars().take(MAX).collect();
    format!("{}{}…", if start > 0 { "…" } else { "" }, s.trim())
}

fn is_separator(l: &str) -> bool {
    let t = l.trim();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '='))
}

/// A short-form draft's items: one item, unless it has separator lines
/// (`---`, `***`, `===`). Blank lines never split: a post's hook / body /
/// CTA blocks and an email's greeting / body / sign-off are one item.
pub fn draft_items(t: &str) -> Vec<(String, [usize; 2])> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut first: Option<usize> = None;
    let mut last = 0;
    let lines: Vec<&str> = t.lines().collect();
    for (i, l) in lines.iter().enumerate().chain(std::iter::once((lines.len(), &"---"))) {
        if is_separator(l) {
            let body = cur.join("\n").trim().to_string();
            if !body.is_empty() {
                out.push((body, [first.unwrap_or(i) + 1, last + 1]));
            }
            cur.clear();
            first = None;
        } else {
            if !l.trim().is_empty() {
                first.get_or_insert(i);
                last = i;
            }
            cur.push(l);
        }
    }
    out
}

// ── Copying ──────────────────────────────────────────────────────────────────

fn ngram_hash(toks: &[String]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    toks.hash(&mut h);
    h.finish()
}

/// Every n-gram of the guarded profiles' stored text: its owner, and for
/// the target, in how many distinct passages/items it occurs (capped at 3).
fn guard_index(guards: &[&Profile], n: usize) -> HashMap<u64, (usize, u8, u32)> {
    let mut idx: HashMap<u64, (usize, u8, u32)> = HashMap::new();
    for (gi, g) in guards.iter().enumerate() {
        for (ci, c) in g.chunks.iter().enumerate() {
            let toks: Vec<String> = c.paragraphs.iter().flat_map(|p| text::word_tokens(p)).collect();
            for w in toks.windows(n) {
                let e = idx.entry(ngram_hash(w)).or_insert((gi, 0, u32::MAX));
                if e.0 == gi && e.2 != ci as u32 {
                    e.1 = (e.1 + 1).min(3);
                    e.2 = ci as u32;
                }
            }
        }
    }
    idx
}

/// A run of words the draft shares with a guarded profile's text.
struct Run {
    /// Tokens in the run.
    len: usize,
    /// Tokens that aren't names: "Jessica Livingston and Robert Morris for
    /// reading drafts" is a thank-you line, not lifted phrasing.
    phrasing: usize,
    /// Non-name words rarer than general English's top ~20k
    /// (`RARE_ZIPF`): what makes a run someone's line, not an idiom.
    rare: usize,
    /// 1-based line of its first word.
    line: usize,
}

/// Zipf below which a word is rare in general English (~3 per million:
/// "drafts" 3.6 and "stare" 3.9 are common; "tapestry" 3.2 is not).
const RARE_ZIPF: f32 = 3.5;
/// A shared run this long is a lifted line whatever its words.
pub const COPY_HARD_WORDS: usize = 12;

/// Long-form: 12+ words of phrasing, or 8+ with two rare words, is a lifted
/// line; 8–11 common words ("and I'd be lying if I said I") are an idiom
/// the author's own unseen chapter shares with their books, so only a soft
/// note; runs that are mostly names are no finding at all.
fn long_copy_verdict(r: &Run) -> Option<bool> {
    (r.phrasing >= COPY_NGRAM).then_some(r.phrasing >= COPY_HARD_WORDS || r.rare >= 2)
}

/// Runs of `n`+ words shared with the guarded text. The author's own
/// formulas (runs in 3+ of their passages or posts: "new episode of the pod
/// is out") are signature phrasing, not copying, and are exempt. `verdict`
/// decides from the run whether it fails (true), is a soft note (false),
/// or is no finding (None).
fn copy_hits(d: &Draft, guards: &[&Profile], n: usize, verdict: &dyn Fn(&Run) -> Option<bool>) -> Vec<Hit> {
    if d.tokens.len() < n {
        return Vec::new();
    }
    let idx = guard_index(guards, n);
    let copied = |i: usize| -> Option<usize> {
        let &(owner, seen, _) = idx.get(&ngram_hash(&d.tokens[i..i + n]))?;
        (owner != 0 || seen < 3).then_some(owner)
    };
    let draft_names = d.names();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= d.tokens.len() {
        let Some(owner) = copied(i) else {
            i += 1;
            continue;
        };
        let mut end = i + n;
        while end < d.tokens.len() && copied(end + 1 - n).is_some() {
            end += 1;
        }
        let is_name = |w: &String| {
            guards[owner].is_proper_noun(w) || (draft_names.contains(w) && lexicon::zipf(w).is_none_or(|z| z < 4.0))
        };
        let words = &d.tokens[i..end];
        let run = Run {
            len: end - i,
            phrasing: words.iter().filter(|w| !is_name(w)).count(),
            rare: words
                .iter()
                .filter(|w| !is_name(w) && w.chars().all(|c| c.is_alphabetic() || c == '-'))
                .filter(|w| lexicon::zipf(w).is_none_or(|z| z < RARE_ZIPF))
                .count(),
            line: d.token_line[i] + 1,
        };
        let Some(is_hard) = verdict(&run) else {
            i = end;
            continue;
        };
        let len = run.len;
        let stock = !is_hard && run.rare < 2;
        let mut h = Hit::new(
            "copy_overlap",
            d.tokens[i..i + n.min(4)].join(" "),
            Some(run.line),
            if stock {
                format!("{len} consecutive words match {}'s own text, all of them common words: a stock phrase, not a lifted line", guards[owner].name)
            } else {
                format!("{len} consecutive words match {}'s own text", guards[owner].name)
            },
            if stock {
                "Keep it if it is your own phrasing; reword it only if it came from the author's text".into()
            } else {
                "Rewrite this passage in new words: imitate the author's mechanisms, never their lines".into()
            },
        );
        h.excerpt = d.cased[i..end.min(i + 25)].join(" ");
        h.hard = is_hard;
        h.severity = if is_hard { 1000.0 + len as f64 } else if stock { 20.0 } else { 70.0 };
        out.push(h);
        i = end;
    }
    out
}

/// Banned words and phrases (one per line, '#' comments) from --avoid files.
fn avoid_hits(d: &Draft, avoid: &[String], concepts: &[Concept]) -> Vec<Hit> {
    let mut out = Vec::new();
    for entry in avoid {
        let pat = text::word_tokens(entry);
        if pat.is_empty() {
            continue;
        }
        for i in 0..d.tokens.len().saturating_sub(pat.len() - 1) {
            if d.tokens[i..i + pat.len()] == pat[..] {
                let alt = preferred_for(&pat.join(" "), concepts);
                let mut h = Hit::new(
                    "avoid_list",
                    entry.clone(),
                    Some(d.token_line[i] + 1),
                    format!("'{entry}' is on the avoid list"),
                    match &alt {
                        Some((w, s)) => format!("Use '{w}' (the author's choice, {:.0}% of uses) or rephrase", s * 100.0),
                        None => "Cut it or rephrase in plain words".into(),
                    },
                );
                h.excerpt = d.excerpt_at(i, pat.len());
                h.hard = true;
                h.severity = 900.0;
                h.alt = alt.map(|a| a.0);
                out.push(h);
            }
        }
    }
    out
}

/// The author's preferred member of `w`'s synonym group and its share.
fn preferred_for(w: &str, concepts: &[Concept]) -> Option<(String, f64)> {
    let g = lexicon::group_of(w)?;
    let c = concepts.iter().find(|c| c.group == g)?;
    (c.preferred != w).then(|| (c.preferred.clone(), c.preferred_share))
}

fn share_in(c: &Concept, w: &str) -> f64 {
    c.members.iter().find(|m| m.word == w).map(|m| m.share).unwrap_or(0.0)
}

// ── Lexis ────────────────────────────────────────────────────────────────────

/// Expected uses at the draft's length below which one use of a tell is
/// already more than the author would write (they'd have it in under one
/// draft in ten this long).
const LONE_TELL_EXPECTED: f64 = 0.1;
/// Per 1k words: an author this far under it all but never writes the tell.
const NEVER_TELL_RATE: f64 = 0.05;

/// LLM tells the draft uses more than the author would at this length.
/// Long drafts need repeats the author's rate can't explain. A post almost
/// never repeats a word, so in a short draft (`short`) one tell counts when
/// the author all but never writes it, or when it comes with another one.
fn tell_hits(d: &Draft, rates: &HashMap<&'static str, f64>, concepts: &[Concept], short: bool) -> Vec<Hit> {
    let words = d.tokens.len().max(1) as f64;
    let names = d.names();
    let hits: Vec<voice::TellHit> = metrics::stock_phrases(&d.tokens)
        .into_iter()
        .filter(|h| !names.contains(&d.tokens[h.at]))
        .collect();
    let mut by_tell: HashMap<&str, usize> = HashMap::new();
    for h in &hits {
        *by_tell.entry(h.tell).or_insert(0) += 1;
    }
    let distinct = by_tell.len();
    hits.iter()
        .filter_map(|h| {
            let author = rates.get(h.tell).copied().unwrap_or(0.0);
            let expected = author * words / 1000.0;
            let n = by_tell[h.tell];
            // About three Poisson SDs above what the author's own rate explains.
            let repeated = n >= 2 && (n as f64) >= expected + 3.0 * expected.sqrt() + 1.0;
            let lone = short && expected < LONE_TELL_EXPECTED && (author < NEVER_TELL_RATE || distinct >= 2);
            if !repeated && !lone {
                return None;
            }
            let alt = preferred_for(h.tell, concepts);
            let author_use = if author <= 0.0 {
                "the author never writes it".to_string()
            } else if author < 1.0 {
                format!("the author writes it about once per {} words", num(1000.0 / author))
            } else {
                format!("the author uses it {author:.2} times per 1k words")
            };
            let message = if short {
                let with = if distinct >= 2 { format!("; the draft has {distinct} different stock phrases") } else { String::new() };
                format!("'{}' reads as LLM prose: {author_use}{with}", h.tell)
            } else {
                format!("'{}' reads as LLM prose: {author_use}, this draft {:.2} per 1k", h.tell, n as f64 * 1000.0 / words)
            };
            let mut hit = Hit::new(
                "llm_tell",
                h.tell,
                Some(d.token_line[h.at] + 1),
                message,
                match &alt {
                    Some((w, s)) => format!("Use '{w}' (the author's choice, {:.0}%) or say it plainly", s * 100.0),
                    None => "Cut it, or say the thing plainly the way the author would".into(),
                },
            );
            hit.excerpt = d.excerpt_at(h.at, h.len);
            hit.severity = 60.0 + (n as f64 - expected).min(20.0) + if distinct >= 2 { 10.0 } else { 0.0 };
            hit.alt = alt.map(|a| a.0);
            Some(hit)
        })
        .collect()
}

/// Words the author avoids, and synonyms the draft mixes where the author
/// keeps to one.
fn word_hits(d: &Draft, target: &Profile, concepts: &[Concept], avoided: &[voice::AvoidedWord]) -> Vec<Hit> {
    let by_group: HashMap<usize, &Concept> = concepts.iter().map(|c| (c.group, c)).collect();
    let avoided: HashMap<&str, &voice::AvoidedWord> = avoided.iter().map(|a| (a.word.as_str(), a)).collect();
    let used: HashSet<&str> = d.tokens.iter().map(String::as_str).collect();
    let names = d.names();
    let n_author = target.n_tokens.max(1) as f64;
    let mut out = Vec::new();
    for (i, w) in d.tokens.iter().enumerate() {
        if names.contains(w) || target.is_proper_noun(w) {
            continue;
        }
        let concept = lexicon::group_of(w).and_then(|g| by_group.get(&g)).filter(|c| c.total >= MIN_CONCEPT_USES);
        if let Some(c) = concept {
            let s = share_in(c, w);
            if c.preferred == *w || s >= RARE_SHARE {
                continue;
            }
            let mixes = used.contains(c.preferred.as_str());
            let mut h = Hit::new(
                if mixes { "synonym_variation" } else { "off_voice_word" },
                w.clone(),
                Some(d.token_line[i] + 1),
                if mixes {
                    format!(
                        "the draft mixes '{w}' with '{}'; the author keeps to '{}' ({:.0}% of uses)",
                        c.preferred, c.preferred, c.preferred_share * 100.0
                    )
                } else {
                    format!(
                        "'{w}' is off-voice: {:.0}% of the author's uses in this group (preferred '{}', {:.0}%)",
                        s * 100.0, c.preferred, c.preferred_share * 100.0
                    )
                },
                format!("Use '{}'", c.preferred),
            );
            h.excerpt = d.excerpt_at(i, 1);
            h.severity = if mixes { 40.0 } else { 50.0 };
            h.alt = Some(c.preferred.clone());
            h.strong = s == 0.0 && c.total >= MIN_CONCEPT_USES;
            out.push(h);
            continue;
        }
        let ly_never = metrics::is_ly_adverb(w)
            && !target.word_counts.contains_key(w)
            && lexicon::per_million(w) * n_author / 1e6 >= 8.0;
        if let Some(a) = avoided.get(w.as_str()).filter(|a| a.expected >= MIN_AVOIDED_EXPECTED) {
            out.push(off_voice(d, i, w, a.observed, a.expected, a.instead.as_deref()));
        } else if ly_never {
            out.push(off_voice(d, i, w, 0, lexicon::per_million(w) * n_author / 1e6, None));
        }
    }
    out
}

/// A word finding needs two hits in the draft, or an author who never uses
/// the word where they had every chance to. One rare-but-used word is what
/// an unseen piece by the author contains anyway (measured: a dozen per
/// 3,000-word Hoover chapter), so alone it is noise.
fn with_evidence(hits: Vec<Hit>) -> Vec<Hit> {
    let mut per_word: HashMap<String, usize> = HashMap::new();
    for h in &hits {
        *per_word.entry(h.key.clone()).or_insert(0) += 1;
    }
    hits.into_iter().filter(|h| h.strong || per_word[&h.key] >= 2).collect()
}

fn off_voice(d: &Draft, i: usize, w: &str, observed: u64, expected: f64, instead: Option<&str>) -> Hit {
    let mut h = Hit::new(
        "off_voice_word",
        w,
        Some(d.token_line[i] + 1),
        format!("the author almost never uses '{w}' ({observed} times where general English would have ~{expected:.0})"),
        match instead {
            Some(alt) => format!("Use '{alt}', the author's word for this"),
            None => "Cut it or rephrase in the author's plainer words".into(),
        },
    );
    h.excerpt = d.excerpt_at(i, 1);
    h.severity = 45.0;
    h.alt = instead.map(str::to_string);
    h.strong = observed == 0 && expected >= 15.0;
    h
}

// ── Long-form measures ───────────────────────────────────────────────────────

fn rows_for(values: &[f64], bands: &[Band], words: usize) -> Vec<MetricRow> {
    bands
        .iter()
        .zip(values)
        .map(|(b, &v)| MetricRow {
            key: b.key.clone(),
            label: b.label.clone(),
            family: b.family,
            value: metrics::round_unit(v, b.unit),
            low: b.low,
            median: b.median,
            high: b.high,
            status: metrics::status(v, b, words),
        })
        .collect()
}

/// Measures that a finding or plan step may name: not derived, not a
/// function-word rate, not unmeasured.
fn actionable(r: &MetricRow) -> bool {
    // "Repeat phrases more" is not advice anyone should follow.
    let fewer_echoes = r.key == "echo_phrases_per_1k" && r.value < r.median;
    !metrics::is_derived(&r.key) && r.status != Status::Unmeasured && !fewer_echoes
}

/// Measures left out of a window's distance: openings mean nothing inside
/// a window taken mid-work.
fn skip_in_window(b: &Band) -> bool {
    b.key == "opening_paragraph_words"
}

/// Measures a window of pooled posts leaves out: the item measures already
/// judge links, and pooled posts have no opening paragraph.
fn skip_in_pooled(b: &Band) -> bool {
    skip_in_window(b) || b.key == "links_per_1k"
}

/// The draft measured as a window of the author's text at its own length.
struct Windowed {
    values: Vec<f64>,
    ranges: metrics::Ranges,
    rows: Vec<MetricRow>,
    distance: f64,
    /// Percentile among the author's own windows (None under 3 of them).
    score: Option<f64>,
    compared: usize,
    holdout: &'static str,
    mode: Option<Mode>,
    /// Author windows the bands came from.
    windows: usize,
    /// Share of those windows whose narration mixes tenses (30–60% present).
    author_mixed: f64,
}

/// Rates, bands and score of `paragraphs` (a long draft's prose, or a
/// short-form draft's items pooled) among the author's `words`-word windows.
fn windowed(target: &Profile, paragraphs: &[String], words: usize, fw: &[String]) -> Windowed {
    let pooled = target.register.is_short_form();
    let values = metrics::measure_named(paragraphs, fw, pooled, &target.proper_nouns);
    let defs = metrics::defs(fw);
    let at = |k: &str| defs.iter().position(|d| d.key == k).unwrap_or(0);
    // Narrative mode: compare a present-tense chapter with present-tense
    // chapters (fiction with clear modes only).
    let all_rows = metrics::window_rows(target, words, fw, true);
    let draft_mode = metrics::mode_of(values[at("pov_first_share")], values[at("present_tense_share")]);
    let mut mode = None;
    let mut rows_used: Vec<&WindowRow> = all_rows.iter().collect();
    if target.register == Register::Fiction && words >= 300 && draft_mode.tense != "mixed" {
        let modes = metrics::chunk_modes(target);
        let same: Vec<&WindowRow> = all_rows.iter().filter(|r| metrics::window_mode(r, &modes) == draft_mode).collect();
        if same.len() >= 8 && same.len() < all_rows.len() {
            rows_used = same;
            mode = Some(draft_mode);
        }
    }
    let ranges = metrics::ranges_from(target, words, fw, &rows_used);
    let mut rows = rows_for(&values, &ranges.bands, words);
    // A long draft is a whole piece, so its opening counts; pooled posts
    // skip on both sides what the item measures already judge.
    let distance = if pooled {
        for (r, b) in rows.iter_mut().zip(&ranges.bands) {
            if skip_in_pooled(b) {
                r.status = Status::Unmeasured;
            }
        }
        metrics::distance(&values, &ranges.bands, words, &skip_in_pooled)
    } else {
        metrics::distance(&values, &ranges.bands, words, &|_| false)
    };
    let (refs, holdout) =
        reference_distances(target, &rows_used, fw, words, if pooled { &skip_in_pooled } else { &skip_in_window });
    let score = (refs.len() >= 3).then(|| (metrics::percentile_above(&refs, distance) * 10.0).round() / 10.0);
    let mixed = |v: f64| v > 0.3 && v < 0.6;
    let author_mixed = rows_used.iter().filter(|r| mixed(r.values[at("present_tense_share")])).count() as f64
        / rows_used.len().max(1) as f64;
    Windowed {
        values,
        rows,
        distance,
        score,
        compared: refs.len(),
        holdout,
        mode,
        windows: rows_used.len(),
        author_mixed,
        ranges,
    }
}

/// Measured families of a window's bands, and how many measures.
fn measured_families(w: &Windowed) -> (Vec<&'static str>, usize) {
    let measured: Vec<&MetricRow> = w.rows.iter().filter(|r| r.status != Status::Unmeasured).collect();
    let mut families: Vec<&'static str> = measured.iter().map(|r| r.family).collect();
    families.sort_unstable();
    families.dedup();
    (families, measured.len())
}

/// Two percentiles (0–100) combined by Fisher's method: the share of the
/// author's own text at least this unusual on both counts, if the two were
/// independent. Each is floored at half a step of its resolution.
fn combine(a: f64, n_a: usize, b: f64, n_b: usize) -> f64 {
    let p = |s: f64, n: usize| (s / 100.0).max(0.5 / n.max(1) as f64).min(1.0);
    let q = p(a, n_a) * p(b, n_b);
    ((q * (1.0 - q.ln())).min(1.0) * 1000.0).round() / 10.0
}

/// Reference distances: each author window measured against bands built
/// without its own source (with fewer than three works, or for a blend,
/// without its own half of its work: holding out a whole blend component
/// would make its windows alien), so an unseen piece by the author is
/// compared like with like.
fn reference_distances(p: &Profile, rows: &[&WindowRow], fw: &[String], words: usize, skip: &dyn Fn(&Band) -> bool) -> (Vec<(f64, f64)>, &'static str) {
    let defs = metrics::defs(fw);
    let presence = metrics::markup_presence(p);
    let mut sources: Vec<u32> = rows.iter().map(|r| r.source(p)).collect();
    sources.sort_unstable();
    sources.dedup();
    let (folds, holdout): (Vec<usize>, &'static str) = if sources.len() >= 3 && p.kind != crate::engine::profile::ProfileKind::Blend {
        let nf = sources.len().min(10);
        (
            rows.iter()
                .map(|r| sources.iter().position(|s| *s == r.source(p)).unwrap() * nf / sources.len())
                .collect(),
            "source",
        )
    } else {
        // A contiguous third of every source per fold: big enough that a
        // held-out block has its own scenes and cast, as an unseen work does.
        const BLOCKS: usize = 2;
        let mut seen: HashMap<u32, usize> = HashMap::new();
        let mut total: HashMap<u32, usize> = HashMap::new();
        for r in rows {
            *total.entry(r.source(p)).or_insert(0) += 1;
        }
        let folds = rows
            .iter()
            .map(|r| {
                let s = r.source(p);
                let k = seen.entry(s).or_insert(0);
                *k += 1;
                (*k - 1) * BLOCKS / total[&s].max(1)
            })
            .collect();
        (folds, "block")
    };
    let nf = folds.iter().max().map(|m| m + 1).unwrap_or(0);
    let mut out = vec![(0.0, 0.0); rows.len()];
    for f in 0..nf {
        let train: Vec<&WindowRow> = rows.iter().zip(&folds).filter(|(_, g)| **g != f).map(|(r, _)| *r).collect();
        if train.len() < 3 {
            continue;
        }
        let bands = metrics::bands_from(p, &defs, &train, &presence);
        for (i, r) in rows.iter().enumerate() {
            if folds[i] == f {
                out[i] = (metrics::distance(&r.values, &bands, words, skip), r.weight);
            }
        }
    }
    (out.into_iter().filter(|r| r.1 > 0.0).collect(), holdout)
}

fn reliability(compared: usize, words: usize, short: bool) -> &'static str {
    match (short, compared, words) {
        (true, n, _) if n >= 100 => "high",
        (true, n, _) if n >= 30 => "medium",
        (false, n, w) if n >= 40 && w >= 1000 => "high",
        (false, n, w) if n >= 15 && w >= 300 => "medium",
        _ => "low",
    }
}

/// The polish measures, what their runs are, and what to do about them.
const POLISH: [(&str, &str, &str); 3] = [
    ("short_triplets_per_1k",
     "runs of three or more short sentences (6 words or fewer): tidy beats",
     "Join two of the beats, or let one run on with a clause"),
    ("anaphora_runs_per_1k",
     "runs of sentences opening the same way",
     "Vary the openings, or fold the run into one sentence"),
    ("mirror_pairs_per_1k",
     "mirrored pairs: a sentence and its negation side by side",
     "Say it once: cut the mirror, or let the contradiction sit inside one plainer sentence"),
];

/// Lines of each construction above the author's band, grouped per pattern.
fn structure_hits(d: &Draft, rows: &[MetricRow]) -> Vec<Hit> {
    let above = |key: &str| {
        rows.iter()
            .find(|r| r.key == key)
            .is_some_and(|r| r.status == Status::OutOfRange && r.value > r.high)
    };
    let (chains, not_but, ing, echoes) = (
        above("and_chains_per_1k_sentences"),
        above("not_but_per_1k"),
        above("trailing_ing_per_1k"),
        above("echo_phrases_per_1k"),
    );
    let mut out = Vec::new();
    let hit = |key: &str, line: usize, ex: String, message: &str, suggestion: &str| {
        let mut h = Hit::new("structure", key, Some(line), message.to_string(), suggestion.to_string());
        h.excerpt = ex;
        h.severity = 30.0;
        h
    };
    for (pi, (para, _, _)) in d.paragraphs.iter().enumerate() {
        let sents = text::sentences(para);
        let mut cursor = 0;
        for (si, s) in sents.iter().enumerate() {
            let off = para[cursor..].find(s.as_str()).map(|o| o + cursor).unwrap_or(cursor);
            cursor = off;
            let line = d.line_of(pi, off);
            let short = excerpt(s, "");
            if chains && metrics::is_and_chain(&text::word_tokens(s)) {
                out.push(hit("and_chains_per_1k_sentences", line, short.clone(), "sentences chaining three or more 'and's",
                    "Break them up, or build them from but/because/when clauses instead"));
            }
            if ing && metrics::trailing_ing_count(s) > 0 {
                out.push(hit("trailing_ing_per_1k", line, short.clone(), "trailing ', -ing' clauses",
                    "End on the main verb; give the -ing action its own sentence or cut it"));
            }
            let pair = &sents[si..(si + 2).min(sents.len())];
            if not_but && metrics::not_but_count(&pair[..1]) + cross_frame(pair) > 0 {
                out.push(hit("not_but_per_1k", line, short, "'not X, but Y' contrast frames",
                    "Say what it is directly; drop the 'not this, but that' setup"));
            }
        }
    }
    // Polish: each run is one hit on its first line, cited whole.
    for (key, message, suggestion) in POLISH {
        let Some(r) = rows.iter().find(|r| r.key == key && r.status == Status::OutOfRange && r.value > r.high) else { continue };
        for (line, run) in d.polish_runs(key) {
            let mut h = Hit::new(
                "structure",
                key,
                Some(line),
                format!("{message}: {} per 1k words, the author {}\u{2013}{} (median {})", r.value, r.low, r.high, r.median),
                suggestion.into(),
            );
            h.excerpt = excerpt(&run, "");
            h.severity = 35.0;
            out.push(h);
        }
    }
    if echoes {
        // One finding for all echoes: the repeats of the top phrases.
        let top: Vec<(String, usize)> = metrics::echo_phrases(&d.tokens).into_iter().take(5).collect();
        let names = top.iter().map(|(p, n)| format!("'{p}' ×{n}")).collect::<Vec<_>>().join(", ");
        for (phrase, _) in &top {
            let pat: Vec<&str> = phrase.split(' ').collect();
            for i in (0..d.tokens.len().saturating_sub(2)).filter(|&i| pat.iter().zip(&d.tokens[i..]).all(|(a, b)| a == b)).skip(1) {
                let mut h = Hit::new(
                    "structure",
                    "echo_phrases_per_1k",
                    Some(d.token_line[i] + 1),
                    format!("repeated three-word phrases (echoes): {names}"),
                    "Vary or cut the repeats unless one is a deliberate refrain".into(),
                );
                h.excerpt = d.excerpt_at(i, 3);
                h.severity = 25.0;
                out.push(h);
            }
        }
    }
    out
}

// ── Typography ───────────────────────────────────────────────────────────────

/// How a form reads in a finding: (slug for the key, the form as named, how to write it).
fn form_words(family: &str, form: &str) -> (String, String, String) {
    let (slug, name, how) = match (family, form) {
        ("double_quotes", "curly") => ("curly", "curly quotes (\u{201C} \u{201D})", "\u{201C} to open, \u{201D} to close"),
        ("double_quotes", _) => ("straight", "straight quotes (\")", "\" for both"),
        ("apostrophes", "curly") => ("curly", "curly apostrophes (\u{2019})", "\u{2019} (don\u{2019}t, it\u{2019}s)"),
        ("apostrophes", _) => ("straight", "straight apostrophes (')", "' (don't, it's)"),
        (_, "\u{2026}") => ("ellipsis_character", "ellipses as one character (\u{2026})", "the single character \u{2026}"),
        (_, "...") => ("three_dots", "ellipses as three dots (...)", "three dots ..."),
        (_, ". . .") => ("spaced_dots", "spaced ellipses (. . .)", "spaced dots . . ."),
        (_, "..") => ("two_dots", "two-dot pauses (..)", "two dots .."),
        (_, "\u{2014}") => ("closed_em_dash", "closed em dashes (word\u{2014}word)", "an em dash with no spaces: word\u{2014}word"),
        (_, " \u{2014} ") => ("spaced_em_dash", "spaced em dashes (word \u{2014} word)", "an em dash with a space each side: word \u{2014} word"),
        (_, "--") => ("double_hyphen", "double hyphens (--)", "two hyphens: word--word"),
        (_, " - ") => ("spaced_hyphen", "spaced hyphens as dashes (word - word)", "a hyphen with a space each side: word - word"),
        (_, "\u{2013}") => ("en_dash", "en dashes (\u{2013})", "an en dash: word \u{2013} word"),
        (_, f) => return (f.to_string(), format!("'{f}' scene breaks"), format!("a line with only {f}")),
    };
    (slug.to_string(), name.to_string(), how.to_string())
}

/// The draft's typographic forms per family, each with the lines it is on.
fn draft_forms(d: &Draft, long_form: bool) -> Vec<(&'static str, Vec<(String, usize)>)> {
    let (mut quotes, mut apostrophes, mut ellipses, mut dashes, mut breaks) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (line, prose) in d.prose_lines() {
        for c in prose.chars() {
            match c {
                '"' => quotes.push(("straight".to_string(), line)),
                '\u{201C}' | '\u{201D}' => quotes.push(("curly".to_string(), line)),
                _ => {}
            }
        }
        for (_, curly) in metrics::apostrophes_at(&prose) {
            apostrophes.push((if curly { "curly" } else { "straight" }.to_string(), line));
        }
        ellipses.extend(metrics::ellipsis_forms(&prose).into_iter().map(|(f, _)| (f.to_string(), line)));
        dashes.extend(metrics::dash_forms(&prose).into_iter().map(|(f, _)| (f.to_string(), line)));
    }
    // Short-form drafts use separator lines between items, not scene breaks.
    if long_form {
        let blank = |i: Option<&&str>| i.is_none_or(|l| l.trim().is_empty());
        for (i, l) in d.lines.iter().enumerate() {
            let standalone = blank(i.checked_sub(1).and_then(|j| d.lines.get(j))) && blank(d.lines.get(i + 1));
            if let Some(f) = crate::engine::profile::scene_break_form(l).filter(|_| standalone && !is_fence_line(l)) {
                breaks.push((f, i + 1));
            }
        }
    }
    vec![("double_quotes", quotes), ("apostrophes", apostrophes), ("ellipses", ellipses), ("dashes", dashes), ("scene_breaks", breaks)]
}

/// Off-habit dashes or ellipses a draft needs before they are a finding.
const TYPOGRAPHY_MIN_STRAYS: usize = 3;

/// Typographic forms the author (almost) never uses where they hold another
/// form at least 80% of the time: one finding per form, with the lines and
/// the exact conversion. Mechanical and certain, so they lead the plan.
fn typography_hits(d: &Draft, habits: &metrics::TypographyHabits, long_form: bool) -> Vec<Hit> {
    let family_habits = [
        ("double_quotes", &habits.double_quotes),
        ("apostrophes", &habits.apostrophes),
        ("ellipses", &habits.ellipses),
        ("dashes", &habits.dashes),
        ("scene_breaks", &habits.scene_breaks),
    ];
    let mut out = Vec::new();
    for (family, found) in draft_forms(d, long_form) {
        let Some((_, shares)) = family_habits.iter().find(|f| f.0 == family) else { continue };
        // Scene breaks are told apart by their glyph: "•••" and "• • •" are
        // one mark spaced by two typesetters (an unseen Hoover novel closes
        // them up); "---" for "• • •" is another mark.
        let mark = |f: &str| -> String {
            if family == "scene_breaks" { f.chars().filter(|c| !c.is_whitespace()).take(1).collect() } else { f.to_string() }
        };
        let mut by_mark: Vec<(String, f64)> = Vec::new();
        for (f, sh) in shares.iter() {
            match by_mark.iter_mut().find(|m| m.0 == mark(f)) {
                Some(m) => m.1 += sh,
                None => by_mark.push((mark(f), *sh)),
            }
        }
        by_mark.sort_by(|a, b| b.1.total_cmp(&a.1));
        let Some((top_mark, top_share)) = by_mark.first().filter(|t| t.1 >= metrics::TYPOGRAPHY_HABIT_SHARE) else { continue };
        let Some((top, _)) = shares.iter().find(|s| &mark(&s.0) == top_mark) else { continue };
        let mut forms: Vec<&String> = found.iter().map(|f| &f.0).collect();
        forms.sort();
        forms.dedup();
        for form in forms {
            let author = by_mark.iter().find(|m| m.0 == mark(form)).map(|m| m.1).unwrap_or(0.0);
            // A form the author really uses (10%+) is voice, not a slip.
            if &mark(form) == top_mark || author >= 0.1 {
                continue;
            }
            let at: Vec<usize> = found.iter().filter(|f| &f.0 == form).map(|f| f.1).collect();
            let n = at.len();
            // Dashes and ellipses are set differently from book to book (an
            // unseen Hoover novel spaces its ellipses), so a stray one or two
            // is no certainty; quotes, apostrophes and break lines are.
            if matches!(family, "dashes" | "ellipses") && n < TYPOGRAPHY_MIN_STRAYS {
                continue;
            }
            let mut lines = at.clone();
            lines.dedup();
            let (slug, from, _) = form_words(family, form);
            let (_, to, how) = form_words(family, top);
            // One hit per mark: the grouped finding counts marks, lists lines.
            for line in at {
                let mut h = Hit::new(
                    "typography",
                    format!("{family}:{slug}"),
                    Some(line),
                    format!("the author writes {to} {} of the time, {from} {}; this draft has {n} {from}", pct(*top_share), pct(author)),
                    format!("Use {to}: {n} {from} on lines {} \u{2192} {how}", join_lines(&lines)),
                );
                h.excerpt = excerpt(d.lines[line - 1].trim(), "");
                h.severity = 300.0 - (n as f64 - 1.0).min(10.0) + (n as f64).min(50.0);
                out.push(h);
            }
        }
    }
    out
}

/// A structure finding with lines makes the bare metric finding for the
/// same measure redundant.
/// Likewise a word flagged as off-voice (with the author's replacement)
/// needs no second finding as an LLM tell: the word finding says both.
fn drop_shadowed(findings: &mut Vec<Finding>) {
    let shown: HashSet<String> = findings.iter().filter(|f| f.kind == "structure").map(|f| f.key.clone()).collect();
    findings.retain(|f| !(f.kind == "metric_out_of_range" && shown.contains(&f.key)));
    let tells: HashSet<String> = findings.iter().filter(|f| f.kind == "llm_tell").map(|f| f.key.clone()).collect();
    let mut merged: HashSet<String> = HashSet::new();
    for f in findings.iter_mut() {
        if matches!(f.kind, "off_voice_word" | "synonym_variation") && tells.contains(&f.key) {
            f.message.push_str("; it is also a stock LLM word");
            f.severity += 15.0;
            merged.insert(f.key.clone());
        }
    }
    findings.retain(|f| !(f.kind == "llm_tell" && merged.contains(&f.key)));
}

/// The two-sentence "It's not X. It's Y." frame starting at pair[0].
fn cross_frame(pair: &[String]) -> usize {
    if pair.len() < 2 {
        return 0;
    }
    metrics::not_but_count(pair) - metrics::not_but_count(&pair[..1]) - metrics::not_but_count(&pair[1..])
}

fn metric_hits(rows: &[MetricRow], defs: &[metrics::Def], words: usize) -> Vec<Hit> {
    rows.iter()
        .zip(defs)
        .filter(|(r, _)| r.status == Status::OutOfRange && actionable(r))
        .map(|(r, def)| {
            let up = r.value < r.low;
            let width = (r.high - r.low).abs().max(1e-6);
            let gap = if up { r.low - r.value } else { r.value - r.high } / width;
            let mut h = Hit::new(
                "metric_out_of_range",
                r.key.clone(),
                None,
                format!(
                    "{}: {} (author {}–{}, median {}): {}",
                    r.label, r.value, r.low, r.high, r.median, if up { "raise" } else { "lower" }
                ),
                capitalize(if up { def.raise } else { def.lower }),
            );
            h.severity = 100.0 + gap.min(50.0) + if words < 300 { -20.0 } else { 0.0 };
            h
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

fn num(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r}") }
}

/// What the draft has of a metric's unit: sentences, paragraphs or words.
struct Counts {
    words: usize,
    sentences: Vec<(usize, usize, String)>,
    paragraphs: Vec<(usize, usize)>,
}

fn counts_of(d: &Draft, words: usize) -> Counts {
    Counts {
        words,
        sentences: d.sentences(),
        paragraphs: d
            .paragraphs
            .iter()
            .filter(|p| metrics::block_kind(&p.0) == metrics::Block::Prose || metrics::block_kind(&p.0) == metrics::Block::Quote)
            .map(|p| (p.2 + 1, text::sentences(&p.0).len()))
            .collect(),
    }
}

/// A plan step for one out-of-range measure, aimed at the author's median
/// (the band edge leaves it "near" after ordinary noise), with a quantity
/// and the lines to work on where they can be pointed at.
fn metric_step(r: &MetricRow, def: &metrics::Def, d: &Draft, c: &Counts) -> PlanStep {
    let up = r.value < r.median;
    let why = format!("{}: draft {}, author median {} (band {}–{})", r.label, fmt_val(r.value, def.unit), fmt_val(r.median, def.unit), fmt_val(r.low, def.unit), fmt_val(r.high, def.unit));
    let n_sents = c.sentences.len();
    let delta_of = |n: usize| ((r.median - r.value) * n as f64).round() as i64;
    let mut lines = Vec::new();
    let (action, quantity) = match r.key.as_str() {
        "short_sentence_share" | "long_sentence_share" => {
            let short = r.key == "short_sentence_share";
            let noun = if short { "sentences of 5 words or fewer" } else { "sentences of 25+ words" };
            let have: Vec<&(usize, usize, String)> = c.sentences.iter().filter(|s| if short { s.1 <= 5 } else { s.1 >= 25 }).collect();
            let delta = delta_of(n_sents);
            if delta < 0 {
                lines = have.iter().map(|s| s.0).collect();
                let verb = if short { "Merge" } else { "Split" };
                let into = if short { " into their neighbours" } else { "" };
                (format!("{verb} about {} of your {} {noun}{into}", -delta, have.len()), format!("{} of {}", -delta, have.len()))
            } else {
                (format!("Add about {} {noun} ({})", delta.max(1), def.raise), format!("+{}", delta.max(1)))
            }
        }
        "one_sentence_paragraph_share" | "dialogue_paragraph_share" => {
            let delta = delta_of(c.paragraphs.len());
            let noun = if r.key.starts_with("one") { "one-sentence paragraphs" } else { "paragraphs with dialogue" };
            if r.key.starts_with("one") && delta < 0 {
                lines = c.paragraphs.iter().filter(|p| p.1 == 1).map(|p| p.0).collect();
            }
            let have = (r.value * c.paragraphs.len() as f64).round() as i64;
            if delta < 0 {
                (format!("Cut about {} of your {have} {noun}: {}", -delta, def.lower), format!("{} of {have}", -delta))
            } else {
                (format!("Add about {} {noun}: {}", delta.max(1), def.raise), format!("+{}", delta.max(1)))
            }
        }
        "dialogue_word_share" => {
            let delta = delta_of(c.words);
            if delta < 0 {
                (format!("Move about {} words of dialogue into narration", -delta), format!("-{} words", -delta))
            } else {
                (format!("Move about {} words of narration into dialogue", delta), format!("+{} words", delta))
            }
        }
        _ => match def.unit {
            Unit::Per1k | Unit::Per1kSentences => {
                let base = if def.unit == Unit::Per1k { c.words } else { n_sents };
                let delta = ((r.median - r.value) * base as f64 / 1000.0).round() as i64;
                let delta = if delta == 0 { if up { 1 } else { -1 } } else { delta };
                let noun = r.label.replace(" per 1k words", "").replace(" per 1k sentences", "");
                if !up {
                    lines = punct_lines(d, &r.key);
                }
                if delta < 0 {
                    (format!("Cut about {} {noun}: {}", -delta, def.lower), format!("{delta}"))
                } else {
                    (format!("Add about {delta} {noun}: {}", def.raise), format!("+{delta}"))
                }
            }
            Unit::Share => (
                format!("{}: aim for about {} (now {})", capitalize(if up { def.raise } else { def.lower }), pct(r.median), pct(r.value)),
                format!("{} → {}", pct(r.value), pct(r.median)),
            ),
            _ => (
                format!("{}: aim for about {} (now {})", capitalize(if up { def.raise } else { def.lower }), num(r.median), num(r.value)),
                format!("{} → {}", num(r.value), num(r.median)),
            ),
        },
    };
    lines.truncate(30);
    PlanStep { action, why, quantity, lines, hard: false, order: 0.0 }
}

fn fmt_val(v: f64, unit: Unit) -> String {
    if unit == Unit::Share { pct(v) } else { num(v) }
}

fn sentence_lines(d: &Draft, f: impl Fn(&str) -> bool) -> Vec<usize> {
    let mut v: Vec<usize> = d.sentences().into_iter().filter(|s| f(&s.2)).map(|s| s.0).collect();
    v.dedup();
    v
}

/// Lines holding the marks a rate counts, so "cut 3 em dashes" says where.
fn punct_lines(d: &Draft, key: &str) -> Vec<usize> {
    match key {
        "questions_per_1k" | "narration_questions_per_1k" => d.lines_with(&["?"]),
        "exclamations_per_1k" => d.lines_with(&["!"]),
        "em_dashes_per_1k" => d.lines_with(&["\u{2014}", "--", " - "]),
        "ellipses_per_1k" => d.lines_with(&["\u{2026}", "..."]),
        "semicolons_per_1k" => d.lines_with(&[";"]),
        "colons_per_1k" => d.lines_with(&[":"]),
        "parentheses_per_1k" => d.lines_with(&["("]),
        "which_clauses_per_1k" => d.lines_with(&[", which"]),
        "similes_per_1k" => d.token_lines(metrics::is_simile_at),
        "the_way_per_1k" => d.token_lines(metrics::is_the_way_at),
        "ly_adverbs_per_1k" => d.token_lines(|t, i| metrics::is_ly_adverb(&t[i])),
        "contractions_per_1k" => d.token_lines(|t, i| crate::engine::style::is_contraction(&t[i])),
        "trailing_ing_per_1k" => sentence_lines(d, |s| metrics::trailing_ing_count(s) > 0),
        "and_chains_per_1k_sentences" => sentence_lines(d, |s| metrics::is_and_chain(&text::word_tokens(s))),
        "not_but_per_1k" => sentence_lines(d, |s| metrics::not_but_count(&[s.to_string()]) > 0),
        "double_punct_per_1k" => d.prose_lines().into_iter().filter(|(_, l)| !metrics::double_punct(l).is_empty()).map(|(i, _)| i).collect(),
        "space_before_punct_per_1k" => d.prose_lines().into_iter().filter(|(_, l)| !metrics::space_before_punct(l).is_empty()).map(|(i, _)| i).collect(),
        k if POLISH.iter().any(|p| p.0 == k) => d.polish_runs(k).into_iter().map(|r| r.0).collect(),
        _ => Vec::new(),
    }
}

/// One plan step per family: the measure furthest from the author's median,
/// the register's headline measures first among near-equals.
fn metric_plan(rows: &[MetricRow], defs: &[metrics::Def], d: &Draft, c: &Counts, words: usize, bands: &[Band], register: Register) -> Vec<PlanStep> {
    let headline = metrics::headline_keys(register);
    let mut best: HashMap<&'static str, (f64, usize)> = HashMap::new();
    for (j, r) in rows.iter().enumerate() {
        if r.status != Status::OutOfRange || !actionable(r) {
            continue;
        }
        let boost = if headline.contains(&r.key.as_str()) { 1.5 } else { 1.0 };
        let gap = boost * (r.value - r.median).abs() / metrics::tolerance(&bands[j], words);
        let e = best.entry(r.family).or_insert((0.0, j));
        if gap > e.0 {
            *e = (gap, j);
        }
    }
    let mut steps: Vec<PlanStep> = best
        .into_values()
        .map(|(gap, j)| {
            let mut s = metric_step(&rows[j], &defs[j], d, c);
            s.order = 100.0 + gap;
            s
        })
        .collect();
    steps.sort_by(|a, b| b.order.total_cmp(&a.order));
    steps
}

/// Plan steps for hard failures and word choices, from the findings.
fn finding_steps(findings: &[Finding]) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    // All copied runs are one step: rewrite these lines.
    let copies: Vec<&Finding> = findings.iter().filter(|f| f.hard && f.kind == "copy_overlap").collect();
    if !copies.is_empty() {
        let lines: Vec<usize> = copies.iter().flat_map(|f| f.lines.iter().copied()).collect();
        let longest = copies.iter().map(|f| f.message.split(' ').next().unwrap_or("").parse::<usize>().unwrap_or(0)).max().unwrap_or(0);
        steps.push(PlanStep {
            action: format!(
                "Rewrite {} passage(s) in new words (lines {}): imitate the author's mechanisms, never their lines",
                lines.len(),
                join_lines(&{
                    let mut l = lines.clone();
                    l.sort_unstable();
                    l
                })
            ),
            why: format!("runs of up to {longest} words match the author's own text"),
            quantity: lines.len().to_string(),
            lines,
            hard: true,
            order: 2000.0,
        });
    }
    for f in findings.iter().filter(|f| f.hard && f.kind != "copy_overlap") {
        let action = match f.kind {
            "avoid_list" => format!("Remove '{}' ({})", f.key, f.suggestion),
            _ => f.suggestion.clone(),
        };
        steps.push(PlanStep {
            action,
            why: f.message.clone(),
            quantity: f.count.to_string(),
            lines: f.lines.clone(),
            hard: true,
            order: 1000.0 + f.severity,
        });
    }
    // Typography is mechanical and certain: one step, ahead of every soft step.
    let typo: Vec<&Finding> = findings.iter().filter(|f| f.kind == "typography").collect();
    if !typo.is_empty() {
        steps.push(PlanStep {
            action: format!("Set type the author's way. {}", typo.iter().map(|f| f.suggestion.as_str()).collect::<Vec<_>>().join("; ")),
            why: typo.iter().map(|f| f.message.as_str()).collect::<Vec<_>>().join("; "),
            quantity: typo.iter().map(|f| f.count).sum::<usize>().to_string(),
            lines: typo.iter().flat_map(|f| f.lines.iter().copied()).collect(),
            hard: false,
            order: 900.0,
        });
    }
    let words: Vec<&Finding> = findings
        .iter()
        .filter(|f| matches!(f.kind, "off_voice_word" | "synonym_variation") && f.alt.is_some())
        .take(4)
        .collect();
    if !words.is_empty() {
        let parts: Vec<String> = words
            .iter()
            .map(|f| format!("'{}' with '{}' (lines {})", f.key, f.alt.as_deref().unwrap_or(""), join_lines(&f.lines)))
            .collect();
        steps.push(PlanStep {
            action: format!("Replace {}", parts.join("; ")),
            why: "the author keeps to one word for these meanings".into(),
            quantity: words.iter().map(|f| f.count).sum::<usize>().to_string(),
            lines: words.iter().flat_map(|f| f.lines.iter().copied()).collect(),
            hard: false,
            order: 99.0,
        });
    }
    let tells: Vec<&Finding> = findings.iter().filter(|f| f.kind == "llm_tell").take(4).collect();
    if !tells.is_empty() {
        let parts: Vec<String> = tells.iter().map(|f| format!("'{}' (lines {})", f.key, join_lines(&f.lines))).collect();
        steps.push(PlanStep {
            action: format!("Cut or rephrase the LLM tells {}", parts.join(", ")),
            why: "the author uses these far less than this draft does".into(),
            quantity: tells.iter().map(|f| f.count).sum::<usize>().to_string(),
            lines: tells.iter().flat_map(|f| f.lines.iter().copied()).collect(),
            hard: false,
            order: 98.0,
        });
    }
    steps
}

fn join_lines(l: &[usize]) -> String {
    let mut v: Vec<String> = l.iter().take(8).map(|x| x.to_string()).collect();
    if l.len() > 8 {
        v.push("…".into());
    }
    v.join(", ")
}

fn finish_plan(mut steps: Vec<PlanStep>) -> Vec<PlanStep> {
    steps.sort_by(|a, b| b.order.total_cmp(&a.order));
    steps.truncate(MAX_PLAN);
    for s in &mut steps {
        s.lines.sort_unstable();
        s.lines.dedup();
    }
    steps
}

fn sort_findings(f: &mut [Finding]) {
    f.sort_by(|a, b| {
        b.severity
            .total_cmp(&a.severity)
            .then(a.lines.first().unwrap_or(&0).cmp(b.lines.first().unwrap_or(&0)))
    });
}

/// Lexical context shared by both paths.
struct Lexis {
    concepts: Vec<Concept>,
    avoided: Vec<voice::AvoidedWord>,
    rates: HashMap<&'static str, f64>,
}

fn lexis(target: &Profile) -> Lexis {
    let corpus = Corpus::of(target);
    Lexis {
        concepts: voice::concepts(&corpus, 10.0),
        avoided: voice::avoided_words(&corpus, 1000),
        rates: voice::author_tell_rates(&corpus),
    }
}

/// Check a draft against a target voice. `kind` is what a short-form draft
/// is (post, reply, thread part); long-form registers ignore it.
pub fn check(target: &Profile, guards: &[&Profile], avoid: &[String], draft_text: &str, kind: ItemKind) -> Checked {
    if target.register.is_short_form() {
        check_short(target, guards, avoid, draft_text, kind)
    } else {
        check_long(target, guards, avoid, draft_text)
    }
}

fn check_long(target: &Profile, guards: &[&Profile], avoid: &[String], draft_text: &str) -> Checked {
    let d = Draft::of(draft_text);
    let words = d.tokens.len().max(1);
    let lx = lexis(target);
    let fw = metrics::top_function_words(target, metrics::FUNCTION_WORDS_TRACKED);
    let mut warnings = Vec::new();
    let w = windowed(target, &d.prose(), words, &fw);
    let (values, ranges, rows) = (&w.values, &w.ranges, &w.rows);
    let at = |k: &str| ranges.defs.iter().position(|d| d.key == k).unwrap_or(0);
    let (distance, score, holdout) = (w.distance, w.score, w.holdout);
    if score.is_none() {
        warnings.push(format!(
            "the author's text has {} windows of {words} words: too few to score a draft this long; check a shorter section, or add more of the author's writing",
            w.windows
        ));
    }
    let (families, n_measured) = measured_families(&w);
    let basis = ScoreBasis {
        unit: "windows",
        compared: w.compared,
        window_words: words,
        holdout,
        kind: None,
        mode: w.mode,
        metrics: n_measured,
        families,
        parts: None,
        note: format!(
            "percentile of the draft's distance among {} of the author's own {words}-word windows (half-overlapping), each judged against bands without its own {}; 50 = as typical as the author's median window",
            w.compared,
            if holdout == "source" { "work" } else { "half of its work" }
        ),
    };

    let mut hits = copy_hits(&d, &{
        let mut g = vec![target];
        g.extend(guards.iter().copied());
        g
    }, COPY_NGRAM, &long_copy_verdict);
    hits.extend(avoid_hits(&d, avoid, &lx.concepts));
    hits.extend(typography_hits(&d, &metrics::typography_habits(target), true));
    hits.extend(metric_hits(rows, &ranges.defs, words));
    hits.extend(tell_hits(&d, &lx.rates, &lx.concepts, words < SHORT_DRAFT_WORDS));
    hits.extend(with_evidence(word_hits(&d, target, &lx.concepts, &lx.avoided)));
    hits.extend(structure_hits(&d, rows));
    // Tense drift: a long draft that sits between the author's modes.
    // Only when the author keeps to one tense per stretch: a dual-timeline
    // author's own windows straddle modes, and a mixed draft is normal there.
    let present = values[at("present_tense_share")];
    if target.register == Register::Fiction && words >= 800 && present > 0.3 && present < 0.6 && w.author_mixed < 0.1 {
        let mut h = Hit::new(
            "tense_drift",
            "present_tense_share",
            None,
            format!("the narration mixes tenses: {} of its verbs are present tense", pct(present)),
            "Pick one tense for this chapter (the author's sections are present at 60%+ or past at 70%+) and convert the rest".into(),
        );
        h.severity = 150.0;
        hits.push(h);
    }
    let mut findings = group(hits);
    drop_shadowed(&mut findings);
    sort_findings(&mut findings);

    let counts = counts_of(&d, words);
    let mut steps = finding_steps(&findings);
    steps.extend(metric_plan(rows, &ranges.defs, &d, &counts, words, &ranges.bands, target.register));
    let plan = finish_plan(steps);

    // Segments: where a long draft drifts.
    let mut segments = Vec::new();
    if words >= 2 * SEGMENT_WORDS {
        let seg = metrics::ranges(target, SEGMENT_WORDS, &fw);
        let mut cur: Vec<String> = Vec::new();
        let mut first = 0;
        let mut n = 0;
        let paras = &d.paragraphs;
        for (i, (p, plines, start)) in paras.iter().enumerate() {
            if cur.is_empty() {
                first = *start;
            }
            cur.push(p.clone());
            n += text::count_words(p);
            let last = i + 1 == paras.len();
            if n >= SEGMENT_WORDS || last {
                if n * 2 >= SEGMENT_WORDS || segments.is_empty() {
                    let v = metrics::measure(&cur, &fw);
                    let out: Vec<String> = rows_for(&v, &seg.bands, n)
                        .into_iter()
                        .filter(|r| r.status == Status::OutOfRange && actionable(r) && r.key != "opening_paragraph_words")
                        .map(|r| r.key)
                        .collect();
                    segments.push(Segment { lines: [first + 1, start + plines.len()], words: n, out_of_range: out });
                }
                cur.clear();
                n = 0;
            }
        }
    }
    if words < 300 {
        warnings.push(format!("{words} words is short: rates swing a lot at this length; the score is directional"));
    }
    Checked {
        register: target.register,
        score,
        distance: (distance * 1000.0).round() / 1000.0,
        reliability: reliability(w.compared, words, false),
        basis,
        findings,
        plan,
        rows: rows.clone(),
        segments,
        habits: Vec::new(),
        items: Vec::new(),
        batch: None,
        warnings,
    }
}

// ── Short-form ───────────────────────────────────────────────────────────────

fn spread(v: &mut [f64]) -> [f64; 2] {
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |x: f64| v[((v.len() - 1) as f64 * x).round() as usize];
    if v.is_empty() { [0.0, 0.0] } else { [q(0.25), q(0.75)] }
}

fn mix(items: &[&ItemMeasure], f: impl Fn(&ItemMeasure) -> &'static str) -> Vec<(String, f64)> {
    let mut m: BTreeMap<&'static str, f64> = BTreeMap::new();
    for i in items {
        *m.entry(f(i)).or_insert(0.0) += 1.0;
    }
    let mut v: Vec<(String, f64)> = m.into_iter().map(|(k, c)| (k.to_string(), (c / items.len() as f64 * 1000.0).round() / 1000.0)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

fn habit_value(feature: &str, m: &ItemMeasure) -> serde_json::Value {
    match feature {
        "ending" => m.ending.into(),
        "layout" => m.layout.into(),
        "lowercase_start" => (m.get("lowercase_start_share") > 0.0).into(),
        "greeting" => m.greeting.clone().map(Into::into).unwrap_or(serde_json::Value::Null),
        "signoff" => m.signoff.clone().map(Into::into).unwrap_or(serde_json::Value::Null),
        k => m.get(k).into(),
    }
}

/// A short-form item as paragraphs, the way profiles store items: blank
/// lines split, line breaks inside a paragraph kept.
fn item_paragraphs(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in t.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line.trim_end());
        }
    }
    out
}

fn check_short(target: &Profile, guards: &[&Profile], avoid: &[String], draft_text: &str, kind: ItemKind) -> Checked {
    let mut d = Draft::of(draft_text);
    d.short_form = true;
    let words = d.tokens.len().max(1);
    let lx = lexis(target);
    let mut warnings = Vec::new();
    // A draft that opens with @handles is a reply, whatever the default says.
    let kind = if kind == ItemKind::Post && draft_text.trim_start().starts_with('@') {
        warnings.push("the draft starts with an @handle: compared with the author's replies (pass --as quote or --as thread_part for those)".into());
        ItemKind::Reply
    } else {
        kind
    };
    let (author, weights, fallback) = metrics::author_items(target, kind);
    if fallback {
        warnings.push(format!(
            "fewer than 20 of the author's items are {}s: compared with all {} items",
            serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            author.len()
        ));
    }
    let email = matches!(target.register, Register::Email | Register::Message);
    let model = ItemModel::new(&author, &weights, email);
    let iranges = metrics::item_ranges_of(&author, &weights, fallback);
    let ahabits = metrics::habits_of(&author, &weights);
    let vocab = metrics::lowercase_vocab(target);
    let items = draft_items(draft_text);
    let measured: Vec<ItemMeasure> = items
        .iter()
        .map(|(t, _)| metrics::measure_item(if kind == ItemKind::Reply { metrics::strip_leading_handles(t) } else { t }, &vocab))
        .collect();
    let item_scores: Vec<ItemScore> = items
        .iter()
        .zip(&measured)
        .enumerate()
        .map(|(i, ((_, lines), m))| ItemScore {
            item: i + 1,
            lines: *lines,
            words: m.get("item_words") as usize,
            score: (model.score(m) * 10.0).round() / 10.0,
        })
        .collect();
    let item_score = (!item_scores.is_empty() && author.len() >= 3)
        .then(|| (item_scores.iter().map(|s| s.score).sum::<f64>() / item_scores.len() as f64 * 10.0).round() / 10.0);
    // 150+ words (an email, an X Article, a batch of posts): the items
    // pooled are also a window of the author's text, judged on rhythm,
    // punctuation, lexis and function words as a long-form draft is.
    let win = (words >= SHORT_DRAFT_WORDS).then(|| {
        let fw = metrics::top_function_words(target, metrics::FUNCTION_WORDS_TRACKED);
        let paras: Vec<String> = items
            .iter()
            .flat_map(|(t, _)| item_paragraphs(if kind == ItemKind::Reply { metrics::strip_leading_handles(t) } else { t }))
            .collect();
        windowed(target, &paras, words, &fw)
    });
    let score = match (item_score, win.as_ref().and_then(|w| w.score.map(|s| (s, w.compared)))) {
        (Some(i), Some((ws, n))) => Some(combine(i, author.len(), ws, n)),
        (None, Some((ws, _))) => Some(ws),
        (i, None) => i,
    };
    if let Some(w) = win.as_ref().filter(|w| w.score.is_none()) {
        warnings.push(format!(
            "the author's items give {} windows of {words} words: too few to score the draft's rates, so the score is its item habits alone",
            w.windows
        ));
    }
    let distance = if measured.is_empty() {
        0.0
    } else {
        measured.iter().map(|m| model.surprisal(m)).sum::<f64>() / measured.len() as f64
    };
    let multi = measured.len() > 1;

    // Rows: the item measures (one item: its values; several: their medians).
    let values: Vec<f64> = (0..metrics::item_defs().len())
        .map(|j| {
            let mut v: Vec<f64> = measured.iter().map(|m| m.values[j]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v.get(v.len() / 2).copied().unwrap_or(0.0)
        })
        .collect();
    let mut rows = rows_for(&values, &iranges.bands, 0);
    for r in &mut rows {
        // A flag's "band" is the author's share: the draft is in range if
        // the author does it at least sometimes (or never does it and
        // neither does the draft).
        if iranges.bands.iter().any(|b| b.key == r.key && b.unit == Unit::Share) {
            let rare = (r.value > 0.5 && r.median < 0.05) || (r.value < 0.5 && r.median > 0.95);
            r.status = if rare { Status::OutOfRange } else { Status::InRange };
        }
    }
    if let Some(w) = &win {
        rows.extend(w.rows.iter().cloned());
    }

    let mut hits = Vec::new();
    // Never-list and X length: hard rules an agent can apply without judgement.
    let long_share = author.iter().filter(|m| m.get("item_chars") > X_LIMIT as f64).count() as f64 / author.len().max(1) as f64;
    for ((_, lines), m) in items.iter().zip(&measured) {
        for (key, label, shown) in metrics::never_flags(m) {
            if !shown {
                continue;
            }
            if let Some(nh) = ahabits.never.iter().find(|n| n.key == key) {
                let mut h = Hit::new(
                    "never",
                    key,
                    Some(lines[0]),
                    format!("the author never uses {label}: {} of {} {}s", nh.count, nh.of, kind_word(kind)),
                    if key == "stock_phrases" {
                        "Cut the stock phrases the llm_tell findings name and say it in the author's own words".into()
                    } else {
                        format!("Remove the {label}")
                    },
                );
                h.hard = true;
                h.severity = 800.0;
                hits.push(h);
            }
        }
        let chars = m.get("item_chars") as usize;
        if target.register == Register::Social && chars > X_LIMIT && long_share < 0.05 {
            let mut h = Hit::new(
                "length",
                "item_chars",
                Some(lines[0]),
                format!("{chars} characters as X counts them (links 23, emoji 2): over the {X_LIMIT} limit, and the author's posts never run longer"),
                format!("Cut at least {} characters", chars - X_LIMIT),
            );
            h.hard = true;
            h.severity = 850.0;
            hits.push(h);
        }
        // Habits the author rarely shows (but not never: those are hard above).
        for (fi, (f, share)) in metrics::ITEM_FEATURES.iter().zip(model.bin_shares(m)).enumerate() {
            // Stock phrases get llm_tell findings, which name the words.
            if matches!(*f, "item_words" | "first_line_words" | "llm_tells_per_item") || share >= 0.05 {
                continue;
            }
            let covered = metrics::never_flags(m).iter().any(|n| n.2 && ahabits.never.iter().any(|x| x.key == n.0) && habit_covers(f, n.0));
            if covered {
                continue;
            }
            let mut h = Hit::new(
                "habit",
                *f,
                Some(lines[0]),
                format!("{}: {} — the author's {}s are like this {}", f.replace('_', " "), habit_value(f, m), kind_word(kind), pct(share)),
                habit_advice(f, &ahabits, m, model.zero_share(fi)),
            );
            h.severity = 90.0 + (0.05 - share) * 200.0;
            hits.push(h);
        }
        // Length against the author's band.
        if let Some(b) = iranges.bands.iter().find(|b| b.key == "item_words") {
            let w = m.get("item_words");
            if metrics::status(w, b, 0) == Status::OutOfRange {
                let mut h = Hit::new(
                    "metric_out_of_range",
                    "item_words",
                    Some(lines[0]),
                    format!("{} words (the author's {}s run {}–{}, median {})", w, kind_word(kind), b.low, b.high, b.median),
                    if w > b.high { format!("Cut to about {} words", b.median) } else { format!("Write it out to about {} words", b.median) },
                );
                h.severity = 95.0;
                hits.push(h);
            }
        }
    }
    let mut guarded = vec![target];
    guarded.extend(guards.iter().copied());
    let item_words_at = |line: usize| {
        items.iter().zip(&measured).find(|((_, l), _)| line >= l[0] && line <= l[1]).map(|(_, m)| m.get("item_words")).unwrap_or(words as f64)
    };
    hits.extend(copy_hits(&d, &guarded, COPY_NGRAM_SHORT, &|r| Some(r.len >= 10 || r.len as f64 >= 0.5 * item_words_at(r.line))));
    hits.extend(avoid_hits(&d, avoid, &lx.concepts));
    hits.extend(typography_hits(&d, &metrics::typography_habits(target), false));
    hits.extend(tell_hits(&d, &lx.rates, &lx.concepts, true));
    hits.extend(with_evidence(word_hits(&d, target, &lx.concepts, &lx.avoided)));
    if let Some(w) = &win {
        // Below the item's own habits, which a reader sees first.
        hits.extend(metric_hits(&w.rows, &w.ranges.defs, words).into_iter().map(|mut h| {
            h.severity -= 15.0;
            h
        }));
        hits.extend(structure_hits(&d, &w.rows));
    }

    // A batch of posts should vary the way the author's do.
    let mut batch = None;
    if measured.len() >= 3 {
        let refs: Vec<&ItemMeasure> = measured.iter().collect();
        let arefs: Vec<&ItemMeasure> = author.iter().collect();
        let b = Batch {
            items: measured.len(),
            length_spread: spread(&mut measured.iter().map(|m| m.get("item_words")).collect::<Vec<_>>()),
            author_length_spread: spread(&mut author.iter().map(|m| m.get("item_words")).collect::<Vec<_>>()),
            layouts: mix(&refs, |m| m.layout),
            author_layouts: mix(&arefs, |m| m.layout),
            endings: mix(&refs, |m| m.ending),
            author_endings: mix(&arefs, |m| m.ending),
        };
        let iqr = b.length_spread[1] - b.length_spread[0];
        let aiqr = b.author_length_spread[1] - b.author_length_spread[0];
        if aiqr >= 4.0 && iqr < 0.4 * aiqr {
            let mut h = Hit::new(
                "batch",
                "length_spread",
                None,
                format!(
                    "the {} posts are all about the same length ({}–{} words); the author's middle half runs {}–{}",
                    b.items, num(b.length_spread[0]), num(b.length_spread[1]), num(b.author_length_spread[0]), num(b.author_length_spread[1])
                ),
                "Vary the lengths: make some much shorter and some longer, as the author does".into(),
            );
            h.severity = 85.0;
            hits.push(h);
        }
        for (name, ours, theirs) in [("layout", &b.layouts, &b.author_layouts), ("ending", &b.endings, &b.author_endings)] {
            let top = &ours[0];
            let author_share = theirs.iter().find(|t| t.0 == top.0).map(|t| t.1).unwrap_or(0.0);
            if top.1 - author_share > 0.4 {
                let mut h = Hit::new(
                    "batch",
                    format!("{name}_mix"),
                    None,
                    format!("{} of the posts share the {name} '{}'; the author's posts do {}", pct(top.1), top.0, pct(author_share)),
                    format!("Mix the {name}s like the author: {}", theirs.iter().take(3).map(|t| format!("{} {}", t.0, pct(t.1))).collect::<Vec<_>>().join(", ")),
                );
                h.severity = 80.0;
                hits.push(h);
            }
        }
        batch = Some(b);
    }
    let mut findings = group(hits);
    drop_shadowed(&mut findings);
    sort_findings(&mut findings);

    let habits: Vec<HabitRow> = measured
        .iter()
        .enumerate()
        .flat_map(|(i, m)| {
            let shares = model.bin_shares(m);
            metrics::ITEM_FEATURES
                .iter()
                .zip(shares)
                .filter(|(f, _)| !matches!(**f, "item_words" | "first_line_words"))
                .map(move |(f, s)| HabitRow {
                    item: multi.then_some(i + 1),
                    key: f,
                    draft_value: habit_value(f, m),
                    author_share: (s * 1000.0).round() / 1000.0,
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let mut steps = finding_steps(&findings);
    // Item findings become steps here; window rates get theirs from metric_plan.
    let item_step = |f: &&Finding| match f.kind {
        "habit" | "batch" => true,
        "metric_out_of_range" => metrics::item_index(&f.key).is_some(),
        _ => false,
    };
    for f in findings.iter().filter(|f| !f.hard).filter(item_step) {
        steps.push(PlanStep {
            action: f.suggestion.clone(),
            why: f.message.clone(),
            quantity: f.count.to_string(),
            lines: f.lines.clone(),
            hard: false,
            order: f.severity,
        });
    }
    if let Some(w) = &win {
        for mut s in metric_plan(&w.rows, &w.ranges.defs, &d, &counts_of(&d, words), words, &w.ranges.bands, target.register) {
            s.order = 85.0 + (s.order - 100.0).min(10.0);
            steps.push(s);
        }
    }
    let plan = finish_plan(steps);
    if words < SHORT_DRAFT_WORDS {
        warnings.push(format!(
            "{words} words: scored on post-level habits only (length, lines, emoji, tags, endings); window rates and voice_match need {SHORT_DRAFT_WORDS}+ words"
        ));
    }
    let mut families = vec!["items"];
    let mut n_metrics = model.features();
    if let Some(w) = &win {
        let (f, n) = measured_families(w);
        families.extend(f.into_iter().filter(|f| *f != "items"));
        n_metrics += n;
    }
    let mut note = format!(
        "{}percentile of how surprising the draft's habits (length, lines, emoji, tags, mentions, links, punctuation, casing, stock phrases, ending, layout) are to the author, among {} of their own {}s (each left out of its own comparison); 50 = as typical as their median {}",
        if multi { "mean over items of the " } else { "" },
        author.len(),
        kind_word(kind),
        kind_word(kind)
    );
    if let Some(w) = win.as_ref().filter(|w| w.score.is_some()) {
        note.push_str(&format!(
            "; combined (Fisher's method) with the percentile of the items' pooled rates (rhythm, punctuation, lexis, function words) among {} of the author's own {words}-word windows, each judged without its own {}: the score is the share of the author's own text at least this unusual on both counts",
            w.compared,
            if w.holdout == "source" { "quarter or 200-item block" } else { "half of the corpus" }
        ));
    }
    let basis = ScoreBasis {
        unit: "items",
        compared: author.len(),
        window_words: if win.is_some() { words } else { 0 },
        holdout: "item",
        kind: Some(kind),
        mode: None,
        metrics: n_metrics,
        families,
        parts: win.as_ref().map(|w| ScoreParts {
            items: item_score,
            windows: w.score,
            windows_compared: w.compared,
            window_distance: (w.distance * 1000.0).round() / 1000.0,
        }),
        note,
    };
    Checked {
        register: target.register,
        score,
        distance: (distance * 1000.0).round() / 1000.0,
        reliability: reliability(author.len(), words, true),
        basis,
        findings,
        plan,
        rows,
        segments: Vec::new(),
        habits,
        items: if multi { item_scores } else { Vec::new() },
        batch,
        warnings,
    }
}

fn kind_word(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Post => "post",
        ItemKind::Reply => "reply",
        ItemKind::ThreadPart => "thread part",
        ItemKind::Quote => "quote post",
    }
}

/// Whether a never-list rule already covers a habit feature.
fn habit_covers(feature: &str, never_key: &str) -> bool {
    matches!(
        (feature, never_key),
        ("hashtags_per_item", "hashtags")
            | ("emoji_per_item", "emoji")
            | ("links_per_item", "links")
            | ("exclaims_per_item", "exclamations")
            | ("all_caps_words_per_item", "emphasis_caps")
            | ("lowercase_start", "lowercase_start" | "capital_start")
            | ("item_lines", "line_breaks")
            | ("layout", "line_breaks")
            | ("ending", "exclamations" | "emoji" | "links")
            | ("llm_tells_per_item", "stock_phrases")
    )
}

fn habit_advice(feature: &str, h: &metrics::ItemHabits, m: &ItemMeasure, none_share: f64) -> String {
    let top = |v: &[(String, f64)]| v.iter().take(3).map(|(k, s)| format!("'{k}' {}", pct(*s))).collect::<Vec<_>>().join(", ");
    let noun = match feature {
        "emoji_per_item" => Some("emoji"),
        "hashtags_per_item" => Some("hashtags"),
        "mentions_per_item" => Some("@mentions"),
        "links_per_item" => Some("links"),
        "exclaims_per_item" => Some("exclamation marks"),
        "questions_per_item" => Some("questions"),
        "all_caps_words_per_item" => Some("CAPS words"),
        _ => None,
    };
    if let Some(noun) = noun {
        return if m.get(feature) > 0.0 && none_share >= 0.5 {
            format!("Cut the {noun}: {} of the author's items have none", pct(none_share))
        } else if m.get(feature) > 0.0 {
            format!("Use fewer {noun}: the author's items usually have at most one")
        } else {
            format!("Add {noun} the way the author does: {} of their items have some", pct(1.0 - none_share))
        };
    }
    match feature {
        "ending" => format!("End the way the author does: {}", top(&h.endings)),
        "layout" => format!("Lay it out the way the author does: {}", top(&h.layouts)),
        "item_lines" => "Match the author's line breaks (see layout)".into(),
        "lowercase_start" => "Match the author's casing of the first letter".into(),
        "emoji_per_item" => format!("Match the author's emoji use{}", if h.top_emojis.is_empty() { " (rare)".into() } else { format!(": {}", top(&h.top_emojis)) }),
        "hashtags_per_item" => "Match the author's hashtag use (usually none or one)".into(),
        "greeting" => format!("Open the way the author does: {}", top(&h.greetings)),
        "signoff" => format!("Sign off the way the author does: {}", top(&h.signoffs)),
        f => format!("Bring {} to what the author usually does", f.replace('_', " ")),
    }
}

/// Finding counts by kind, every kind present (zero when absent).
pub fn counts(findings: &[Finding]) -> BTreeMap<&'static str, usize> {
    let mut m: BTreeMap<&'static str, usize> = [
        "copy_overlap", "avoid_list", "never", "length", "typography", "metric_out_of_range", "habit", "llm_tell",
        "off_voice_word", "synonym_variation", "structure", "tense_drift", "batch",
    ]
    .into_iter()
    .map(|k| (k, 0))
    .collect();
    for f in findings {
        *m.entry(f.kind).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_anchors_tokens_and_paragraphs_to_lines() {
        let d = Draft::of("# Title\n\nFirst line of prose\ncontinues here.\n\nSecond para.");
        assert_eq!(d.paragraphs.len(), 3, "heading kept as a paragraph for structure");
        assert_eq!(d.paragraphs[1].2, 2);
        assert_eq!(d.line_of(1, 25), 4);
        let i = d.tokens.iter().position(|t| t == "continues").unwrap();
        assert_eq!(d.token_line[i], 3);
        assert!(!d.tokens.contains(&"title".to_string()), "heading words are not prose");
    }

    #[test]
    fn link_targets_and_urls_stay_out_of_the_word_stream() {
        let d = Draft::of("See [the post](https://example.com/some-path) and https://t.co/xyz now.");
        assert_eq!(d.tokens, vec!["see", "the", "post", "and", "now"]);
    }

    #[test]
    fn excerpt_keeps_short_lines_whole() {
        assert_eq!(excerpt("short line", "line"), "short line");
        let long = "word ".repeat(60) + "needle " + &"word ".repeat(60);
        let e = excerpt(&long, "needle");
        assert!(e.contains("needle") && e.chars().count() < 150);
    }

    #[test]
    fn a_draft_is_one_item_unless_separated() {
        assert_eq!(draft_items("Hook line here\n\nBody of the post\n\nCTA").len(), 1);
        let two = draft_items("first post words\n---\nsecond post words\n");
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].1, [3, 3]);
    }

    #[test]
    fn grouping_merges_repeats_into_one_finding() {
        let mk = |line| {
            let mut h = Hit::new("off_voice_word", "gazed", Some(line), "m".into(), "s".into());
            h.excerpt = format!("line {line}");
            h
        };
        let g = group(vec![mk(4), mk(19), mk(4), mk(30), mk(41)]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].count, 5);
        assert_eq!(g[0].lines, vec![4, 19, 30, 41]);
        assert_eq!(g[0].excerpts.len(), 3);
    }

    #[test]
    fn a_separator_line_ends_a_paragraph() {
        let d = Draft::of("Post one ends here. Short.\n---\nShort again. And again.");
        assert_eq!(d.paragraphs.len(), 2, "{:?}", d.paragraphs);
        assert_eq!(d.paragraphs[1].2, 2);
    }
}
