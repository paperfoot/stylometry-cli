//! The shared reference model: the most-frequent feature vocabulary and the
//! per-feature mean/SD, computed across ALL loaded profiles' passages at one
//! analysis length.
//!
//! This is the Burrows-Delta reference distribution. z-scoring against it is
//! what makes a frequency "unusual relative to writers in general" rather than
//! just "common", which is the whole point of Delta over raw cosine. Feature
//! spread depends on passage length, so a model is built per length: every
//! profile's `windows(words)` at that length (passages of one work for
//! long-form, pooled posts for short-form).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::engine::features;
use crate::engine::profile::{BASE_CHUNK_WORDS, Profile};

/// Ordered feature names: `words` first, then `trigrams`. Vector layout is
/// `[word rel-freqs .. ; trigram rel-freqs ..]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vocab {
    pub words: Vec<String>,
    pub trigrams: Vec<String>,
}

impl Vocab {
    pub fn dim(&self) -> usize {
        self.words.len() + self.trigrams.len()
    }
}

/// Serializable so `calibrate` can freeze the exact reference (vocab +
/// mean/sd) into the calibration artifact: scoring against a calibrated
/// profile then never silently re-fits as profiles are added or removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceModel {
    pub vocab: Vocab,
    pub mean: Vec<f64>,
    pub sd: Vec<f64>,
    /// Base chunks per passage: how v0.3 artifacts recorded the length.
    /// Kept so they still load; `words` supersedes it.
    #[serde(default = "one")]
    pub k: usize,
    /// Window length in words this model was fit at (0 in v0.3 artifacts,
    /// which mean `k` x 250). Read it through `band_words`.
    #[serde(default)]
    pub words: usize,
    /// Word and trigram -> vector position, built on first use (not stored).
    #[serde(skip)]
    index: OnceLock<(HashMap<String, usize>, HashMap<String, usize>)>,
}

fn one() -> usize {
    1
}

fn top_keys(counts: &HashMap<String, u64>, n: usize, skip: &HashSet<&str>) -> Vec<String> {
    let mut v: Vec<(&String, &u64)> = counts
        .iter()
        .filter(|(k, _)| !skip.contains(k.as_str()))
        .collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
}

impl ReferenceModel {
    /// Build from profiles at a window length of `words`, selecting the top
    /// `n_words` words (proper nouns excluded: names are topic, not style)
    /// and `n_trigrams` trigrams by total count across all profiles.
    pub fn build(profiles: &[Profile], n_words: usize, n_trigrams: usize, words: usize) -> ReferenceModel {
        let mut gw: HashMap<String, u64> = HashMap::new();
        let mut gt: HashMap<String, u64> = HashMap::new();
        let mut names: HashSet<&str> = HashSet::new();
        for p in profiles {
            for (k, v) in &p.word_counts {
                *gw.entry(k.clone()).or_insert(0) += *v;
            }
            for (k, v) in &p.trigram_counts {
                *gt.entry(k.clone()).or_insert(0) += *v;
            }
            names.extend(p.proper_nouns.iter().map(String::as_str));
        }
        let vocab = Vocab {
            words: top_keys(&gw, n_words, &names),
            trigrams: top_keys(&gt, n_trigrams, &HashSet::new()),
        };
        let mut model = ReferenceModel {
            mean: vec![0.0; vocab.dim()],
            sd: vec![1.0; vocab.dim()],
            vocab,
            k: (words / BASE_CHUNK_WORDS).max(1),
            words: words.max(1),
            index: OnceLock::new(),
        };

        let dim = model.vocab.dim();
        let mut sum = vec![0.0f64; dim];
        let mut sumsq = vec![0.0f64; dim];
        let mut n_docs = 0usize;
        for p in profiles {
            for v in model.raw_vectors(p) {
                for j in 0..dim {
                    sum[j] += v[j];
                    sumsq[j] += v[j] * v[j];
                }
                n_docs += 1;
            }
        }
        let n = n_docs.max(1) as f64;
        for j in 0..dim {
            model.mean[j] = sum[j] / n;
            let var = (sumsq[j] / n) - model.mean[j] * model.mean[j];
            // Constant features (var ~ 0) get sd=1 so their z-score is 0 and
            // they contribute nothing rather than exploding.
            model.sd[j] = if var > 1e-12 { var.sqrt() } else { 1.0 };
        }
        model
    }

    /// Window length in words the model was fit at.
    pub fn band_words(&self) -> usize {
        if self.words > 0 { self.words } else { self.k.max(1) * BASE_CHUNK_WORDS }
    }

    /// A profile's windows at this model's length.
    pub fn windows(&self, p: &Profile) -> Vec<Vec<usize>> {
        p.windows(self.band_words())
    }

