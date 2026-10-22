//! `analyze`: the voice report for a profile or a path. JSON for agents,
//! a compact scannable summary for people. `guide` builds on the same report.

use serde::Serialize;

use crate::commands::input;
use crate::engine::metrics::{self, ItemHabits, Ranges, TypographyHabits};
use crate::engine::profile::{ItemKind, Profile, ProfileKind, Register};
use crate::engine::retrieve;
use crate::engine::voice::{self, AvoidedWord, Concept, Corpus, DialogueTags, Modes, Phrase, SignatureWord};
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Debug, Serialize)]
pub struct TellUse {
    pub tell: &'static str,
    pub per_1k: f64,
}

/// Short-form: one kind of item (posts, replies, thread parts) measured on
/// its own, since each is written differently.
#[derive(Debug, Serialize)]
pub struct KindReport {
    pub kind: &'static str,
    pub items: usize,
    /// p15 / median / p85 of per-item measures over this kind's items.
    pub ranges: Ranges,
    pub habits: ItemHabits,
}

/// Everything a voice is made of, measured once.
#[derive(Debug, Serialize)]
pub struct VoiceReport {
    pub profile: String,
    pub kind: ProfileKind,
    pub register: Register,
    pub register_reason: String,
    /// The measures a brief for this register leads with.
    pub headline: Vec<&'static str>,
    pub words: u64,
    pub works: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<(String, f64)>,
    /// Metric bands (p15 / median / p85) over the author's windows of `window_words`.
    pub ranges: Ranges,
    /// How the author sets type: quote, apostrophe, ellipsis, dash and
    /// scene-break forms with their shares (a family with too few marks to
    /// call a habit is empty). Readers notice these before the words.
    pub typography: TypographyHabits,
    /// Function words and contractions the author leans on.
    pub style_words: Vec<SignatureWord>,
    /// Content words the author leans on (habit or subject).
    pub content_words: Vec<SignatureWord>,
    /// The content words that are habits, not subjects: in 30%+ of passages
    /// and not characteristic of any single work.
    pub recurring_words: Vec<SignatureWord>,
    pub avoided_words: Vec<AvoidedWord>,
    /// Synonym groups: the author's preferred member and the ones never used.
    pub concepts: Vec<Concept>,
    pub phrases: Vec<Phrase>,
    /// 4+ word runs repeated in 3+ items or passages: signature phrasing
    /// (the copy guard exempts them).
    pub formulas: Vec<Phrase>,
    pub dialogue_tags: DialogueTags,
    /// Long-form: narration POV/tense per chapter (a dual narrative has two).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Modes>,
    /// Short-form: per-kind item ranges and habits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<KindReport>,
    /// LLM tells the author genuinely uses: voice, not slop, for this author.
    pub tells_used: Vec<TellUse>,
    /// LLM tells the author never uses: the short ban list for a brief.
    pub tells_unused: Vec<&'static str>,
    /// Short-form: social-post tells (phrases, layouts) the author never uses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub social_tells_unused: Vec<&'static str>,
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
    let short = p.register.is_short_form();
    let items: Vec<KindReport> = if short {
        [ItemKind::Post, ItemKind::Reply, ItemKind::ThreadPart, ItemKind::Quote]
            .into_iter()
            .filter_map(|k| {
                let n = p
                    .chunks
                    .iter()
                    .filter(|c| c.meta.as_ref().map(|m| m.kind).unwrap_or_default() == k)
                    .count();
                (n > 0).then(|| KindReport {
                    kind: retrieve::kind_label(k),
                    items: n,
                    ranges: metrics::item_ranges(p, k),
                    habits: metrics::item_habits(p, k),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let social_tells_unused = if short {
        voice::unused_social_tells(&rates, &voice::item_tell_shares(p))
    } else {
        Vec::new()
    };
    VoiceReport {
        profile: p.name.clone(),
        kind: p.kind,
        register: p.register,
        register_reason: p.register_reason.clone(),
        headline: metrics::headline_keys(p.register).to_vec(),
        words: p.n_tokens,
        works: p.sources.len(),
        components: p.components.clone(),
        typography: metrics::typography_habits(p),
        ranges,
        recurring_words: voice::habit_words(p, &content_words),
        style_words,
        content_words,
        avoided_words: voice::avoided_words(&corpus, limit),
        concepts: voice::concepts(&corpus, 10.0).into_iter().take(limit).collect(),
        phrases: voice::signature_phrases(&corpus, limit),
        formulas: voice::signature_formulas(p, limit),
        dialogue_tags: voice::dialogue_tags(&corpus, 12),
        modes: (p.register == Register::Fiction).then(|| voice::section_modes(p)).flatten(),
        items,
        tells_used,
        tells_unused: voice::unused_tells(&rates, limit),
        social_tells_unused,
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
    println!("{} {} ({})", "register".bold(), r.register.as_str(), r.register_reason);
    let t = &r.typography;
    let typo: Vec<String> = [
        ("quotes", &t.double_quotes),
        ("apostrophes", &t.apostrophes),
        ("ellipses", &t.ellipses),
        ("dashes", &t.dashes),
        ("scene breaks", &t.scene_breaks),
    ]
    .iter()
    .filter_map(|(name, forms)| {
        let f: Vec<String> = forms.iter().take(3).map(|(k, s)| format!("{k:?} {:.0}%", s * 100.0)).collect();
        (!f.is_empty()).then(|| format!("{name} {}", f.join(", ")))
    })
    .collect();
    if !typo.is_empty() {
        println!("{} {}", "typography".bold(), typo.join("; "));
    }
    if let Some(m) = &r.modes {
        let modes: Vec<String> = m
            .modes
            .iter()
            .map(|x| format!("{} person {} {:.0}%", x.pov, x.tense, x.share * 100.0))
            .collect();
        println!("{} {} (over {} {})", "modes".bold(), modes.join(", "), m.sections, m.basis);
    }
    for k in &r.items {
        let mut line = format!("{} {}", k.items, k.kind);
        if let Some(b) = k.ranges.bands.iter().find(|b| b.key == "item_words") {
            line.push_str(&format!(", {}–{} words (median {})", b.low, b.high, b.median));
        }
        if !k.habits.endings.is_empty() {
            let e: Vec<String> = k.habits.endings.iter().take(4).map(|(e, s)| format!("{e} {:.0}%", s * 100.0)).collect();
            line.push_str(&format!("; endings {}", e.join(", ")));
        }
        println!("{} {line}", "items".bold());
    }
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
    if !r.formulas.is_empty() {
        println!("{}", "formulas".bold());
        println!(
            "  {}",
            r.formulas.iter().take(10).map(|p| p.phrase.as_str()).collect::<Vec<_>>().join("; ")
        );
    }
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
