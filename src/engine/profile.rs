//! An author profile: the stored, on-disk representation of one person's
//! writing, built by *analysing* (fingerprinting, not GPU training) a corpus.
//!
//! A profile keeps two things: aggregate counts (used to build the shared
//! most-frequent feature vocabulary across all profiles) and per-chunk relative
//! frequencies (the "documents" used to estimate the reference mean/SD and the
//! author's centroid + within-author spread for calibration).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::engine::features;
use crate::engine::text;
use crate::error::AppError;

/// Logistic mapping from a Cosine-Delta value to P(same author), plus the
/// verification quality measured on held-out chunks at calibration time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    /// P(same) = sigmoid(intercept + slope * delta). slope is negative.
    pub slope: f64,
    pub intercept: f64,
    /// Delta threshold that best separates same/different (Youden's J).
    pub threshold: f64,
    /// Area under ROC on the held-out same/different set.
    pub auc: f64,
    /// Accuracy at the chosen threshold (PAN c@1 with no abstention).
    pub c_at_1: f64,
    /// How many imposter profiles were used as negatives.
    pub imposters: usize,
    /// Signature of the reference set this calibration was fit against. If the
    /// loaded profile set no longer matches, the calibrated verdict is stale.
    #[serde(default)]
    pub ref_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub n_tokens: u64,
    pub n_chunks: usize,
    pub chunk_size: usize,
    /// Aggregate word counts over the whole corpus (top entries only).
    pub word_counts: HashMap<String, u64>,
    /// Aggregate character-trigram counts (top entries only).
    pub trigram_counts: HashMap<String, u64>,
    /// Per-chunk word relative frequencies.
    pub chunk_word_freqs: Vec<HashMap<String, f64>>,
    /// Per-chunk character-trigram relative frequencies.
    pub chunk_trigram_freqs: Vec<HashMap<String, f64>>,
    /// Optional verification calibration (filled by `calibrate`).
    pub calibration: Option<Calibration>,
    /// Unix seconds at build time.
    pub created: u64,
}

const AGG_WORDS_KEEP: usize = 6000;
const AGG_TRIGRAMS_KEEP: usize = 10000;
/// Minimum chunks for a usable profile (need spread to estimate variance).
pub const MIN_CHUNKS: usize = 3;

impl Profile {
    /// Build a profile by fingerprinting `text`, split into `chunk_size`-word
    /// chunks. Returns an error if the corpus is too small to yield enough
    /// chunks for a stable fingerprint.
    pub fn build(name: &str, text: &str, chunk_size: usize) -> Result<Profile, AppError> {
        let chunks = text::chunk_by_words(text, chunk_size);
        if chunks.len() < MIN_CHUNKS {
            return Err(AppError::InvalidInput(format!(
                "corpus too small: produced {} chunk(s) at chunk-size {}, need >= {}. \
                 Provide more text or lower --chunk-size.",
                chunks.len(),
                chunk_size,
                MIN_CHUNKS
            )));
        }

        let mut agg_words: HashMap<String, u64> = HashMap::new();
        let mut agg_tri: HashMap<String, u64> = HashMap::new();
        let mut chunk_word_freqs = Vec::with_capacity(chunks.len());
        let mut chunk_trigram_freqs = Vec::with_capacity(chunks.len());
        let mut n_tokens = 0u64;

        for ch in &chunks {
            let wc = features::word_counts(ch);
            let tc = features::trigram_counts(ch);
            let wtot = features::total(&wc);
            let ttot = features::total(&tc);
            n_tokens += wtot;

            // Store ALL feature relative frequencies (no per-chunk pruning):
            // the chunk extractor must match the query extractor in model.rs
            // exactly, or training/calibration and compare disagree on rare
            // features. Vocab selection (top-N) happens later in the model.
            let wf: HashMap<String, f64> = wc
                .iter()
                .map(|(k, &v)| (k.clone(), features::rel_freq(v, wtot)))
                .collect();
            let tf: HashMap<String, f64> = tc
                .iter()
                .map(|(k, &v)| (k.clone(), features::rel_freq(v, ttot)))
                .collect();

            for (k, v) in wc {
                *agg_words.entry(k).or_insert(0) += v;
            }
            for (k, v) in tc {
                *agg_tri.entry(k).or_insert(0) += v;
            }
            chunk_word_freqs.push(wf);
            chunk_trigram_freqs.push(tf);
        }

        Ok(Profile {
            name: name.to_string(),
            n_tokens,
            n_chunks: chunks.len(),
            chunk_size,
            word_counts: features::prune_top(agg_words, AGG_WORDS_KEEP),
            trigram_counts: features::prune_top(agg_tri, AGG_TRIGRAMS_KEEP),
            chunk_word_freqs,
            chunk_trigram_freqs,
            calibration: None,
            created: now_unix(),
        })
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
