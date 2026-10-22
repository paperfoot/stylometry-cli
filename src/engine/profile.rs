//! An author profile: the stored, on-disk representation of one person's
//! writing, built by *analysing* (fingerprinting, not training) a corpus.
//!
//! A profile keeps the corpus as paragraph-aligned chunks of about
//! `BASE_CHUNK_WORDS` words, each with its text, its feature counts, and its
//! style counts. Longer analysis lengths are made by summing consecutive base
//! chunks of one source, so calibration can work at the length of the text
//! being judged. Keeping the text makes a profile self-contained: it can be
//! re-analysed without the original files.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::engine::features;
use crate::engine::lexicon;
use crate::engine::style::StyleCounts;
use crate::engine::text::{self, Doc};
use crate::error::AppError;

/// Current on-disk schema. Profiles from older schemas must be rebuilt.
pub const SCHEMA: u32 = 3;

/// Words per base chunk. Analysis at longer lengths sums consecutive chunks.
pub const BASE_CHUNK_WORDS: usize = 250;

/// Minimum base chunks for a usable profile (need spread to estimate variance).
pub const MIN_CHUNKS: usize = 8;

/// Per-chunk feature counts keep only the profile's most frequent words and
/// trigrams: the shared Delta vocabulary (a few hundred features across all
/// profiles) always falls inside these, and the full text is kept anyway.
const CHUNK_WORDS_KEEP: usize = 2000;
const CHUNK_TRIGRAMS_KEEP: usize = 3000;
/// Aggregate trigram counts kept (word counts are kept in full).
const AGG_TRIGRAMS_KEEP: usize = 10000;
/// Word bigrams/trigrams kept for phrase analysis (count >= 3, most frequent).
const PHRASES_KEEP: usize = 20000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// A voice you analyse, imitate, or verify against.
    #[default]
    Author,
    /// A reference author: used as an imposter and as "writers in general",
    /// hidden from `profile list` unless `--all`.
    Background,
    /// A target voice made from weighted authors (e.g. 60% one, 40% another).
    /// Its passages carry weights; it is a style to aim at, not an author, so
    /// it is never calibrated or used as an imposter.
    Blend,
}

/// What kind of writing a profile holds. It decides the unit of analysis
/// (passages for long-form, one item per post/email/message for short-form),
/// which measures lead, and which brief template `guide` writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Register {
    #[default]
    Fiction,
    Nonfiction,
    Blog,
    /// X/Threads/LinkedIn posts and similar.
    Social,
    Email,
    /// Chat and text messages.
    Message,
}

impl Register {
    pub fn is_short_form(self) -> bool {
        matches!(self, Register::Social | Register::Email | Register::Message)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Register::Fiction => "fiction",
            Register::Nonfiction => "nonfiction",
            Register::Blog => "blog",
            Register::Social => "social",
            Register::Email => "email",
            Register::Message => "message",
        }
    }
}

/// What a short-form item is in its conversation. Replies, thread parts
/// and standalone posts are written differently (replies are short and
/// start mid-thought; thread parts carry an argument; posts have hooks), so
/// ranges, habits and examples are computed per kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    #[default]
    Post,
    Reply,
    /// A self-reply continuing the author's own thread.
    ThreadPart,
    Quote,
}

/// Metadata a short-form item may carry from its source export.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The item this one replies to, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub kind: ItemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub likes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reposts: Option<f64>,
    /// None when the source doesn't report impressions (0 is treated as unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impressions: Option<f64>,
    /// ISO-8601 date when parseable, else the source's string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The post carried media (image/video): its engagement isn't the text's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub media: bool,
}

impl ItemMeta {
    pub fn is_reply(&self) -> bool {
        self.kind == ItemKind::Reply
    }
}

/// Below this many within-work windows, `windows` pools across works.
pub const MIN_WINDOWS: usize = 20;

/// Minimum items for a short-form profile.
pub const MIN_ITEMS: usize = 20;

/// One source file of the corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub words: u64,
}

/// One paragraph-aligned passage of about `BASE_CHUNK_WORDS` words.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Index into `Profile::sources`.
    pub source: u32,
    /// Paragraphs, verbatim (after cleaning).
    pub paragraphs: Vec<String>,
    /// Word counts, restricted to the profile's top `CHUNK_WORDS_KEEP` words.
    pub words: HashMap<String, u32>,
    /// Total word tokens in the chunk (all words, not just kept ones).
    pub n_words: u32,
    /// Character-trigram counts, restricted to the top `CHUNK_TRIGRAMS_KEEP`.
    pub trigrams: HashMap<String, u32>,
    /// Total trigram tokens in the chunk.
    pub n_trigrams: u32,
    pub style: StyleCounts,
    /// Weight of this passage in per-passage statistics (percentile ranges,
    /// weighted means). 1.0 for ordinary profiles; in a blend, scaled so each
    /// component's passages sum to its share of the blend.
    #[serde(default = "unit_weight")]
    pub weight: f32,
    /// Short-form only: the item's source metadata (kind, engagement, date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ItemMeta>,
    /// Long-form only: index of the chapter/section within its source, so
    /// per-section modes (POV, tense) can be told apart in dual narratives.
    #[serde(default)]
    pub section: u32,
}

