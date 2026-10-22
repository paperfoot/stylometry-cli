//! `examples`: the author's own passages or posts most like what is being
//! written, as few-shot examples for an agent. Retrieval lives in
//! `engine::retrieve`; this command shapes it for agents (source,
//! similarity, engagement, text) and says when nothing matches the task.

use std::path::PathBuf;

use serde::Serialize;

use crate::commands::input;
use crate::engine::profile::{ItemKind, Profile};
use crate::engine::retrieve::{self, Request, Retrieval, TopicMatch};
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx};

const BIN: &str = env!("CARGO_BIN_NAME");
/// Long-form examples are two base passages: enough to carry rhythm and
/// paragraphing, short enough to paste into a prompt.
pub const PASSAGE_WORDS: usize = 500;

#[derive(Debug, Serialize)]
pub struct Example {
    pub rank: usize,
    pub source: String,
    /// Blended rank score (topic, shape or engagement by register), 0–1.
    pub similarity: f64,
    /// Vocabulary overlap with the task (TF-IDF cosine), 0 without a task.
    pub topic: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engagement_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// Long-form: nearness of the passage's style to the target (the draft
    /// given with --like, else the author's typical passage), 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<f64>,
    /// Long-form: dialogue share of the passage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialogue_share: Option<f64>,
    /// Fiction: narration mode, e.g. "first person, present".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The opening of its work (blogs keep openings: they are imitated most).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub opening: bool,
    pub words: usize,
    pub text: String,
}

#[derive(Serialize)]
struct ExamplesOut {
    profile: String,
    register: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    /// What the examples were matched to: like (a file), about (words), none.
    task: &'static str,
    topic_match: TopicMatch,
    candidates: usize,
    /// Content words of the task found in the corpus (0 = nothing to match).
    query_terms: usize,
    examples: Vec<Example>,
    /// One line on the examples' marks: their quotes, ellipses and dashes
    /// are the author's typography, part of the voice to copy.
    typography_note: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

/// Rows for a retrieval, in rank order.
pub fn rows(p: &Profile, r: &Retrieval) -> Vec<Example> {
    let short = p.register.is_short_form();
    r.hits
        .iter()
        .enumerate()
        .map(|(rank, h)| {
            let first = &p.chunks[h.chunks[0]];
            let text = h
                .chunks
                .iter()
                .flat_map(|&i| p.chunks[i].paragraphs.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n\n");
            let meta = first.meta.as_ref();
            Example {
                rank: rank + 1,
                source: p.sources[first.source as usize].name.clone(),
                similarity: h.similarity,
                topic: h.topic,
                engagement_pct: h.engagement_pct,
                likes: meta.and_then(|m| m.likes),
                date: meta.and_then(|m| m.date.clone()),
                kind: short.then(|| retrieve::kind_label(meta.map(|m| m.kind).unwrap_or_default())),
                shape: h.shape,
                dialogue_share: h.dialogue_share,
                mode: h.mode.map(|m| format!("{} person, {}", m.pov, m.tense)),
                opening: h.opening,
                words: crate::engine::text::count_words(&text),
                text,
            }
        })
        .collect()
}

pub fn run(
    ctx: Ctx,
    profile: String,
    like: Option<PathBuf>,
    about: Option<String>,
    limit: usize,
    as_kind: ItemKind,
) -> Result<(), AppError> {
    if limit == 0 {
        return Err(AppError::input(
            "-n must be at least 1",
            format!("Ask for a few examples: {BIN} examples {profile} -n 5"),
        ));
    }
    let p = store::load(&profile)?;
    let (task, query) = match (&like, &about) {
        (Some(f), _) => ("like", Some(input::read_text(Some(f.clone()), None, "examples")?)),
        (None, Some(a)) if !a.trim().is_empty() => ("about", Some(a.clone())),
        _ => ("none", None),
    };
    let mut req = Request::new(query.as_deref(), limit, as_kind);
    req.window_words = PASSAGE_WORDS;
    let r = retrieve::retrieve(&p, &req);
    let short = p.register.is_short_form();
    let data = ExamplesOut {
        profile: p.name.clone(),
        register: p.register.as_str(),
        kind: short.then(|| retrieve::kind_label(as_kind)),
        task,
        topic_match: r.topic_match,
        candidates: r.candidates,
        query_terms: r.query_terms,
        examples: rows(&p, &r),
        typography_note: crate::commands::guide::typography_note(&crate::engine::metrics::typography_habits(&p)),
        notes: r.notes.clone(),
    };
    let mut next = Vec::new();
    match (&like, &about) {
        (Some(f), _) => next.push(format!("{BIN} guide {} --for {} -o brief.md", p.name, f.display())),
        (None, Some(a)) => next.push(format!("{BIN} guide {} --about \"{a}\" -o brief.md", p.name)),
        _ => next.push(format!("{BIN} examples {} --about \"<what you are writing>\"", p.name)),
    }
    next.push(format!("{BIN} check draft.md --profile {}", p.name));
    output::print_success_with(ctx, &data, &next, |d| {
        use owo_colors::OwoColorize;
        println!(
            "{} {} examples ({}, topic match: {:?})",
            d.profile.bold(),
            d.examples.len(),
            d.register,
            d.topic_match
        );
        for n in &d.notes {
            println!("{} {n}", "note:".yellow());
        }
        println!("{}", d.typography_note);
        for e in &d.examples {
            let mut tags = vec![format!("similarity {:.2}", e.similarity)];
            if let Some(x) = e.engagement_pct {
                tags.push(format!("engagement p{:.0}", x * 100.0));
            }
            if let Some(m) = &e.mode {
                tags.push(m.clone());
            }
            println!("\n{} {} · {} · {} words", format!("{}.", e.rank).bold(), e.source, tags.join(" · "), e.words);
            for line in e.text.lines() {
                println!("  {line}");
            }
        }
    })
}
