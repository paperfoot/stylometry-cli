//! `diff`: contrast two voices. Words and phrases each leans on that the
//! other doesn't (two-corpus log-odds with a pooled prior, names removed,
//! habits only: dispersed across the corpus), word choices where they
//! part ways, and the measures that differ most at a shared passage length.

use serde::Serialize;

use crate::commands::input;
use crate::engine::metrics;
use crate::engine::profile::Profile;
use crate::engine::voice::{self, Corpus};
use crate::error::AppError;
use crate::output::{self, Ctx};

/// Measures are compared at this length: a scene for long-form voices; for
/// posts and messages a pooled run of about ten items, so even a few
/// hundred posts give enough windows.
fn window_for(a: &Profile, b: &Profile) -> usize {
    if a.register.is_short_form() || b.register.is_short_form() { 250 } else { 1000 }
}

/// A word must appear in at least this share of the favouring side's
/// passages (or items, which are far shorter) to count as a habit rather
/// than a topic.
fn min_spread(p: &Profile) -> f64 {
    if p.register.is_short_form() { 0.01 } else { 0.03 }
}

#[derive(Serialize)]
struct Leaning {
    word: String,
    a_per_1k: f64,
    b_per_1k: f64,
    z: f64,
}

#[derive(Serialize)]
struct ChoiceGap {
    concept: String,
    a_prefers: String,
    a_share: f64,
    b_prefers: String,
    b_share: f64,
}

#[derive(Serialize)]
struct MeasureGap {
    key: String,
    label: String,
    a_median: f64,
    b_median: f64,
    /// Gap in units of the two bands' average width (|a - b| / width).
    gap: f64,
}

#[derive(Serialize)]
struct Diff {
    a: String,
    b: String,
    /// Length (words) the measures were compared at.
    window_words: usize,
    a_words: Vec<Leaning>,
    b_words: Vec<Leaning>,
    word_choices: Vec<ChoiceGap>,
    measures: Vec<MeasureGap>,
}

fn leanings(a: &Profile, b: &Profile, limit: usize) -> (Vec<Leaning>, Vec<Leaning>) {
    let (na, nb) = (a.n_tokens.max(1) as f64, b.n_tokens.max(1) as f64);
    let mut words: Vec<&String> = a.word_counts.keys().chain(b.word_counts.keys()).collect();
    words.sort();
    words.dedup();
    let mut scored: Vec<Leaning> = words
        .into_iter()
        // "url" and "user" are the placeholders links and @handles become.
        .filter(|w| !matches!(w.as_str(), "url" | "user"))
        .filter(|w| !voice::is_name_or_number(a, w) && !voice::is_name_or_number(b, w))
        .filter_map(|w| {
            let ya = a.word_counts.get(w).copied().unwrap_or(0) as f64;
            let yb = b.word_counts.get(w).copied().unwrap_or(0) as f64;
            if ya + yb < 10.0 {
                return None;
            }
            let pooled = (ya + yb) / (na + nb);
            let z = voice::log_odds_z2(ya, na, yb, nb, pooled);
            let favoured = if z > 0.0 { a } else { b };
            if voice::spread(favoured, w) < min_spread(favoured) {
                return None;
            }
            Some(Leaning {
                word: w.clone(),
                a_per_1k: voice::r3(ya * 1000.0 / na),
                b_per_1k: voice::r3(yb * 1000.0 / nb),
                z: voice::r3(z),
            })
        })
        .collect();
    scored.sort_by(|x, y| y.z.total_cmp(&x.z));
    let a_words: Vec<Leaning> = scored.iter().filter(|l| l.z > 3.0).take(limit).map(clone_l).collect();
    let b_words: Vec<Leaning> = scored.iter().rev().filter(|l| l.z < -3.0).take(limit).map(clone_l).collect();
    (a_words, b_words)
}

fn clone_l(l: &Leaning) -> Leaning {
    Leaning {
        word: l.word.clone(),
        a_per_1k: l.a_per_1k,
        b_per_1k: l.b_per_1k,
        z: l.z,
    }
}

pub fn run(ctx: Ctx, a: String, b: String, limit: usize) -> Result<(), AppError> {
    let pa = input::resolve_profile(&a)?;
    let pb = input::resolve_profile(&b)?;
    let (a_words, b_words) = leanings(&pa, &pb, limit);

    let (ca, cb) = (Corpus::of(&pa), Corpus::of(&pb));
    let concepts_b = voice::concepts(&cb, 10.0);
    let mut word_choices: Vec<ChoiceGap> = voice::concepts(&ca, 10.0)
        .into_iter()
        .filter_map(|x| {
            let y = concepts_b.iter().find(|y| y.group == x.group)?;
            (x.preferred != y.preferred).then(|| ChoiceGap {
                concept: x.members.iter().map(|m| m.word.as_str()).collect::<Vec<_>>().join("/"),
                a_prefers: x.preferred.clone(),
                a_share: voice::r3(x.preferred_share),
                b_prefers: y.preferred.clone(),
                b_share: voice::r3(y.preferred_share),
            })
        })
        .collect();
    word_choices.truncate(limit);

    let fw = metrics::top_function_words(&pa, metrics::FUNCTION_WORDS_TRACKED);
    let window = window_for(&pa, &pb);
    let ra = metrics::ranges(&pa, window, &fw);
    let rb = metrics::ranges(&pb, window, &fw);
    let mut measures: Vec<MeasureGap> = ra
        .bands
        .iter()
        .filter_map(|x| {
            let y = rb.bands.iter().find(|y| y.key == x.key)?;
            let width = ((x.high - x.low) + (y.high - y.low)) / 2.0;
            if width <= 1e-9 {
                return None;
            }
            Some(MeasureGap {
                key: x.key.clone(),
                label: x.label.clone(),
                a_median: voice::r3(x.median),
                b_median: voice::r3(y.median),
                gap: voice::r3((x.median - y.median).abs() / width),
            })
        })
        .collect();
    measures.sort_by(|x, y| y.gap.total_cmp(&x.gap));
    measures.truncate(limit);

    let data = Diff {
        a: pa.name.clone(),
        b: pb.name.clone(),
        window_words: window,
        a_words,
        b_words,
        word_choices,
        measures,
    };
    output::print_success_or(ctx, &data, |d| {
        use owo_colors::OwoColorize;
        let line = |ws: &[Leaning]| ws.iter().take(15).map(|l| l.word.clone()).collect::<Vec<_>>().join(", ");
        println!("{} leans on: {}", d.a.bold(), line(&d.a_words));
        println!("{} leans on: {}", d.b.bold(), line(&d.b_words));
        for c in d.word_choices.iter().take(10) {
            println!(
                "  {}: {} '{}' ({:.0}%) vs {} '{}' ({:.0}%)",
                c.concept.dimmed(),
                d.a,
                c.a_prefers,
                c.a_share * 100.0,
                d.b,
                c.b_prefers,
                c.b_share * 100.0
            );
        }
        for m in d.measures.iter().take(8) {
            println!("  {}: {} vs {}", m.label, m.a_median, m.b_median);
        }
    })
}