fn unit_weight() -> f32 {
    1.0
}

impl Chunk {
    pub fn text(&self) -> String {
        self.paragraphs.join("\n\n")
    }
}

/// Logistic mapping from a Cosine-Delta value to P(same author) at one
/// analysis length, plus the verification quality measured there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandCalibration {
    /// Analysis length in words (a multiple of BASE_CHUNK_WORDS).
    pub words: usize,
    /// P(same) = sigmoid(intercept + slope * delta). slope is negative.
    pub slope: f64,
    pub intercept: f64,
    pub auc: f64,
    #[serde(default)]
    pub holdout_accuracy: Option<f64>,
    #[serde(default)]
    pub holdout_brier: Option<f64>,
    #[serde(default)]
    pub c_at_1: Option<f64>,
    pub positives: usize,
    pub negatives: usize,
    /// The exact reference model (vocab + mean/sd) this band was fit in,
    /// frozen so scoring never re-fits as the profile set changes.
    pub reference: crate::engine::model::ReferenceModel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    pub bands: Vec<BandCalibration>,
    /// How many imposter profiles were used as negatives.
    pub imposters: usize,
    /// True when positives were scored against centroids that excluded the
    /// positive's whole source work (cross-work), not just the chunk.
    pub cross_work: bool,
    /// Signature of the profile set at calibration time; a mismatch means
    /// the imposter pool has drifted since (re-calibrate to account for it).
    pub ref_signature: String,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub schema: u32,
    pub name: String,
    #[serde(default)]
    pub kind: ProfileKind,
    /// Kind of writing; profiles from before registers get one detected on load.
    #[serde(default)]
    pub register: Register,
    /// How the register was chosen ("--register", "X archive", "dialogue 23%"...).
    #[serde(default)]
    pub register_reason: String,
    pub created: u64,
    pub sources: Vec<Source>,
    pub n_tokens: u64,
    pub chunks: Vec<Chunk>,
    /// Full vocabulary: every lowercased word and its count.
    pub word_counts: BTreeMap<String, u64>,
    /// In how many base chunks each word appears (dispersion: a word used
    /// everywhere is a habit; one used in a single scene is topic).
    pub word_chunks: BTreeMap<String, u32>,
    /// Aggregate character-trigram counts (top entries only).
    pub trigram_counts: HashMap<String, u64>,
    /// Word bigrams and trigrams (within sentences) seen >= 3 times.
    pub phrase_counts: BTreeMap<String, u32>,
    /// Lowercased words that are almost always capitalized mid-sentence:
    /// names of people and places. Excluded from vocabulary analysis and
    /// from the Delta feature set, since they are topic, not style.
    pub proper_nouns: Vec<String>,
    /// Sentence openers: first word of each sentence, lowercased.
    pub openers: BTreeMap<String, u32>,
    /// Lengths in words of every sentence, in corpus order.
    pub sentence_lengths: Vec<u16>,
    /// For a blend: (component profile, share), shares summing to 1.
    #[serde(default)]
    pub components: Vec<(String, f64)>,
    pub calibration: Option<Calibration>,
}

impl Profile {
    /// Build a profile from a corpus of documents. Errors if the corpus is
    /// too small to yield `MIN_CHUNKS` base chunks.
    pub fn build(name: &str, docs: &[Doc], kind: ProfileKind) -> Result<Profile, AppError> {
        Profile::build_with(name, docs, kind, None, MIN_CHUNKS)
    }

    /// `build` with a custom minimum passage count. Ad-hoc analysis of a
    /// single draft (never saved, never calibrated) accepts one passage.
    pub fn build_min(
        name: &str,
        docs: &[Doc],
        kind: ProfileKind,
        min_chunks: usize,
    ) -> Result<Profile, AppError> {
        Profile::build_with(name, docs, kind, None, min_chunks)
    }

