//! The shared reference model: the most-frequent feature vocabulary and the
//! per-feature mean/SD, computed across ALL loaded profiles' chunks.
//!
//! This is the Burrows-Delta reference distribution. z-scoring against it is
//! what makes a frequency "unusual relative to writers in general" rather than
//! just "common", which is the whole point of Delta over raw cosine.

use std::collections::HashMap;

use crate::engine::features;
use crate::engine::profile::Profile;

/// Ordered feature names: `words` first, then `trigrams`. Vector layout is
/// `[word rel-freqs .. ; trigram rel-freqs ..]`.
#[derive(Debug, Clone)]
pub struct Vocab {
    pub words: Vec<String>,
    pub trigrams: Vec<String>,
}

impl Vocab {
    pub fn dim(&self) -> usize {
        self.words.len() + self.trigrams.len()
    }
}

#[derive(Debug, Clone)]
pub struct ReferenceModel {
    pub vocab: Vocab,
    pub mean: Vec<f64>,
    pub sd: Vec<f64>,
}

fn top_keys(counts: &HashMap<String, u64>, n: usize) -> Vec<String> {
    let mut v: Vec<(&String, &u64)> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
}

impl ReferenceModel {
    /// Build from profiles, selecting the top `n_words` words and `n_trigrams`
    /// trigrams by total count across all profiles.
    pub fn build(profiles: &[Profile], n_words: usize, n_trigrams: usize) -> ReferenceModel {
        let mut gw: HashMap<String, u64> = HashMap::new();
        let mut gt: HashMap<String, u64> = HashMap::new();
        for p in profiles {
            for (k, v) in &p.word_counts {
                *gw.entry(k.clone()).or_insert(0) += *v;
            }
            for (k, v) in &p.trigram_counts {
                *gt.entry(k.clone()).or_insert(0) += *v;
            }
        }
        let vocab = Vocab {
            words: top_keys(&gw, n_words),
            trigrams: top_keys(&gt, n_trigrams),
        };

        // Index lookup for fast vectorization.
        let widx: HashMap<&str, usize> = vocab
            .words
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i))
            .collect();
        let off = vocab.words.len();
        let tidx: HashMap<&str, usize> = vocab
            .trigrams
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i + off))
            .collect();

        let dim = vocab.dim();
        let mut sum = vec![0.0f64; dim];
        let mut sumsq = vec![0.0f64; dim];
        let mut n_docs = 0usize;

        for p in profiles {
            for (wf, tf) in p
                .chunk_word_freqs
                .iter()
                .zip(p.chunk_trigram_freqs.iter())
            {
                let v = vectorize_with_index(wf, tf, &widx, &tidx, dim);
                for j in 0..dim {
                    sum[j] += v[j];
                    sumsq[j] += v[j] * v[j];
                }
                n_docs += 1;
            }
        }

        let n = n_docs.max(1) as f64;
        let mut mean = vec![0.0; dim];
        let mut sd = vec![1.0; dim];
        for j in 0..dim {
            mean[j] = sum[j] / n;
            let var = (sumsq[j] / n) - mean[j] * mean[j];
            // Constant features (var ~ 0) get sd=1 so their z-score is 0 and
            // they contribute nothing rather than exploding.
            sd[j] = if var > 1e-12 { var.sqrt() } else { 1.0 };
        }

        ReferenceModel { vocab, mean, sd }
    }

    /// Raw relative-frequency vector for a feature pair over this vocab.
    pub fn vectorize(&self, wf: &HashMap<String, f64>, tf: &HashMap<String, f64>) -> Vec<f64> {
        let widx: HashMap<&str, usize> = self
            .vocab
            .words
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i))
            .collect();
        let off = self.vocab.words.len();
        let tidx: HashMap<&str, usize> = self
            .vocab
            .trigrams
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i + off))
            .collect();
        vectorize_with_index(wf, tf, &widx, &tidx, self.vocab.dim())
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

    /// Z-scored vectors for every chunk of a profile.
    pub fn chunk_zvectors(&self, p: &Profile) -> Vec<Vec<f64>> {
        p.chunk_word_freqs
            .iter()
            .zip(p.chunk_trigram_freqs.iter())
            .map(|(wf, tf)| self.zscore(&self.vectorize(wf, tf)))
            .collect()
    }

    /// The author's centroid in z-space (mean of chunk z-vectors).
    pub fn centroid(&self, p: &Profile) -> Vec<f64> {
        mean_vec(&self.chunk_zvectors(p), self.vocab.dim())
    }
}

fn vectorize_with_index(
    wf: &HashMap<String, f64>,
    tf: &HashMap<String, f64>,
    widx: &HashMap<&str, usize>,
    tidx: &HashMap<&str, usize>,
    dim: usize,
) -> Vec<f64> {
    let mut v = vec![0.0f64; dim];
    for (k, &freq) in wf {
        if let Some(&i) = widx.get(k.as_str()) {
            v[i] = freq;
        }
    }
    for (k, &freq) in tf {
        if let Some(&i) = tidx.get(k.as_str()) {
            v[i] = freq;
        }
    }
    v
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

/// A stable signature of the reference set + feature settings. A calibration is
/// only valid against the exact reference it was fit on; `compare` recomputes
/// this and flags the calibration stale if the profile set has changed.
pub fn reference_signature(profiles: &[Profile], n_words: usize, n_trigrams: usize) -> String {
    let mut parts: Vec<String> = profiles
        .iter()
        .map(|p| format!("{}:{}:{}", p.name, p.n_chunks, p.n_tokens))
        .collect();
    parts.sort();
    format!("mfw={n_words};tri={n_trigrams};{}", parts.join("|"))
}
