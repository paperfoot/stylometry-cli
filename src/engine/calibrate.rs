//! Turn a Cosine-Delta value into a calibrated P(same author), and measure how
//! well the verifier separates same-author from different-author samples.
//!
//! Calibration is per length band (250 to 4,000 words; 250 to 1,000 for
//! posts and messages): Delta's spread shrinks as passages grow, so a mapping
//! fit on one length is wrong at another. A band's samples are the profiles'
//! `windows(words)`: passages of one work for long-form, pooled consecutive
//! items for short-form (a 500-word window of posts is ~12 posts). Per band,
//! labelled data is:
//! - positives: the target's own windows, each scored against a centroid
//!   that never saw it — built from windows sharing none of its sources when
//!   the target has several (a new book, or a later quarter of posts, is the
//!   real use case), else excluding the window and its two neighbours on
//!   each side (adjacent windows share topic);
//! - negatives: every other non-blend profile's windows (background authors
//!   and other voices) against the target's full centroid.
//!
//! The logistic is class-balanced (thousands of negatives must not drown a
//! few hundred positives) and its quality is measured on data it was not fit
//! on: held-out works or a contiguous tail for positives, whole held-out
//! imposter authors for negatives (unseen authors are the failure that
//! matters).

use std::collections::HashMap;

use crate::engine::delta::cosine_delta;
use crate::engine::model::ReferenceModel;
use crate::engine::profile::{BandCalibration, Calibration, Profile, ProfileKind, Register, now_unix};
use crate::engine::{DEFAULT_MFW, DEFAULT_TRIGRAMS, model};
use crate::error::AppError;

/// Analysis lengths in words for long-form writing.
pub const BAND_WORDS: [usize; 5] = [250, 500, 1000, 2000, 4000];
/// Short-form bands: pooled windows of posts. A batch an agent checks is
/// rarely over 1,000 words, and longer pools leave too few windows.
pub const SHORT_BAND_WORDS: [usize; 3] = [250, 500, 1000];

/// The band lengths a profile of this register is calibrated and scored at.
pub fn bands_for(register: Register) -> &'static [usize] {
    if register.is_short_form() { &SHORT_BAND_WORDS } else { &BAND_WORDS }
}
/// A band is fit only with at least this much labelled data.
pub const MIN_POSITIVES: usize = 6;
pub const MIN_NEGATIVES: usize = 20;
/// Fewer imposters than this and P(same) is fit against too few writers.
pub const MIN_IMPOSTERS: usize = 10;
/// Share of each class held out to measure the fitted band honestly.
const TEST_FRACTION: f64 = 0.3;
/// Samples per class used to fit the 1-D logistic (evenly thinned beyond
/// this; two parameters do not need thousands of points).
const FIT_CAP: usize = 400;
/// Neighbouring passages excluded from a positive's centroid on each side.
const NEIGHBOURS: usize = 2;

/// Abstention band on P(same author): inside it the verdict is
/// "inconclusive" rather than a forced same/different call. PAN's c@1 metric
/// rewards abstaining exactly where the verifier has no real signal.
pub const P_ABSTAIN_LOW: f64 = 0.35;
pub const P_ABSTAIN_HIGH: f64 = 0.65;

#[inline]
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Split a delta series into (train, test) with a contiguous tail as test.
/// Contiguous — not interleaved — because adjacent chunks of one text share
/// topic; an interleaved split would leak that into the "holdout". Returns an
/// empty test set when there is too little data to spare.
pub fn tail_split(deltas: &[f64], test_fraction: f64) -> (Vec<f64>, Vec<f64>) {
    let n = deltas.len();
    let n_test = ((n as f64) * test_fraction).floor() as usize;
    if n_test == 0 || n - n_test < 2 {
        return (deltas.to_vec(), Vec::new());
    }
    let cut = n - n_test;
    (deltas[..cut].to_vec(), deltas[cut..].to_vec())
}

