//! `guide`: a Markdown style brief an agent can write from, with one
//! template per register: fiction (modes, dialogue, scene rhythm),
//! non-fiction and blogs (openings, structure, stance, closings), social
//! posts (length, layout, hooks, endings, emoji/hashtags, a never-list) and
//! email/messages (greetings, sign-offs, length).
//!
//! Every brief is built on the `analyze` report, stays under ~1,200 words
//! before the exemplars (long briefs get skimmed and over-applied), leads
//! with 5–7 numeric targets, lists word choices only where the author has a
//! word they never use, and lists recurring words only when they are habits
//! rather than subjects. Then the author's own pieces nearest the task (see
//! `engine::retrieve`), a passage to continue (continuation beats
//! description for holding a voice; `--for previous_chapter.md` continues
//! the agent's own last chapter, while `--for outline.md` only picks the
//! exemplars), and the write -> check -> revise loop.
//!
//! Blind readers caught every guided imitation in the round-1 A/B test by
//! things word lists never show: straight quotes in a writer who sets them
//! curly, flawless mechanics in a writer with "..", "e.g." and loose
//! endings, and tidy beats (three short sentences in a row, mirrored
//! pairs). So every brief also carries the author's typography, their
//! mechanics, and the polish their own text almost never has.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::commands::analyze::{self, KindReport, VoiceReport};
use crate::commands::examples::{self, Example};
use crate::commands::input;
use crate::engine::metrics::{self, Band, ItemHabits, TypographyHabits, Unit};
use crate::engine::profile::{ItemKind, Profile, Register};
use crate::engine::retrieve::{self, Request, TopicMatch};
use crate::engine::{lexicon, store, text, voice};
use crate::error::AppError;
use crate::output::{self, Ctx};

const BIN: &str = env!("CARGO_BIN_NAME");
/// Long-form exemplars are two base passages (~500 words).
const EXEMPLAR_WINDOW: usize = 500;
/// The continuation warm-up taken from the end of `--for FILE`.
const WARMUP_WORDS: usize = 300;
/// Numeric targets a brief leads with.
const MAX_TARGETS: usize = 7;

/// One section of a brief. Exemplar sections hold the author's own text
/// and are not counted against the brief's length budget.
struct Section {
    heading: String,
    body: String,
    exemplar: bool,
}

#[derive(Serialize)]
struct ExemplarRow {
    source: String,
    similarity: f64,
    /// Vocabulary overlap with the task (0 without one): read with
    /// `topic_match`, since `similarity` also counts style nearness.
    topic: f64,
    words: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    engagement_pct: Option<f64>,
}

#[derive(Serialize)]
struct Guide {
    profile: String,
    /// The template used: the profile's register unless `--register`.
    register: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    window_words: usize,
    /// What the exemplars were matched to: for (a file), about (words), none.
    task: &'static str,
    topic_match: TopicMatch,
    /// Words in the brief, exemplars and warm-up excluded.
    brief_words: usize,
    exemplar_words: usize,
    sections: Vec<String>,
    exemplars: Vec<ExemplarRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
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
    pub register: Option<Register>,
    /// Short-form: which kind of item the brief is for.
    pub as_kind: ItemKind,
    pub limit: usize,
    pub window: usize,
    pub out: Option<PathBuf>,
}

/// Everything a template needs.
struct Ctxt<'a> {
    p: &'a Profile,
    r: &'a VoiceReport,
    register: Register,
    kind: ItemKind,
    /// Short-form: the items of the brief's kind.
    items: Vec<usize>,
    query: Option<String>,
    /// The `--for` file's text (a previous chapter or a draft).
    for_text: Option<String>,
    exemplars: usize,
    /// What retrieval said about the exemplars' fit to the task (a weak or
    /// missing topic match): surfaced as warnings.
    notes: std::cell::RefCell<Vec<String>>,
}

pub fn run(ctx: Ctx, a: GuideArgs) -> Result<(), AppError> {
    let p = store::load(&a.profile)?;
    let r = analyze::report(&p, a.limit, a.window);
    let mut warnings = r.warnings.clone();
    // A template needs the matching unit of analysis: posts can't yield a
    // chapter brief, nor chapters a post brief. Across that line the
    // profile's own register wins, with a warning saying how to get the
    // other.
    let register = match a.register {
        Some(want) if want.is_short_form() != p.register.is_short_form() => {
            warnings.push(format!(
                "'{}' holds {} writing, so this is a {} brief; for a {} brief build a profile from that kind of writing: {BIN} profile build <name> --corpus <path> --register {}",
                p.name,
                p.register.as_str(),
                p.register.as_str(),
                want.as_str(),
                want.as_str()
            ));
            p.register
        }
        Some(want) => want,
        None => p.register,
    };
    let for_text = match &a.for_file {
        Some(f) => Some(input::read_text(Some(f.clone()), None, "guide")?),
        None => None,
    };
    let query = for_text.clone().or_else(|| a.about.clone().filter(|s| !s.trim().is_empty()));
    let task = if a.for_file.is_some() {
        "for"
    } else if query.is_some() {
        "about"
    } else {
        "none"
    };
    let (items, kind_note) = kind_items(&p, a.as_kind);
    warnings.extend(kind_note);
    let c = Ctxt {
        p: &p,
        r: &r,
        register,
        kind: a.as_kind,
        items,
        query,
        for_text,
        exemplars: a.exemplars,
        notes: Default::default(),
    };
    let (sections, picks, topic_match) = match register {
        Register::Social => social(&c),
        Register::Email | Register::Message => mail(&c),
        Register::Fiction => fiction(&c),
        Register::Nonfiction | Register::Blog => prose(&c),
    };
    warnings.extend(c.notes.take());
    let markdown = render(&sections);
    let brief_words: usize = sections.iter().filter(|s| !s.exemplar).map(|s| count(&s.heading) + count(&s.body)).sum();
    let exemplar_words: usize = sections.iter().filter(|s| s.exemplar).map(|s| count(&s.body)).sum();
    let path = match &a.out {
        Some(path) => {
            std::fs::write(path, &markdown)?;
            Some(path.display().to_string())
        }
        None => None,
    };
    let data = Guide {
        profile: p.name.clone(),
        register: register.as_str(),
        kind: p.register.is_short_form().then(|| retrieve::kind_label(a.as_kind)),
        window_words: a.window,
        task,
        topic_match,
        brief_words,
        exemplar_words,
        sections: sections.iter().map(|s| s.heading.clone()).collect(),
        exemplars: picks
            .iter()
            .map(|e| ExemplarRow {
                source: e.source.clone(),
                similarity: e.similarity,
                topic: e.topic,
                words: e.words,
                engagement_pct: e.engagement_pct,
            })
            .collect(),
        path,
        markdown,
        warnings,
    };
    let draft = if p.register.is_short_form() { "post.md" } else { "draft.md" };
    let mut next = vec![format!("{BIN} check {draft} --profile {}", p.name)];
    if task == "none" {
        next.push(format!("{BIN} guide {} --about \"<what you are writing>\" -o brief.md", p.name));
    }
    output::print_success_with(ctx, &data, &next, |d| match &d.path {
        Some(path) => println!(
            "Wrote the {} {} brief to {path} ({} words + {} of exemplars)",
            d.profile, d.register, d.brief_words, d.exemplar_words
        ),
        None => println!("{}", d.markdown),
    })
}

fn count(s: &str) -> usize {
    text::count_words(s)
}

/// The items of `kind` (posts include quote posts); all items, with a
/// warning, when the profile has none of that kind.
fn kind_items(p: &Profile, kind: ItemKind) -> (Vec<usize>, Option<String>) {
    if !p.register.is_short_form() {
        return (Vec::new(), None);
    }
    let of_kind: Vec<usize> = (0..p.chunks.len())
        .filter(|&i| {
            let k = p.chunks[i].meta.as_ref().map(|m| m.kind).unwrap_or_default();
            k == kind || (kind == ItemKind::Post && k == ItemKind::Quote)
        })
        .collect();
    if of_kind.is_empty() {
        let note = format!(
            "no {} items in '{}': the brief describes all its items",
            retrieve::kind_label(kind),
            p.name
        );
        (( 0..p.chunks.len()).collect(), Some(note))
    } else {
        (of_kind, None)
    }
}

fn render(sections: &[Section]) -> String {
    let mut md = String::new();
    for (i, s) in sections.iter().enumerate() {
        if i == 0 {
            let _ = writeln!(md, "# {}\n", s.heading);
        } else {
            let _ = writeln!(md, "## {}\n", s.heading);
        }
        let _ = writeln!(md, "{}\n", s.body.trim_end());
    }
    md
}

fn sec(heading: impl Into<String>, body: String) -> Section {
    Section {
        heading: heading.into(),
        body,
        exemplar: false,
    }
}

fn fmt_num(v: f64) -> String {
    if v.abs() >= 10.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Counts read better whole ("2 lines", not "2.0 lines").
fn fmt_count(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { fmt_num(v) }
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

fn band<'a>(r: &'a VoiceReport, key: &str) -> Option<&'a Band> {
    r.ranges.bands.iter().find(|b| b.key == key)
}

