//! `guide`: a Markdown style brief an agent can write from. Built on the
//! `analyze` report, ordered the way the evidence says briefs work best:
//! the voice in plain words, measurable targets, per-concept word choices,
//! a short ban list, then the author's own passages to imitate and a passage
//! to continue (continuation beats description for holding a voice), and
//! finally the write → check → revise loop.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::commands::analyze::{self, VoiceReport};
use crate::engine::metrics::{self, Band, Unit};
use crate::engine::profile::Profile;
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx};

const BIN: &str = env!("CARGO_BIN_NAME");
/// Exemplars are two base passages (~500 words): long enough to carry
/// rhythm and paragraphing, short enough to sit in a prompt.
const EXEMPLAR_WINDOW: usize = 500;
/// Metrics the plain-words summary and the target table lead with.
const HEADLINE: &[&str] = &[
    "sentence_words_median",
    "sentence_words_mean",
    "short_sentence_share",
    "long_sentence_share",
    "one_sentence_paragraph_share",
    "dialogue_word_share",
    "questions_per_1k",
    "contractions_per_1k",
    "em_dashes_per_1k",
    "semicolons_per_1k",
    "similes_per_1k",
    "ly_adverbs_per_1k",
];

#[derive(Serialize)]
struct Exemplar {
    work: String,
    words: usize,
    dialogue: bool,
    text: String,
}