/// AUC of the classifier "smaller delta => same author".
/// Equivalent to P(delta_pos < delta_neg) with ties counted as 0.5.
pub fn auc(pos_deltas: &[f64], neg_deltas: &[f64]) -> f64 {
    if pos_deltas.is_empty() || neg_deltas.is_empty() {
        return 0.5;
    }
    let mut wins = 0.0f64;
    for &p in pos_deltas {
        for &n in neg_deltas {
            if p < n {
                wins += 1.0;
            } else if (p - n).abs() < f64::EPSILON {
                wins += 0.5;
            }
        }
    }
    wins / (pos_deltas.len() as f64 * neg_deltas.len() as f64)
}

/// L2 strengths tried by `fit_logistic`; the winner is picked by 3-fold
/// cross-validated Brier score, not hard-coded. The grid spans "barely
/// regularized" to "strong" — strong wins on tiny/noisy data, weak wins when
/// separation is real and probabilities deserve to be confident.
pub const LAMBDA_GRID: [f64; 4] = [0.01, 0.05, 0.2, 0.5];
/// With fewer samples than this, CV folds are too small to justify the weak
/// end of the grid — extreme confidence needs data to back it, so only the
/// strong half is tried.
const SMALL_SAMPLE: usize = 30;
/// Fallback for tiny sample sets where CV folds would be degenerate.
const LAMBDA_FALLBACK: f64 = 0.5;

/// Per-sample weights giving each class half the total weight, so the fit
/// and its scores are not dominated by whichever class has more samples.
fn class_weights(samples: &[(f64, bool)]) -> Vec<f64> {
    let np = samples.iter().filter(|s| s.1).count();
    let nn = samples.len() - np;
    let (wp, wn) = if np == 0 || nn == 0 {
        let w = 1.0 / samples.len().max(1) as f64;
        (w, w)
    } else {
        (0.5 / np as f64, 0.5 / nn as f64)
    };
    samples.iter().map(|s| if s.1 { wp } else { wn }).collect()
}

/// Class-balanced Brier score: the mean of each class's Brier score.
pub fn balanced_brier(samples: &[(f64, bool)], slope: f64, intercept: f64) -> f64 {
    if samples.is_empty() {
        return 0.25;
    }
    let w = class_weights(samples);
    samples
        .iter()
        .zip(&w)
        .map(|(&(d, y), wi)| {
            let p = probability(d, slope, intercept);
            let t = if y { 1.0 } else { 0.0 };
            wi * (p - t) * (p - t)
        })
        .sum()
}

/// Class-balanced holdout quality of a fitted logistic: (balanced accuracy
/// of "same when P > 0.5", balanced Brier, balanced c@1 with the verdict's
/// abstention band). Every number weights both classes equally.
pub fn balanced_metrics(samples: &[(f64, bool)], slope: f64, intercept: f64) -> (f64, f64, f64) {
    let w = class_weights(samples);
    let (mut acc, mut nc, mut nu) = (0.0, 0.0, 0.0);
    for (&(d, y), wi) in samples.iter().zip(&w) {
        let p = probability(d, slope, intercept);
        if (p > 0.5) == y {
            acc += wi;
        }
        if (P_ABSTAIN_LOW..=P_ABSTAIN_HIGH).contains(&p) {
            nu += wi;
        } else if (p > P_ABSTAIN_HIGH) == y {
            nc += wi;
        }
    }
    (acc, balanced_brier(samples, slope, intercept), nc + nu * nc)
}

