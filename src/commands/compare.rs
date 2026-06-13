//! `compare`: score a text against a target profile and return a verdict.
//!
//! Reports Cosine Delta and Classic Burrows Delta to the target, the nearest
//! profile overall, a General-Imposters score (how much closer the text is to
//! the target than to the other profiles), and — when the target is calibrated
//! — a P(same author) and a same/different verdict.

use std::path::PathBuf;

use serde::Serialize;

use crate::engine::{DEFAULT_MFW, DEFAULT_TRIGRAMS, calibrate, delta, model::ReferenceModel, store};
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct Ranked {
    profile: String,
    cosine_delta: f64,
}

#[derive(Serialize)]
struct CompareResult {
    profile: String,
    cosine_delta: f64,
    classic_delta: f64,
    nearest_profile: String,
    nearest_cosine_delta: f64,
    gi_score: Option<f64>,
    p_same_author: Option<f64>,
    verdict: String,
    ranking: Vec<Ranked>,
}

pub fn run(
    ctx: Ctx,
    profile: String,
    file: Option<PathBuf>,
    text: Option<String>,
) -> Result<(), AppError> {
    let query_text = match (file, text) {
        (Some(p), _) => crate::engine::text::read_corpus(&p)?,
        (None, Some(t)) => t,
        (None, None) => {
            return Err(AppError::InvalidInput(
                "provide a text to compare: a file path argument or --text \"...\"".into(),
            ));
        }
    };
    if query_text.split_whitespace().count() < 50 {
        return Err(AppError::InvalidInput(
            "text is very short (<50 words); stylometry is unreliable below a few hundred words"
                .into(),
        ));
    }

    let target = store::load(&profile)?;
    let all = store::load_all();
    if all.is_empty() {
        return Err(AppError::Config("no profiles found".into()));
    }
    let model = ReferenceModel::build(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS);

    let qz = model.zscore(&model.vectorize_text(&query_text));

    // Delta to every profile centroid.
    let mut ranking: Vec<Ranked> = Vec::new();
    let mut target_cosine = f64::INFINITY;
    let mut target_classic = f64::INFINITY;
    let mut imposter_deltas: Vec<f64> = Vec::new();
    for p in &all {
        let c = model.centroid(p);
        let cos = delta::cosine_delta(&qz, &c);
        if p.name == target.name {
            target_cosine = cos;
            target_classic = delta::classic_delta(&qz, &c);
        } else {
            imposter_deltas.push(cos);
        }
        ranking.push(Ranked {
            profile: p.name.clone(),
            cosine_delta: cos,
        });
    }
    ranking.sort_by(|a, b| a.cosine_delta.partial_cmp(&b.cosine_delta).unwrap());
    let nearest = ranking[0].profile.clone();
    let nearest_delta = ranking[0].cosine_delta;

    let gi_score = if imposter_deltas.is_empty() {
        None
    } else {
        let farther = imposter_deltas.iter().filter(|&&d| d > target_cosine).count();
        Some(farther as f64 / imposter_deltas.len() as f64)
    };

    let (p_same, verdict) = match &target.calibration {
        Some(cal) => {
            let p = calibrate::probability(target_cosine, cal.slope, cal.intercept);
            let v = if target_cosine <= cal.threshold {
                "same_author"
            } else {
                "different_author"
            };
            (Some(p), v.to_string())
        }
        None => {
            let v = match gi_score {
                Some(g) if g >= 0.5 && nearest == target.name => "same_author_uncalibrated",
                Some(_) => "different_author_uncalibrated",
                None => "insufficient_background",
            };
            (None, v.to_string())
        }
    };

    let data = CompareResult {
        profile: target.name.clone(),
        cosine_delta: target_cosine,
        classic_delta: target_classic,
        nearest_profile: nearest,
        nearest_cosine_delta: nearest_delta,
        gi_score,
        p_same_author: p_same,
        verdict,
        ranking,
    };
    output::print_success_or(ctx, &data, |d| {
        use owo_colors::OwoColorize;
        let verdict_c = if d.verdict.starts_with("same") {
            d.verdict.green().to_string()
        } else {
            d.verdict.yellow().to_string()
        };
        println!("vs profile {}: {}", d.profile.bold(), verdict_c);
        println!(
            "  cosine δ {:.4}   classic δ {:.3}   nearest: {} ({:.4})",
            d.cosine_delta, d.classic_delta, d.nearest_profile, d.nearest_cosine_delta
        );
        if let Some(p) = d.p_same_author {
            println!("  P(same author) = {:.3}", p);
        }
        if let Some(g) = d.gi_score {
            println!("  GI score = {:.2} (closer to target than {:.0}% of imposters)", g, g * 100.0);
        }
    });
    Ok(())
}