fn quote(text: &str) -> String {
    text.split("\n\n")
        .map(|para| para.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n"))
        .collect::<Vec<_>>()
        .join("\n>\n")
}

fn bullets(lines: &[String]) -> String {
    lines.iter().map(|l| format!("- {l}\n")).collect()
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// p15 / median / p85 of a list of values.
fn spread3(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.total_cmp(b));
    (quantile(&v, 0.15), quantile(&v, 0.5), quantile(&v, 0.85))
}

// ── shared sections ────────────────────────────────────────────────────────

fn header(c: &Ctxt, what: &str) -> Section {
    let p = c.p;
    let mut body = String::new();
    if !p.components.is_empty() {
        let mix: Vec<String> = p.components.iter().map(|(n, s)| format!("{n} {}", pct(*s))).collect();
        let _ = writeln!(body, "A blended voice: {}.\n", mix.join(" + "));
    }
    let copy_run = if p.register.is_short_form() { 6 } else { 8 };
    let measured = if p.register.is_short_form() {
        let dates: Vec<&str> = c
            .items
            .iter()
            .filter_map(|&i| p.chunks[i].meta.as_ref()?.date.as_deref())
            .filter_map(|d| d.get(0..10))
            .collect();
        let span = match (dates.iter().min(), dates.iter().max()) {
            (Some(a), Some(b)) => format!(", {a} to {b}"),
            _ => String::new(),
        };
        format!("Measured from {} {} by this author{span}.", c.items.len(), item_noun(c))
    } else {
        format!("Measured from {} words in {} work(s).", c.r.words, c.r.works)
    };
    let _ = writeln!(
        body,
        "{measured} Write so a reader of this author would not notice the switch. \
         Imitate the mechanisms below, never the lines: drafts are checked for copied {copy_run}-word runs."
    );
    sec(format!("Writing {what} as `{}`", p.name), body)
}

/// What the brief's items are called: emails and messages have no kinds.
fn item_noun(c: &Ctxt) -> &'static str {
    match c.register {
        Register::Email => "emails",
        Register::Message => "messages",
        _ => plural_kind(c.kind),
    }
}

fn plural_kind(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Post | ItemKind::Quote => "posts",
        ItemKind::Reply => "replies",
        ItemKind::ThreadPart => "thread parts",
    }
}

/// 5–7 numeric targets from the register's headline measures. A measure
/// at zero across the author's whole range says "never": it goes on one
/// line instead of taking a row a real target could use, and unmeasured
/// markup is no target at all. An email's final mark is its sign-off's,
/// so "ends with . ? ! or …" says nothing about how it is punctuated.
fn targets(c: &Ctxt, ranges: &metrics::Ranges, title: String) -> Option<Section> {
    let keys = metrics::headline_keys(c.register);
    let (zero, rows): (Vec<&Band>, Vec<&Band>) = keys
        .iter()
        .filter(|k| **k != "ends_with_punct_share" || ending_is_the_writers(c.p.register))
        .filter_map(|k| ranges.bands.iter().find(|b| b.key == *k))
        .filter(|b| !b.unmeasured)
        .partition(|b| b.low == 0.0 && b.high == 0.0);
    if rows.is_empty() && zero.is_empty() {
        return None;
    }
    let mut body = String::new();
    if !rows.is_empty() {
        body.push_str("| measure | range | typical |\n|---|---|---|\n");
    }
    for b in rows.into_iter().take(MAX_TARGETS) {
        let f = |v: f64| if matches!(b.unit, Unit::Share) { pct(v) } else { fmt_num(v) };
        let _ = writeln!(body, "| {} | {}–{} | {} |", b.label, f(b.low), f(b.high), f(b.median));
    }
    if !zero.is_empty() {
        let unit = if c.p.register.is_short_form() { item_noun(c) } else { "passages" };
        let labels: Vec<&str> = zero.iter().map(|b| b.label.as_str()).collect();
        let _ = writeln!(body, "\nZero in most of the author's {unit}: {}.", labels.join(", "));
    }
    let _ = writeln!(
        body,
        "\nAim for the typical value and stay inside the range. `{BIN} analyze {} --json` has every measure; `check` scores them all at the draft's own length.",
        c.p.name
    );
    Some(sec(title, body))
}

/// "when you mean X, write A (not B, C)": only concepts with a word the
/// author never uses that an agent would plausibly reach for, and never a
/// row whose preferred word is itself a machine tell (it would order the
/// tell up).
fn word_choices(r: &VoiceReport) -> Option<Section> {
    let rows: Vec<String> = r
        .concepts
        .iter()
        .filter(|c| c.preferred_share >= 0.5 && !is_tell(&c.preferred))
        .filter_map(|c| {
            let never: Vec<&str> = c
                .never_used
                .iter()
                .map(String::as_str)
                .filter(|w| lexicon::zipf(w).is_some_and(|z| z >= 3.0))
                .collect();
            (!never.is_empty()).then(|| format!("**{}**, not {}", c.preferred, never.join(", ")))
        })
        .take(10)
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(sec(
        "Word choices: when you mean one of these, write the bold word",
        bullets(&rows),
    ))
}

fn is_tell(w: &str) -> bool {
    lexicon::llm_tells().contains(&w) || lexicon::slop_words().contains(&w)
}

/// Glue words, habit words and phrases, with subjects filtered out.
fn habits(c: &Ctxt) -> Option<Section> {
    let r = c.r;
    let topical = voice::topical_words(c.p, 50);
    let style: Vec<&str> = r.style_words.iter().take(12).map(|w| w.word.as_str()).collect();
    let recurring: Vec<&str> = r.recurring_words.iter().take(8).map(|w| w.word.as_str()).collect();
    let phrases: Vec<&str> = r
        .phrases
        .iter()
        .filter(|ph| !ph.phrase.split(' ').any(|w| topical.contains(w) || crate::engine::text::is_placeholder(w)))
        .take(8)
        .map(|ph| ph.phrase.as_str())
        .collect();
    let mut lines = Vec::new();
    if !style.is_empty() {
        lines.push(format!("Glue words leaned on: {}", style.join(", ")));
    }
    if !recurring.is_empty() {
        lines.push(format!("Recurring words (habits, not subjects): {}", recurring.join(", ")));
    }
    if !phrases.is_empty() {
        lines.push(format!("Phrases: {}", phrases.join("; ")));
    }
    if lines.is_empty() {
        return None;
    }
    Some(sec("Signature habits (use naturally, never as a checklist)", bullets(&lines)))
}

/// What gives an imitation away, all author-relative: the tidy moves this
/// author's own text almost never has (blind readers named them first),
/// then (posts) what they never do, the words they don't use and the stock
/// tells they never write.
fn giveaways(c: &Ctxt, extra: &[String]) -> Option<Section> {
    let r = c.r;
    let mut body = String::new();
    let polish = polish_rows(&r.ranges.bands);
    if !polish.is_empty() {
        let _ = writeln!(
            body,
            "Imitations read too tidy. These are rare in this author's own text (per 1,000 words, 85th percentile); write them no more often:\n"
        );
        body.push_str(&bullets(&polish));
        body.push('\n');
    }
    let mut lines: Vec<String> = extra.to_vec();
    // Short-form: only words with the author's own replacement; "his" or
    // "she" missing from posts says nothing about voice.
    let short = c.p.register.is_short_form();
    let avoided: Vec<String> = r
        .avoided_words
        .iter()
        .filter(|a| !short || a.instead.is_some())
        .take(12)
        .map(|a| match &a.instead {
            Some(i) => format!("{} (→ {i})", a.word),
            None => a.word.clone(),
        })
        .collect();
    if !avoided.is_empty() {
        lines.push(format!("Words this author doesn't use: {}", avoided.join(", ")));
    }
    if !r.tells_unused.is_empty() {
        // `check` catches every tell; the brief names the ones agents reach for most.
        let tells = distinct_tells(&r.tells_unused, if short { 8 } else { 10 });
        lines.push(format!("Machine-prose tells this author never writes: {}", tells.join(", ")));
    }
    body.push_str(&bullets(&lines));
    if body.trim().is_empty() {
        return None;
    }
    Some(sec("What gives imitations away", body))
}

/// Tidy moves blind readers named in imitations, with a plain example each.
/// Each is listed only where the author's own text rarely has it: a writer
/// who really stacks short sentences is not giving anything away.
const POLISH: [(&str, &str); 6] = [
    ("short_triplets_per_1k", "three or more short sentences in a row (\"He left. She stayed. Nobody spoke.\")"),
    ("anaphora_runs_per_1k", "runs of sentences that open the same way (\"Same street. Same light. Same silence.\")"),
    ("mirror_pairs_per_1k", "mirrored pairs, one negated (\"I want to go. I don't want to go.\")"),
    ("not_but_per_1k", "\"not X, but Y\" contrasts (\"It's not the money. It's the silence.\")"),
    ("narration_questions_per_1k", "rhetorical questions (\"Why am I doing this?\")"),
    ("similes_per_1k", "similes (\"like a …\", \"as if …\")"),
];

/// "Rare": the author's 85th-percentile passage has at most one per 1,000 words.
const RARE_PER_1K: f64 = 1.0;

fn polish_rows(bands: &[Band]) -> Vec<String> {
    POLISH
        .iter()
        .filter_map(|(key, what)| {
            let b = bands.iter().find(|b| b.key == *key && !b.unmeasured && b.high <= RARE_PER_1K)?;
            Some(format!("{what}: {}", if b.high == 0.0 { "none".to_string() } else { fmt_num(b.high) }))
        })
        .collect()
}

// ── typography and mechanics ───────────────────────────────────────────────

/// A typography family, as `metrics::TypographyHabits` reports it.
#[derive(Clone, Copy, PartialEq)]
enum Family {
    Quotes,
    Apostrophes,
    Ellipses,
    Dashes,
    Breaks,
}

/// The author's main form in full: the glyphs to copy and how they sit.
/// A form this table doesn't know is shown as it is.
fn form_long(f: Family, form: &str) -> String {
    let s = match (f, form) {
        (Family::Quotes, "curly") => "curly \u{201C} \u{201D}",
        (Family::Quotes, "straight") => "straight \"",
        (Family::Apostrophes, "curly") => "curly \u{2019} (don\u{2019}t)",
        (Family::Apostrophes, "straight") => "straight ' (don't)",
        (Family::Ellipses, "\u{2026}") => "\u{2026} (one character, no spaces)",
        (Family::Ellipses, "...") => "... (three dots)",
        (Family::Ellipses, ". . .") => ". . . (spaced dots)",
        (Family::Ellipses, "..") => ".. (two dots, as a joiner)",
        (Family::Dashes, "\u{2014}") => "closed em dash (word\u{2014}word)",
        (Family::Dashes, " \u{2014} ") => "spaced em dash (word \u{2014} word)",
        (Family::Dashes, "--") => "double hyphen (--)",
        (Family::Dashes, " - ") => "spaced hyphen (word - word)",
        (Family::Dashes, "\u{2013}") => "en dash (\u{2013})",
        (Family::Breaks, b) => return format!("`{b}` on a line of its own"),
        (_, other) => return format!("\"{other}\""),
    };
    s.to_string()
}