    /// Build with an explicit register (or `None` to detect one). Long-form
    /// registers cut ~250-word paragraph-aligned passages; short-form ones
    /// keep one chunk per item (post, email, message), verbatim.
    pub fn build_with(
        name: &str,
        docs: &[Doc],
        kind: ProfileKind,
        register: Option<(Register, String)>,
        min_chunks: usize,
    ) -> Result<Profile, AppError> {
        let (register, register_reason) = register.unwrap_or_else(|| detect_register(docs, None));
        let mut sources = Vec::new();
        let mut raw_chunks: Vec<(u32, Vec<String>, Origin)> = Vec::new();
        for d in docs {
            if register.is_short_form() {
                let items = if d.items.is_empty() {
                    crate::engine::import::split_items_for(&d.text, register)
                } else {
                    d.items.clone()
                };
                let items: Vec<_> = items.into_iter().filter(|i| text::count_words(&i.text) >= 3).collect();
                let words: usize = items.iter().map(|i| text::count_words(&i.text)).sum();
                if words == 0 {
                    continue;
                }
                // Sources are time blocks (calendar quarters, or runs of 200
                // undated items) so calibration can hold out whole periods
                // rather than one giant "work" per archive file.
                for (block, group) in time_blocks(items, &d.name) {
                    let words: usize = group.iter().map(|i| text::count_words(&i.text)).sum();
                    let idx = match sources.iter().position(|s: &Source| s.name == block) {
                        Some(i) => {
                            sources[i].words += words as u64;
                            i as u32
                        }
                        None => {
                            sources.push(Source { name: block, words: words as u64 });
                            (sources.len() - 1) as u32
                        }
                    };
                    for it in group {
                        raw_chunks.push((idx, item_paragraphs(&it.text), Origin::Item(it.meta)));
                    }
                }
                let _ = (&d.name, words);
                continue;
            }
            // Long-form: a structured export of long items (blog posts in a
            // JSON/CSV) makes each item its own work.
            let works: Vec<(String, String)> = if d.items.is_empty() {
                vec![(d.name.clone(), d.text.clone())]
            } else {
                d.items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| (format!("{}#{}", d.name, i + 1), it.text.clone()))
                    .collect()
            };
            for (wname, wtext) in works {
                let sections: Vec<Vec<String>> = split_sections(&wtext)
                    .iter()
                    .map(|sec| text::paragraphs(sec))
                    .filter(|p| !p.is_empty())
                    .collect();
                let words: usize = sections.iter().flatten().map(|p| text::count_words(p)).sum();
                if words == 0 {
                    continue;
                }
                let idx = sources.len() as u32;
                sources.push(Source {
                    name: wname,
                    words: words as u64,
                });
                for (si, paras) in sections.iter().enumerate() {
                    for ch in text::chunk_paragraphs(paras, BASE_CHUNK_WORDS) {
                        raw_chunks.push((idx, ch, Origin::Section(si as u32)));
                    }
                }
            }
        }
        // Ad-hoc analysis (min 1) accepts one item; a saved short-form
        // profile needs enough items for per-item ranges.
        let min = match (register.is_short_form(), min_chunks <= 1) {
            (_, true) => 1,
            (true, false) => MIN_ITEMS,
            (false, false) => min_chunks,
        };
        if raw_chunks.len() < min {
            let (unit, hint) = if register.is_short_form() {
                ("item(s)", "Add more posts/messages: an X archive (tweets.js), xmaster --json output, JSONL/CSV, or a text file with items separated by --- lines")
            } else {
                ("passage(s) of ~250 words", "Add more of the author's writing to the corpus (a folder of .md/.txt files works)")
            };
            return Err(AppError::input(
                format!(
                    "corpus too small for {} writing: {} {unit}, need at least {min}",
                    register.as_str(),
                    raw_chunks.len()
                ),
                hint,
            ));
        }

        // Pass 1: full counts per chunk plus corpus-wide tallies.
        let mut word_counts: HashMap<String, u64> = HashMap::new();
        let mut word_chunks: HashMap<String, u32> = HashMap::new();
        let mut trigram_counts: HashMap<String, u64> = HashMap::new();
        let mut phrase_counts: HashMap<String, u32> = HashMap::new();
        let mut openers: HashMap<String, u32> = HashMap::new();
        let mut sentence_lengths: Vec<u16> = Vec::new();
        // Proper-noun evidence: (mid-sentence capitalized, mid-sentence total).
        let mut caps: HashMap<String, (u32, u32)> = HashMap::new();