/// Fit P(same) = sigmoid(intercept + slope * delta) with a fixed L2 penalty
/// on the slope, by gradient descent on the class-balanced negative
/// log-likelihood. Delta is standardized internally for stable steps, then
/// coefficients are mapped back to raw-delta units. The penalty exists
/// because the labelled deltas are often linearly separable, which drives an
/// unregularized slope to infinity and yields fake 0/1 "probabilities".
pub fn fit_logistic_with(samples: &[(f64, bool)], lambda: f64) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let n = samples.len() as f64;
    let mu = samples.iter().map(|(d, _)| d).sum::<f64>() / n;
    let var = samples.iter().map(|(d, _)| (d - mu).powi(2)).sum::<f64>() / n;
    let sigma = if var > 1e-12 { var.sqrt() } else { 1.0 };

    let weights = class_weights(samples);
    let xs: Vec<(f64, f64, f64)> = samples
        .iter()
        .zip(&weights)
        .map(|((d, same), w)| ((d - mu) / sigma, if *same { 1.0 } else { 0.0 }, *w))
        .collect();

    let (mut w0, mut w1) = (0.0f64, 0.0f64);
    let lr = 0.3;
    for _ in 0..4000 {
        let (mut g0, mut g1) = (0.0f64, 0.0f64);
        for &(x, y, w) in &xs {
            let p = sigmoid(w0 + w1 * x);
            let err = w * (p - y);
            g0 += err;
            g1 += err * x;
        }
        w0 -= lr * g0;
        w1 -= lr * (g1 + lambda * w1);
    }
    // Map back: w0 + w1 * (d - mu)/sigma = (w0 - w1*mu/sigma) + (w1/sigma) * d
    let slope = w1 / sigma;
    let intercept = w0 - w1 * mu / sigma;
    (slope, intercept)
}

/// Fit the logistic with the L2 strength selected by 3-fold cross-validated
/// Brier score over `LAMBDA_GRID`, then refit on all samples with the winner.
/// Folds are deterministic (index mod 3), which stratifies both classes when
/// samples are ordered positives-then-negatives as `calibrate` produces them.
pub fn fit_logistic(samples: &[(f64, bool)]) -> (f64, f64) {
    const K: usize = 3;
    if samples.len() < 2 * K {
        return fit_logistic_with(samples, LAMBDA_FALLBACK);
    }
    let grid: &[f64] = if samples.len() < SMALL_SAMPLE {
        &LAMBDA_GRID[2..]
    } else {
        &LAMBDA_GRID
    };
    let mut best = (LAMBDA_FALLBACK, f64::INFINITY);
    for &lambda in grid {
        let mut score_sum = 0.0;
        let mut folds_used = 0usize;
        for fold in 0..K {
            let train: Vec<(f64, bool)> = samples
                .iter()
                .enumerate()
                .filter(|(i, _)| i % K != fold)
                .map(|(_, s)| *s)
                .collect();
            let test: Vec<(f64, bool)> = samples
                .iter()
                .enumerate()
                .filter(|(i, _)| i % K == fold)
                .map(|(_, s)| *s)
                .collect();
            // A fold whose train side lost a whole class can't be fit.
            let has_both =
                train.iter().any(|&(_, y)| y) && train.iter().any(|&(_, y)| !y);
            if test.is_empty() || !has_both {
                continue;
            }
            let (sl, ic) = fit_logistic_with(&train, lambda);
            score_sum += balanced_brier(&test, sl, ic);
            folds_used += 1;
        }
        if folds_used == 0 {
            continue;
        }
        let mean = score_sum / folds_used as f64;
        if mean < best.1 {
            best = (lambda, mean);
        }
    }
    fit_logistic_with(samples, best.0)
}

#[inline]
pub fn probability(delta: f64, slope: f64, intercept: f64) -> f64 {
    sigmoid(intercept + slope * delta)
}

/// A profile's own windows at one length, each scored against a centroid
/// that never saw it. `deltas[i]` belongs to `works[i]` (the source holding
/// most of the window's words) and carries `weights[i]` (1.0, or the
/// window's blend weight).
pub struct OwnDeltas {
    pub deltas: Vec<f64>,
    pub works: Vec<u32>,
    pub weights: Vec<f64>,
    /// True when centroids excluded every window sharing a source with the
    /// scored one (other works, or other quarters of posts).
    pub cross_work: bool,
}

/// One window's weight (mean chunk weight), main source (most words) and
/// every source it touches (pooled windows can span several).
struct WindowMeta {
    weight: f64,
    work: u32,
    sources: Vec<u32>,
}