/// Another form, in short.
fn form_short(f: Family, form: &str) -> String {
    let s = match (f, form) {
        (Family::Quotes, "curly") => "curly \u{201C} \u{201D}",
        (Family::Quotes, "straight") => "straight \"",
        (Family::Apostrophes, "curly") => "curly \u{2019}",
        (Family::Apostrophes, "straight") => "straight '",
        (Family::Dashes, "\u{2014}") => "word\u{2014}word",
        (Family::Dashes, " \u{2014} ") => "word \u{2014} word",
        (Family::Dashes, " - ") => "word - word",
        (Family::Breaks, b) => return format!("`{b}`"),
        (_, other) => other,
    };
    s.to_string()
}

/// A form under this share is one the author doesn't use (a few strays in
/// a long corpus are noise, not a habit).
const NEVER_FORM_SHARE: f64 = 0.05;
/// Other forms worth naming beside the main one.
const ALSO_FORM_SHARE: f64 = 0.1;

fn share_of(forms: &[(String, f64)], form: &str) -> f64 {
    forms.iter().find(|f| f.0 == form).map_or(0.0, |f| f.1)
}

/// One line per family with enough evidence, then the forms agents default
/// to (straight quotes, "...", "---") where this author never uses them.
/// Posts have no scene breaks (a `---` line separates posts in a batch).
fn typography_lines(t: &TypographyHabits, short_form: bool) -> Vec<String> {
    let families = [
        (Family::Quotes, "Quotes", &t.double_quotes),
        (Family::Apostrophes, "Apostrophes", &t.apostrophes),
        (Family::Ellipses, "Ellipses", &t.ellipses),
        (Family::Dashes, "Dashes", &t.dashes),
        (Family::Breaks, "Scene breaks", &t.scene_breaks),
    ];
    let mut out = Vec::new();
    for (f, name, forms) in families {
        if f == Family::Breaks && short_form {
            continue;
        }
        let Some((top, share)) = forms.first() else { continue };
        let mut line = format!("{name}: {} ({})", form_long(f, top), pct(*share));
        let also: Vec<String> = forms[1..]
            .iter()
            .filter(|x| x.1 >= ALSO_FORM_SHARE)
            .take(2)
            .map(|(x, s)| format!("{} {}", form_short(f, x), pct(*s)))
            .collect();
        if !also.is_empty() {
            let _ = write!(line, "; also {}", also.join(", "));
        }
        line.push('.');
        out.push(line);
    }
    let never = |forms: &[(String, f64)], form: &str| !forms.is_empty() && share_of(forms, form) < NEVER_FORM_SHARE;
    let held = |forms: &[(String, f64)]| forms.first().is_some_and(|f| f.1 >= HABIT_SHARE);
    let mut defaults = Vec::new();
    let mut all_held = true;
    let mut flag = |forms: &[(String, f64)], form: &str, shown: &'static str| {
        if never(forms, form) {
            defaults.push(shown);
            all_held &= held(forms);
        }
    };
    flag(&t.double_quotes, "straight", "straight \" quotes");
    flag(&t.apostrophes, "straight", "straight ' apostrophes");
    flag(&t.ellipses, "...", "\"...\"");
    flag(&t.dashes, "\u{2014}", "closed em dashes (\u{2014})");
    if !short_form {
        flag(&t.scene_breaks, "---", "`---` breaks");
    }
    if !defaults.is_empty() {
        out.push(format!(
            "Agents default to forms this author never uses: {}. Convert every one{}.",
            defaults.join(", "),
            if all_held { "; `check` flags them" } else { "" }
        ));
    }
    out
}

/// The share at which a form is a habit `check` holds a draft to (its
/// typography findings use the same threshold).
const HABIT_SHARE: f64 = 0.8;

fn typography_section(c: &Ctxt) -> Option<Section> {
    let lines = typography_lines(&c.r.typography, c.p.register.is_short_form());
    (!lines.is_empty()).then(|| sec("Typography (set it exactly like this)", bullets(&lines)))
}

/// The exemplars' one-line reminder that their marks are the voice too,
/// naming the forms the author holds 80% of the time or more.
pub fn typography_note(t: &TypographyHabits) -> String {
    let families = [
        (Family::Quotes, &t.double_quotes),
        (Family::Apostrophes, &t.apostrophes),
        (Family::Ellipses, &t.ellipses),
        (Family::Dashes, &t.dashes),
    ];
    let forms: Vec<String> = families
        .iter()
        .filter_map(|(f, v)| v.first().filter(|x| x.1 >= HABIT_SHARE).map(|x| form_short(*f, &x.0)))
        .collect();
    let what = if forms.is_empty() { String::new() } else { format!(" ({})", forms.join(", ")) };
    format!("Their punctuation and quote marks are part of the voice{what}: set yours the same way.")
}

/// The marks of a real hand, counted in the author's text: doubled marks,
/// abbreviations kept in running prose and (posts) loose starts and endings.
#[derive(Default)]
struct Hand {
    /// "..", "!!", "??", "?!" the author writes, per 1,000 words.
    doubled: Vec<(&'static str, f64)>,
    /// "etc", "e.g.", "i.e." in running text, per 1,000 words.
    abbrevs: Vec<(&'static str, f64)>,
    /// Short-form: lowercase starts, no final full stop.
    loose: Vec<String>,
}

/// Whether an item's last mark is the writer's: an email ends on its
/// sign-off ("Best,\nBoris"), so "no final full stop" there is the name,
/// not a loose hand.
fn ending_is_the_writers(register: Register) -> bool {
    register != Register::Email
}

const DOUBLED: [&str; 4] = ["..", "!!", "??", "?!"];

/// Doubled marks in a text, in `DOUBLED` order. A run of three or more dots
/// is an ellipsis, not a doubled mark; "?!" covers "!?" and longer mixes.
fn doubled_marks(t: &str) -> [usize; 4] {
    let mut out = [0; 4];
    let cs: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if !matches!(cs[i], '.' | '!' | '?') {
            i += 1;
            continue;
        }
        let run: Vec<char> = cs[i..].iter().copied().take_while(|ch| matches!(ch, '.' | '!' | '?')).collect();
        let has = |ch: char| run.contains(&ch);
        if run.len() == 2 && run.iter().all(|&ch| ch == '.') {
            out[0] += 1;
        } else if run.len() >= 2 && !has('.') {
            match (has('!'), has('?')) {
                (true, true) => out[3] += 1,
                (true, false) => out[1] += 1,
                _ => out[2] += 1,
            }
        }
        i += run.len();
    }
    out
}

fn hand_of(c: &Ctxt, habits: Option<&ItemHabits>) -> Hand {
    let p = c.p;
    let words = p.n_tokens.max(1) as f64;
    let mut counts = [0usize; 4];
    for para in p.chunks.iter().flat_map(|ch| ch.paragraphs.iter()) {
        for (n, m) in counts.iter_mut().zip(doubled_marks(para)) {
            *n += m;
        }
    }
    // Three uses and one per 10,000 words make a habit, not a stray.
    let doubled = DOUBLED
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n >= 3 && *n as f64 * 1000.0 / words >= 0.1)
        .map(|(f, n)| (*f, n as f64 * 1000.0 / words))
        .collect();
    let abbrevs = [("etc", "etc"), ("e.g", "e.g."), ("i.e", "i.e.")]
        .iter()
        .filter_map(|(key, shown)| {
            let n = p.word_counts.get(*key).copied().unwrap_or(0);
            let rate = n as f64 * 1000.0 / words;
            (n >= 2 && rate >= 0.2).then_some((*shown, rate))
        })
        .collect();
    let mut loose = Vec::new();
    if p.register.is_short_form() && !c.items.is_empty() {
        let lower = c
            .items
            .iter()
            .filter(|&&i| p.chunks[i].text().chars().find(|ch| ch.is_alphabetic()).is_some_and(|ch| ch.is_lowercase()))
            .count() as f64
            / c.items.len() as f64;
        if lower >= 0.08 {
            loose.push(format!("lowercase starts ({})", pct(lower)));
        }
        let bare = habits.filter(|_| ending_is_the_writers(p.register)).map_or(0.0, |h| share_of(&h.endings, "none"));
        if bare >= 0.08 {
            loose.push(format!("no final full stop ({} of {})", pct(bare), item_noun(c)));
        }
    }
    Hand { doubled, abbrevs, loose }
}