        struct Full {
            source: u32,
            paragraphs: Vec<String>,
            wc: HashMap<String, u64>,
            tc: HashMap<String, u64>,
            style: StyleCounts,
        }
        let mut full: Vec<Full> = Vec::with_capacity(raw_chunks.len());
        let mut metas: Vec<Origin> = Vec::with_capacity(raw_chunks.len());
        for (source, paragraphs, origin) in raw_chunks {
            metas.push(origin);
            let joined = paragraphs.join("\n\n");
            let wc = features::word_counts(&joined);
            let tc = features::trigram_counts(&joined);
            for (k, v) in &wc {
                *word_counts.entry(k.clone()).or_insert(0) += v;
                *word_chunks.entry(k.clone()).or_insert(0) += 1;
            }
            for (k, v) in &tc {
                *trigram_counts.entry(k.clone()).or_insert(0) += v;
            }
            for p in &paragraphs {
                for s in text::sentences(p) {
                    let cased = text::word_tokens_cased(&s);
                    if cased.is_empty() {
                        continue;
                    }
                    sentence_lengths.push(cased.len().min(u16::MAX as usize) as u16);
                    *openers.entry(cased[0].to_lowercase()).or_insert(0) += 1;
                    // Title-case lines (headings, contents lists, sign text)
                    // capitalize common words; they are no evidence of names.
                    let capped = cased
                        .iter()
                        .filter(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
                        .count();
                    if cased.len() < 4 || capped * 10 <= cased.len() * 4 {
                        for w in cased.iter().skip(1) {
                            let lw = w.to_lowercase();
                            let e = caps.entry(lw).or_insert((0, 0));
                            e.1 += 1;
                            if w.chars().next().is_some_and(|c| c.is_uppercase()) {
                                e.0 += 1;
                            }
                        }
                    }
                    let lower: Vec<String> = cased.iter().map(|w| w.to_lowercase()).collect();
                    for n in 2..=3 {
                        for win in lower.windows(n) {
                            *phrase_counts.entry(win.join(" ")).or_insert(0) += 1;
                        }
                    }
                }
            }
            let style = StyleCounts::of_paragraphs(&paragraphs);
            full.push(Full {
                source,
                paragraphs,
                wc,
                tc,
                style,
            });
        }

        let mut proper_nouns: Vec<String> = caps
            .into_iter()
            .filter(|(w, (c, t))| {
                // Common English words need near-unanimous capitalization
                // ("Bill", "Amber", "Will" are names only if always capped).
                let common = lexicon::zipf(w).is_some_and(|z| z >= 4.0);
                let (min_n, min_ratio) = if common { (5, 0.95) } else { (3, 0.8) };
                *t >= min_n
                    && (*c as f64) / (*t as f64) >= min_ratio
                    && w != "i"
                    && !w.starts_with("i'")
                    && w.chars().any(|ch| ch.is_alphabetic())
            })
            .map(|(w, _)| w)
            .collect();
        proper_nouns.sort();

        // Pass 2: restrict per-chunk counts to the profile's frequent features.
        let keep_words: HashSet<String> = top_keys(&word_counts, CHUNK_WORDS_KEEP);
        let keep_tri: HashSet<String> = top_keys(&trigram_counts, CHUNK_TRIGRAMS_KEEP);
        let chunks: Vec<Chunk> = full
            .into_iter()
            .zip(metas)
            .map(|(f, origin)| Chunk {
                source: f.source,
                n_words: features::total(&f.wc) as u32,
                n_trigrams: features::total(&f.tc) as u32,
                words: f
                    .wc
                    .into_iter()
                    .filter(|(k, _)| keep_words.contains(k))
                    .map(|(k, v)| (k, v as u32))
                    .collect(),
                trigrams: f
                    .tc
                    .into_iter()
                    .filter(|(k, _)| keep_tri.contains(k))
                    .map(|(k, v)| (k, v as u32))
                    .collect(),
                paragraphs: f.paragraphs,
                style: f.style,
                weight: 1.0,
                section: match &origin {
                    Origin::Section(s) => *s,
                    Origin::Item(_) => 0,
                },
                meta: match origin {
                    Origin::Item(m) => Some(m),
                    Origin::Section(_) => None,
                },
            })
            .collect();

        let n_tokens = word_counts.values().sum();
        let phrase_counts: BTreeMap<String, u32> = {
            let mut v: Vec<(String, u32)> =
                phrase_counts.into_iter().filter(|(_, c)| *c >= 3).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            v.truncate(PHRASES_KEEP);
            v.into_iter().collect()
        };

        Ok(Profile {
            schema: SCHEMA,
            name: name.to_string(),
            kind,
            register,
            register_reason,
            created: now_unix(),
            sources,
            n_tokens,
            chunks,
            word_counts: word_counts.into_iter().collect(),
            word_chunks: word_chunks.into_iter().collect(),
            trigram_counts: features::prune_top(trigram_counts, AGG_TRIGRAMS_KEEP),
            phrase_counts,
            proper_nouns,
            openers: openers.into_iter().collect(),
            sentence_lengths,
            components: Vec::new(),
            calibration: None,
        })
    }

