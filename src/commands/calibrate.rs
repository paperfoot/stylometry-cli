//! `calibrate`: fit a target profile's delta -> P(same author) mapping and
//! measure verification quality (AUC, accuracy) against the other profiles,
//! which serve as imposters / negatives.
//!
//! Positives use leave-one-out centroids so a chunk is never compared to a
//! centroid it helped define — that keeps the reported AUC honest.

use serde::Serialize;

use crate::engine::{
    DEFAULT_MFW, DEFAULT_TRIGRAMS, calibrate, delta,
    model::{self, ReferenceModel},
    profile::Calibration,
    store,
};
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct CalibrationReport {
    profile: String,
    auc: f64,
    accuracy: f64,
    threshold: f64,
    positives: usize,
    negatives: usize,
    imposters: usize,
}

pub fn run(ctx: Ctx, name: String) -> Result<(), AppError> {
    let target = store::load(&name)?;
    let all = store::load_all();
    let imposters: Vec<&_> = all.iter().filter(|p| p.name != target.name).collect();
    if imposters.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "calibration needs at least one other profile as a background/imposter. \
             Build another profile, then re-run: {} calibrate {}",
            env!("CARGO_PKG_NAME"),
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

    // Negatives: each imposter chunk vs the (full) target centroid.
    let mut neg_deltas = Vec::new();
    for imp in &imposters {
        for chunk in model.chunk_zvectors(imp) {
            neg_deltas.push(delta::cosine_delta(&chunk, &centroid));
        }
    }

    let auc = calibrate::auc(&pos_deltas, &neg_deltas);
    let (threshold, accuracy) = calibrate::best_threshold(&pos_deltas, &neg_deltas);

    let mut samples: Vec<(f64, bool)> = Vec::new();
    samples.extend(pos_deltas.iter().map(|&d| (d, true)));
    samples.extend(neg_deltas.iter().map(|&d| (d, false)));
    let (slope, intercept) = calibrate::fit_logistic(&samples);

    let mut updated = target;
    updated.calibration = Some(Calibration {
        slope,
        intercept,
        threshold,
        auc,
        c_at_1: accuracy,
        imposters: imposters.len(),
        ref_signature: model::reference_signature(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS),
    });
    store::save(&updated)?;

    let report = CalibrationReport {
        profile: updated.name.clone(),
        auc,
        accuracy,
        threshold,
        positives: pos_deltas.len(),
        negatives: neg_deltas.len(),
        imposters: imposters.len(),
    };
    output::print_success_or(ctx, &report, |r| {
        use owo_colors::OwoColorize;
        println!("Calibrated {}", r.profile.green().bold());
        println!(
            "  AUC {:.3}   accuracy {:.3}   threshold δ={:.4}",
            r.auc, r.accuracy, r.threshold
        );
        println!(
            "  {} positives (own chunks), {} negatives from {} imposter(s)",
            r.positives, r.negatives, r.imposters
        );
    });
    Ok(())
}
