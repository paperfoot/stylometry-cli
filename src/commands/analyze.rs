//! `analyze`: the voice report for a profile or a path. JSON for agents,
//! a compact scannable summary for people. `guide` builds on the same report.

use serde::Serialize;

use crate::commands::input;
use crate::engine::metrics::{self, Ranges};
use crate::engine::profile::{Profile, ProfileKind};
use crate::engine::voice::{self, AvoidedWord, Concept, Corpus, DialogueTags, Phrase, SignatureWord};
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Debug, Serialize)]
pub struct TellUse {
    pub tell: &'static str,
    pub per_1k: f64,
}

/// Everything a voice is made of, measured once.
#[derive(Debug, Serialize)]
pub struct VoiceReport {
    pub profile: String,
    pub kind: ProfileKind,
    pub words: u64,
    pub works: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<(String, f64)>,
    /// Metric bands (p15 / median / p85) over the author's windows of `window_words`.
    pub ranges: Ranges,
    /// Function words and contractions the author leans on.
    pub style_words: Vec<SignatureWord>,
    /// Content words the author leans on (habit or subject).
    pub content_words: Vec<SignatureWord>,
    pub avoided_words: Vec<AvoidedWord>,
    /// Synonym groups: the author's preferred member and the ones never used.
    pub concepts: Vec<Concept>,
    pub phrases: Vec<Phrase>,
    pub dialogue_tags: DialogueTags,
    /// LLM tells the author genuinely uses: voice, not slop, for this author.
    pub tells_used: Vec<TellUse>,
    /// LLM tells the author never uses: the short ban list for a brief.
    pub tells_unused: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn report(p: &Profile, limit: usize, window: usize) -> VoiceReport {
    let fw = metrics::top_function_words(p, metrics::FUNCTION_WORDS_TRACKED);
    let ranges = metrics::ranges(p, window, &fw);
    let corpus = Corpus::of(p);
    let (style_words, content_words) = voice::signature_words(p, limit);
    let rates = voice::author_tell_rates(&corpus);
    let mut tells_used: Vec<TellUse> = rates
        .iter()
        .filter(|(_, r)| **r >= 0.05)
        .map(|(t, r)| TellUse { tell: t, per_1k: voice::r3(*r) })
        .collect();
    tells_used.sort_by(|a, b| b.per_1k.total_cmp(&a.per_1k));
    tells_used.truncate(limit);
    let mut warnings = Vec::new();
    if ranges.directional {
        warnings.push(format!(
            "only {} windows of {} words in this corpus: ranges are directional; add text or use a shorter --window",
            ranges.windows, window
        ));
    }
    if p.n_tokens < 20_000 {
        warnings.push(format!(
            "{} words is a small corpus: word lists are directional (aim for 50k+)",
            p.n_tokens
        ));
    }
    VoiceReport {
        profile: p.name.clone(),
        kind: p.kind,
        words: p.n_tokens,
        works: p.sources.len(),
        components: p.components.clone(),
        ranges,
        style_words,
        content_words,
        avoided_words: voice::avoided_words(&corpus, limit),
        concepts: voice::concepts(&corpus, 10.0).into_iter().take(limit).collect(),
        phrases: voice::signature_phrases(&corpus, limit),
        dialogue_tags: voice::dialogue_tags(&corpus, 12),
        tells_used,
        tells_unused: voice::unused_tells(&rates, limit),
        warnings,
    }
}

pub fn run(ctx: Ctx, source: String, limit: usize, window: usize) -> Result<(), AppError> {
    let p = input::resolve_profile(&source)?;
    let r = report(&p, limit, window);
    let bin = env!("CARGO_BIN_NAME");
    let next = if crate::engine::store::exists(&p.name) {
        vec![
            format!("{bin} guide {} -o brief.md", p.name),
            format!("{bin} check draft.md --profile {}", p.name),
        ]
    } else {
        vec![format!("{bin} profile build <name> --corpus {source}")]
    };
    output::print_success_with(ctx, &r, &next, print_human)
}

fn words_line(ws: &[SignatureWord], n: usize) -> String {
    ws.iter()
        .take(n)
        .map(|w| format!("{} ({:.1}/1k)", w.word, w.per_1k))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_human(r: &VoiceReport) {
    use owo_colors::OwoColorize;
    println!(
        "{} {} words, {} works{}",
        r.profile.bold(),
        r.words,
        r.works,
        if r.components.is_empty() {
            String::new()
        } else {
            format!(
                " (blend: {})",
                r.components
                    .iter()
                    .map(|(n, s)| format!("{n} {:.0}%", s * 100.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    println!("{}", "style words".bold());
    println!("  {}", words_line(&r.style_words, 15));
    println!("{}", "content words".bold());
    println!("  {}", words_line(&r.content_words, 15));
    println!("{}", "never/rarely uses".bold());
    println!(
        "  {}",
        r.avoided_words
            .iter()
            .take(20)
            .map(|a| match &a.instead {
                Some(i) => format!("{} (uses {i})", a.word),
                None => a.word.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("{}", "synonym habits".bold());
    for c in r.concepts.iter().take(12) {
        let alts = c
            .members
            .iter()
            .skip(1)
            .take(3)
            .map(|m| format!("{} {:.0}%", m.word, m.share * 100.0))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {} {:.0}%{}{}",
            c.preferred,
            c.preferred_share * 100.0,
            if alts.is_empty() { String::new() } else { format!("  ({alts})") },
            if c.never_used.is_empty() {
                String::new()
            } else {
                format!("  never: {}", c.never_used.iter().take(4).cloned().collect::<Vec<_>>().join(", "))
            }
        );
    }
    println!("{}", "phrases".bold());
    println!(
        "  {}",
        r.phrases.iter().take(15).map(|p| p.phrase.as_str()).collect::<Vec<_>>().join(", ")
    );
    println!("{}", "dialogue tags".bold());
    println!(
        "  {}",
        r.dialogue_tags
            .verbs
            .iter()
            .take(8)
            .map(|v| format!("{} {:.0}%", v.verb, v.share * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "{} at {} words ({} windows{})",
        "ranges".bold(),
        r.ranges.window_words,
        r.ranges.windows,
        if r.ranges.directional { ", directional" } else { "" }
    );
    for b in r.ranges.bands.iter().take(metrics::defs(&[]).len()) {
        println!("  {:<46} {:>8} – {:<8} (median {})", b.label, b.low, b.high, b.median);
    }
    for w in &r.warnings {
        println!("{} {w}", "warning:".yellow());
    }
}