    /// Blend weighted profiles into one target voice. Shares are normalized
    /// to sum to 1. Passages keep their text and counts and get a weight so
    /// each component's passages sum to its share (times the total passage
    /// count, so unweighted code still sees comparable magnitudes). Corpus
    /// aggregates become weighted pseudo-counts: each component's counts are
    /// rescaled so its share of the blend's tokens equals its share.
    pub fn blend(name: &str, parts: &[(Profile, f64)]) -> Result<Profile, AppError> {
        let total_share: f64 = parts.iter().map(|(_, w)| *w).sum();
        if parts.len() < 2 || total_share <= 0.0 || parts.iter().any(|(_, w)| *w <= 0.0) {
            return Err(AppError::input(
                "a blend needs at least two profiles with positive shares",
                format!(
                    "Example: {} profile blend {name} --from hoover=0.6 --from fisher=0.4",
                    env!("CARGO_BIN_NAME")
                ),
            ));
        }
        if let Some((p, _)) = parts.iter().find(|(p, _)| p.kind == ProfileKind::Blend) {
            return Err(AppError::input(
                format!("'{}' is itself a blend; blend its components instead", p.name),
                format!("Inspect it with: {} profile show {}", env!("CARGO_BIN_NAME"), p.name),
            ));
        }
        let total_tokens: f64 = parts.iter().map(|(p, _)| p.n_tokens as f64).sum();
        let total_chunks: f64 = parts.iter().map(|(p, _)| p.chunks.len() as f64).sum();

        let mut out = Profile {
            schema: SCHEMA,
            name: name.to_string(),
            kind: ProfileKind::Blend,
            register: parts[0].0.register,
            register_reason: format!("blend of {}", parts.iter().map(|(p, _)| p.register.as_str()).collect::<Vec<_>>().join(" + ")),
            created: now_unix(),
            sources: Vec::new(),
            n_tokens: 0,
            chunks: Vec::new(),
            word_counts: BTreeMap::new(),
            word_chunks: BTreeMap::new(),
            trigram_counts: HashMap::new(),
            phrase_counts: BTreeMap::new(),
            proper_nouns: Vec::new(),
            openers: BTreeMap::new(),
            sentence_lengths: Vec::new(),
            components: Vec::new(),
            calibration: None,
        };
        let mut names: HashSet<String> = HashSet::new();
        for (p, w) in parts {
            let share = w / total_share;
            out.components.push((p.name.clone(), share));
            let scale = share * total_tokens / (p.n_tokens.max(1) as f64);
            let chunk_weight = (share * total_chunks / p.chunks.len().max(1) as f64) as f32;
            let offset = out.sources.len() as u32;
            out.sources.extend(p.sources.iter().map(|s| Source {
                name: format!("{}/{}", p.name, s.name),
                words: s.words,
            }));
            out.chunks.extend(p.chunks.iter().map(|c| {
                let mut c = c.clone();
                c.source += offset;
                c.weight *= chunk_weight;
                c
            }));
            let scaled = |v: u64| (v as f64 * scale).round() as u64;
            for (k, v) in &p.word_counts {
                *out.word_counts.entry(k.clone()).or_insert(0) += scaled(*v);
            }
            for (k, v) in &p.word_chunks {
                *out.word_chunks.entry(k.clone()).or_insert(0) += *v;
            }
            for (k, v) in &p.trigram_counts {
                *out.trigram_counts.entry(k.clone()).or_insert(0) += scaled(*v);
            }
            for (k, v) in &p.phrase_counts {
                *out.phrase_counts.entry(k.clone()).or_insert(0) += scaled(*v as u64) as u32;
            }
            for (k, v) in &p.openers {
                *out.openers.entry(k.clone()).or_insert(0) += scaled(*v as u64) as u32;
            }
            out.sentence_lengths.extend(&p.sentence_lengths);
            names.extend(p.proper_nouns.iter().cloned());
        }
        out.word_counts.retain(|_, v| *v > 0);
        out.n_tokens = out.word_counts.values().sum();
        out.proper_nouns = names.into_iter().collect();
        out.proper_nouns.sort();
        Ok(out)
    }

    pub fn is_proper_noun(&self, w: &str) -> bool {
        self.proper_nouns.binary_search_by(|p| p.as_str().cmp(w)).is_ok()
    }

    /// Style counts summed over the whole corpus.
    pub fn style_totals(&self) -> StyleCounts {
        let mut t = StyleCounts::default();
        for c in &self.chunks {
            t.add(&c.style);
        }
        t
    }

    /// Spans of `k` consecutive base chunks within one source (never across
    /// sources), non-overlapping, in corpus order. Each span is a list of
    /// chunk indices. A source with fewer than `k` chunks yields no span.
    pub fn spans(&self, k: usize) -> Vec<Vec<usize>> {
        let k = k.max(1);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.chunks.len() {
            let src = self.chunks[i].source;
            let mut j = i;
            while j < self.chunks.len() && self.chunks[j].source == src {
                j += 1;
            }
            let mut s = i;
            while s + k <= j {
                out.push((s..s + k).collect());
                s += k;
            }
            i = j;
        }
        out
    }