fn window_meta(p: &Profile, windows: &[Vec<usize>]) -> Vec<WindowMeta> {
    windows
        .iter()
        .map(|win| {
            let weight = win.iter().map(|&i| p.chunks[i].weight as f64).sum::<f64>() / win.len().max(1) as f64;
            let mut by_src: Vec<(u32, u32)> = Vec::new();
            for &i in win {
                let c = &p.chunks[i];
                match by_src.iter_mut().find(|(s, _)| *s == c.source) {
                    Some(e) => e.1 += c.n_words,
                    None => by_src.push((c.source, c.n_words)),
                }
            }
            let work = by_src.iter().max_by_key(|(s, n)| (*n, std::cmp::Reverse(*s))).map(|e| e.0).unwrap_or(0);
            let mut sources: Vec<u32> = by_src.into_iter().map(|e| e.0).collect();
            sources.sort_unstable();
            WindowMeta { weight, work, sources }
        })
        .collect()
}

/// Weighted centroid of a profile's windows in a model's z-space (a blend's
/// windows count by their share; everyone else's equally).
pub fn weighted_centroid(model: &ReferenceModel, p: &Profile) -> Vec<f64> {
    if p.kind != ProfileKind::Blend {
        return model.centroid(p);
    }
    let wins = model.windows(p);
    let z: Vec<Vec<f64>> = wins.iter().map(|w| model.zscore(&model.window_vector(p, w))).collect();
    let w: Vec<f64> = window_meta(p, &wins).iter().map(|m| m.weight).collect();
    weighted_mean(&z, &w, model.vocab.dim())
}

fn weighted_mean(z: &[Vec<f64>], w: &[f64], dim: usize) -> Vec<f64> {
    let total: f64 = w.iter().sum();
    let mut m = vec![0.0; dim];
    if total <= 0.0 {
        return m;
    }
    for (v, wi) in z.iter().zip(w) {
        for j in 0..dim {
            m[j] += v[j] * wi;
        }
    }
    m.iter_mut().for_each(|x| *x /= total);
    m
}

/// Held-out deltas of a profile's own windows at the model's length.
///
/// With at least two works (sources: books, posts' quarters) that each hold
/// two or more windows, a window is scored against the centroid of the
/// windows that share none of its sources, so a window pooled across two
/// blog posts never meets either post again. Otherwise (one work, or the
/// exclusion leaves too few scored windows) against the centroid without it
/// and its `NEIGHBOURS` on each side.
pub fn own_deltas(model: &ReferenceModel, p: &Profile) -> OwnDeltas {
    let wins = model.windows(p);
    let z: Vec<Vec<f64>> = wins.iter().map(|w| model.zscore(&model.window_vector(p, w))).collect();
    let meta = window_meta(p, &wins);
    let mut per_work: HashMap<u32, usize> = HashMap::new();
    for m in &meta {
        *per_work.entry(m.work).or_insert(0) += 1;
    }
    if per_work.values().filter(|&&c| c >= 2).count() >= 2 {
        let out = cross_work_deltas(&z, &meta, model.vocab.dim());
        if out.deltas.len() >= MIN_POSITIVES {
            return out;
        }
    }
    neighbour_deltas(&z, &meta, model.vocab.dim())
}

/// Cross-work held-out deltas: windows are grouped by the exact set of
/// sources they touch, and each group's centroid is the total minus every
/// group it shares a source with (few groups, so this stays O(windows)).
fn cross_work_deltas(z: &[Vec<f64>], meta: &[WindowMeta], dim: usize) -> OwnDeltas {
    let mut groups: Vec<(&[u32], Vec<f64>, f64)> = Vec::new();
    let mut group_of = Vec::with_capacity(z.len());
    let (mut total, mut total_w) = (vec![0.0; dim], 0.0);
    for (v, m) in z.iter().zip(meta) {
        let g = match groups.iter().position(|g| g.0 == m.sources.as_slice()) {
            Some(g) => g,
            None => {
                groups.push((&m.sources, vec![0.0; dim], 0.0));
                groups.len() - 1
            }
        };
        group_of.push(g);
        for j in 0..dim {
            let x = v[j] * m.weight;
            groups[g].1[j] += x;
            total[j] += x;
        }
        groups[g].2 += m.weight;
        total_w += m.weight;
    }
    let shares = |a: &[u32], b: &[u32]| a.iter().any(|s| b.binary_search(s).is_ok());
    let held_out: Vec<Option<Vec<f64>>> = groups
        .iter()
        .map(|g| {
            let (mut sum, mut sw) = (total.clone(), total_w);
            for h in groups.iter().filter(|h| shares(g.0, h.0)) {
                for j in 0..dim {
                    sum[j] -= h.1[j];
                }
                sw -= h.2;
            }
            (sw > 1e-9).then(|| sum.iter().map(|x| x / sw).collect())
        })
        .collect();
    let mut out = OwnDeltas { deltas: Vec::new(), works: Vec::new(), weights: Vec::new(), cross_work: true };
    for (i, v) in z.iter().enumerate() {
        if let Some(c) = &held_out[group_of[i]] {
            out.deltas.push(cosine_delta(v, c));
            out.works.push(meta[i].work);
            out.weights.push(meta[i].weight);
        }
    }
    out
}