#[derive(Serialize)]
struct Guide {
    profile: String,
    window_words: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    exemplars: usize,
    markdown: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// Arguments of `guide` (see cli.rs for their help text).
pub struct GuideArgs {
    pub profile: String,
    pub exemplars: usize,
    /// A draft/outline whose subject and form the exemplars should match.
    pub for_file: Option<PathBuf>,
    /// A few words on what is being written, for the same purpose.
    pub about: Option<String>,
    /// Template override (default: the profile's register).
    pub register: Option<crate::engine::profile::Register>,
    /// Short-form: which kind of item the brief is for.
    pub as_kind: crate::engine::profile::ItemKind,
    pub limit: usize,
    pub window: usize,
    pub out: Option<PathBuf>,
}

pub fn run(ctx: Ctx, a: GuideArgs) -> Result<(), AppError> {
    let GuideArgs { profile, exemplars, limit, window, out, .. } = a;
    let p = store::load(&profile)?;
    let r = analyze::report(&p, limit, window);
    let picks = pick_exemplars(&p, exemplars + 1);
    let markdown = render(&p, &r, &picks, exemplars);
    let path = match &out {
        Some(path) => {
            std::fs::write(path, &markdown)?;
            Some(path.display().to_string())
        }
        None => None,
    };
    let data = Guide {
        profile: p.name.clone(),
        window_words: window,
        path,
        exemplars: picks.len().min(exemplars),
        markdown,
        warnings: r.warnings.clone(),
    };
    let next = vec![format!("{BIN} check draft.md --profile {}", p.name)];
    output::print_success_with(ctx, &data, &next, |d| match &d.path {
        Some(path) => println!("Wrote the {} brief to {path}", d.profile),
        None => println!("{}", d.markdown),
    })
}

/// The author's most typical passages: spans whose metrics sit closest to
/// the author's own medians (distance in band widths), skipping each work's
/// first and last spans (front matter, endings), at most one per work until
/// every work has given one, and including one dialogue-rich span when the
/// author writes dialogue.
fn pick_exemplars(p: &Profile, n: usize) -> Vec<Exemplar> {
    if n == 0 {
        return Vec::new();
    }
    let fw = metrics::top_function_words(p, metrics::FUNCTION_WORDS_TRACKED);
    let ranges = metrics::ranges(p, EXEMPLAR_WINDOW, &fw);
    let (k, rows) = metrics::window_measures(p, EXEMPLAR_WINDOW, &fw);
    let spans = p.spans(k);
    let dialogue_j = ranges.bands.iter().position(|b| b.key == "dialogue_word_share");
    let median_dialogue = dialogue_j.map(|j| ranges.bands[j].median).unwrap_or(0.0);

    // Position of each span within its work, to skip the edges.
    let mut scored: Vec<(f64, usize, bool)> = Vec::new();
    for (si, ((vals, _), span)) in rows.iter().zip(&spans).enumerate() {
        let src = p.chunks[span[0]].source;
        let same: Vec<usize> = (0..spans.len())
            .filter(|&o| p.chunks[spans[o][0]].source == src)
            .collect();
        let pos = same.iter().position(|&o| o == si).unwrap_or(0);
        if same.len() > 4 && (pos < 2 || pos + 2 >= same.len()) {
            continue;
        }
        let dist: f64 = ranges
            .bands
            .iter()
            .enumerate()
            .filter(|(_, b)| HEADLINE.contains(&b.key.as_str()))
            .map(|(j, b)| {
                let width = (b.high - b.low).abs().max(1e-6);
                ((vals[j] - b.median) / width).powi(2)
            })
            .sum();
        let dialogue = dialogue_j.is_some_and(|j| vals[j] > median_dialogue.max(0.15));
        scored.push((dist, si, dialogue));
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut chosen: Vec<usize> = Vec::new();
    let mut used_src = std::collections::HashSet::new();
    let n_src = p.sources.len();
    if median_dialogue >= 0.1 {
        if let Some(&(_, si, _)) = scored.iter().find(|s| s.2) {
            chosen.push(si);
            used_src.insert(p.chunks[spans[si][0]].source);
        }
    }
    for &(_, si, _) in &scored {
        if chosen.len() >= n {
            break;
        }
        let src = p.chunks[spans[si][0]].source;
        let fresh_src = !used_src.contains(&src) || used_src.len() >= n_src;
        if !chosen.contains(&si) && fresh_src {
            chosen.push(si);
            used_src.insert(src);
        }
    }
    chosen
        .into_iter()
        .map(|si| {
            let text = spans[si]
                .iter()
                .flat_map(|&i| p.chunks[i].paragraphs.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n\n");
            let dialogue = dialogue_j.is_some_and(|j| rows[si].0[j] > median_dialogue.max(0.15));
            Exemplar {
                work: p.sources[p.chunks[spans[si][0]].source as usize].name.clone(),
                words: crate::engine::text::count_words(&text),
                dialogue,
                text,
            }
        })
        .collect()
}

fn band<'a>(r: &'a VoiceReport, key: &str) -> Option<&'a Band> {
    r.ranges.bands.iter().find(|b| b.key == key)
}

fn fmt_num(v: f64) -> String {
    if v.abs() >= 10.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

/// The voice in a few plain sentences, generated from the measured medians.
fn summary(r: &VoiceReport) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(med), Some(short)) = (band(r, "sentence_words_median"), band(r, "short_sentence_share")) {
        out.push(format!(
            "Typical sentence about {} words; {} of sentences are five words or fewer.",
            fmt_num(med.median),
            pct(short.median)
        ));
    }
    if let Some(d) = band(r, "dialogue_word_share") {
        let how = if d.median >= 0.35 {
            "Dialogue carries much of the text"
        } else if d.median >= 0.15 {
            "Dialogue and narration are balanced"
        } else {
            "Mostly narration; dialogue is sparse"
        };
        out.push(format!("{how} (about {} of words are spoken).", pct(d.median)));
    }
    if let Some(c) = band(r, "contractions_per_1k") {
        let how = if c.median >= 25.0 {
            "Contractions are the norm"
        } else if c.median >= 10.0 {
            "Contractions are common"
        } else {
            "Contractions are rare"
        };
        out.push(format!("{how} ({} per 1,000 words).", fmt_num(c.median)));
    }
    if let Some(q) = band(r, "questions_per_1k") {
        if q.median >= 4.0 {
            out.push(format!("Questions are frequent ({} per 1,000 words).", fmt_num(q.median)));
        }
    }
    let tags: Vec<String> = r
        .dialogue_tags
        .verbs
        .iter()
        .take(3)
        .map(|v| format!("*{}* {}", v.verb, pct(v.share)))
        .collect();
    if !tags.is_empty() {
        out.push(format!("Dialogue tags: {}.", tags.join(", ")));
    }
    out
}

