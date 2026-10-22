//! `calibrate`: fit a profile's P(same author) per length band against the
//! background and other profiles, and report how well each band verifies on
//! data it was not fit on. See `engine::calibrate` for the method.

use serde::Serialize;

use crate::engine::calibrate::{self, BandReport, MIN_IMPOSTERS};
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct Report {
    profile: String,
    register: &'static str,
    /// What a band's windows are made of: book/blog passages, or posts
    /// pooled until the window reaches its length.
    unit: &'static str,
    imposters: usize,
    /// Positives were scored against centroids built from the author's other works.
    cross_work: bool,
    bands: Vec<BandReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub fn run(ctx: Ctx, name: String) -> Result<(), AppError> {
    let bin = env!("CARGO_BIN_NAME");
    let target = store::load(&name)?;
    let refs = calibrate::reference_set(&store::load_all());
    let (cal, bands) = calibrate::calibrate(&target, &refs)?;

    let mut warnings = Vec::new();
    if cal.imposters < MIN_IMPOSTERS {
        warnings.push(format!(
            "only {} imposter profile(s): P(same author) is fit against too few other writers and will accept unseen authors too easily; install the reference authors with `{bin} background fetch`",
            cal.imposters
        ));
    }
    let short_form = target.register.is_short_form();
    if !cal.cross_work {
        warnings.push(if short_form {
            "positives come from the same block of posts as their centroid (undated posts, or too few per quarter): accuracy on new posts is likely lower than reported; build from dated posts spanning two or more quarters".into()
        } else {
            "positives come from the same work as their centroid (one work, or too little per work): accuracy on a new book by this author is likely lower than reported; build the profile from two or more works".into()
        });
    }
    let short_imposters = refs
        .iter()
        .filter(|p| p.name != target.name && p.register.is_short_form())
        .count();
    if short_form && short_imposters < 3 {
        warnings.push(format!(
            "only {short_imposters} short-form imposter(s): the background is books, so another person's posts can pass as same_author; add 3+ other writers' posts with `{bin} profile build <name> --corpus <their-posts.json> --background`, then re-calibrate"
        ));
    }
    let mut updated = target;
    let report = Report {
        profile: updated.name.clone(),
        register: updated.register.as_str(),
        unit: if short_form { "pooled posts" } else { "passages" },
        imposters: cal.imposters,
        cross_work: cal.cross_work,
        bands,
        warnings,
    };
    updated.calibration = Some(cal);
    store::save(&updated)?;

    // Posts verify only pooled: the file holds 10+ posts separated by --- lines.
    let what = if short_form { "<posts-separated-by-dashes.md>" } else { "<file>" };
    let mut next = vec![format!("{bin} compare {what} --profile {}", report.profile)];
    if report.imposters < MIN_IMPOSTERS {
        next.insert(0, format!("{bin} background fetch"));
    }
    output::print_success_with(ctx, &report, &next, |r| {
        use owo_colors::OwoColorize;
        println!(
            "Calibrated {} ({}, {}) against {} imposter(s){}",
            r.profile.green().bold(),
            r.register,
            r.unit,
            r.imposters,
            if r.cross_work { ", cross-work" } else { "" }
        );
        println!("  words    AUC  holdout-acc  Brier   c@1   pos/neg");
        let f = |x: Option<f64>| x.map(|v| format!("{v:.3}")).unwrap_or_else(|| "  -  ".into());
        for b in &r.bands {
            println!(
                "  {:>5}  {:.3}  {:>11}  {:>5}  {:>5}  {}/{}",
                b.words,
                b.auc,
                f(b.holdout_accuracy),
                f(b.holdout_brier),
                f(b.c_at_1),
                b.positives,
                b.negatives
            );
        }
        for w in &r.warnings {
            println!("  {} {}", "warning:".yellow().bold(), w.yellow());
        }
    })
}
