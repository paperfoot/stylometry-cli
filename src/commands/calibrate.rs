//! `calibrate`: fit a target profile's delta -> P(same author) mapping and
//! measure verification quality (AUC, holdout accuracy, Brier, c@1) against
//! the other profiles, which serve as imposters / negatives.
//!
//! Honesty rules:
//! - Positives use leave-one-out centroids so a chunk is never compared to a
//!   centroid it helped define.
//! - The threshold is selected on a train split and its accuracy reported on
//!   a held-out tail split it never saw (`holdout_accuracy`). The shipped
//!   threshold/logistic are then refit on all data.
//! - The reference model (vocab + mean/sd) is frozen into the calibration so
//!   later profile changes never silently shift this profile's z-space.

use serde::Serialize;

use crate::engine::{
    DEFAULT_MFW, DEFAULT_TRIGRAMS, calibrate, delta,
    model::{self, ReferenceModel},
    profile::Calibration,
    store,
};
use crate::error::AppError;
use crate::output::{self, Ctx};

/// Fraction of each delta series held out (as a contiguous tail) to measure
/// generalization of the train-selected threshold.
const TEST_FRACTION: f64 = 0.3;
/// Below this many imposter profiles the negatives come from too few authors
/// for the probabilities to generalize; warn.
const MIN_IMPOSTERS_FOR_TRUST: usize = 3;

