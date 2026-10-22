//! `background`: download and build the reference authors.
//!
//! `fetch` downloads each catalog book from Project Gutenberg (sequentially,
//! with a User-Agent naming the tool), caches the raw text under
//! `<data dir>/background-src/`, and builds a `kind=background` profile
//! named `bg-<slug>`. Existing profiles are kept unless `--force`.
//!
//! Offline / tests: set `STYLOMETRY_BACKGROUND_SOURCE=<dir>` to read
//! `<dir>/<gutenberg id>.txt` instead of the network.

use serde::Serialize;

use crate::engine::background::{self, Work};
use crate::engine::profile::ProfileKind;
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx, Format};

#[derive(Serialize)]
struct Failed {
    profile: String,
    gutenberg_id: u32,
    error: String,
}

#[derive(Serialize)]
struct FetchReport {
    built: Vec<String>,
    skipped: Vec<String>,
    failed: Vec<Failed>,
    /// Background profiles installed after this run (catalog and user-built).
    installed: usize,
    catalog: usize,
}

pub fn fetch(ctx: Ctx, force: bool) -> Result<(), AppError> {
    let works = background::catalog();
    let progress = matches!(ctx.format, Format::Human) && !ctx.quiet;
    let (mut built, mut skipped, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for (i, w) in works.iter().enumerate() {
        let name = w.profile_name();
        if !force && store::exists(&name) {
            skipped.push(name);
            continue;
        }
        if progress {
            eprintln!("[{}/{}] {} — {}", i + 1, works.len(), w.author, w.title);
        }
        match fetch_one(w) {
            Ok(()) => built.push(name),
            Err(e) => failed.push(Failed {
                profile: name,
                gutenberg_id: w.id,
                error: e.to_string(),
            }),
        }
    }
    if built.is_empty() && skipped.is_empty() && !failed.is_empty() {
        return Err(AppError::setup(
            format!(
                "all {} background downloads failed (first: {})",
                failed.len(),
                failed[0].error
            ),
            format!(
                "Check the network and retry: {bin} background fetch. Offline: put Gutenberg <id>.txt files in a folder and set {}=<folder>",
                background::SOURCE_ENV,
                bin = env!("CARGO_BIN_NAME")
            ),
        ));
    }
    let report = FetchReport {
        built,
        skipped,
        failed,
        installed: store::load_kind(ProfileKind::Background).len(),
        catalog: works.len(),
    };
    let mut next = Vec::new();
    if !report.failed.is_empty() {
        next.push(format!("{} background fetch", env!("CARGO_BIN_NAME")));
    }
    for p in store::load_kind(ProfileKind::Author) {
        next.push(format!("{} calibrate {}", env!("CARGO_BIN_NAME"), p.name));
    }
    output::print_success_with(ctx, &report, &next, |r| {
        println!(
            "Background: {} built, {} already installed, {} failed ({} installed in total)",
            r.built.len(),
            r.skipped.len(),
            r.failed.len(),
            r.installed
        );
        for f in &r.failed {
            println!("  failed {} (#{}): {}", f.profile, f.gutenberg_id, f.error);
        }
    })
}

fn fetch_one(w: &Work) -> Result<(), AppError> {
    let raw = background::source_text(w)?;
    let profile = background::build_profile(w, &raw)?;
    store::save(&profile)?;
    Ok(())
}

#[derive(Serialize)]
struct Entry {
    profile: String,
    author: String,
    title: String,
    year: u16,
    installed: bool,
}

#[derive(Serialize)]
struct StatusReport {
    installed: usize,
    missing: usize,
    works: Vec<Entry>,
    /// Background profiles built by hand (`profile build --background`).
    extra: Vec<String>,
}

pub fn status(ctx: Ctx) -> Result<(), AppError> {
    let names = store::list_names();
    let works: Vec<Entry> = background::catalog()
        .into_iter()
        .map(|w| Entry {
            installed: names.binary_search(&w.profile_name()).is_ok(),
            profile: w.profile_name(),
            author: w.author,
            title: w.title,
            year: w.year,
        })
        .collect();
    let extra: Vec<String> = store::load_kind(ProfileKind::Background)
        .into_iter()
        .map(|p| p.name)
        .filter(|n| !works.iter().any(|w| &w.profile == n))
        .collect();
    let installed = works.iter().filter(|w| w.installed).count();
    let report = StatusReport {
        installed,
        missing: works.len() - installed,
        works,
        extra,
    };
    let next = if report.missing > 0 {
        vec![format!("{} background fetch", env!("CARGO_BIN_NAME"))]
    } else {
        Vec::new()
    };
    output::print_success_with(ctx, &report, &next, |r| {
        println!(
            "Background: {} of {} installed{}",
            r.installed,
            r.installed + r.missing,
            if r.extra.is_empty() {
                String::new()
            } else {
                format!(", plus {} of your own", r.extra.len())
            }
        );
        let missing: Vec<&str> = r
            .works
            .iter()
            .filter(|w| !w.installed)
            .map(|w| w.profile.as_str())
            .collect();
        if !missing.is_empty() && missing.len() <= 10 {
            println!("  missing: {}", missing.join(", "));
        }
    })
}
