//! `doctor`: "can this tool actually work right now?"
//!
//! Checks the data dir, every stored profile (usable, outdated or corrupt,
//! each with its exact fix), the background of reference authors, which
//! voices are calibrated, the embedded lexicons, and the config file.
//! Warnings exit 0; any failed check exits 2 with the report in
//! `error.details`. Read-only apart from a probe file in the data dir.

use serde::{Deserialize, Serialize, de::IgnoredAny};

use crate::config;
use crate::engine::{lexicon, profile, store};
use crate::error::AppError;
use crate::output::{self, Ctx};

const BIN: &str = env!("CARGO_BIN_NAME");

/// Below this many reference authors, calibration mostly learns "not these
/// few" and accepts unseen writers (measured on v0.2 with 3 imposters).
const FEW_BACKGROUND: usize = 10;

#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Serialize)]
struct DoctorCheck {
    name: String,
    status: CheckStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
}

impl DoctorCheck {
    fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            message: message.into(),
            suggestion: None,
        }
    }

    fn fix(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

#[derive(Serialize)]
struct DoctorSummary {
    pass: usize,
    warn: usize,
    fail: usize,
}

#[derive(Serialize)]
struct DoctorReport {
    data_dir: String,
    checks: Vec<DoctorCheck>,
    summary: DoctorSummary,
}

fn check_data_dir() -> DoctorCheck {
    let dir = store::profiles_dir();
    let probe = dir.join(".doctor-probe");
    let writable = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&probe, b"ok"))
        .and_then(|_| std::fs::remove_file(&probe));
    match writable {
        Ok(()) => DoctorCheck::new("data_dir", CheckStatus::Pass, "writable"),
        Err(e) => DoctorCheck::new(
            "data_dir",
            CheckStatus::Fail,
            format!("{} is not writable: {e}", dir.display()),
        )
        .fix("Point STYLOMETRY_DATA_DIR at a writable directory, e.g. export STYLOMETRY_DATA_DIR=$HOME/.stylometry"),
    }
}

/// Just the fields doctor needs; the rest of a (possibly large) profile is
/// skipped without being built.
#[derive(Deserialize)]
struct Head {
    #[serde(default)]
    schema: u64,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    calibration: Option<IgnoredAny>,
    #[serde(default)]
    chunks: Option<IgnoredAny>,
}

#[derive(Default)]
struct Inventory {
    authors: Vec<String>,
    blends: usize,
    background: usize,
    calibrated: usize,
    uncalibrated: Vec<String>,
    outdated_background: usize,
}

/// One check per unusable profile (with its exact fix), then summaries of
/// what is usable.
fn check_profiles(checks: &mut Vec<DoctorCheck>) -> Inventory {
    let mut inv = Inventory::default();
    for name in store::list_names() {
        let head = store::path_for(&name)
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|raw| serde_json::from_slice::<Head>(&raw).ok());
        let Some(head) = head else {
            checks.push(
                DoctorCheck::new(
                    format!("profile {name}"),
                    CheckStatus::Fail,
                    "unreadable or corrupt profile file",
                )
                .fix(format!(
                    "{BIN} profile remove {name} --confirm, then {BIN} profile build {name} --corpus <path>"
                )),
            );
            continue;
        };
        let kind = head.kind.as_deref().unwrap_or("author");
        if head.schema < u64::from(profile::SCHEMA) {
            if kind == "background" {
                inv.outdated_background += 1;
                continue;
            }
            // Schema 3+ profiles carry their text and can be re-analysed in
            // place; older ones must be rebuilt from the corpus.
            let fix = if head.schema >= 3 && head.chunks.is_some() {
                format!("{BIN} profile rebuild {name}")
            } else {
                format!("{BIN} profile build {name} --corpus <path> --force")
            };
            checks.push(
                DoctorCheck::new(
                    format!("profile {name}"),
                    CheckStatus::Fail,
                    format!(
                        "built by an older version (schema {}, current {})",
                        head.schema,
                        profile::SCHEMA
                    ),
                )
                .fix(format!("{fix} (or {BIN} profile remove {name} --confirm)")),
            );
            continue;
        }
        match kind {
            "background" => inv.background += 1,
            "blend" => inv.blends += 1,
            _ => {
                if head.calibration.is_some() {
                    inv.calibrated += 1;
                } else {
                    inv.uncalibrated.push(name.clone());
                }
                inv.authors.push(name);
            }
        }
    }

    let usable = inv.authors.len() + inv.blends;
    checks.push(if usable == 0 {
        DoctorCheck::new("profiles", CheckStatus::Warn, "no voice profiles yet")
            .fix(format!("{BIN} profile build <name> --corpus <dir>"))
    } else {
        DoctorCheck::new(
            "profiles",
            CheckStatus::Pass,
            format!("{} author, {} blend", inv.authors.len(), inv.blends),
        )
    });
    inv
}

fn check_background(inv: &Inventory) -> DoctorCheck {
    let message = format!("{} reference author profile(s) installed", inv.background);
    if inv.outdated_background > 0 {
        DoctorCheck::new(
            "background",
            CheckStatus::Warn,
            format!("{message}; {} outdated", inv.outdated_background),
        )
        .fix(format!("{BIN} background fetch --force"))
    } else if inv.background == 0 {
        DoctorCheck::new(
            "background",
            CheckStatus::Warn,
            "no reference authors installed; calibrate and compare need them",
        )
        .fix(format!("{BIN} background fetch"))
    } else if inv.background < FEW_BACKGROUND {
        DoctorCheck::new(
            "background",
            CheckStatus::Warn,
            format!("{message}; calibration needs a broad set to reject unseen authors"),
        )
        .fix(format!("{BIN} background fetch"))
    } else {
        DoctorCheck::new("background", CheckStatus::Pass, message)
    }
}