/// Mechanics from the author's bands (`double_punct_per_1k`,
/// `space_before_punct_per_1k`, `nonword_per_1k`) and the forms behind
/// them. Words outside the dictionary are stated, never prescribed: the
/// brief must not tell an agent to misspell.
fn mechanics_lines(bands: &[Band], h: &Hand) -> Vec<String> {
    let band = |key: &str| bands.iter().find(|b| b.key == key && !b.unmeasured);
    let Some(doubled) = band("double_punct_per_1k") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let quoted = |v: &[(&'static str, f64)]| v.iter().map(|(f, _)| format!("\"{f}\"")).collect::<Vec<_>>();
    if h.doubled.is_empty() {
        if doubled.high == 0.0 {
            out.push("Doubled marks (!!, ?!, ..): never.".to_string());
        }
    } else {
        out.push(format!(
            "Doubled marks: {} (typically {} per 1,000 words, up to {}). They are the author's: use them where the author would.",
            quoted(&h.doubled).join(", "),
            fmt_num(doubled.median),
            fmt_num(doubled.high)
        ));
    }
    let spaced = band("space_before_punct_per_1k").filter(|b| b.median >= 0.5);
    if let Some(b) = spaced {
        out.push(format!(
            "A space before punctuation (\"word ,\", \"word ?\"): typically {} per 1,000 words.",
            fmt_num(b.median)
        ));
    }
    let nonword = band("nonword_per_1k")
        .map(|b| {
            format!(
                " About {} words per 1,000 are outside the dictionary (names, coinages, jargon, slips); never invent misspellings.",
                fmt_num(b.median)
            )
        })
        .unwrap_or_default();
    let informal = !h.doubled.is_empty() || spaced.is_some() || !h.loose.is_empty();
    if informal {
        let mut kept = quoted(&h.doubled);
        kept.extend(quoted(&h.abbrevs));
        if spaced.is_some() {
            kept.push("the space before a mark".into());
        }
        kept.extend(h.loose.iter().cloned());
        out.push(format!("Polish: informal. Keeps {}; don't tidy these away.{nonword}", kept.join(", ")));
    } else {
        let uses = if h.abbrevs.is_empty() { String::new() } else { format!(" (uses {})", quoted(&h.abbrevs).join(", ")) };
        out.push(format!("Polish: edited copy, standard spelling and punctuation{uses}.{nonword}"));
    }
    out
}

fn mechanics_section(c: &Ctxt, habits: Option<&ItemHabits>) -> Option<Section> {
    let lines = mechanics_lines(&c.r.ranges.bands, &hand_of(c, habits));
    (!lines.is_empty()).then(|| sec("Mechanics", bullets(&lines)))
}

/// Typography and mechanics, the two mechanical, certain sections.
fn type_and_hand(c: &Ctxt, habits: Option<&ItemHabits>) -> Vec<Section> {
    typography_section(c).into_iter().chain(mechanics_section(c, habits)).collect()
}

/// Tells without their inflected repeats ("delve" covers "delves",
/// "delving"; "testament" covers "a testament to").
fn distinct_tells(tells: &[&'static str], n: usize) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for t in tells {
        let covered = out.iter().any(|o| {
            let n = o.chars().count();
            let stem: String = o.chars().take(4.max(n.saturating_sub(2))).collect();
            (!o.contains(' ') && !t.contains(' ') && n >= 5 && t.starts_with(&stem))
                || t.split(' ').any(|w| w == *o)
        });
        if !covered {
            out.push(t);
        }
        if out.len() >= n {
            break;
        }
    }
    out
}

/// The exemplars, led by one line saying their marks are part of the voice
/// (the author's own text shows their typography better than any rule).
fn exemplar_section(c: &Ctxt, heading: String, rows: &[Example], label: impl Fn(&Example) -> String) -> Option<Section> {
    if rows.is_empty() {
        return None;
    }
    let mut body = format!("{}\n\n", typography_note(&c.r.typography));
    for (i, e) in rows.iter().enumerate() {
        let _ = writeln!(body, "### {} {} ({})\n", heading_unit(&heading), i + 1, label(e));
        let _ = writeln!(body, "{}\n", quote(&e.text));
    }
    Some(Section {
        heading,
        body,
        exemplar: true,
    })
}

fn heading_unit(heading: &str) -> &'static str {
    if heading.contains("post") || heading.contains("repl") || heading.contains("thread") {
        "Post"
    } else if heading.contains("message") || heading.contains("email") {
        "Message"
    } else {
        "Passage"
    }
}

/// The last ~300 words of a text, whole paragraphs.
fn tail_words(t: &str, words: usize) -> String {
    let paras = text::paragraphs(t);
    let mut out: Vec<&String> = Vec::new();
    let mut n = 0;
    for para in paras.iter().rev() {
        out.push(para);
        n += count(para);
        if n >= words {
            break;
        }
    }
    out.reverse();
    out.into_iter().cloned().collect::<Vec<_>>().join("\n\n")
}

/// Words of running prose in a text, and all its words, line by line
/// (paragraphs join an outline's bullets into one): list items and
/// headings are notes, not prose.
fn prose_words(t: &str) -> (usize, usize) {
    let (mut prose, mut all) = (0, 0);
    for line in t.lines() {
        let n = count(line);
        all += n;
        if !is_heading(line) && !is_list(line) {
            prose += n;
        }
    }
    (prose, all)
}

/// The `--for` file when it is text to continue (a previous chapter or a
/// draft: 150+ words of prose, 70% or more of its words) rather than an
/// outline or notes, which have no voice to carry on from.
fn continued_text<'a>(c: &'a Ctxt) -> Option<&'a str> {
    let t = c.for_text.as_deref()?;
    let (prose, all) = prose_words(t);
    (prose >= 150 && prose * 10 >= all * 7).then_some(t)
}

/// Continuation warm-up: the end of the agent's own previous chapter when
/// `--for` gives one (continuity matters more than the source's text, and it
/// carries no copy risk), else one of the author's typical passages (also
/// when `--for` is an outline).
fn warmup(c: &Ctxt, spare: Option<&Example>, what: &str) -> Option<Section> {
    if let Some(t) = continued_text(c) {
        let tail = tail_words(t, WARMUP_WORDS);
        if count(&tail) >= 50 {
            return Some(Section {
                heading: format!("Warm-up: re-read the end of your previous text, then continue it in this voice"),
                body: quote(&tail),
                exemplar: true,
            });
        }
    }
    let e = spare?;
    Some(Section {
        heading: format!("Warm-up: continue this passage for ~300 words in the same voice, then write your {what}"),
        body: quote(&e.text),
        exemplar: true,
    })
}

fn loop_long(c: &Ctxt, what: &str) -> Section {
    let name = &c.p.name;
    let body = format!(
        "1. Draft the {what} from this brief.\n\
         2. `{BIN} check draft.md --profile {name}`: fix hard failures (copied runs, banned words) first, then the revision plan, then word-level findings.\n\
         3. Re-run `check` until it passes; judge passages of 1,000+ words (shorter is directional).\n\
         4. Need examples closer to a scene or topic? `{BIN} examples {name} --about \"...\"`.\n"
    );
    sec("Loop", body)
}

/// Retrieve and shape exemplars, keeping retrieval's notes on the task fit
/// (blog openings are a side sample, so theirs are not kept).
fn pick(c: &Ctxt, n: usize, mix_dialogue: bool, openings_only: bool) -> (Vec<Example>, TopicMatch) {
    let mut req = Request::new(c.query.as_deref(), n, c.kind);
    req.window_words = EXEMPLAR_WINDOW;
    req.mix_dialogue = mix_dialogue;
    req.openings_only = openings_only;
    let r = retrieve::retrieve(c.p, &req);
    if !openings_only {
        c.notes.borrow_mut().extend(r.notes.iter().cloned());
    }
    (examples::rows(c.p, &r), r.topic_match)
}

// ── fiction ────────────────────────────────────────────────────────────────

fn fiction(c: &Ctxt) -> (Vec<Section>, Vec<Example>, TopicMatch) {
    let r = c.r;
    let mut out = vec![header(c, "fiction")];
    out.push(sec("The voice in brief", bullets(&long_summary(r, true))));
    out.extend(type_and_hand(c, None));
    if let Some(s) = modes_section(c) {
        out.push(s);
    }
    if let Some(s) = dialogue_section(c) {
        out.push(s);
    }
    out.extend(targets(c, &r.ranges, format!("Scene rhythm targets ({}-word passages)", r.ranges.window_words)));
    out.extend(word_choices(r));
    out.extend(habits(c));
    out.extend(giveaways(c, &[]));
    let (picks, tm) = pick(c, c.exemplars, true, false);
    let spare = spare_passage(c, &picks);
    let shown = &picks[..];
    out.extend(exemplar_section(
        c,
        "The author's own passages (study the mechanics; do not reuse)".into(),
        shown,
        |e| {
            let kind = if e.dialogue_share.unwrap_or(0.0) >= 0.15 { "dialogue" } else { "narration" };
            let mode = e.mode.clone().map(|m| format!(", {m}")).unwrap_or_default();
            format!("{}, {} words, {kind}{mode}", e.source, e.words)
        },
    ));
    out.extend(warmup(c, spare.as_ref(), "scene"));
    out.push(loop_long(c, "scene"));
    (out, picks, tm)
}

/// One more typical passage, none of the exemplars, for the warm-up
/// (none needed when `--for` is text to continue).
fn spare_passage(c: &Ctxt, shown: &[Example]) -> Option<Example> {
    if continued_text(c).is_some() {
        return None;
    }
    let mut req = Request::new(c.query.as_deref(), 1, c.kind);
    req.window_words = EXEMPLAR_WINDOW;
    let texts: std::collections::HashSet<&str> = shown.iter().map(|e| e.text.as_str()).collect();
    let r = retrieve::retrieve(c.p, &req);
    let mut rows = examples::rows(c.p, &r);
    if rows.first().is_some_and(|e| texts.contains(e.text.as_str())) {
        req.n = shown.len() + 1;
        let r = retrieve::retrieve(c.p, &req);
        rows = examples::rows(c.p, &r).into_iter().filter(|e| !texts.contains(e.text.as_str())).collect();
    }
    rows.into_iter().next()
}

/// The voice in a few plain sentences, from the measured medians.
fn long_summary(r: &VoiceReport, fiction: bool) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(med), Some(short)) = (band(r, "sentence_words_median"), band(r, "short_sentence_share")) {
        out.push(format!(
            "Typical sentence about {} words; {} of sentences are five words or fewer.",
            fmt_num(med.median),
            pct(short.median)
        ));
    }
    if let (Some(pw), Some(one)) = (band(r, "paragraph_words_mean"), band(r, "one_sentence_paragraph_share")) {
        out.push(format!(
            "Paragraphs average {} words; {} are a single sentence.",
            fmt_num(pw.median),
            pct(one.median)
        ));
    }
    if let Some(d) = band(r, "dialogue_word_share") {
        if fiction || d.median >= 0.05 {
            let how = if d.median >= 0.35 {
                "Dialogue carries much of the text"
            } else if d.median >= 0.15 {
                "Dialogue and narration are balanced"
            } else {
                "Mostly narration; dialogue is sparse"
            };
            out.push(format!("{how} (about {} of words are spoken).", pct(d.median)));
        }
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
    out
}