/// Within-work held-out deltas: each window against the centroid without it
/// and its `NEIGHBOURS` on each side.
fn neighbour_deltas(z: &[Vec<f64>], meta: &[WindowMeta], dim: usize) -> OwnDeltas {
    let n = z.len();
    let (mut total, mut total_w) = (vec![0.0; dim], 0.0);
    for (v, m) in z.iter().zip(meta) {
        for j in 0..dim {
            total[j] += v[j] * m.weight;
        }
        total_w += m.weight;
    }
    let mut out = OwnDeltas { deltas: Vec::new(), works: Vec::new(), weights: Vec::new(), cross_work: false };
    for i in 0..n {
        let (mut sum, mut sw) = (total.clone(), total_w);
        for o in i.saturating_sub(NEIGHBOURS)..(i + NEIGHBOURS + 1).min(n) {
            for j in 0..dim {
                sum[j] -= z[o][j] * meta[o].weight;
            }
            sw -= meta[o].weight;
        }
        if sw <= 1e-9 {
            continue;
        }
        let c: Vec<f64> = sum.iter().map(|x| x / sw).collect();
        out.deltas.push(cosine_delta(&z[i], &c));
        out.works.push(meta[i].work);
        out.weights.push(meta[i].weight);
    }
    out
}

/// What one band's fit measured, for the `calibrate` report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BandReport {
    pub words: usize,
    pub auc: f64,
    /// Class-balanced accuracy on held-out works and held-out imposter authors.
    pub holdout_accuracy: Option<f64>,
    pub holdout_brier: Option<f64>,
    pub c_at_1: Option<f64>,
    pub positives: usize,
    pub negatives: usize,
    pub cross_work: bool,
    /// Cosine Delta at which P(same author) = 0.5.
    pub delta_at_p50: f64,
}

/// Evenly thinned copy of at most `cap` values (keeps the spread).
fn thin(v: &[f64], cap: usize) -> Vec<f64> {
    if v.len() <= cap {
        return v.to_vec();
    }
    (0..cap).map(|i| v[i * v.len() / cap]).collect()
}

fn labelled(pos: &[f64], neg: &[f64]) -> Vec<(f64, bool)> {
    let mut s: Vec<(f64, bool)> = thin(pos, FIT_CAP).into_iter().map(|d| (d, true)).collect();
    s.extend(thin(neg, FIT_CAP).into_iter().map(|d| (d, false)));
    s
}

/// Held-out positives: whole works from the end of the corpus when the
/// passages are cross-work (at least one work stays in training), else a
/// contiguous tail (adjacent passages share topic; interleaving would leak).
fn split_positives(own: &OwnDeltas) -> (Vec<f64>, Vec<f64>) {
    if own.cross_work {
        // Works in first-appearance order (pooled windows can interleave).
        let mut order: Vec<u32> = Vec::new();
        for w in &own.works {
            if !order.contains(w) {
                order.push(*w);
            }
        }
        let want = (own.deltas.len() as f64 * TEST_FRACTION).ceil() as usize;
        let mut test_works: Vec<u32> = Vec::new();
        let mut count = 0;
        for w in order.iter().rev().take(order.len().saturating_sub(1)) {
            if count >= want {
                break;
            }
            test_works.push(*w);
            count += own.works.iter().filter(|x| *x == w).count();
        }
        let (mut tr, mut te) = (Vec::new(), Vec::new());
        for (d, w) in own.deltas.iter().zip(&own.works) {
            if test_works.contains(w) { te.push(*d) } else { tr.push(*d) }
        }
        return (tr, te);
    }
    tail_split(&own.deltas, TEST_FRACTION)
}

