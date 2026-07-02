//! Turn a Cosine-Delta value into a calibrated P(same author), and measure how
//! well the verifier separates same-author from different-author samples.
//!
//! Labelled data: positives = the target's own held-out chunks (small delta to
//! its centroid); negatives = other profiles' chunks (large delta to the
//! target centroid). We fit a 1-D logistic on delta -> {same, different} and
//! report AUC and accuracy at the best threshold (PAN c@1 with no abstention).

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

/// Brier score of the logistic on labelled deltas: mean (P(same) - y)^2.
/// 0 is perfect, 0.25 is what always-saying-50% scores.
pub fn brier(samples: &[(f64, bool)], slope: f64, intercept: f64) -> f64 {
    if samples.is_empty() {
        return 0.25;
    }
    let sum: f64 = samples
        .iter()
        .map(|&(d, y)| {
            let p = probability(d, slope, intercept);
            let t = if y { 1.0 } else { 0.0 };
            (p - t) * (p - t)
        })
        .sum();
    sum / samples.len() as f64
}

/// PAN c@1: accuracy that rewards abstention. With n samples, nc correct and
/// nu unanswered (probability inside the abstention band):
/// c@1 = (nc + nu * nc / n) / n.
pub fn c_at_1(samples: &[(f64, bool)], slope: f64, intercept: f64, threshold: f64) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let mut nc = 0usize;
    let mut nu = 0usize;
    for &(d, y) in samples {
        let p = probability(d, slope, intercept);
        if (P_ABSTAIN_LOW..=P_ABSTAIN_HIGH).contains(&p) {
            nu += 1;
        } else if (d <= threshold) == y {
            nc += 1;
        }
    }
    let n = n as f64;
    let nc = nc as f64;
    let nu = nu as f64;
    (nc + nu * nc / n) / n
}

/// Accuracy of a fixed threshold on labelled deltas (same = delta <= t).
pub fn accuracy_at(pos: &[f64], neg: &[f64], t: f64) -> f64 {
    let total = pos.len() + neg.len();
    if total == 0 {
        return 0.0;
    }
    let tp = pos.iter().filter(|&&d| d <= t).count();
    let tn = neg.iter().filter(|&&d| d > t).count();
    (tp + tn) as f64 / total as f64
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

/// Best delta threshold (Youden's J: maximize TPR - FPR) and the accuracy
/// there. "Positive" = same author = delta <= threshold.
pub fn best_threshold(pos_deltas: &[f64], neg_deltas: &[f64]) -> (f64, f64) {
    let mut candidates: Vec<f64> = pos_deltas.iter().chain(neg_deltas.iter()).copied().collect();
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    candidates.dedup();
    let np = pos_deltas.len() as f64;
    let nn = neg_deltas.len() as f64;
    let mut best_t = candidates.first().copied().unwrap_or(0.0);
    let mut best_j = f64::NEG_INFINITY;
    let mut best_acc = 0.0;
    for &t in &candidates {
        let tp = pos_deltas.iter().filter(|&&d| d <= t).count() as f64;
        let fp = neg_deltas.iter().filter(|&&d| d <= t).count() as f64;
        let tpr = tp / np;
        let fpr = fp / nn;
        let j = tpr - fpr;
        if j > best_j {
            best_j = j;
            best_t = t;
            let tn = nn - fp;
            best_acc = (tp + tn) / (np + nn);
        }
    }
    (best_t, best_acc)
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

/// Fit P(same) = sigmoid(intercept + slope * delta) with a fixed L2 penalty
/// on the slope, by gradient descent on the negative log-likelihood. Delta is
/// standardized internally for stable steps, then coefficients are mapped
/// back to raw-delta units. The penalty exists because the labelled deltas
/// are often linearly separable, which drives an unregularized slope to
/// infinity and yields fake 0/1 "probabilities".
pub fn fit_logistic_with(samples: &[(f64, bool)], lambda: f64) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let n = samples.len() as f64;
    let mu = samples.iter().map(|(d, _)| d).sum::<f64>() / n;
    let var = samples.iter().map(|(d, _)| (d - mu).powi(2)).sum::<f64>() / n;
    let sigma = if var > 1e-12 { var.sqrt() } else { 1.0 };

    let xs: Vec<(f64, f64)> = samples
        .iter()
        .map(|(d, same)| ((d - mu) / sigma, if *same { 1.0 } else { 0.0 }))
        .collect();

    let (mut w0, mut w1) = (0.0f64, 0.0f64);
    let lr = 0.3;
    for _ in 0..4000 {
        let (mut g0, mut g1) = (0.0f64, 0.0f64);
        for &(x, y) in &xs {
            let p = sigmoid(w0 + w1 * x);
            let err = p - y;
            g0 += err;
            g1 += err * x;
        }
        w0 -= lr * g0 / n;
        w1 -= lr * (g1 / n + lambda * w1);
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
            score_sum += brier(&test, sl, ic);
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
    fn best_threshold_separates_cleanly() {
        let (t, acc) = best_threshold(&[0.1, 0.2, 0.3], &[0.7, 0.8, 0.9]);
        assert!((0.3..0.7).contains(&t));
        assert!((acc - 1.0).abs() < 1e-12);
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
        assert!((brier(&s, 0.0, 0.0) - 0.25).abs() < 1e-12);
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
        let score = c_at_1(&s, slope, intercept, 0.5);
        // The regularized logistic keeps borderline points inside the
        // abstention band on 6 samples, so c@1 lands below plain accuracy
        // (1.0) but stays well above chance — abstaining is penalized less
        // than answering wrong.
        assert!(score > 0.8, "clean separation should score high, got {score}");
        // And abstaining on everything scores 0: nc = 0.
        let all_abstain = c_at_1(&s, 0.0, 0.0, 0.5);
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
}