fn modes_section(c: &Ctxt) -> Option<Section> {
    let m = c.r.modes.as_ref()?;
    (!m.modes.is_empty()).then(|| sec("Narration mode", modes_text(m)))
}

/// The narration modes as instructions. "Pick one per chapter" needs two
/// real modes to pick from; chapters that mix tenses are no mode to
/// imitate, so their share is said outright rather than dropped.
fn modes_text(m: &voice::Modes) -> String {
    let label = |x: &voice::Mode| format!("{} person, {} tense", x.pov, x.tense);
    let pure: Vec<&voice::Mode> = m.modes.iter().filter(|x| x.tense != "mixed").collect();
    let mixed: f64 = m.modes.iter().filter(|x| x.tense == "mixed").map(|x| x.share).sum();
    let over = format!("measured over {} {}", m.sections, m.basis);
    let scene_rule = "change tense only at a scene break, never inside a scene.";
    let mut body = match pure.as_slice() {
        [top, ..] if top.share >= 0.85 => format!("Narration: {} throughout ({} of the text).", label(top), pct(top.share)),
        [only] if only.share > mixed => format!("Narration: {} in most chapters ({} of the text, {over}).", label(only), pct(only.share)),
        [] | [_] => {
            let mut povs: Vec<&str> = Vec::new();
            for x in &m.modes {
                if !povs.contains(&x.pov) {
                    povs.push(x.pov);
                }
            }
            let rest = pure.first().map(|x| format!("; the rest is {} ({})", label(x), pct(x.share))).unwrap_or_default();
            return format!(
                "Narration: {} person; most chapters mix tenses ({} of the text, {over}), so no one tense describes them{rest}. Keep each scene in one tense: {scene_rule}\n",
                povs.join(" or "),
                pct(mixed)
            );
        }
        list => {
            let list: Vec<String> = list.iter().map(|x| format!("{} ({} of the text)", label(x), pct(x.share))).collect();
            format!(
                "Each chapter holds one mode: {} ({over}). Pick one per chapter and keep it; the averages across modes describe no real chapter.",
                list.join("; ")
            )
        }
    };
    if mixed >= 0.1 {
        let _ = write!(body, " {} of the text is in chapters that mix tenses: {scene_rule}", pct(mixed));
    } else if pure.len() >= 2 && pure[0].share < 0.85 {
        body.push_str(" Never drift between tenses inside a scene.");
    }
    body.push('\n');
    body
}

const PRESENT_TAGS: [&str; 6] = ["says", "asks", "tells", "whispers", "replies", "mutters"];
const PAST_TAGS: [&str; 6] = ["said", "asked", "told", "whispered", "replied", "muttered"];

fn dialogue_section(c: &Ctxt) -> Option<Section> {
    let r = c.r;
    let d = band(r, "dialogue_word_share")?;
    if d.median < 0.03 || r.dialogue_tags.verbs.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let tags: Vec<String> = r
        .dialogue_tags
        .verbs
        .iter()
        .take(5)
        .map(|v| format!("*{}* {}", v.verb, pct(v.share)))
        .collect();
    lines.push(format!("Tags: {}. Most lines need no tag beyond these.", tags.join(", ")));
    let has = |set: &[&str]| r.dialogue_tags.verbs.iter().any(|v| set.contains(&v.verb.as_str()) && v.share >= 0.05);
    if has(&PRESENT_TAGS) && has(&PAST_TAGS) {
        lines.push("Match the tag's tense to the chapter: says/asks in present-tense chapters, said/asked in past-tense ones.".into());
    }
    if let Some(dp) = band(r, "dialogue_paragraph_share") {
        lines.push(format!("{} of paragraphs carry speech.", pct(dp.median)));
    }
    Some(sec("Dialogue", bullets(&lines)))
}

// ── non-fiction and blogs ──────────────────────────────────────────────────

/// Each work's paragraphs in order (headings kept when the corpus kept them).
/// Chapters stand in for works when a single file holds a collection
/// (essays, stories) split at chapter headings.
fn works(p: &Profile) -> Vec<Vec<&String>> {
    let by_section = p.chunks.iter().any(|c| c.section > 0);
    let mut out: Vec<Vec<&String>> = Vec::new();
    let mut key = (u32::MAX, u32::MAX);
    for ch in &p.chunks {
        let k = (ch.source, if by_section { ch.section } else { 0 });
        if k != key {
            key = k;
            out.push(Vec::new());
        }
        out.last_mut().unwrap().extend(ch.paragraphs.iter());
    }
    out.retain(|w| !w.is_empty());
    out
}

fn is_heading(para: &str) -> bool {
    para.trim_start().starts_with('#')
}

fn is_list(para: &str) -> bool {
    let t = para.trim_start();
    t.starts_with("- ") || t.starts_with("* ") || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.len() <= 2 && n.chars().all(|ch| ch.is_ascii_digit()))
}

/// A piece's prose paragraphs: no headings, list blocks or title lines.
fn prose_paras<'a>(w: &[&'a String]) -> Vec<&'a String> {
    w.iter()
        .copied()
        .filter(|para| !is_heading(para) && !is_list(para) && count(para) >= 8)
        .collect()
}

fn first_sentences(para: &str, n: usize) -> String {
    text::sentences(para).into_iter().take(n).collect::<Vec<_>>().join(" ")
}

fn openings_closings(c: &Ctxt) -> Vec<Section> {
    let ws = works(c.p);
    let mut out = Vec::new();
    // Openings need several real pieces; a book split into a few
    // sections has none worth averaging.
    if ws.len() < 5 {
        return out;
    }
    let firsts: Vec<&String> = ws.iter().filter_map(|w| prose_paras(w).first().copied()).collect();
    let lasts: Vec<&String> = ws.iter().filter_map(|w| prose_paras(w).last().copied()).collect();
    let (lo, med, hi) = spread3(firsts.iter().map(|p| count(p) as f64).collect());
    let q_open = firsts.iter().filter(|p| first_sentences(p, 1).trim_end().ends_with('?')).count();
    let mut lines = vec![
        format!(
            "First paragraph: {}–{} words (typically {}).",
            fmt_num(lo),
            fmt_num(hi),
            fmt_num(med)
        ),
        format!("{} of {} pieces open with a question.", q_open, firsts.len()),
    ];
    let mut body = bullets(&lines);
    if c.register == Register::Blog {
        let (opens, _) = pick(c, 5, false, true);
        if !opens.is_empty() {
            body.push_str("\nHow real posts begin (first two sentences, verbatim; imitate the move, not the words):\n\n");
            for e in &opens {
                let first = e
                    .text
                    .split("\n\n")
                    .find(|p| !is_heading(p) && !is_list(p) && !p.starts_with('('))
                    .unwrap_or("");
                let _ = writeln!(body, "- {} — \"{}\"", e.source, first_sentences(first, 2));
            }
        }
    }
    out.push(sec("Openings", body));
    let (lo, med, hi) = spread3(lasts.iter().map(|p| count(p) as f64).collect());
    let q_close = lasts.iter().filter(|p| p.trim_end().ends_with('?')).count();
    lines = vec![
        format!("Last paragraph: {}–{} words (typically {}).", fmt_num(lo), fmt_num(hi), fmt_num(med)),
        format!("{} of {} pieces end on a question.", q_close, lasts.len()),
    ];
    out.push(sec("Closings", bullets(&lines)));
    out
}

/// Heading outlines and list use, when the corpus kept its markdown.
fn structure(c: &Ctxt) -> Option<Section> {
    let ws = works(c.p);
    let paras: usize = ws.iter().map(|w| w.len()).sum();
    let lists: usize = ws.iter().map(|w| w.iter().filter(|p| is_list(p)).count()).sum();
    let mut outlines: Vec<(usize, String)> = Vec::new();
    for w in &ws {
        let mut parts: Vec<(String, usize)> = Vec::new();
        for para in w {
            if is_heading(para) {
                parts.push((para.trim_start_matches('#').trim().to_string(), 0));
            } else if let Some(last) = parts.last_mut() {
                last.1 += count(para);
            }
        }
        if parts.len() >= 2 {
            let line = parts
                .iter()
                .map(|(h, n)| format!("{h} ({n} words)"))
                .collect::<Vec<_>>()
                .join(" → ");
            outlines.push((parts.len(), line));
        }
    }
    let mut lines = Vec::new();
    if !outlines.is_empty() {
        let mut ns: Vec<usize> = outlines.iter().map(|o| o.0).collect();
        ns.sort_unstable();
        let median = ns[ns.len() / 2];
        lines.push(format!(
            "{} of {} pieces use subheadings (typically {}).",
            outlines.len(),
            ws.len(),
            median
        ));
        outlines.sort_by_key(|o| o.0.abs_diff(median));
        for (_, o) in outlines.iter().take(3) {
            lines.push(format!("Outline: {o}"));
        }
    }
    if lists > 0 && paras > 0 {
        lines.push(format!("{} of paragraphs are list blocks.", pct(lists as f64 / paras as f64)));
    }
    if lines.is_empty() {
        return None;
    }
    Some(sec("Structure", bullets(&lines)))
}

const TRANSITIONS: [&str; 14] = [
    "but", "so", "and", "now", "yet", "still", "also", "then", "because", "if", "or", "however", "thus", "therefore",
];
const HEDGES: [&str; 12] = [
    "perhaps", "maybe", "probably", "seems", "likely", "possibly", "arguably", "rather", "quite", "usually", "often",
    "generally",
];