#[derive(Serialize)]
struct CalibrationReport {
    profile: String,
    auc: f64,
    /// Accuracy of the train-selected threshold on the holdout tail. Falls
    /// back to fit accuracy (flagged by holdout=false) with tiny data.
    accuracy: f64,
    holdout: bool,
    holdout_brier: Option<f64>,
    c_at_1: f64,
    threshold: f64,
    positives: usize,
    negatives: usize,
    imposters: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

pub fn run(ctx: Ctx, name: String) -> Result<(), AppError> {
    let target = store::load(&name)?;
    let all = store::load_all();
    let imposters: Vec<&_> = all.iter().filter(|p| p.name != target.name).collect();
    if imposters.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "calibration needs at least one other profile as a background/imposter. \
             Build another profile, then re-run: {} calibrate {}",
            env!("CARGO_BIN_NAME"),
            name
        )));
    }

    let model = ReferenceModel::build(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS);
    let dim = model.vocab.dim();

    // Target chunks and full centroid.
    let tz = model.chunk_zvectors(&target);
    let n = tz.len();
    let centroid = model::mean_vec(&tz, dim);

    // Positives: leave-one-out delta of each target chunk to the target centroid.
    let mut pos_deltas = Vec::with_capacity(n);
    if n >= 2 {
        for chunk in &tz {
            let mut loo = vec![0.0; dim];
            for j in 0..dim {
                loo[j] = (centroid[j] * n as f64 - chunk[j]) / (n as f64 - 1.0);
            }
            pos_deltas.push(delta::cosine_delta(chunk, &loo));
        }
    }

    // Negatives: each imposter chunk vs the (full) target centroid, kept
    // per-imposter so the tail split holds out a tail of EVERY imposter
    // rather than dropping whole authors from the train side.
    let mut neg_by_imposter: Vec<Vec<f64>> = Vec::new();
    for imp in &imposters {
        let mut ds = Vec::new();
        for chunk in model.chunk_zvectors(imp) {
            ds.push(delta::cosine_delta(&chunk, &centroid));
        }
        neg_by_imposter.push(ds);
    }
    let neg_deltas: Vec<f64> = neg_by_imposter.iter().flatten().copied().collect();

    // Train/test: contiguous tails (chunks are sequential text; interleaving
    // would leak adjacent-chunk topic into the holdout).
    let (pos_train, pos_test) = calibrate::tail_split(&pos_deltas, TEST_FRACTION);
    let (mut neg_train, mut neg_test) = (Vec::new(), Vec::new());
    for ds in &neg_by_imposter {
        let (tr, te) = calibrate::tail_split(ds, TEST_FRACTION);
        neg_train.extend(tr);
        neg_test.extend(te);
    }
    let have_holdout = !pos_test.is_empty() && !neg_test.is_empty();

    // AUC on everything (no threshold selection involved, so no leakage).
    let auc = calibrate::auc(&pos_deltas, &neg_deltas);

    // Honest generalization numbers: select on train, measure on test.
    let (holdout_accuracy, holdout_brier, holdout_c_at_1) = if have_holdout {
        let (t_train, _) = calibrate::best_threshold(&pos_train, &neg_train);
        let mut train_samples: Vec<(f64, bool)> = Vec::new();
        train_samples.extend(pos_train.iter().map(|&d| (d, true)));
        train_samples.extend(neg_train.iter().map(|&d| (d, false)));
        let (sl, ic) = calibrate::fit_logistic(&train_samples);

        let mut test_samples: Vec<(f64, bool)> = Vec::new();
        test_samples.extend(pos_test.iter().map(|&d| (d, true)));
        test_samples.extend(neg_test.iter().map(|&d| (d, false)));

        (
            Some(calibrate::accuracy_at(&pos_test, &neg_test, t_train)),
            Some(calibrate::brier(&test_samples, sl, ic)),
            Some(calibrate::c_at_1(&test_samples, sl, ic, t_train)),
        )
    } else {
        (None, None, None)
    };

    // Shipped calibration: refit on all data (standard practice — metrics
    // above are the honest report; the final model uses every sample).
    let (threshold, fit_accuracy) = calibrate::best_threshold(&pos_deltas, &neg_deltas);
    let mut samples: Vec<(f64, bool)> = Vec::new();
    samples.extend(pos_deltas.iter().map(|&d| (d, true)));
    samples.extend(neg_deltas.iter().map(|&d| (d, false)));
    let (slope, intercept) = calibrate::fit_logistic(&samples);

    let accuracy = holdout_accuracy.unwrap_or(fit_accuracy);
    let c1 = holdout_c_at_1
        .unwrap_or_else(|| calibrate::c_at_1(&samples, slope, intercept, threshold));

    let mut warnings: Vec<String> = Vec::new();
    if imposters.len() < MIN_IMPOSTERS_FOR_TRUST {
        warnings.push(format!(
            "only {} imposter profile(s): P(same author) is calibrated against too few \
             other authors and will be overconfident — build at least {} profiles",
            imposters.len(),
            MIN_IMPOSTERS_FOR_TRUST + 1
        ));
    }
    if !have_holdout {
        warnings.push(
            "too few chunks for a holdout split: accuracy is measured on the same data \
             the threshold was fit on (optimistic)"
                .to_string(),
        );
    }
    let warning = if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("; "))
    };

    let mut updated = target;
    updated.calibration = Some(Calibration {
        slope,
        intercept,
        threshold,
        auc,
        c_at_1: c1,
        imposters: imposters.len(),
        ref_signature: model::reference_signature(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS),
        holdout_accuracy,
        holdout_brier,
        chunk_words: Some(updated.chunk_size),
        frozen_reference: Some(model.clone()),
    });
    store::save(&updated)?;

    let report = CalibrationReport {
        profile: updated.name.clone(),
        auc,
        accuracy,
        holdout: have_holdout,
        holdout_brier,
        c_at_1: c1,
        threshold,
        positives: pos_deltas.len(),
        negatives: neg_deltas.len(),
        imposters: imposters.len(),
        warning,
    };
    output::print_success_or(ctx, &report, |r| {
        use owo_colors::OwoColorize;
        println!("Calibrated {}", r.profile.green().bold());
        let acc_label = if r.holdout { "holdout accuracy" } else { "fit accuracy" };
        println!(
            "  AUC {:.3}   {} {:.3}   c@1 {:.3}   threshold δ={:.4}",
            r.auc, acc_label, r.accuracy, r.c_at_1, r.threshold
        );
        if let Some(b) = r.holdout_brier {
            println!("  holdout Brier {:.3} (0 = perfect, 0.25 = uninformative)", b);
        }
        println!(
            "  {} positives (own chunks), {} negatives from {} imposter(s)",
            r.positives, r.negatives, r.imposters
        );
        if let Some(w) = &r.warning {
            println!("  {} {}", "warning:".yellow().bold(), w.yellow());
        }
    });
    Ok(())
}
