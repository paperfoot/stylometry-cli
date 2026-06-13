//! Turn a Cosine-Delta value into a calibrated P(same author), and measure how
//! well the verifier separates same-author from different-author samples.
//!
//! Labelled data: positives = the target's own held-out chunks (small delta to
//! its centroid); negatives = other profiles' chunks (large delta to the
//! target centroid). We fit a 1-D logistic on delta -> {same, different} and
//! report AUC and accuracy at the best threshold (PAN c@1 with no abstention).

#[inline]
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
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

/// Fit P(same) = sigmoid(intercept + slope * delta) by gradient descent on the
/// negative log-likelihood. Delta is standardized internally for stable steps,
/// then coefficients are mapped back to raw-delta units.
pub fn fit_logistic(samples: &[(f64, bool)]) -> (f64, f64) {
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

    // GD on standardized x.
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
        w1 -= lr * g1 / n;
    }
    // Map back: w0 + w1 * (d - mu)/sigma = (w0 - w1*mu/sigma) + (w1/sigma) * d
    let slope = w1 / sigma;
    let intercept = w0 - w1 * mu / sigma;
    (slope, intercept)
}

#[inline]
pub fn probability(delta: f64, slope: f64, intercept: f64) -> f64 {
    sigmoid(intercept + slope * delta)
}