fn per_1k(p: &Profile, words: &[&str]) -> f64 {
    let n: u64 = words.iter().map(|w| p.word_counts.get(*w).copied().unwrap_or(0)).sum();
    n as f64 * 1000.0 / p.n_tokens.max(1) as f64
}

fn stance(c: &Ctxt) -> Section {
    let p = c.p;
    let mut lines = vec![format!(
        "Pronouns per 1,000 words: I/me/my {}, you/your {}, we/our {}.",
        fmt_num(per_1k(p, &["i", "me", "my", "mine", "myself"])),
        fmt_num(per_1k(p, &["you", "your", "yours", "yourself"])),
        fmt_num(per_1k(p, &["we", "us", "our", "ours", "ourselves"]))
    )];
    let total: f64 = p.openers.values().map(|v| *v as f64).sum::<f64>().max(1.0);
    let mut opens: Vec<(&str, f64)> = TRANSITIONS
        .iter()
        .map(|w| (*w, p.openers.get(*w).copied().unwrap_or(0) as f64 / total))
        .collect();
    opens.sort_by(|a, b| b.1.total_cmp(&a.1));
    let used: Vec<String> = opens.iter().filter(|o| o.1 >= 0.01).take(5).map(|(w, s)| format!("{w} {}", pct(*s))).collect();
    let rare: Vec<&str> = opens.iter().filter(|o| o.1 < 0.001).map(|o| o.0).collect();
    if !used.is_empty() {
        lines.push(format!("Sentences open with: {} of sentences.", used.join(", ")));
    }
    if !rare.is_empty() {
        lines.push(format!("Almost never opens a sentence with: {}.", rare.join(", ")));
    }
    let mut hedges: Vec<(&str, f64)> = HEDGES.iter().map(|w| (*w, per_1k(p, &[w]))).filter(|h| h.1 >= 0.2).collect();
    hedges.sort_by(|a, b| b.1.total_cmp(&a.1));
    if !hedges.is_empty() {
        let h: Vec<String> = hedges.iter().take(5).map(|(w, r)| format!("{w} {}", fmt_num(*r))).collect();
        lines.push(format!("Hedges per 1,000 words: {}.", h.join(", ")));
    }
    sec("Stance and transitions", bullets(&lines))
}

fn prose(c: &Ctxt) -> (Vec<Section>, Vec<Example>, TopicMatch) {
    let r = c.r;
    let what = if c.register == Register::Blog { "blog posts" } else { "non-fiction" };
    let mut out = vec![header(c, what)];
    out.push(sec("The voice in brief", bullets(&long_summary(r, false))));
    out.extend(type_and_hand(c, None));
    out.extend(openings_closings(c));
    out.extend(structure(c));
    out.push(stance(c));
    out.extend(targets(c, &r.ranges, format!("Targets ({}-word passages)", r.ranges.window_words)));
    out.extend(word_choices(r));
    out.extend(habits(c));
    out.extend(giveaways(c, &[]));
    let (picks, tm) = pick(c, c.exemplars, false, false);
    let spare = spare_passage(c, &picks);
    out.extend(exemplar_section(
        c,
        "The author's own passages (study the mechanics; do not reuse)".into(),
        &picks,
        |e| format!("{}, {} words{}", e.source, e.words, if e.opening { ", opening" } else { "" }),
    ));
    out.extend(warmup(c, spare.as_ref(), "piece"));
    out.push(loop_long(c, if c.register == Register::Blog { "post" } else { "piece" }));
    (out, picks, tm)
}

// ── short-form ─────────────────────────────────────────────────────────────

/// X counts a link as 23 characters and an emoji as 2.
fn x_chars(t: &str) -> usize {
    t.split_inclusive(char::is_whitespace)
        .map(|tok| {
            let core = tok.trim_end();
            if core.starts_with("http://") || core.starts_with("https://") {
                23 + (tok.len() - core.len())
            } else {
                tok.chars().map(|ch| if voice::is_emoji(ch) { 2 } else { 1 }).sum()
            }
        })
        .sum()
}

fn kind_report<'a>(c: &'a Ctxt) -> Option<&'a KindReport> {
    let label = retrieve::kind_label(c.kind);
    c.r.items.iter().find(|k| k.kind == label)
}

fn shares(v: &[(String, f64)], n: usize) -> String {
    v.iter().take(n).map(|(k, s)| format!("{k} {}", pct(*s))).collect::<Vec<_>>().join(", ")
}

/// Length and layout: per-item ranges when the metrics give them, else the
/// item lengths measured here.
fn length_section(c: &Ctxt, habits: Option<&ItemHabits>) -> Section {
    let p = c.p;
    let texts: Vec<String> = c.items.iter().map(|&i| p.chunks[i].text()).collect();
    let (wlo, wmed, whi) = spread3(texts.iter().map(|t| count(t) as f64).collect());
    let (_, lmed, lhi) = spread3(texts.iter().map(|t| t.lines().filter(|l| !l.trim().is_empty()).count() as f64).collect());
    let mut lines = vec![format!(
        "Length: {}–{} words, typically {}; {} lines typically, up to {}.",
        fmt_count(wlo),
        fmt_count(whi),
        fmt_count(wmed),
        fmt_count(lmed),
        fmt_count(lhi)
    )];
    lines.extend(casing_and_numbers(&texts));
    if c.register == Register::Social {
        let over = texts.iter().filter(|t| x_chars(t) > 280).count();
        let share = over as f64 / texts.len().max(1) as f64;
        if share >= 0.05 {
            lines.push(format!("{} of their posts run past 280 characters (long posts are part of the voice).", pct(share)));
        } else {
            lines.push("Stay under 280 characters (a link counts 23, an emoji 2).".into());
        }
    }
    if let Some(h) = habits.filter(|h| !h.layouts.is_empty()) {
        lines.push(format!("Layout mix: {}. Vary it across a batch the same way.", shares(&h.layouts, 4)));
    }
    sec("Length and layout", bullets(&lines))
}

const NUMBER_WORDS: [&str; 10] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

/// Lowercase starts, lowercase "i", numerals vs spelled-out numbers: the
/// things that make a post read as theirs at a glance.
fn casing_and_numbers(texts: &[String]) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::new();
    let n = texts.len().max(1);
    let lower_start = texts
        .iter()
        .filter(|t| t.chars().find(|ch| ch.is_alphabetic()).is_some_and(|ch| ch.is_lowercase()))
        .count();
    out.push(format!("{lower_start} of {n} start in lowercase."));
    let (mut i_lower, mut i_any) = (0, 0);
    let (mut digits, mut spelled) = (0, 0);
    for t in texts {
        let words: Vec<&str> = t.unicode_words().collect();
        let is_i = |w: &str| w == "i" || w == "I" || w.starts_with("i'") || w.starts_with("I'");
        if words.iter().any(|w| is_i(w)) {
            i_any += 1;
            if words.iter().any(|w| *w == "i" || w.starts_with("i'")) {
                i_lower += 1;
            }
        }
        digits += words.iter().filter(|w| w.chars().all(|ch| ch.is_ascii_digit())).count();
        spelled += words.iter().filter(|w| NUMBER_WORDS.contains(&w.to_lowercase().as_str())).count();
    }
    if i_any > 0 {
        out.push(format!("Writes lowercase \"i\" in {i_lower} of the {i_any} items that use it."));
    }
    if digits + spelled >= 5 {
        out.push(format!(
            "Numbers: {} as numerals ({digits} numerals, {spelled} spelled out).",
            pct(digits as f64 / (digits + spelled) as f64)
        ));
    }
    out
}

fn social(c: &Ctxt) -> (Vec<Section>, Vec<Example>, TopicMatch) {
    let r = c.r;
    let kr = kind_report(c);
    let habits = kr.map(|k| &k.habits).filter(|h| h.items > 0);
    let mut out = vec![header(c, plural_kind(c.kind))];
    out.push(length_section(c, habits));
    out.extend(type_and_hand(c, habits));
    if let Some(k) = kr {
        out.extend(targets(c, &k.ranges, "Targets per post".into()));
    }
    let (picks, tm) = pick(c, c.exemplars.clamp(6, 10), false, false);
    out.extend(hooks_section(c, habits, &picks));
    if let Some(h) = habits {
        let mut lines = Vec::new();
        if !h.endings.is_empty() {
            lines.push(format!("Endings: {}.", shares(&h.endings, 6)));
        }
        if !h.top_emojis.is_empty() {
            lines.push(format!("Emoji they use: {}.", shares(&h.top_emojis, 6)));
        }
        if !h.top_hashtags.is_empty() {
            lines.push(format!("Hashtags they use: {}.", shares(&h.top_hashtags, 6)));
        }
        if !lines.is_empty() {
            out.push(sec("Endings, emoji, hashtags", bullets(&lines)));
        }
    }
    out.extend(short_words(c));
    out.extend(thread_section(c));
    // What they almost never do identifies them more than averages.
    let mut extra: Vec<String> = Vec::new();
    if let Some(h) = habits.filter(|h| !h.never.is_empty()) {
        let rows: Vec<String> = h.never.iter().map(|n| format!("{} {} of {}", n.label, n.count, n.of)).collect();
        extra.push(format!("Almost never in their {}: {}", plural_kind(c.kind), rows.join("; ")));
    }
    let layout: Vec<&str> = r.social_tells_unused.iter().copied().filter(|t| voice::ITEM_TELLS.contains(t)).collect();
    let phrases = r.social_tells_unused.iter().copied().filter(|t| !voice::ITEM_TELLS.contains(t)).take(10);
    let tells: Vec<&str> = layout.into_iter().chain(phrases).collect();
    if !tells.is_empty() {
        extra.push(format!("Social-post tells this author never writes: {}", tells.join(", ")));
    }
    out.extend(giveaways(c, &extra));
    out.extend(exemplar_section(
        c,
        format!("Example {} (study the moves; never reuse the lines)", plural_kind(c.kind)),
        &picks,
        |e| {
            let eng = e.engagement_pct.map(|x| format!(", engagement p{:.0}", x * 100.0)).unwrap_or_default();
            format!("{}{}, {} words", e.date.as_deref().and_then(|d| d.get(0..10)).unwrap_or(&e.source), eng, e.words)
        },
    ));
    out.push(loop_short(c));
    (out, picks, tm)
}

