//! `check`: one voice score, a short revision plan, and grouped findings
//! against a voice. The draft passes when it has no hard failures and
//! scores at least 10 (as typical as the author's own 10th-percentile
//! passage or post). Authorship verification (voice_match) loads every
//! profile, so it runs only with --verify, on the final pass.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::commands::input;
use crate::engine::checks::{self, Batch, Finding, HabitRow, ItemScore, MetricRow, PlanStep, ScoreBasis, Segment};
use crate::engine::metrics::Status;
use crate::engine::profile::Register;
use crate::engine::score::{self, VoiceMatch};
use crate::engine::texture::{self, Texture};
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
    /// Run voice_match (loads every profile); off while iterating.
    pub verify: bool,
    /// Short-form: the kind of item the draft is.
    pub as_kind: crate::engine::profile::ItemKind,
}

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub profile: String,
    pub register: Register,
    pub words: usize,
    /// No hard failures and score ≥ 10.
    pub pass: bool,
    /// 0–100: the draft's percentile among the author's own windows (or
    /// posts) of the same length; 50 = as typical as the author's median.
    /// Null when the author has too little text at this length.
    pub score: Option<f64>,
    /// The continuous gap behind the score (lower is closer): mean squared
    /// distance from the author's medians in tolerance units, families
    /// weighted equally; for posts, the mean surprisal of their habits.
    pub distance: f64,
    pub score_basis: ScoreBasis,
    /// high | medium | low: how firmly the score can be read.
    pub score_reliability: &'static str,
    pub hard_failures: usize,
    /// Do these, in order: hard failures first, then one step per family.
    pub plan: Vec<PlanStep>,
    pub findings: Vec<Finding>,
    /// Findings left out by --limit.
    pub findings_omitted: usize,
    pub counts: BTreeMap<&'static str, usize>,
    /// Measures not clearly outside the author's range, e.g. "61/74".
    pub metrics_in_range: String,
    pub metrics: Vec<MetricRow>,
    /// Long drafts: ~1,000-word stretches and what drifts in each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
    /// Short drafts: each habit of the draft and how often the author shows it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub habits: Vec<HabitRow>,
    /// Drafts of several `---`-separated items: each item's score.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemScore>,
    /// 3+ items: how the batch spreads compared with the author's posts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<Batch>,
    /// Only with --verify.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_match: Option<VoiceMatch>,
    /// Prose texture next to the author's own, judged by JEV (on when a key
    /// is found): advice with its own `pass`; it never changes `pass`,
    /// `score` or `--strict`. `status: unavailable` with the reason otherwise.
    pub texture: Texture,
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
    let mut checked = checks::check(&target, &guard_refs, &avoid, &draft, args.as_kind);
    // Decided before texture is added: texture is advice and never moves it.
    let pass = checked.pass();
    let mut texture = texture::assess(&target, &draft);
    // One-sentence paragraphs are counted, not judged: the check's own metric.
    if let Some(r) = checked.rows.iter().find(|r| r.key == "one_sentence_paragraph_share" && r.status != Status::Unmeasured) {
        texture.add_counted("oneliner", r.value, [r.low, r.median, r.high]);
    }
    checks::add_texture(&mut checked, &texture, &draft);
    let mut warnings = checked.warnings.clone();
    let voice_match = if !args.verify {
        None
    } else if words < score::MIN_WORDS {
        warnings.push(format!(
            "voice_match skipped: {words} words; it needs {}+ (pool several posts separated by --- lines)",
            score::MIN_WORDS
        ));
        None
    } else {
        let mut all = store::load_all();
        if !all.iter().any(|p| p.name == target.name) {
            all.push(target.clone());
        }
        match score::voice_match(&target, &all, &draft) {
            Ok(v) => Some(v),
            Err(e) => {
                warnings.push(format!("voice_match unavailable: {e}"));
                None
            }
        }
    };

    let hard = checked.hard_failures();
    let judged: Vec<&MetricRow> = checked.rows.iter().filter(|r| r.status != Status::Unmeasured).collect();
    let out = judged.iter().filter(|r| r.status == Status::OutOfRange).count();
    let counts = checks::counts(&checked.findings);
    let mut findings = checked.findings;
    let omitted = findings.len().saturating_sub(args.limit);
    findings.truncate(args.limit);

    let report = CheckReport {
        profile: target.name.clone(),
        register: checked.register,
        words,
        pass,
        score: checked.score,
        distance: checked.distance,
        score_basis: checked.basis,
        score_reliability: checked.reliability,
        hard_failures: hard,
        plan: checked.plan,
        findings,
        findings_omitted: omitted,
        counts,
        metrics_in_range: format!("{}/{}", judged.len() - out, judged.len()),
        metrics: checked.rows,
        segments: checked.segments,
        habits: checked.habits,
        items: checked.items,
        batch: checked.batch,
        voice_match,
        texture,
        warnings,
    };

    let draft_arg = args
        .file
        .as_ref()
        .map(|f| f.display().to_string())
        .unwrap_or_else(|| "draft.md".into());
    let as_flag = if target.register.is_short_form() {
        match serde_json::to_value(args.as_kind).ok().and_then(|v| v.as_str().map(str::to_string)) {
            Some(k) if k != "post" => format!(" --as {k}"),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let p = &report.profile;
    let next = if !pass {
        vec![
            format!("Apply the plan (hard steps first), then re-check: {bin} check {draft_arg} --profile {p}{as_flag}"),
            format!("Few-shot examples of the author for the same task: {bin} examples {p} --like {draft_arg}"),
        ]
    } else if args.verify {
        vec![format!("Done: the draft passes. Optional: {bin} compare {draft_arg} --profile {p}")]
    } else if !report.plan.is_empty() {
        // Passing with steps left (LLM tells, a drifting rate): closer is better.
        vec![
            format!(
                "It passes; to get closer to the author, apply the {} plan step(s) and re-check: {bin} check {draft_arg} --profile {p}{as_flag}",
                report.plan.len()
            ),
            format!("Final pass with authorship verification: {bin} check {draft_arg} --profile {p}{as_flag} --verify"),
        ]
    } else {
        vec![format!("Final pass with authorship verification: {bin} check {draft_arg} --profile {p}{as_flag} --verify")]
    };
    if args.strict && !pass {
        let v = serde_json::to_value(&report).map_err(|_| AppError::Serialization)?;
        return Err(AppError::CheckFailed(v));
    }
    output::print_success_with(ctx, &report, &next, print_human)
}

fn print_human(r: &CheckReport) {
    use owo_colors::OwoColorize;
    let verdict = if r.pass { "PASS".green().bold().to_string() } else { "FAIL".red().bold().to_string() };
    let score = r.score.map(|s| format!("{s:.0}/100")).unwrap_or_else(|| "n/a".into());
    println!(
        "{verdict} score {score} ({} reliability) · {} words vs {} ({}) · hard failures {}",
        r.score_reliability,
        r.words,
        r.profile,
        r.register.as_str(),
        r.hard_failures
    );
    println!("{}", r.score_basis.note.dimmed());
    if let Some(v) = &r.voice_match {
        println!("voice match: typicality {:.2}, nearest {}, {}", v.typicality, v.nearest_profile, v.verdict);
    }
    print_texture(&r.texture);
    if !r.plan.is_empty() {
        println!("{}", "plan".bold());
        for (i, s) in r.plan.iter().enumerate() {
            let tag = if s.hard { " [hard]".red().to_string() } else { String::new() };
            println!("  {}. {}{tag}", i + 1, s.action);
            println!("     {}", s.why.dimmed());
        }
    }
    for f in &r.findings {
        let lines = if f.lines.is_empty() {
            "--".to_string()
        } else {
            format!("L{}", f.lines.iter().take(6).map(|l| l.to_string()).collect::<Vec<_>>().join(","))
        };
        let count = if f.count > 1 { format!(" ×{}", f.count) } else { String::new() };
        println!("{:>10} {}{count} {}", lines.dimmed(), f.kind.yellow(), f.message);
        for e in &f.excerpts {
            println!("           {}", e.dimmed());
        }
        println!("           → {}", f.suggestion);
    }
    if r.findings_omitted > 0 {
        println!("{}", format!("… {} more (raise -n)", r.findings_omitted).dimmed());
    }
    for s in &r.segments {
        if !s.out_of_range.is_empty() {
            println!("{} L{}–{} ({} words): {}", "segment".dimmed(), s.lines[0], s.lines[1], s.words, s.out_of_range.join(", "));
        }
    }
    for w in &r.warnings {
        println!("{} {w}", "warning:".yellow());
    }
}

/// One line for the texture verdict, then the keys off the author's band.
fn print_texture(t: &Texture) {
    use owo_colors::OwoColorize;
    if !t.measured() {
        println!("{}", format!("texture: unavailable ({})", t.reason.as_deref().unwrap_or("")).dimmed());
        return;
    }
    let verdict = if t.pass == Some(true) { "reads like the author".green().to_string() } else { "off the author's".yellow().to_string() };
    let usage = t.usage.map(|u| format!("{} passage(s), {} request(s), ${:.4}", u.passages, u.requests, u.cost_usd)).unwrap_or_default();
    println!("texture (JEV, advice only): {verdict} · {usage}");
    if let Some(m) = t.author_measured {
        println!("{}", format!("  measured {} of the author's passages first ({} requests, ${:.4}); saved for next time", m.passages, m.requests, m.cost_usd).dimmed());
    }
    for k in t.off() {
        println!(
            "  {}{} {:.2} (author {:.2}–{:.2})",
            k.label,
            if k.gate { "" } else { " [advisory]" },
            k.draft,
            k.low,
            k.high
        );
    }
}