/// Fit one length band. `refs` are the non-blend profiles (target included)
/// that define the reference model and supply the imposters.
pub fn fit_band(target: &Profile, refs: &[Profile], words: usize) -> Option<(BandCalibration, BandReport)> {
    let model = ReferenceModel::build(refs, DEFAULT_MFW, DEFAULT_TRIGRAMS, words);
    let own = own_deltas(&model, target);
    let centroid = weighted_centroid(&model, target);
    // Negatives per imposter author, so whole authors can be held out.
    let by_imposter: Vec<Vec<f64>> = refs
        .iter()
        .filter(|p| p.name != target.name)
        .map(|p| {
            model
                .passage_zvectors(p)
                .iter()
                .map(|z| cosine_delta(z, &centroid))
                .collect::<Vec<f64>>()
        })
        .filter(|d| !d.is_empty())
        .collect();
    let neg: Vec<f64> = by_imposter.iter().flatten().copied().collect();
    if own.deltas.len() < MIN_POSITIVES || neg.len() < MIN_NEGATIVES {
        return None;
    }

    let (pos_train, pos_test) = split_positives(&own);
    let (mut neg_train, mut neg_test) = (Vec::new(), Vec::new());
    for (i, ds) in by_imposter.iter().enumerate() {
        if by_imposter.len() >= 3 {
            if i % 3 == 2 { neg_test.extend(ds) } else { neg_train.extend(ds) }
        } else {
            let (tr, te) = tail_split(ds, TEST_FRACTION);
            neg_train.extend(tr);
            neg_test.extend(te);
        }
    }
    let holdout = if !pos_test.is_empty() && !neg_test.is_empty() && pos_train.len() >= 2 {
        let (sl, ic) = fit_logistic(&labelled(&pos_train, &neg_train));
        let mut test: Vec<(f64, bool)> = pos_test.iter().map(|&d| (d, true)).collect();
        test.extend(neg_test.iter().map(|&d| (d, false)));
        Some(balanced_metrics(&test, sl, ic))
    } else {
        None
    };

    // Shipped mapping: refit on every sample (the holdout numbers above are
    // the honest report of how this generalizes).
    let (slope, intercept) = fit_logistic(&labelled(&own.deltas, &neg));
    let auc = auc(&own.deltas, &neg);
    let band = BandCalibration {
        words,
        slope,
        intercept,
        auc,
        holdout_accuracy: holdout.map(|h| h.0),
        holdout_brier: holdout.map(|h| h.1),
        c_at_1: holdout.map(|h| h.2),
        positives: own.deltas.len(),
        negatives: neg.len(),
        reference: model,
    };
    let r3 = |x: f64| crate::engine::score::round(x, 3);
    let report = BandReport {
        words: band.words,
        auc: r3(auc),
        holdout_accuracy: band.holdout_accuracy.map(r3),
        holdout_brier: band.holdout_brier.map(r3),
        c_at_1: band.c_at_1.map(r3),
        positives: band.positives,
        negatives: band.negatives,
        cross_work: own.cross_work,
        delta_at_p50: if slope.abs() > 1e-12 {
            crate::engine::score::round(-intercept / slope, 4)
        } else {
            f64::NAN
        },
    };
    Some((band, report))
}

/// The profiles that form the reference set: everything except blends (a
/// blend re-uses its components' passages; counting them twice would skew
/// the reference and make a component its own imposter).
pub fn reference_set(all: &[Profile]) -> Vec<Profile> {
    all.iter().filter(|p| p.kind != ProfileKind::Blend).cloned().collect()
}