/// Openers (the first one or two words) and the first lines of the
/// author's best typical posts, apart from the examples.
fn hooks_section(c: &Ctxt, habits: Option<&ItemHabits>, picks: &[Example]) -> Option<Section> {
    let p = c.p;
    let mut lines = Vec::new();
    let (_, fmed, fhi) = spread3(
        c.items
            .iter()
            .filter_map(|&i| p.chunks[i].paragraphs.first().and_then(|para| para.lines().next()).map(|l| count(l) as f64))
            .collect(),
    );
    lines.push(format!("First line: typically {} words, up to {}.", fmt_count(fmed), fmt_count(fhi)));
    if let Some(h) = habits.filter(|h| !h.openers.is_empty()) {
        lines.push(format!("Openers: {}.", shares(&h.openers, 6)));
    }
    let shown: std::collections::HashSet<&str> = picks.iter().map(|e| e.text.as_str()).collect();
    let eng = retrieve::engagement_percentiles(p, &c.items);
    let mut best: Vec<(f64, usize)> = c
        .items
        .iter()
        .filter(|&&i| {
            let t = p.chunks[i].text();
            retrieve::bare_words(&t) >= 6
                && !p.chunks[i].meta.as_ref().is_some_and(|m| m.media)
                && !shown.contains(t.as_str())
        })
        .filter_map(|&i| eng.get(&i).map(|e| (*e, i)))
        .collect();
    best.sort_by(|a, b| b.0.total_cmp(&a.0));
    let firsts: Vec<String> = best
        .iter()
        .take(5)
        .filter_map(|(_, i)| {
            let line = p.chunks[*i].paragraphs.first()?.lines().next()?.to_string();
            let words: Vec<&str> = line.split_whitespace().collect();
            let mut s = words.iter().take(12).copied().collect::<Vec<_>>().join(" ");
            if words.len() > 12 {
                s.push_str(" …");
            }
            Some(format!("\"{s}\""))
        })
        .collect();
    if !firsts.is_empty() {
        lines.push(format!("First lines of their best-received {}: {}", plural_kind(c.kind), firsts.join("; ")));
    }
    Some(sec("Hooks", bullets(&lines)))
}

/// Informal words, word choices and recurring formulas.
fn short_words(c: &Ctxt) -> Vec<Section> {
    let mut out = Vec::new();
    let (used, never) = voice::informal_use(c.p, &c.items);
    let mut lines = Vec::new();
    if !used.is_empty() {
        let u: Vec<String> = used.iter().take(10).map(|w| format!("{} {}", w.word, pct(w.share))).collect();
        lines.push(format!("Informal words they use (share of {}): {}.", item_noun(c), u.join(", ")));
    }
    if !never.is_empty() {
        lines.push(format!("Informal words they never use: {}.", never.iter().take(12).copied().collect::<Vec<_>>().join(", ")));
    }
    let formulas: Vec<String> = c.r.formulas.iter().take(8).map(|f| format!("\"{}\"", f.phrase)).collect();
    if !formulas.is_empty() {
        lines.push(format!("Their recurring formulas (reuse is in voice): {}.", formulas.join("; ")));
    }
    if !lines.is_empty() {
        out.push(sec("Vocabulary", bullets(&lines)));
    }
    out.extend(word_choices(c.r));
    out
}

/// Thread habits from the author's own thread parts: how many, numbering.
fn thread_section(c: &Ctxt) -> Option<Section> {
    let p = c.p;
    let parts: Vec<String> = p
        .chunks
        .iter()
        .filter(|ch| ch.meta.as_ref().is_some_and(|m| m.kind == ItemKind::ThreadPart))
        .map(|ch| ch.text())
        .collect();
    if parts.len() < 5 || c.kind == ItemKind::Reply {
        return None;
    }
    let numbered = parts
        .iter()
        .filter(|t| {
            let first = t.split_whitespace().next().unwrap_or("");
            first.contains('/') && first.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        })
        .count();
    let posts = c.items.len().max(1);
    let mut lines = vec![format!(
        "{} thread parts against {} {}: threads are {}.",
        parts.len(),
        posts,
        plural_kind(c.kind),
        if parts.len() * 4 >= posts { "a regular form" } else { "occasional" }
    )];
    lines.push(format!(
        "{} of thread parts are numbered (\"1/\", \"2/7\").",
        pct(numbered as f64 / parts.len() as f64)
    ));
    Some(sec("Threads", bullets(&lines)))
}

fn loop_short(c: &Ctxt) -> Section {
    let name = &c.p.name;
    let kind = match c.kind {
        ItemKind::Reply => " --as reply",
        ItemKind::ThreadPart => " --as thread",
        _ => "",
    };
    let body = format!(
        "1. Write three versions; they should differ the way the author's own {k} differ (length, layout, ending).\n\
         2. `{BIN} check post.md --profile {name}{kind}`: fix hard failures (never-list, copied runs, length) first.\n\
         3. Keep the version that passes with the best score. For a batch, separate {k} with `---` lines and check them together.\n\
         4. More examples near a topic: `{BIN} examples {name}{kind} --about \"...\"`.\n",
        k = item_noun(c)
    );
    sec("Loop", body)
}