    /// Windows of at least `words` words: consecutive chunks grouped until
    /// they reach the length. Long-form windows stay inside one work;
    /// short-form windows pool consecutive items (a 1,000-word window of
    /// posts is ~40 posts). A trailing group under half the length is
    /// dropped. Use this, not `spans`, wherever a length is meant.
    pub fn windows(&self, words: usize) -> Vec<Vec<usize>> {
        if self.register.is_short_form() {
            return self.group_windows(words, true);
        }
        let within = self.group_windows(words, false);
        // Works shorter than the window (blog posts under a 1,500-word
        // draft) would leave almost nothing to compare against: pool
        // consecutive works instead.
        if within.len() < MIN_WINDOWS && self.sources.len() > 1 {
            let pooled = self.group_windows(words, true);
            if pooled.len() > within.len() {
                return pooled;
            }
        }
        within
    }

    fn group_windows(&self, words: usize, pool: bool) -> Vec<Vec<usize>> {
        let words = words.max(1) as u32;
        let mut out = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        let mut n = 0u32;
        for (i, c) in self.chunks.iter().enumerate() {
            if !pool && !cur.is_empty() && self.chunks[cur[0]].source != c.source {
                if n * 2 >= words {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                n = 0;
            }
            cur.push(i);
            n += c.n_words;
            if n >= words {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
        }
        if !cur.is_empty() && n * 2 >= words {
            out.push(cur);
        }
        out
    }

    /// Relative-frequency maps (words, trigrams) of a span of chunks.
    pub fn span_freqs(&self, span: &[usize]) -> (HashMap<String, f64>, HashMap<String, f64>) {
        let mut wc: HashMap<&str, u64> = HashMap::new();
        let mut tc: HashMap<&str, u64> = HashMap::new();
        let (mut wt, mut tt) = (0u64, 0u64);
        for &i in span {
            let c = &self.chunks[i];
            wt += c.n_words as u64;
            tt += c.n_trigrams as u64;
            for (k, v) in &c.words {
                *wc.entry(k.as_str()).or_insert(0) += *v as u64;
            }
            for (k, v) in &c.trigrams {
                *tc.entry(k.as_str()).or_insert(0) += *v as u64;
            }
        }
        (
            wc.into_iter()
                .map(|(k, v)| (k.to_string(), features::rel_freq(v, wt)))
                .collect(),
            tc.into_iter()
                .map(|(k, v)| (k.to_string(), features::rel_freq(v, tt)))
                .collect(),
        )
    }
}

/// Where a chunk came from: a short-form item, or a section of a long work.
enum Origin {
    Item(ItemMeta),
    Section(u32),
}

/// True for a chapter-style heading line: "# Chapter 3", "CHAPTER IV",
/// "Part Two", "Prologue", a bare "12." or "XII".
pub fn is_chapter_heading(line: &str) -> bool {
    let t = line.trim().trim_start_matches('#').trim();
    if t.is_empty() || t.split_whitespace().count() > 6 {
        return false;
    }
    let first = t.split_whitespace().next().unwrap_or("").to_lowercase();
    let first = first.trim_end_matches(['.', ':']);
    // Single "c"/"l" are initials far more often than numerals.
    let roman = !first.is_empty()
        && first.chars().all(|c| "ivxlc".contains(c))
        && !matches!(first, "c" | "l");
    let numeric = !first.is_empty() && first.chars().all(|c| c.is_ascii_digit());
    matches!(first, "chapter" | "part" | "book" | "prologue" | "epilogue" | "interlude")
        || ((roman || numeric) && t.split_whitespace().count() <= 2)
}

/// A standalone ALL-CAPS title line ("THE PILLAR-BOX"): how Gutenberg
/// essay collections and many novels head their sections.
fn is_caps_title(line: &str) -> bool {
    let t = line.trim().trim_start_matches('#').trim();
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3
        && t.split_whitespace().count() <= 8
        && t.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase())
        && !t.ends_with([',', ';'])
}

/// Split a work into sections at chapter headings and standalone title
/// lines. A heading must stand alone (blank line or edge on both sides), so
/// a stray "C." or a shouted line of dialogue mid-paragraph never splits.
fn split_sections(t: &str) -> Vec<String> {
    let lines: Vec<&str> = t.lines().collect();
    let blank = |i: Option<&&str>| i.is_none_or(|l| l.trim().is_empty());
    let mut out: Vec<String> = vec![String::new()];
    for (i, line) in lines.iter().enumerate() {
        let standalone = blank(if i == 0 { None } else { lines.get(i - 1) }) && blank(lines.get(i + 1));
        let heading = standalone && (is_chapter_heading(line) || is_caps_title(line));
        if heading && !out.last().unwrap().trim().is_empty() {
            out.push(String::new());
        }
        let cur = out.last_mut().unwrap();
        cur.push_str(line);
        cur.push('\n');
    }
    out.retain(|s| !s.trim().is_empty());
    out
}

/// "YYYY-Qn" from an ISO date ("2026-10-19T…") or an X-archive date
/// ("Wed Oct 10 20:19:24 +0000 2018").
pub fn quarter_of(date: &str) -> Option<String> {
    let d = date.trim();
    let (year, month) = if d.len() >= 7 && d.as_bytes()[4] == b'-' {
        (d[..4].parse::<u32>().ok()?, d[5..7].parse::<u32>().ok()?)
    } else {
        const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        let parts: Vec<&str> = d.split_whitespace().collect();
        let m = parts.iter().find_map(|p| MONTHS.iter().position(|m| p.to_lowercase().starts_with(m)))?;
        let y = parts.iter().rev().find_map(|p| p.parse::<u32>().ok().filter(|y| *y > 1900))?;
        (y, m as u32 + 1)
    };
    (1..=12).contains(&month).then(|| format!("{year}-Q{}", (month - 1) / 3 + 1))
}

/// Group items into time blocks: by quarter when most items are dated,
/// else consecutive runs of 200. Block order follows first appearance.
fn time_blocks(items: Vec<crate::engine::text::Item>, doc: &str) -> Vec<(String, Vec<crate::engine::text::Item>)> {
    let dated = items
        .iter()
        .filter(|i| i.meta.date.as_deref().and_then(quarter_of).is_some())
        .count();
    let by_quarter = dated * 2 > items.len();
    let mut out: Vec<(String, Vec<crate::engine::text::Item>)> = Vec::new();
    for (n, it) in items.into_iter().enumerate() {
        let key = if by_quarter {
            it.meta
                .date
                .as_deref()
                .and_then(quarter_of)
                .unwrap_or_else(|| "undated".into())
        } else {
            format!("{doc} items {}-{}", n / 200 * 200 + 1, n / 200 * 200 + 200)
        };
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(it),
            None => out.push((key, vec![it])),
        }
    }
    out
}