/// Calibrate `target` against `refs` (non-blend profiles, target included)
/// at every band with enough data.
pub fn calibrate(target: &Profile, refs: &[Profile]) -> Result<(Calibration, Vec<BandReport>), AppError> {
    let bin = env!("CARGO_BIN_NAME");
    if target.kind == ProfileKind::Blend {
        let parts: Vec<&str> = target.components.iter().map(|(n, _)| n.as_str()).collect();
        return Err(AppError::input(
            format!("'{}' is a blend: a target style, not an author, so it has no P(same author)", target.name),
            format!("Calibrate its components instead: {bin} calibrate {}", parts.first().copied().unwrap_or("<author>")),
        ));
    }
    let imposters = refs.iter().filter(|p| p.name != target.name).count();
    if imposters == 0 {
        return Err(AppError::setup(
            "calibration needs other writers as imposters, and there are none",
            format!("Install the reference authors, then re-run: {bin} background fetch && {bin} calibrate {}", target.name),
        ));
    }
    let mut bands = Vec::new();
    let mut reports = Vec::new();
    for &words in bands_for(target.register) {
        if let Some((b, r)) = fit_band(target, refs, words) {
            bands.push(b);
            reports.push(r);
        }
    }
    if bands.is_empty() {
        let unit = if target.register.is_short_form() {
            "250-word windows of pooled posts (about 1,500+ words of posts in all)"
        } else {
            "passages"
        };
        return Err(AppError::input(
            format!(
                "not enough text to calibrate '{}': every length band needs {MIN_POSITIVES}+ of its {unit} and {MIN_NEGATIVES}+ imposter windows",
                target.name
            ),
            format!("Add more of the author's writing: {bin} profile build {} --corpus <folder> --force", target.name),
        ));
    }
    let cross_work = reports.iter().all(|r| r.cross_work);
    let cal = Calibration {
        bands,
        imposters,
        cross_work,
        ref_signature: model::reference_signature(refs, DEFAULT_MFW, DEFAULT_TRIGRAMS),
        created: now_unix(),
    };
    Ok((cal, reports))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_zero_is_half() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn auc_perfect_separation_is_one() {
        // smaller delta = same author (positive)
        assert!((auc(&[0.1, 0.2, 0.3], &[0.8, 0.9, 1.0]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn auc_reversed_is_zero() {
        assert!(auc(&[0.8, 0.9], &[0.1, 0.2]).abs() < 1e-12);
    }

    #[test]
    fn logistic_never_saturates_on_separable_data() {
        // The labelled deltas are perfectly separable here; without L2 the
        // slope diverges and probabilities hit exact 0/1. This pins the
        // regularization contract: confidence must stay finite and honest.
        let mut s: Vec<(f64, bool)> = Vec::new();
        for d in [0.10, 0.15, 0.20, 0.25] {
            s.push((d, true));
        }
        for d in [0.70, 0.80, 0.90, 1.00] {
            s.push((d, false));
        }
        let (slope, intercept) = fit_logistic(&s);
        for d in [0.05, 0.10, 0.5, 1.0, 1.2] {
            let p = probability(d, slope, intercept);
            assert!(
                (0.01..=0.99).contains(&p),
                "P(same) saturated to {p} at delta {d}"
            );
        }
    }

    #[test]
    fn tail_split_holds_out_the_tail() {
        let d: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let (train, test) = tail_split(&d, 0.3);
        assert_eq!(train.len(), 7);
        assert_eq!(test, vec![7.0, 8.0, 9.0]);
        // Too little data: no test set rather than a degenerate train set.
        let (train, test) = tail_split(&[1.0, 2.0], 0.3);
        assert_eq!(train.len(), 2);
        assert!(test.is_empty());
    }

    #[test]
    fn brier_uninformative_is_quarter() {
        // slope 0, intercept 0 -> always P=0.5 -> Brier 0.25 exactly.
        let s = [(0.1, true), (0.9, false)];
        assert!((balanced_brier(&s, 0.0, 0.0) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn c_at_1_rewards_abstention_over_wrong_answers() {
        let mut s: Vec<(f64, bool)> = Vec::new();
        for d in [0.10, 0.15, 0.20] {
            s.push((d, true));
        }
        for d in [0.80, 0.90, 1.00] {
            s.push((d, false));
        }
        let (slope, intercept) = fit_logistic(&s);
        let (acc, _, score) = balanced_metrics(&s, slope, intercept);
        assert!((acc - 1.0).abs() < 1e-12);
        // The regularized logistic keeps borderline points inside the
        // abstention band on 6 samples, so c@1 lands below plain accuracy
        // (1.0) but stays well above chance — abstaining is penalized less
        // than answering wrong.
        assert!(score > 0.8, "clean separation should score high, got {score}");
        // And abstaining on everything scores 0: nc = 0.
        let (_, _, all_abstain) = balanced_metrics(&s, 0.0, 0.0);
        assert!(all_abstain.abs() < 1e-12);
    }

    #[test]
    fn logistic_decreases_with_delta() {
        let mut s: Vec<(f64, bool)> = Vec::new();
        for d in [0.10, 0.15, 0.20, 0.25] {
            s.push((d, true));
        }
        for d in [0.70, 0.80, 0.90, 1.00] {
            s.push((d, false));
        }
        let (slope, intercept) = fit_logistic(&s);
        assert!(slope < 0.0, "bigger delta must mean lower P(same)");
        let p_small = probability(0.1, slope, intercept);
        let p_big = probability(1.0, slope, intercept);
        assert!(p_small > p_big);
        assert!(p_small > 0.5 && p_big < 0.5);
    }

    fn meta(sources: &[u32]) -> WindowMeta {
        WindowMeta { weight: 1.0, work: sources[0], sources: sources.to_vec() }
    }

    #[test]
    fn a_window_spanning_two_sources_never_meets_either_again() {
        // Windows over sources {0}, {0,1}, {1}, {2}. The pooled {0,1} window
        // must be scored against {2} alone: if any window of source 0 or 1
        // leaked into its centroid, the delta would drop below 1.
        let z = vec![vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0], vec![1.0, 0.0]];
        let m = vec![meta(&[0]), meta(&[0, 1]), meta(&[1]), meta(&[2])];
        let out = cross_work_deltas(&z, &m, 2);
        assert!(out.cross_work);
        assert_eq!(out.deltas.len(), 4);
        assert!((out.deltas[1] - 1.0).abs() < 1e-12, "pooled window leaked: {}", out.deltas[1]);
        // Source 0's window sees sources 1 and 2 (not the pooled {0,1}):
        // centroid (0.5, 0.5), delta 1 - cos 45°.
        let half = 1.0 - std::f64::consts::FRAC_1_SQRT_2;
        assert!((out.deltas[0] - half).abs() < 1e-12, "{}", out.deltas[0]);
        assert!((out.deltas[2] - half).abs() < 1e-12, "{}", out.deltas[2]);
        // Source 2's window sees sources 0 and 1 only.
        assert!((out.deltas[3] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn one_source_falls_back_to_neighbour_exclusion() {
        let z: Vec<Vec<f64>> = (0..8).map(|i| vec![1.0, i as f64 * 0.1]).collect();
        let m: Vec<WindowMeta> = (0..8).map(|_| meta(&[0])).collect();
        let out = neighbour_deltas(&z, &m, 2);
        assert!(!out.cross_work);
        assert_eq!(out.deltas.len(), 8);
    }

    #[test]
    fn short_form_bands_stop_at_1000_words() {
        assert_eq!(bands_for(Register::Social), &[250, 500, 1000]);
        assert_eq!(bands_for(Register::Email), &[250, 500, 1000]);
        assert_eq!(bands_for(Register::Fiction), &[250, 500, 1000, 2000, 4000]);
    }

    #[test]
    fn balanced_fit_is_not_swamped_by_the_bigger_class() {
        // 5 positives near 0.3, 200 negatives near 0.7: an unweighted fit
        // pulls P below 0.5 even at the positives' own deltas.
        let mut s: Vec<(f64, bool)> = [0.26, 0.28, 0.30, 0.32, 0.34].iter().map(|&d| (d, true)).collect();
        s.extend((0..200).map(|i| (0.6 + (i % 20) as f64 * 0.01, false)));
        let (slope, intercept) = fit_logistic(&s);
        assert!(probability(0.30, slope, intercept) > 0.5);
        assert!(probability(0.70, slope, intercept) < 0.5);
    }
}