fn mail(c: &Ctxt) -> (Vec<Section>, Vec<Example>, TopicMatch) {
    let r = c.r;
    let kr = kind_report(c);
    let habits = kr.map(|k| &k.habits).filter(|h| h.items > 0);
    let what = if c.register == Register::Email { "emails" } else { "messages" };
    let mut out = vec![header(c, what)];
    if let Some(h) = habits {
        let mut lines = Vec::new();
        if !h.greetings.is_empty() {
            lines.push(format!("Greetings: {}.", shares(&h.greetings, 5)));
        }
        if !h.signoffs.is_empty() {
            lines.push(format!("Sign-offs: {}.", shares(&h.signoffs, 5)));
        }
        if !lines.is_empty() {
            out.push(sec("Greeting and sign-off", bullets(&lines)));
        }
    }
    let mut len = length_section(c, habits);
    let paras: Vec<f64> = c.items.iter().map(|&i| c.p.chunks[i].paragraphs.len() as f64).collect();
    let (_, pmed, phi) = spread3(paras);
    let _ = writeln!(len.body, "- Paragraphs: typically {}, up to {}.", fmt_count(pmed), fmt_count(phi));
    len.heading = "Length and paragraphing".into();
    out.push(len);
    out.extend(type_and_hand(c, habits));
    let mut tone = Vec::new();
    if let Some(cb) = band(r, "contractions_per_1k") {
        tone.push(format!("Contractions: {} per 1,000 words.", fmt_num(cb.median)));
    }
    if let Some(e) = band(r, "exclamations_per_1k") {
        tone.push(format!("Exclamation marks: {} per 1,000 words.", fmt_num(e.median)));
    }
    if let Some(q) = band(r, "questions_per_1k") {
        tone.push(format!("Questions: {} per 1,000 words.", fmt_num(q.median)));
    }
    if !tone.is_empty() {
        out.push(sec("Tone", bullets(&tone)));
    }
    if let Some(k) = kr {
        out.extend(targets(c, &k.ranges, format!("Targets per {}", &what[..what.len() - 1])));
    }
    out.extend(short_words(c));
    out.extend(giveaways(c, &[]));
    let (picks, tm) = pick(c, c.exemplars.max(4), false, false);
    out.extend(exemplar_section(
        c,
        format!("Example {what} (study the moves; never reuse the lines)"),
        &picks,
        |e| format!("{}, {} words", e.source, e.words),
    ));
    out.push(loop_short(c));
    (out, picks, tm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_chars_weights_links_and_emoji() {
        assert_eq!(x_chars("hi"), 2);
        assert_eq!(x_chars("see https://example.com/a/very/long/path"), 4 + 23);
        assert_eq!(x_chars("ok 🚀"), 5);
    }

    fn modes(list: &[(&'static str, &'static str, f64)]) -> voice::Modes {
        voice::Modes {
            basis: "sections",
            sections: 30,
            modes: list.iter().map(|&(pov, tense, share)| voice::Mode { pov, tense, share, sections: 1 }).collect(),
        }
    }

    #[test]
    fn modes_say_the_mixed_share_and_never_offer_one_choice() {
        // hf: present-tense first person plus chapters that mix tenses.
        let t = modes_text(&modes(&[("first", "present", 0.689), ("first", "mixed", 0.274)]));
        assert!(!t.contains("Each chapter holds one mode"), "{t}");
        assert!(t.contains("first person, present tense in most chapters (69% of the text"), "{t}");
        assert!(t.contains("27% of the text is in chapters that mix tenses"), "{t}");
        // A dual narrative: two real modes to pick from.
        let t = modes_text(&modes(&[("first", "present", 0.6), ("first", "past", 0.35)]));
        assert!(t.contains("Each chapter holds one mode: first person, present tense (60% of the text); first person, past tense (35% of the text)"), "{t}");
        assert!(!t.contains("mix tenses"), "{t}");
        // Mixed dominates: say the measure can't name a tense.
        let t = modes_text(&modes(&[("first", "mixed", 0.7), ("first", "past", 0.25)]));
        assert!(!t.contains("Each chapter holds one mode") && !t.contains("in most chapters ("), "{t}");
        assert!(t.contains("first person; most chapters mix tenses (70% of the text"), "{t}");
        assert!(t.contains("the rest is first person, past tense (25%)"), "{t}");
        let t = modes_text(&modes(&[("third", "mixed", 0.9)]));
        assert!(t.contains("third person; most chapters mix tenses (90% of the text"), "{t}");
        assert!(!t.contains("throughout"), "{t}");
        let t = modes_text(&modes(&[("third", "past", 0.92)]));
        assert!(t.starts_with("Narration: third person, past tense throughout (92% of the text)."), "{t}");
    }

    #[test]
    fn outlines_are_notes_not_prose() {
        let outline = "# Chapter 12 outline\n\n- Open in the kitchen at night; Claire is waiting up.\n- She has found the second phone.\n1. He laughs it off.";
        assert_eq!(prose_words(outline).0, 0);
        let mixed = "She waited up for him.\n\n- the phone\n- the glass";
        let (prose, all) = prose_words(mixed);
        assert_eq!((prose, all), (5, 9));
    }

    #[test]
    fn warmup_takes_whole_paragraphs_from_the_end() {
        let t = (0..10).map(|i| format!("Paragraph {i} has exactly some words in it here.")).collect::<Vec<_>>().join("\n\n");
        let tail = tail_words(&t, 20);
        assert!(tail.starts_with("Paragraph 7"), "{tail}");
        assert!(tail.ends_with("Paragraph 9 has exactly some words in it here."));
    }

    #[test]
    fn lists_and_headings_are_recognised() {
        assert!(is_list("- one item"));
        assert!(is_list("12. twelfth"));
        assert!(!is_list("2025. A year."));
        assert!(is_heading("## Why"));
        assert!(!is_heading("Why #1"));
    }

    #[test]
    fn tells_lose_inflected_repeats() {
        let t = distinct_tells(&["delve", "delves", "delving", "testament", "a testament to", "navigate", "navigating", "realm"], 10);
        assert_eq!(t, vec!["delve", "testament", "navigate", "realm"]);
    }

    fn curly_book() -> TypographyHabits {
        TypographyHabits {
            double_quotes: vec![("curly".into(), 0.98), ("straight".into(), 0.02)],
            apostrophes: vec![("curly".into(), 0.99), ("straight".into(), 0.01)],
            ellipses: vec![("\u{2026}".into(), 0.9), ("..".into(), 0.07), ("...".into(), 0.03)],
            dashes: vec![("\u{2014}".into(), 0.85), ("--".into(), 0.15)],
            scene_breaks: vec![("\u{2022} \u{2022} \u{2022}".into(), 1.0)],
        }
    }

    /// Round 1: judges named straight quotes, "..." and "---" breaks in
    /// imitations of a book set with curly quotes, "…" and "• • •".
    #[test]
    fn typography_names_the_forms_and_the_defaults_the_author_never_uses() {
        let lines = typography_lines(&curly_book(), false);
        let t = lines.join("\n");
        assert!(t.contains("Quotes: curly \u{201C} \u{201D} (98%)."), "{t}");
        assert!(t.contains("Apostrophes: curly \u{2019} (don\u{2019}t) (99%)."), "{t}");
        assert!(t.contains("Ellipses: \u{2026} (one character, no spaces) (90%)."), "{t}");
        assert!(t.contains("Dashes: closed em dash (word\u{2014}word) (85%); also -- 15%."), "{t}");
        assert!(t.contains("Scene breaks: `\u{2022} \u{2022} \u{2022}` on a line of its own (100%)."), "{t}");
        let defaults = lines.last().unwrap();
        assert!(defaults.starts_with("Agents default to forms this author never uses"), "{defaults}");
        assert!(defaults.ends_with("Convert every one; `check` flags them."), "{defaults}");
        for want in ["straight \" quotes", "straight ' apostrophes", "\"...\"", "`---` breaks"] {
            assert!(defaults.contains(want), "{want}: {defaults}");
        }
        assert!(!defaults.contains("em dash"), "the author's own dash is no default to avoid: {defaults}");

        // Posts: no scene breaks (`---` separates posts in a batch), and an
        // author who writes straight apostrophes has nothing to convert.
        let posts = TypographyHabits {
            apostrophes: vec![("straight".into(), 0.94), ("curly".into(), 0.06)],
            ellipses: vec![("..".into(), 0.73), ("...".into(), 0.27)],
            dashes: vec![(" - ".into(), 0.4), ("--".into(), 0.4), ("\u{2014}".into(), 0.2)],
            scene_breaks: vec![("---".into(), 1.0)],
            ..Default::default()
        };
        let t = typography_lines(&posts, true).join("\n");
        assert!(t.contains("Apostrophes: straight ' (don't) (94%)."), "{t}");
        assert!(t.contains("Ellipses: .. (two dots, as a joiner) (73%); also ... 27%."), "{t}");
        assert!(t.contains("Dashes: spaced hyphen (word - word) (40%); also -- 40%, word\u{2014}word 20%."), "{t}");
        assert!(!t.contains("Scene breaks") && !t.contains("Agents default"), "{t}");
        // A default the author never uses, in a family with no 80% habit:
        // convert it, but `check` holds no draft to a mixed habit.
        let mixed = TypographyHabits { dashes: vec![("--".into(), 0.45), (" - ".into(), 0.33), (" \u{2014} ".into(), 0.22)], ..Default::default() };
        let t = typography_lines(&mixed, true).join("\n");
        assert!(t.contains("Agents default to forms this author never uses: closed em dashes (\u{2014}). Convert every one."), "{t}");
        assert!(!t.contains("`check` flags"), "{t}");
        // Nothing measured, nothing said.
        assert!(typography_lines(&TypographyHabits::default(), false).is_empty());
    }

    #[test]
    fn exemplar_note_names_the_forms_the_author_holds() {
        assert_eq!(
            typography_note(&curly_book()),
            "Their punctuation and quote marks are part of the voice (curly \u{201C} \u{201D}, curly \u{2019}, \u{2026}, word\u{2014}word): set yours the same way."
        );
        assert_eq!(
            typography_note(&TypographyHabits::default()),
            "Their punctuation and quote marks are part of the voice: set yours the same way."
        );
    }

    fn mk_band(key: &str, low: f64, median: f64, high: f64) -> Band {
        Band { key: key.into(), label: key.into(), unit: Unit::Per1k, low, median, high, family: "mechanics", unmeasured: false }
    }

    #[test]
    fn doubled_marks_skip_ellipses() {
        assert_eq!(doubled_marks("wait.. what?! no!! really?? fine... ok."), [1, 1, 1, 1]);
        assert_eq!(doubled_marks("He paused... then. . . left! Why?"), [0, 0, 0, 0]);
        assert_eq!(doubled_marks("so..\nand!?"), [1, 0, 0, 1]);
    }

    /// Round 1: posts imitations had "no typos, no '!!', no 'e.g.'/'etc'".
    /// The brief keeps the author's loose hand but never asks for slips.
    #[test]
    fn mechanics_keep_an_informal_hand_and_never_prescribe_slips() {
        let bands = vec![
            mk_band("double_punct_per_1k", 0.0, 3.0, 6.0),
            mk_band("space_before_punct_per_1k", 0.0, 1.0, 2.0),
            mk_band("nonword_per_1k", 4.0, 9.0, 15.0),
        ];
        let hand = Hand {
            doubled: vec![("..", 3.6)],
            abbrevs: vec![("e.g.", 0.7)],
            loose: vec!["no final full stop (10% of posts)".into()],
        };
        let t = mechanics_lines(&bands, &hand).join("\n");
        assert!(t.contains("Doubled marks: \"..\" (typically 3.0 per 1,000 words, up to 6.0)."), "{t}");
        assert!(t.contains("A space before punctuation (\"word ,\", \"word ?\"): typically 1.0 per 1,000 words."), "{t}");
        assert!(
            t.contains("Polish: informal. Keeps \"..\", \"e.g.\", the space before a mark, no final full stop (10% of posts); don't tidy these away."),
            "{t}"
        );
        assert!(t.contains("About 9.0 words per 1,000 are outside the dictionary"), "{t}");
        assert_eq!(t.matches("misspell").count(), t.matches("never invent misspellings").count(), "{t}");
        assert!(!t.contains("typo"), "{t}");

        let bands = vec![mk_band("double_punct_per_1k", 0.0, 0.0, 0.0), mk_band("space_before_punct_per_1k", 0.0, 0.2, 1.0)];
        let edited = Hand { abbrevs: vec![("etc", 0.3)], ..Default::default() };
        let t = mechanics_lines(&bands, &edited).join("\n");
        assert!(t.contains("Doubled marks (!!, ?!, ..): never."), "{t}");
        assert!(t.contains("Polish: edited copy, standard spelling and punctuation (uses \"etc\")."), "{t}");
        assert!(!t.contains("space before"), "an occasional stray space is no habit: {t}");
        // Without the mechanics measures there is nothing to say.
        assert!(mechanics_lines(&[], &hand).is_empty());
    }

    #[test]
    fn polish_rows_list_only_what_the_author_rarely_does() {
        let bands = vec![
            mk_band("short_triplets_per_1k", 0.0, 0.0, 0.0),
            mk_band("anaphora_runs_per_1k", 0.0, 0.0, 1.0),
            mk_band("mirror_pairs_per_1k", 0.0, 1.0, 3.0),
            Band { unmeasured: true, ..mk_band("not_but_per_1k", 0.0, 0.0, 0.0) },
        ];
        let rows = polish_rows(&bands);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].starts_with("three or more short sentences in a row") && rows[0].ends_with(": none"), "{rows:?}");
        assert!(rows[1].starts_with("runs of sentences that open the same way") && rows[1].ends_with(": 1.0"), "{rows:?}");
    }

    #[test]
    fn shares_render_percentages() {
        let v = vec![("none".to_string(), 0.5), (".".to_string(), 0.25)];
        assert_eq!(shares(&v, 5), "none 50%, . 25%");
    }
}
