//! `compare`: score a text against a target profile and return a verdict.
//!
//! Reports Cosine Delta and Classic Burrows Delta to the target, the nearest
//! profile overall, a background-rank score (how much closer the text is to the
//! target than to the other profiles — a simple rank fraction, NOT full
//! Koppel-Winter General Imposters), and — when the target is calibrated — a
//! P(same author) with a same/different/inconclusive verdict.
//!
//! Calibrated profiles carry a FROZEN reference model (vocab + mean/sd), so
//! the verdict is computed in the exact z-space the calibration was fit in and
//! never shifts when profiles are added or removed. Legacy calibrations
//! (pre-freeze) still fall back to the live reference and go stale when the
//! profile set changes.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use serde::Serialize;

use crate::engine::{
    DEFAULT_MFW, DEFAULT_TRIGRAMS, calibrate, delta, model,
    model::ReferenceModel, store, text,
};
use crate::error::AppError;
use crate::output::{self, Ctx};

/// Query texts shorter than half or longer than twice the calibration chunk
/// length get a length-mismatch warning: delta variance depends on text
/// length, so P(same author) is only calibrated near the fitted length.
const LENGTH_RATIO_LOW: f64 = 0.5;
const LENGTH_RATIO_HIGH: f64 = 2.0;

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
    /// Fraction of imposter profiles farther from the text than the target is.
    /// Simple rank fraction, not full General Imposters.
    background_rank: Option<f64>,
    p_same_author: Option<f64>,
    /// True if the profile set changed since calibration. With a frozen
    /// reference the calibrated verdict itself remains valid, but the imposter
    /// pool it was calibrated against has drifted — re-calibrate to account
    /// for it. Legacy calibrations (no frozen reference) lose their calibrated
    /// verdict entirely when stale.
    calibration_stale: bool,
    /// Unicode word count of the query text.
    query_words: usize,
    /// True when the query length is far from the calibration chunk length,
    /// so the calibrated probability is unreliable.
    length_mismatch: bool,
    verdict: String,
    ranking: Vec<Ranked>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

fn read_query_text(file: Option<PathBuf>, text_arg: Option<String>) -> Result<String, AppError> {
    match (file, text_arg) {
        (Some(p), _) if p.as_os_str() == "-" => read_stdin(),
        (Some(p), _) => crate::engine::text::read_corpus(&p),
        (None, Some(t)) => Ok(t),
        (None, None) => {
            if std::io::stdin().is_terminal() {
                Err(AppError::InvalidInput(
                    "provide a text to compare: a file path argument, '-' or piped stdin, \
                     or --text \"...\""
                        .into(),
                ))
            } else {
                read_stdin()
            }
        }
    }
}

fn read_stdin() -> Result<String, AppError> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| AppError::InvalidInput(format!("failed to read stdin: {e}")))?;
    if buf.trim().is_empty() {
        return Err(AppError::InvalidInput("stdin was empty".into()));
    }
    Ok(buf)
}

pub fn run(
    ctx: Ctx,
    profile: String,
    file: Option<PathBuf>,
    text_arg: Option<String>,
) -> Result<(), AppError> {
    let query_text = read_query_text(file, text_arg)?;
    let query_words = text::count_words(&query_text);
    if query_words < 50 {
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

    // Use the frozen reference from the target's calibration when present:
    // the calibrated z-space, unaffected by later profile changes. Otherwise
    // build the live reference from all loaded profiles.
    let frozen = target
        .calibration
        .as_ref()
        .and_then(|c| c.frozen_reference.clone());
    let has_frozen = frozen.is_some();
    let model = match frozen {
        Some(m) => m,
        None => ReferenceModel::build(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS),
    };

    let qz = model.zscore(&model.vectorize_text(&query_text));

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

    let background_rank = if imposter_deltas.is_empty() {
        None
    } else {
        let farther = imposter_deltas.iter().filter(|&&d| d > target_cosine).count();
        Some(farther as f64 / imposter_deltas.len() as f64)
    };

    // Signature drift: with a frozen reference this only means the imposter
    // pool changed since calibration; without one it invalidates the verdict.
    let current_sig = model::reference_signature(&all, DEFAULT_MFW, DEFAULT_TRIGRAMS);
    let sig_matches = target
        .calibration
        .as_ref()
        .map(|c| c.ref_signature == current_sig)
        .unwrap_or(false);
    let calibration_stale = target.calibration.is_some() && !sig_matches;
    let calibration_usable = target.calibration.is_some() && (has_frozen || sig_matches);

    // Length mismatch vs the calibration chunk length.
    let cal_words = target.calibration.as_ref().and_then(|c| c.chunk_words);
    let length_mismatch = match cal_words {
        Some(cw) if cw > 0 => {
            let ratio = query_words as f64 / cw as f64;
            !(LENGTH_RATIO_LOW..=LENGTH_RATIO_HIGH).contains(&ratio)
        }
        _ => false,
    };

    let (p_same, verdict) = if calibration_usable {
        let cal = target.calibration.as_ref().unwrap();
        let p = calibrate::probability(target_cosine, cal.slope, cal.intercept);
        let v = if (calibrate::P_ABSTAIN_LOW..=calibrate::P_ABSTAIN_HIGH).contains(&p) {
            "inconclusive"
        } else if target_cosine <= cal.threshold {
            "same_author"
        } else {
            "different_author"
        };
        (Some(p), v.to_string())
    } else {
        let v = match background_rank {
            Some(g) if g >= 0.5 && nearest == target.name => "same_author_uncalibrated",
            Some(_) => "different_author_uncalibrated",
            None => "insufficient_background",
        };
        (None, v.to_string())
    };

    let mut warnings: Vec<String> = Vec::new();
    if length_mismatch {
        if let Some(cw) = cal_words {
            warnings.push(format!(
                "text is {} words but the calibration was fit on ~{}-word chunks; \
                 P(same author) is unreliable at this length",
                query_words, cw
            ));
        }
    }
    if calibration_stale && calibration_usable {
        warnings.push(
            "profile set changed since calibration; the frozen verdict is still valid but \
             the imposter pool drifted — re-run calibrate"
                .to_string(),
        );
    }
    let warning = if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("; "))
    };

    let data = CompareResult {
        profile: target.name.clone(),
        cosine_delta: target_cosine,
        classic_delta: target_classic,
        nearest_profile: nearest,
        nearest_cosine_delta: nearest_delta,
        background_rank,
        p_same_author: p_same,
        calibration_stale,
        query_words,
        length_mismatch,
        verdict,
        ranking,
        warning,
    };
    output::print_success_or(ctx, &data, |d| {
        use owo_colors::OwoColorize;
        let verdict_c = if d.verdict.starts_with("same") {
            d.verdict.green().to_string()
        } else if d.verdict == "inconclusive" {
            d.verdict.yellow().bold().to_string()
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
        if let Some(g) = d.background_rank {
            println!(
                "  background rank = {:.2} (closer to target than {:.0}% of imposters)",
                g,
                g * 100.0
            );
        }
        if d.calibration_stale && d.p_same_author.is_none() {
            println!(
                "  {}",
                "calibration is stale (profile set changed) — re-run calibrate".yellow()
            );
        }
        if let Some(w) = &d.warning {
            println!("  {} {}", "warning:".yellow().bold(), w.yellow());
        }
    });
    Ok(())
}
