//! `compare`: authorship verification — was this text written by the
//! profile's author? Scored at the text's own length band; the verdict
//! follows P(same author). See `engine::score`.

use serde::Serialize;

use crate::commands::input;
use crate::engine::profile::ProfileKind;
use crate::engine::score::{self, VoiceMatch};
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct Report {
    profile: String,
    #[serde(flatten)]
    m: VoiceMatch,
}

pub fn run(
    ctx: Ctx,
    profile: String,
    file: Option<std::path::PathBuf>,
    text: Option<String>,
) -> Result<(), AppError> {
    let bin = env!("CARGO_BIN_NAME");
    let text = input::read_text(file, text, "compare")?;
    let target = store::load(&profile)?;
    let all = store::load_all();
    let m = score::voice_match(&target, &all, &text)?;

    let mut next = Vec::new();
    if !all.iter().any(|p| p.kind == ProfileKind::Background) {
        next.push(format!("{bin} background fetch"));
    }
    if target.kind != ProfileKind::Blend && (target.calibration.is_none() || m.calibration_stale) {
        next.push(format!("{bin} calibrate {profile}"));
    }
    if !m.passages.is_empty() {
        next.push(format!("{bin} check <file> --profile {profile}"));
    }
    let report = Report { profile, m };
    output::print_success_with(ctx, &report, &next, |r| {
        use owo_colors::OwoColorize;
        let m = &r.m;
        let p = m
            .p_same_author
            .map(|p| format!("P(same author) {p:.2}, "))
            .unwrap_or_default();
        let pooled = m.items.map(|n| format!(" ({n} items pooled)")).unwrap_or_default();
        println!("{} words{pooled} vs {}: {}", m.words, r.profile, m.verdict.bold());
        println!(
            "  {p}δ {:.3} at {} words, typicality {:.2}, nearest {}",
            m.cosine_delta, m.band_words, m.typicality, m.nearest_profile
        );
        if !m.passages.is_empty() {
            let off = m
                .passages
                .iter()
                .filter(|s| s.nearest_profile != r.profile)
                .count();
            println!(
                "  {} passages; {} nearer another profile (see --json passages)",
                m.passages.len(),
                off
            );
        }
        for w in &m.warnings {
            println!("  {} {}", "warning:".yellow().bold(), w.yellow());
        }
    })
}
