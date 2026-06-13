//! `profile` subcommands: build / list / show / remove.

use std::path::PathBuf;

use serde::Serialize;

use crate::engine::{profile::Profile, store, text};
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct BuiltProfile {
    name: String,
    tokens: u64,
    chunks: usize,
    chunk_size: usize,
    path: String,
}

pub fn build(
    ctx: Ctx,
    name: String,
    corpus: PathBuf,
    chunk_size: usize,
    force: bool,
) -> Result<(), AppError> {
    if store::exists(&name) && !force {
        return Err(AppError::InvalidInput(format!(
            "profile '{name}' already exists; pass --force to overwrite"
        )));
    }
    let text = text::read_corpus(&corpus)?;
    let profile = Profile::build(&name, &text, chunk_size)?;
    let path = store::save(&profile)?;

    let data = BuiltProfile {
        name: profile.name.clone(),
        tokens: profile.n_tokens,
        chunks: profile.n_chunks,
        chunk_size: profile.chunk_size,
        path: path.display().to_string(),
    };
    output::print_success_or(ctx, &data, |d| {
        use owo_colors::OwoColorize;
        println!(
            "Built profile {} ({} tokens, {} chunks)",
            d.name.green().bold(),
            d.tokens,
            d.chunks
        );
        println!("  {}", d.path.dimmed());
    });
    Ok(())
}

#[derive(Serialize)]
struct ProfileSummary {
    name: String,
    tokens: u64,
    chunks: usize,
    calibrated: bool,
}

pub fn list(ctx: Ctx) -> Result<(), AppError> {
    let mut rows = Vec::new();
    for name in store::list_names() {
        if let Ok(p) = store::load(&name) {
            rows.push(ProfileSummary {
                name: p.name,
                tokens: p.n_tokens,
                chunks: p.n_chunks,
                calibrated: p.calibration.is_some(),
            });
        }
    }
    output::print_success_or(ctx, &rows, |rows| {
        use comfy_table::{Table, presets::UTF8_FULL};
        if rows.is_empty() {
            println!("No profiles yet. Build one with: stylometry profile build <name> --corpus <path>");
            return;
        }
        let mut t = Table::new();
        t.load_preset(UTF8_FULL)
            .set_header(vec!["profile", "tokens", "chunks", "calibrated"]);
        for r in rows {
            t.add_row(vec![
                r.name.clone(),
                r.tokens.to_string(),
                r.chunks.to_string(),
                if r.calibrated { "yes" } else { "no" }.to_string(),
            ]);
        }
        println!("{t}");
    });
    Ok(())
}

#[derive(Serialize)]
struct TopWord {
    word: String,
    count: u64,
}

#[derive(Serialize)]
struct ShowProfile {
    name: String,
    tokens: u64,
    chunks: usize,
    chunk_size: usize,
    distinct_words: usize,
    distinct_trigrams: usize,
    top_words: Vec<TopWord>,
    calibration: Option<crate::engine::profile::Calibration>,
}

pub fn show(ctx: Ctx, name: String) -> Result<(), AppError> {
    let p = store::load(&name)?;
    let mut tw: Vec<(&String, &u64)> = p.word_counts.iter().collect();
    tw.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let top_words: Vec<TopWord> = tw
        .into_iter()
        .take(15)
        .map(|(w, c)| TopWord {
            word: w.clone(),
            count: *c,
        })
        .collect();

    let data = ShowProfile {
        name: p.name.clone(),
        tokens: p.n_tokens,
        chunks: p.n_chunks,
        chunk_size: p.chunk_size,
        distinct_words: p.word_counts.len(),
        distinct_trigrams: p.trigram_counts.len(),
        top_words,
        calibration: p.calibration.clone(),
    };
    output::print_success_or(ctx, &data, |d| {
        use owo_colors::OwoColorize;
        println!("{} — {} tokens, {} chunks", d.name.green().bold(), d.tokens, d.chunks);
        match &d.calibration {
            Some(c) => println!(
                "  calibrated: AUC {:.3}, accuracy {:.3}, threshold δ={:.3} ({} imposters)",
                c.auc, c.c_at_1, c.threshold, c.imposters
            ),
            None => println!("  not calibrated (run: stylometry calibrate {})", d.name),
        }
        let words: Vec<String> = d.top_words.iter().map(|t| t.word.clone()).collect();
        println!("  top words: {}", words.join(", ").dimmed());
    });
    Ok(())
}

pub fn remove(ctx: Ctx, name: String) -> Result<(), AppError> {
    store::remove(&name)?;
    #[derive(Serialize)]
    struct Removed {
        removed: String,
    }
    output::print_success_or(ctx, &Removed { removed: name.clone() }, |d| {
        println!("Removed profile {}", d.removed);
    });
    Ok(())
}
