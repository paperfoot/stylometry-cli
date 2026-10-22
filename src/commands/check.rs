//! `check`: draft feedback against a voice. Findings come most important
//! first, each with a line and a concrete fix; the summary says whether the
//! draft passes (no hard failures, at most a fifth of metrics out of range)
//! and how close it sits to the voice overall.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::commands::input;
use crate::engine::checks::{self, Finding, MetricRow};
use crate::engine::metrics::Status;
use crate::engine::score::{self, VoiceMatch};
use crate::engine::{store, text};
use crate::error::AppError;
use crate::output::{self, Ctx};

pub struct CheckArgs {
    pub profile: String,
    pub file: Option<PathBuf>,
    pub text: Option<String>,
    pub limit: usize,
    /// Banned word/phrase files (one per line); any hit is a hard failure.
    pub avoid: Vec<PathBuf>,
    /// Extra profiles whose stored text the draft must not copy.
    pub guard: Vec<String>,
    /// Gate mode: a failed check returns `AppError::CheckFailed(report)`.
    pub strict: bool,
}

/// Out-of-range metrics allowed before the draft fails.
const MAX_OUT_OF_RANGE: f64 = 0.2;

#[derive(Debug, Serialize)]
pub struct Summary {
    pub pass: bool,
    pub hard_failures: usize,
    /// Metrics not clearly outside the author's range, e.g. "41/86".
    pub metrics_in_range: String,
    pub counts: BTreeMap<&'static str, usize>,
    pub voice_match: Option<VoiceMatch>,
}

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub profile: String,
    pub words: usize,
    /// Window length the author's ranges were measured at (the draft's length).
    pub window_words: usize,
    /// Author windows of that length behind the ranges.
    pub windows: usize,
    /// Few windows (or a short draft): treat ranges as rough guidance.
    pub directional: bool,
    pub summary: Summary,
    pub findings: Vec<Finding>,
    /// Findings left out by --limit.
    pub findings_omitted: usize,
    pub metrics: Vec<MetricRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn read_avoid(files: &[PathBuf]) -> Result<Vec<String>, AppError> {
    let mut out = Vec::new();
    for f in files {
        let raw = std::fs::read_to_string(f).map_err(|e| {
            AppError::input(
                format!("can't read avoid list {}: {e}", f.display()),
                "Pass a text file with one word or phrase per line: --avoid banned.txt",
            )
        })?;
        out.extend(
            raw.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_lowercase),
        );
    }
    Ok(out)
}

pub fn run(ctx: Ctx, args: CheckArgs) -> Result<(), AppError> {
    let bin = env!("CARGO_BIN_NAME");
    let draft = input::read_text(args.file.clone(), args.text.clone(), "check")?;
    let target = input::resolve_profile(&args.profile)?;
    let guards = args
        .guard
        .iter()
        .map(|g| input::resolve_profile(g))
        .collect::<Result<Vec<_>, _>>()?;
    let avoid = read_avoid(&args.avoid)?;
    let words = text::count_words(&draft);
    if words == 0 {
        return Err(AppError::input(
            "the draft has no words",
            format!("Pass a draft with prose: {bin} check draft.md --profile {}", args.profile),
        ));
    }

    let guard_refs: Vec<_> = guards.iter().collect();
    let checked = checks::check(&target, &guard_refs, &avoid, &draft);
    let mut warnings = Vec::new();
    let mut all = store::load_all();
    if !all.iter().any(|p| p.name == target.name) {
        all.push(target.clone());
    }
    let voice_match = match score::voice_match(&target, &all, &draft) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("voice_match unavailable: {e}"));
            None
        }
    };
    if words < 1000 {
        warnings.push(format!(
            "{words} words is short: rates swing a lot at this length, so treat metric findings as directional (3,000+ words is reliable)"
        ));
    }
    if checked.ranges.directional {
        warnings.push(format!(
            "the profile has only {} windows of {} words: ranges are directional",
            checked.ranges.windows, checked.ranges.window_words
        ));
    }

    let counts = checks::counts(&checked.findings);
    let hard = checked.findings.iter().filter(|f| f.hard).count();
    let total = checked.rows.len();
    let out = checked.rows.iter().filter(|r| r.status == Status::OutOfRange).count();
    let pass = hard == 0 && (out as f64) <= MAX_OUT_OF_RANGE * total as f64;
    let mut findings = checked.findings;
    let omitted = findings.len().saturating_sub(args.limit);
    findings.truncate(args.limit);

    let report = CheckReport {
        profile: target.name.clone(),
        words,
        window_words: checked.ranges.window_words,
        windows: checked.ranges.windows,
        directional: checked.ranges.directional || words < 1000,
        summary: Summary {
            pass,
            hard_failures: hard,
            metrics_in_range: format!("{}/{}", total - out, total),
            counts,
            voice_match,
        },
        findings,
        findings_omitted: omitted,
        metrics: checked.rows,
        warnings,
    };

    let draft_arg = args
        .file
        .as_ref()
        .map(|f| f.display().to_string())
        .unwrap_or_else(|| "draft.md".into());
    let next = vec![
        if pass {
            format!("{bin} compare {draft_arg} --profile {}", report.profile)
        } else {
            format!("Revise the findings (hard first), then: {bin} check {draft_arg} --profile {}", report.profile)
        },
    ];
    if args.strict && !pass {
        let v = serde_json::to_value(&report).map_err(|_| AppError::Serialization)?;
        return Err(AppError::CheckFailed(v));
    }
    output::print_success_with(ctx, &report, &next, print_human)
}

fn print_human(r: &CheckReport) {
    use owo_colors::OwoColorize;
    let verdict = if r.summary.pass { "PASS".green().bold().to_string() } else { "FAIL".red().bold().to_string() };
    println!(
        "{verdict} {} words vs {} · metrics in range {} · hard failures {}",
        r.words, r.profile, r.summary.metrics_in_range, r.summary.hard_failures
    );
    if let Some(v) = &r.summary.voice_match {
        println!(
            "voice match: typicality {:.2}, nearest {}, {}",
            v.typicality, v.nearest_profile, v.verdict
        );
    }
    let counts: Vec<String> = r
        .summary
        .counts
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    if !counts.is_empty() {
        println!("{}", counts.join(" · ").dimmed());
    }
    for f in &r.findings {
        let line = f.line.map(|l| format!("L{l}")).unwrap_or_else(|| "--".into());
        println!("{:>5} {} {}", line.dimmed(), f.kind.yellow(), f.message);
        if !f.excerpt.is_empty() {
            println!("      {}", f.excerpt.dimmed());
        }
        println!("      → {}", f.suggestion);
    }
    if r.findings_omitted > 0 {
        println!("{}", format!("… {} more (raise -n)", r.findings_omitted).dimmed());
    }
    for w in &r.warnings {
        println!("{} {w}", "warning:".yellow());
    }
}