/// A short-form item as paragraphs: split on blank lines, line breaks inside
/// a paragraph kept (a post's line breaks are part of its voice).
fn item_paragraphs(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in t.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line.trim_end());
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
}

/// Pick a register from the corpus when none is given: the source format
/// decides for structured exports; prose is a blog when it is structured
/// with markdown headings or lists, fiction when it has dialogue, else
/// non-fiction. Returns the register and a short reason.
pub fn detect_register(docs: &[Doc], format: Option<Register>) -> (Register, String) {
    if let Some(r) = format {
        return (r, "source format".into());
    }
    let has_items = docs.iter().any(|d| !d.items.is_empty());
    if has_items {
        let mut w: Vec<usize> = docs
            .iter()
            .flat_map(|d| &d.items)
            .map(|i| text::count_words(&i.text))
            .collect();
        w.sort_unstable();
        let med = w.get(w.len() / 2).copied().unwrap_or(0);
        return if med <= 60 {
            (Register::Social, format!("items, median {med} words"))
        } else {
            (Register::Blog, format!("items, median {med} words"))
        };
    }
    // Long-form prose. Chapter headings and headings repeated across files
    // (calibre's "# CHAPTER 3 {#index_split…}") are book furniture, not blog
    // structure; a blog needs real sub-headings or lists in most posts.
    let mut words = 0usize;
    let mut struct_sources = 0usize;
    let mut quote_open = 0usize;
    let mut paragraphs = 0usize;
    let mut tags = 0usize;
    let mut heading_seen: HashMap<String, usize> = HashMap::new();
    for d in docs {
        let mut headings: HashSet<String> = HashSet::new();
        let mut lists = 0usize;
        for line in d.text.lines() {
            let t = line.trim_start();
            if t.starts_with('#') && !is_chapter_heading(t) {
                let h = t.trim_start_matches('#').trim().to_lowercase();
                *heading_seen.entry(h.clone()).or_insert(0) += 1;
                headings.insert(h);
            } else if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("1. ") {
                lists += 1;
            }
        }
        if headings.len() >= 2 || (headings.len() == 1 && lists >= 2) {
            struct_sources += 1;
        }
        for p in text::paragraphs(&d.text) {
            words += text::count_words(&p);
            paragraphs += 1;
            if p.starts_with(['"', '\u{201C}', '\u{2018}']) {
                quote_open += 1;
            }
            for w in text::word_tokens(&p) {
                if matches!(w.as_str(), "said" | "says" | "asked" | "asks" | "replied" | "whispered") {
                    tags += 1;
                }
            }
        }
    }
    let repeated = heading_seen.values().filter(|&&n| n > 3).count();
    let words_f = words.max(1) as f64;
    let quote_share = quote_open as f64 / paragraphs.max(1) as f64;
    let tags_per_1k = tags as f64 * 1000.0 / words_f;
    let structured = struct_sources * 2 >= docs.len().max(1) && repeated * 2 < heading_seen.len().max(1);
    if quote_share >= 0.15 || tags_per_1k >= 3.0 {
        (Register::Fiction, format!("{:.0}% of paragraphs open with dialogue, {tags_per_1k:.1} speech tags per 1k", quote_share * 100.0))
    } else if structured {
        (Register::Blog, format!("headings or lists in {struct_sources} of {} files", docs.len()))
    } else {
        (Register::Nonfiction, format!("prose without dialogue ({:.0}% dialogue-opening paragraphs); pass --register if this is fiction or a blog", quote_share * 100.0))
    }
}

