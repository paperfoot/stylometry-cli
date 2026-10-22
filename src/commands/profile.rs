//! `profile` subcommands: build / list / show / rebuild / remove.

use std::path::PathBuf;

use serde::Serialize;

use crate::engine::profile::{self, Profile, ProfileKind, Register, BASE_CHUNK_WORDS, MIN_CHUNKS};
use crate::engine::{import, store};
use crate::error::AppError;
use crate::output::{self, Ctx};

const BIN: &str = env!("CARGO_BIN_NAME");

#[derive(Serialize)]
struct BuiltProfile {
    name: String,
    kind: ProfileKind,
    register: Register,
    register_reason: String,
    words: u64,
    works: usize,
    passages: usize,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hygiene: Option<crate::engine::text::Hygiene>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

fn built(p: &Profile, path: PathBuf) -> BuiltProfile {
    BuiltProfile {
        name: p.name.clone(),
        kind: p.kind,
        register: p.register,
        register_reason: p.register_reason.clone(),
        words: p.n_tokens,
        works: p.sources.len(),
        passages: p.chunks.len(),
        path: path.display().to_string(),
        hygiene: None,
        warnings: Vec::new(),
    }
}

fn print_built(ctx: Ctx, data: &BuiltProfile, verb: &str) -> Result<(), AppError> {
    let next = if data.kind == ProfileKind::Author {
        vec![
            format!("{BIN} analyze {}", data.name),
            format!("{BIN} guide {}", data.name),
            format!("{BIN} check draft.md --profile {}", data.name),
        ]
    } else {
        vec![]
    };
    output::print_success_with(ctx, data, &next, |d| {
        use owo_colors::OwoColorize;
        println!(
            "{verb} {} profile {} ({} words, {} work(s), {} {})",
            d.register.as_str(),
            d.name.green().bold(),
            d.words,
            d.works,
            d.passages,
            if d.register.is_short_form() { "items" } else { "passages" }
        );
        println!("  register: {} ({}); override with --register", d.register.as_str(), d.register_reason);
        for w in &d.warnings {
            println!("  {} {}", "warning:".yellow().bold(), w.yellow());
        }
        println!("  {}", d.path.dimmed());
    })
}

/// Arguments of `profile build`.
pub struct BuildArgs {
    pub name: String,
    pub corpus: PathBuf,
    pub register: Option<Register>,
    /// Keep only items dated on/after this ISO day.
    pub since: Option<String>,
    pub background: bool,
    pub force: bool,
}

pub fn build(ctx: Ctx, a: BuildArgs) -> Result<(), AppError> {
    let BuildArgs { name, corpus, register, since, background, force } = a;
    if store::exists(&name) && !force {
        return Err(AppError::input(
            format!("profile '{name}' already exists"),
            format!("Pass --force to overwrite: {BIN} profile build {name} --corpus <path> --force"),
        ));
    }
    let _ = crate::engine::text::take_hygiene();
    let mut corpus_in = import::read_corpus(&corpus)?;
    let hygiene = crate::engine::text::take_hygiene();
    if let Some(day) = &since {
        import::keep_since(&mut corpus_in, day)?;
    }
    let kind = if background {
        ProfileKind::Background
    } else {
        ProfileKind::Author
    };
    let register = match register {
        Some(r) => (r, "--register".to_string()),
        None => profile::detect_register(&corpus_in.docs, corpus_in.format_register),
    };
    let profile = Profile::build_with(&name, &corpus_in.docs, kind, Some(register), MIN_CHUNKS)?;
    let path = store::save(&profile)?;
    let mut data = built(&profile, path);
    data.hygiene = Some(hygiene);
    if hygiene.total() as f64 > profile.n_tokens as f64 / 1000.0 {
        data.warnings.push(format!(
            "cleaned conversion debris: {} escapes, {} fence lines, {} attribute blocks, {} broken paragraphs rejoined; spot-check a passage with `{BIN} examples {name}`",
            hygiene.escapes, hygiene.fences, hygiene.attributes, hygiene.broken_paragraphs
        ));
    }
    print_built(ctx, &data, "Built")
}

pub fn blend(ctx: Ctx, name: String, from: Vec<String>, force: bool) -> Result<(), AppError> {
    if store::exists(&name) && !force {
        return Err(AppError::input(
            format!("profile '{name}' already exists"),
            format!("Pass --force to overwrite: {BIN} profile blend {name} --from a=0.6 --from b=0.4 --force"),
        ));
    }
    let mut parts = Vec::new();
    for spec in &from {
        let (pname, share) = spec.split_once('=').ok_or_else(|| {
            AppError::input(
                format!("bad --from '{spec}': expected PROFILE=SHARE"),
                format!("Example: {BIN} profile blend {name} --from hoover=0.6 --from fisher=0.4"),
            )
        })?;
        let share: f64 = share.trim().parse().map_err(|_| {
            AppError::input(
                format!("bad share in --from '{spec}': not a number"),
                format!("Example: {BIN} profile blend {name} --from hoover=0.6 --from fisher=0.4"),
            )
        })?;
        parts.push((store::load(pname.trim())?, share));
    }
    let profile = Profile::blend(&name, &parts)?;
    let path = store::save(&profile)?;
    let data = built(&profile, path);
    let next = vec![
        format!("{BIN} analyze {name}"),
        format!("{BIN} guide {name}"),
        format!("{BIN} check draft.md --profile {name}"),
    ];
    output::print_success_with(ctx, &data, &next, |d| {
        use owo_colors::OwoColorize;
        let mix: Vec<String> = profile
            .components
            .iter()
            .map(|(n, s)| format!("{n} {:.0}%", s * 100.0))
            .collect();
        println!("Blended {} from {}", d.name.green().bold(), mix.join(" + "));
        println!("  {}", d.path.dimmed());
    })
}

pub fn rebuild(ctx: Ctx, name: String) -> Result<(), AppError> {
    if let Ok(p) = store::load(&name) {
        if p.kind == ProfileKind::Blend {
            return Err(AppError::input(
                format!("'{name}' is a blend; rebuild its components, then re-blend"),
                format!("Re-blend with: {BIN} profile blend {name} --from <a>=<share> --from <b>=<share> --force"),
            ));
        }
    }
    let (kind, docs) = store::load_text(&name)?;
    let register = store::load(&name).ok().map(|p| (p.register, p.register_reason));
    let profile = Profile::build_with(&name, &docs, kind, register, MIN_CHUNKS)?;
    let path = store::save(&profile)?;
    print_built(ctx, &built(&profile, path), "Rebuilt")
}

#[derive(Serialize)]
struct ProfileSummary {
    name: String,
    kind: ProfileKind,
    words: u64,
    works: usize,
    calibrated: bool,
}

#[derive(Serialize)]
struct Outdated {
    name: String,
    error: String,
}

#[derive(Serialize)]
struct ProfileList {
    profiles: Vec<ProfileSummary>,
    /// Profiles that exist on disk but can't be used (older schema, corrupt).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unusable: Vec<Outdated>,
    background_installed: usize,
}

pub fn list(ctx: Ctx, all: bool) -> Result<(), AppError> {
    let mut profiles = Vec::new();
    let mut unusable = Vec::new();
    let mut background_installed = 0;
    for name in store::list_names() {
        match store::load(&name) {
            Ok(p) => {
                if p.kind == ProfileKind::Background {
                    background_installed += 1;
                    if !all {
                        continue;
                    }
                }
                profiles.push(ProfileSummary {
                    name: p.name,
                    kind: p.kind,
                    words: p.n_tokens,
                    works: p.sources.len(),
                    calibrated: p.calibration.is_some(),
                });
            }
            Err(e) => unusable.push(Outdated {
                name,
                error: e.to_string(),
            }),
        }
    }
    let data = ProfileList {
        profiles,
        unusable,
        background_installed,
    };
    let mut next = Vec::new();
    if data.profiles.iter().all(|p| p.kind == ProfileKind::Background) {
        next.push(format!("{BIN} profile build <name> --corpus <folder-of-md-or-txt>"));
    }
    if data.background_installed == 0 {
        next.push(format!("{BIN} background fetch"));
    }
    output::print_success_with(ctx, &data, &next, |d| {
        use comfy_table::{Table, presets::UTF8_FULL};
        use owo_colors::OwoColorize;
        if d.profiles.is_empty() {
            println!("No voice profiles yet.");
        } else {
            let mut t = Table::new();
            t.load_preset(UTF8_FULL)
                .set_header(vec!["profile", "kind", "words", "works", "calibrated"]);
            for r in &d.profiles {
                t.add_row(vec![
                    r.name.clone(),
                    format!("{:?}", r.kind).to_lowercase(),
                    r.words.to_string(),
                    r.works.to_string(),
                    if r.calibrated { "yes" } else { "no" }.to_string(),
                ]);
            }
            println!("{t}");
        }
        println!("{} background author(s) installed", d.background_installed);
        for u in &d.unusable {
            println!("  {} {}: {}", "unusable".yellow(), u.name, u.error);
        }
    })
}

#[derive(Serialize)]
struct ShowProfile {
    name: String,
    kind: ProfileKind,
    register: Register,
    register_reason: String,
    words: u64,
    passages: usize,
    passage_words: usize,
    works: Vec<crate::engine::profile::Source>,
    distinct_words: usize,
    proper_nouns: usize,
    calibration: Option<CalibrationSummary>,
}

#[derive(Serialize)]
struct CalibrationSummary {
    imposters: usize,
    cross_work: bool,
    bands: Vec<BandSummary>,
}

#[derive(Serialize)]
struct BandSummary {
    words: usize,
    auc: f64,
    holdout_accuracy: Option<f64>,
}

pub fn show(ctx: Ctx, name: String) -> Result<(), AppError> {
    let p = store::load(&name)?;
    let data = ShowProfile {
        name: p.name.clone(),
        kind: p.kind,
        register: p.register,
        register_reason: p.register_reason.clone(),
        words: p.n_tokens,
        passages: p.chunks.len(),
        passage_words: BASE_CHUNK_WORDS,
        works: p.sources.clone(),
        distinct_words: p.word_counts.len(),
        proper_nouns: p.proper_nouns.len(),
        calibration: p.calibration.as_ref().map(|c| CalibrationSummary {
            imposters: c.imposters,
            cross_work: c.cross_work,
            bands: c
                .bands
                .iter()
                .map(|b| BandSummary {
                    words: b.words,
                    auc: b.auc,
                    holdout_accuracy: b.holdout_accuracy,
                })
                .collect(),
        }),
    };
    let next = vec![format!("{BIN} analyze {name}")];
    output::print_success_with(ctx, &data, &next, |d| {
        use owo_colors::OwoColorize;
        println!(
            "{} ({}) — {} words in {} work(s), {} passages",
            d.name.green().bold(),
            d.register.as_str(),
            d.words,
            d.works.len(),
            d.passages
        );
        for w in &d.works {
            println!("  {} ({} words)", w.name.dimmed(), w.words);
        }
        match &d.calibration {
            Some(c) => {
                for b in &c.bands {
                    println!(
                        "  calibrated @{} words: AUC {:.3}, holdout accuracy {}",
                        b.words,
                        b.auc,
                        b.holdout_accuracy
                            .map(|a| format!("{a:.3}"))
                            .unwrap_or_else(|| "n/a".into())
                    );
                }
            }
            None => println!("  not calibrated (run: {BIN} calibrate {})", d.name),
        }
    })
}

pub fn remove(ctx: Ctx, name: String, confirm: bool) -> Result<(), AppError> {
    if !confirm {
        return Err(AppError::input(
            format!("removing profile '{name}' needs confirmation"),
            format!("Re-run with --confirm: {BIN} profile remove {name} --confirm"),
        ));
    }
    store::remove(&name)?;
    #[derive(Serialize)]
    struct Removed {
        removed: String,
    }
    output::print_success_or(ctx, &Removed { removed: name.clone() }, |d| {
        println!("Removed profile {}", d.removed);
    })
}