    fn index(&self) -> &(HashMap<String, usize>, HashMap<String, usize>) {
        self.index.get_or_init(|| {
            let pos = |v: &[String]| v.iter().enumerate().map(|(j, s)| (s.clone(), j)).collect();
            (pos(&self.vocab.words), pos(&self.vocab.trigrams))
        })
    }

    /// Relative-frequency vector of a window of chunks, summed straight into
    /// the vocab's positions: compare builds a centroid for every loaded
    /// profile on every call, so no per-window maps.
    pub fn window_vector(&self, p: &Profile, window: &[usize]) -> Vec<f64> {
        let (wi, ti) = self.index();
        let nw = self.vocab.words.len();
        let mut v = vec![0.0f64; self.vocab.dim()];
        let (mut wt, mut tt) = (0u64, 0u64);
        for &i in window {
            let c = &p.chunks[i];
            wt += c.n_words as u64;
            tt += c.n_trigrams as u64;
            for (w, n) in &c.words {
                if let Some(&j) = wi.get(w) {
                    v[j] += *n as f64;
                }
            }
            for (t, n) in &c.trigrams {
                if let Some(&j) = ti.get(t) {
                    v[nw + j] += *n as f64;
                }
            }
        }
        let (wt, tt) = (wt.max(1) as f64, tt.max(1) as f64);
        for (j, x) in v.iter_mut().enumerate() {
            *x /= if j < nw { wt } else { tt };
        }
        v
    }

    /// Raw relative-frequency vector for a feature pair over this vocab.
    pub fn vectorize(&self, wf: &HashMap<String, f64>, tf: &HashMap<String, f64>) -> Vec<f64> {
        let mut v = Vec::with_capacity(self.vocab.dim());
        v.extend(self.vocab.words.iter().map(|w| wf.get(w).copied().unwrap_or(0.0)));
        v.extend(self.vocab.trigrams.iter().map(|t| tf.get(t).copied().unwrap_or(0.0)));
        v
    }

    /// Relative-frequency vector computed directly from raw text.
    pub fn vectorize_text(&self, text: &str) -> Vec<f64> {
        let wc = features::word_counts(text);
        let tc = features::trigram_counts(text);
        let wtot = features::total(&wc);
        let ttot = features::total(&tc);
        let wf: HashMap<String, f64> = wc
            .into_iter()
            .map(|(k, v)| (k, features::rel_freq(v, wtot)))
            .collect();
        let tf: HashMap<String, f64> = tc
            .into_iter()
            .map(|(k, v)| (k, features::rel_freq(v, ttot)))
            .collect();
        self.vectorize(&wf, &tf)
    }

    /// Standardize a relative-frequency vector into z-scores.
    pub fn zscore(&self, v: &[f64]) -> Vec<f64> {
        (0..v.len())
            .map(|j| (v[j] - self.mean[j]) / self.sd[j])
            .collect()
    }

    /// Raw vectors of every window of a profile at this model's length.
    pub fn raw_vectors(&self, p: &Profile) -> Vec<Vec<f64>> {
        self.windows(p).iter().map(|w| self.window_vector(p, w)).collect()
    }

    /// Z-scored vectors for every window of a profile at this length.
    pub fn passage_zvectors(&self, p: &Profile) -> Vec<Vec<f64>> {
        self.raw_vectors(p).iter().map(|v| self.zscore(v)).collect()
    }

    /// The author's centroid in z-space (mean of passage z-vectors).
    pub fn centroid(&self, p: &Profile) -> Vec<f64> {
        mean_vec(&self.passage_zvectors(p), self.vocab.dim())
    }
}

pub fn mean_vec(vectors: &[Vec<f64>], dim: usize) -> Vec<f64> {
    if vectors.is_empty() {
        return vec![0.0; dim];
    }
    let mut m = vec![0.0f64; dim];
    for v in vectors {
        for j in 0..dim {
            m[j] += v[j];
        }
    }
    let n = vectors.len() as f64;
    for x in &mut m {
        *x /= n;
    }
    m
}

/// A stable signature of the profile set + feature settings: name, size, and
/// creation time per profile. A changed signature means the imposter pool or
/// a profile's content changed since calibration.
pub fn reference_signature(profiles: &[Profile], n_words: usize, n_trigrams: usize) -> String {
    let mut parts: Vec<String> = profiles
        .iter()
        .map(|p| format!("{}:{}:{}:{}", p.name, p.chunks.len(), p.n_tokens, p.created))
        .collect();
    parts.sort();
    format!("mfw={n_words};tri={n_trigrams};{}", parts.join("|"))
}