fn check_calibration(inv: &Inventory) -> DoctorCheck {
    if inv.authors.is_empty() {
        return DoctorCheck::new(
            "calibration",
            CheckStatus::Pass,
            "no author profiles to calibrate",
        );
    }
    let message = format!(
        "{} of {} author profiles calibrated",
        inv.calibrated,
        inv.authors.len()
    );
    match inv.uncalibrated.first() {
        // Only verification (compare) needs calibration; voice work doesn't.
        Some(first) => {
            let more = inv.uncalibrated.len() - 1;
            let rest = if more > 0 {
                format!(" (and {more} more)")
            } else {
                String::new()
            };
            DoctorCheck::new(
                "calibration",
                CheckStatus::Warn,
                format!("{message}; compare returns uncalibrated verdicts for the rest"),
            )
            .fix(format!("{BIN} calibrate {first}{rest}"))
        }
        None => DoctorCheck::new("calibration", CheckStatus::Pass, message),
    }
}

fn check_lexicons() -> DoctorCheck {
    let sizes = [
        (
            "function words",
            usize::from(lexicon::is_function_word("the")),
        ),
        (
            "word frequencies",
            usize::from(lexicon::zipf("the").is_some()),
        ),
        ("synonym groups", lexicon::synonym_groups().len()),
        ("LLM tells", lexicon::llm_tells().len()),
    ];
    let empty: Vec<&str> = sizes
        .iter()
        .filter(|(_, n)| *n == 0)
        .map(|(l, _)| *l)
        .collect();
    if empty.is_empty() {
        DoctorCheck::new(
            "lexicons",
            CheckStatus::Pass,
            format!(
                "{} synonym groups, {} LLM tells",
                lexicon::synonym_groups().len(),
                lexicon::llm_tells().len()
            ),
        )
    } else {
        DoctorCheck::new(
            "lexicons",
            CheckStatus::Fail,
            format!("embedded lexicons failed to load: {}", empty.join(", ")),
        )
        .fix(format!("Reinstall {BIN}; the binary is damaged"))
    }
}

fn check_config() -> DoctorCheck {
    match config::load() {
        Ok(_) => DoctorCheck::new(
            "config",
            CheckStatus::Pass,
            "loads cleanly (a missing file means defaults)",
        ),
        Err(e) => DoctorCheck::new("config", CheckStatus::Fail, e.to_string()).fix(format!(
            "Repair or remove {}",
            config::config_path().display()
        )),
    }
}

/// JEV is optional: without it, texture checks and example re-ranking are
/// skipped and everything else works, so a missing key is a warning.
fn check_jev() -> DoctorCheck {
    let cfg = config::load().map(|c| c.jev).unwrap_or_default();
    match crate::engine::jev::unavailable_reason(&cfg) {
        None => DoctorCheck::new(
            "jev",
            CheckStatus::Pass,
            format!("on ({}): texture checks and example re-ranking send passages to api.typesafe.ai", cfg.model),
        ),
        Some(_) if !cfg.enabled => DoctorCheck::new("jev", CheckStatus::Pass, "off by config: everything stays local"),
        Some(why) => DoctorCheck::new("jev", CheckStatus::Warn, format!("{why}; texture checks and example re-ranking are skipped"))
            .fix(format!("export {}=<key>, or set jev.key_command in {}", cfg.key_env, config::config_path().display())),
    }
}

pub fn run(ctx: Ctx) -> Result<(), AppError> {
    let mut checks = vec![check_data_dir()];
    let inv = check_profiles(&mut checks);
    checks.push(check_background(&inv));
    checks.push(check_calibration(&inv));
    checks.push(check_lexicons());
    checks.push(check_config());
    checks.push(check_jev());

    let count = |s: CheckStatus| checks.iter().filter(|c| c.status == s).count();
    let summary = DoctorSummary {
        pass: count(CheckStatus::Pass),
        warn: count(CheckStatus::Warn),
        fail: count(CheckStatus::Fail),
    };
    let failed = summary.fail > 0;
    let report = DoctorReport {
        data_dir: store::data_dir().display().to_string(),
        checks,
        summary,
    };

    if failed {
        return Err(AppError::Diagnostics(
            serde_json::to_value(&report).map_err(|_| AppError::Serialization)?,
        ));
    }

    // Warnings carry their fix; surface the first ones as next steps.
    let next: Vec<String> = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .filter_map(|c| c.suggestion.clone())
        .collect();
    output::print_success_with(ctx, &report, &next, |r| {
        use owo_colors::OwoColorize;
        println!("{}", r.data_dir.dimmed());
        for check in &r.checks {
            let icon = match check.status {
                CheckStatus::Pass => "✓".green().to_string(),
                CheckStatus::Warn => "!".yellow().to_string(),
                CheckStatus::Fail => "✗".red().to_string(),
            };
            println!("  {icon} {}: {}", check.name, check.message);
        }
        println!(
            "{} pass, {} warn, {} fail",
            r.summary.pass, r.summary.warn, r.summary.fail
        );
    })
}