fn render(p: &Profile, r: &VoiceReport, picks: &[Exemplar], n_exemplars: usize) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Writing in the voice of `{}`\n", p.name);
    if !p.components.is_empty() {
        let mix: Vec<String> = p
            .components
            .iter()
            .map(|(n, s)| format!("{n} {}", pct(*s)))
            .collect();
        let _ = writeln!(md, "A blended voice: {}.\n", mix.join(" + "));
    }
    let _ = writeln!(
        md,
        "Measured from {} words in {} work(s). Write so a reader of this author would not notice the switch. \
         Imitate the mechanisms below, never the lines: drafts are checked for copied 8-word runs.\n",
        r.words, r.works
    );

    let _ = writeln!(md, "## The voice in brief\n");
    for s in summary(r) {
        let _ = writeln!(md, "- {s}");
    }

    let _ = writeln!(
        md,
        "\n## Targets ({}-word passages; hit the range, not just the median)\n",
        r.ranges.window_words
    );
    let _ = writeln!(md, "| measure | range | typical |\n|---|---|---|");
    for key in HEADLINE {
        if let Some(b) = band(r, key) {
            let f = |v: f64| if matches!(b.unit, Unit::Share) { pct(v) } else { fmt_num(v) };
            let _ = writeln!(md, "| {} | {}–{} | {} |", b.label, f(b.low), f(b.high), f(b.median));
        }
    }
    let _ = writeln!(
        md,
        "\nThe full set ({} measures) is in `{BIN} analyze {} --json`; `check` scores all of them at the draft's own length.",
        r.ranges.bands.len(),
        p.name
    );

    let concepts: Vec<_> = r
        .concepts
        .iter()
        .filter(|c| c.preferred_share >= 0.5)
        .collect();
    if !concepts.is_empty() {
        let _ = writeln!(md, "\n## Word choices (use the left column)\n");
        let _ = writeln!(md, "| use | share | never/rarely |\n|---|---|---|");
        for c in concepts {
            let never = if c.never_used.is_empty() {
                c.members
                    .iter()
                    .filter(|m| m.word != c.preferred && m.share < 0.05)
                    .map(|m| m.word.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                c.never_used.join(", ")
            };
            let _ = writeln!(md, "| {} | {} | {} |", c.preferred, pct(c.preferred_share), never);
        }
    }

    let style: Vec<String> = r.style_words.iter().map(|w| w.word.clone()).collect();
    let content: Vec<String> = r.content_words.iter().map(|w| w.word.clone()).collect();
    let phrases: Vec<String> = r.phrases.iter().map(|w| w.phrase.clone()).collect();
    if !style.is_empty() || !phrases.is_empty() {
        let _ = writeln!(md, "\n## Signature habits (use naturally, never as a checklist)\n");
        if !style.is_empty() {
            let _ = writeln!(md, "- Glue words leaned on: {}", style.join(", "));
        }
        if !content.is_empty() {
            let _ = writeln!(md, "- Recurring words: {}", content.join(", "));
        }
        if !phrases.is_empty() {
            let _ = writeln!(md, "- Phrases: {}", phrases.join("; "));
        }
    }

    let avoided: Vec<String> = r
        .avoided_words
        .iter()
        .map(|a| match &a.instead {
            Some(i) => format!("{} (→ {i})", a.word),
            None => a.word.clone(),
        })
        .collect();
    if !avoided.is_empty() || !r.tells_unused.is_empty() {
        let _ = writeln!(md, "\n## Avoid\n");
        if !avoided.is_empty() {
            let _ = writeln!(md, "- Words this author doesn't use: {}", avoided.join(", "));
        }
        if !r.tells_unused.is_empty() {
            let _ = writeln!(
                md,
                "- Machine-prose tells this author never writes: {}",
                r.tells_unused.join(", ")
            );
        }
    }

    let (examples, cont) = if picks.len() > n_exemplars {
        (&picks[..n_exemplars], picks.last())
    } else {
        (picks, None)
    };
    if !examples.is_empty() {
        let _ = writeln!(md, "\n## The author's own passages (study the mechanics; do not reuse)\n");
        for (i, e) in examples.iter().enumerate() {
            let kind = if e.dialogue { "dialogue" } else { "narration" };
            let _ = writeln!(md, "### Passage {} ({}, {} words, {kind})\n", i + 1, e.work, e.words);
            let _ = writeln!(md, "{}\n", quote(&e.text));
        }
    }
    if let Some(c) = cont {
        let _ = writeln!(
            md,
            "## Warm-up: continue this passage for ~300 words in the same voice, then write your scene\n"
        );
        let _ = writeln!(md, "{}\n", quote(&c.text));
    }

    let _ = writeln!(md, "## Loop\n");
    let _ = writeln!(md, "1. Draft the scene from your brief.");
    let _ = writeln!(
        md,
        "2. `{BIN} check draft.md --profile {}`: fix hard failures (copied runs, banned words) first, then out-of-range measures, then word-level findings.",
        p.name
    );
    let _ = writeln!(md, "3. Re-run `check` until it passes; judge passages of 1,000+ words (shorter is directional).");
    md
}

fn quote(text: &str) -> String {
    text.split("\n\n")
        .map(|para| format!("> {para}"))
        .collect::<Vec<_>>()
        .join("\n>\n")
}