/// Register for a profile saved before registers existed: fiction if it has
/// dialogue, else non-fiction.
pub fn detect_from_chunks(p: &Profile) -> Register {
    let t = p.style_totals();
    if t.dialogue_words as f64 >= 0.08 * t.words.max(1) as f64 {
        Register::Fiction
    } else {
        Register::Nonfiction
    }
}

fn top_keys<V: Copy + Ord>(m: &HashMap<String, V>, n: usize) -> HashSet<String> {
    let mut v: Vec<(&String, V)> = m.iter().map(|(k, v)| (k, *v)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(name: &str, text: String) -> Doc {
        Doc {
            name: name.into(),
            text,
            items: Vec::new(),
        }
    }

    fn prose(n_paras: usize) -> String {
        let p = "Arthur Dent walked to the pub with Ford, and it wasn't raining. \
                 He said nothing at all; the beer was warm. \"Fine,\" said Ford.";
        (0..n_paras).map(|_| p).collect::<Vec<_>>().join("\n\n")
    }

    #[test]
    fn builds_chunks_spans_and_proper_nouns() {
        let p = Profile::build(
            "t",
            &[doc("a.md", prose(60)), doc("b.md", prose(60))],
            ProfileKind::Author,
        )
        .unwrap();
        assert_eq!(p.sources.len(), 2);
        assert!(p.chunks.len() >= MIN_CHUNKS);
        assert!(p.is_proper_noun("ford"), "{:?}", p.proper_nouns);
        assert!(p.is_proper_noun("dent"));
        assert!(!p.is_proper_noun("pub"));
        // Spans never cross sources.
        for span in p.spans(2) {
            let s = p.chunks[span[0]].source;
            assert!(span.iter().all(|&i| p.chunks[i].source == s));
        }
        let (wf, _) = p.span_freqs(&p.spans(2)[0]);
        let total: f64 = wf.values().sum();
        assert!(total > 0.9 && total <= 1.0 + 1e-9);
    }

    #[test]
    fn blend_weights_passages_and_counts_by_share() {
        let a = Profile::build("a", &[doc("a.md", prose(100))], ProfileKind::Author).unwrap();
        let other = prose(200).replace("pub", "tavern");
        let b = Profile::build("b", &[doc("b.md", other)], ProfileKind::Author).unwrap();
        let m = Profile::blend("m", &[(a.clone(), 3.0), (b.clone(), 1.0)]).unwrap();
        assert_eq!(m.kind, ProfileKind::Blend);
        assert_eq!(m.chunks.len(), a.chunks.len() + b.chunks.len());
        let wa: f32 = m.chunks.iter().filter(|c| c.source == 0).map(|c| c.weight).sum();
        let wt: f32 = m.chunks.iter().map(|c| c.weight).sum();
        assert!((wa / wt - 0.75).abs() < 1e-3, "share {}", wa / wt);
        // "pub" only in a, "tavern" only in b: pseudo-counts follow shares 3:1
        // even though b's corpus is twice as long.
        let pubs = m.word_counts["pub"] as f64;
        let taverns = m.word_counts["tavern"] as f64;
        assert!((pubs / taverns - 3.0).abs() < 0.05, "{pubs} vs {taverns}");
        assert!(Profile::blend("x", &[(a, 1.0)]).is_err());
    }

    #[test]
    fn tiny_corpus_is_rejected() {
        let e = Profile::build("t", &[doc("a", prose(3))], ProfileKind::Author);
        assert!(e.is_err());
    }
}
