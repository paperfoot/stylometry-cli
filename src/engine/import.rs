//! Corpus importers. Long-form text (.md/.txt) is read as documents; this
//! module adds the short-form shapes writers actually have: an unzipped X
//! archive (`data/tweets.js`, `tweets-part1.js`, `note-tweet.js`,
//! `account.js`, optionally `direct-messages.js`), xmaster JSON, JSON/JSONL
//! exports, CSV, and .eml emails.
//!
//! Each item keeps only what the person wrote, verbatim (line breaks matter
//! in a post): retweets, other people's posts and DMs, the @handles X puts
//! in front of a reply, and media links are removed, because every one of
//! them would be measured as the author's voice. Items carry their kind
//! (post / reply / thread part / quote), ids, engagement, ISO date and a
//! media flag, so ranges and exemplars can be computed per kind.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use walkdir::WalkDir;

use crate::engine::profile::{ItemKind, ItemMeta, Register};
use crate::engine::text::{self, Doc, Item};
use crate::error::AppError;

/// Field names that hold an item's text, in priority order.
const TEXT_FIELDS: [&str; 6] = ["full_text", "text", "content", "body", "message", "tweet"];

/// Fields naming the item a reply answers.
const PARENT_FIELDS: [&str; 6] = [
    "parent_id",
    "in_reply_to_status_id_str",
    "in_reply_to_status_id",
    "in_reply_to_id",
    "reply_to",
    "in_reply_to",
];

/// Items shorter than this (counted with links and handles as one word
/// each) carry no measurable voice.
const MIN_ITEM_WORDS: usize = 3;

const BIN: &str = env!("CARGO_BIN_NAME");

/// What a corpus path contained: the documents, and the register the source
/// format implies (an X archive is social whatever its length), if any.
pub struct Corpus {
    pub docs: Vec<Doc>,
    pub format_register: Option<Register>,
    /// Items skipped for any reason (see `report.skipped` for why).
    pub skipped: usize,
    /// What was read and what was left out, for the build report.
    pub report: ImportReport,
}

/// The import summary `profile build` can show: item counts per kind, what
/// was dropped and why, and whose account the items were kept for.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    /// Structured items kept (posts, replies, messages, emails).
    pub items: usize,
    /// Kept items per kind: post, reply, thread_part, quote.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kinds: BTreeMap<&'static str, usize>,
    pub skipped: Skipped,
    /// Files left out on purpose, with the reason (likes, deleted posts, DMs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files_skipped: Vec<String>,
    /// The account whose writing was kept: the archive's `account.js`
    /// username, or the dominant author of an xmaster dump.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Long posts whose truncated archive text was replaced by the full
    /// text from `note-tweet.js`.
    #[serde(skip_serializing_if = "is_zero")]
    pub note_tweets_merged: usize,
    /// Archive parts read alongside a `tweets.js` passed on its own
    /// (`tweets-part1.js`, ...: the rest of the same history).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_read: Vec<String>,
    /// Anything that makes the profile differ from what the agent passed:
    /// files left out as unreadable, other accounts' posts dropped.
    /// `profile build` should print these.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Items dropped, by reason.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Skipped {
    pub retweets: usize,
    /// Under 3 words once reply handles and media links are removed.
    pub too_short: usize,
    /// Posts by other accounts (home-timeline dumps mix them in).
    pub other_authors: usize,
    /// Same id seen in an earlier file.
    pub duplicates: usize,
    /// Direct messages sent by the other side of the conversation.
    pub other_senders: usize,
}

impl Skipped {
    fn total(&self) -> usize {
        self.retweets + self.too_short + self.other_authors + self.duplicates + self.other_senders
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Read a file or folder of any supported format (register detected later).
pub fn read_corpus(path: &Path) -> Result<Corpus, AppError> {
    read_corpus_as(path, None)
}

/// Read a corpus for a requested register. `Some(Register::Message)` on an
/// X archive reads the owner's direct messages instead of their posts; any
/// other register reads posts and leaves DMs out. A short-form register also
/// keeps the blank-line boundaries of .md/.txt files (see `clean_plain`).
pub fn read_corpus_as(path: &Path, register: Option<Register>) -> Result<Corpus, AppError> {
    let single = path.is_file();
    let mut files = list_files(path)?;
    let root = if single {
        path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
    } else {
        path
    };

    // Archive context: the owner's account and the full text of long posts
    // live in sibling files of tweets.js.
    // A file passed alone looks only at its direct siblings (not a walk of
    // the whole folder it happens to sit in).
    let context_files: Vec<PathBuf> = if single {
        let mut v: Vec<PathBuf> = std::fs::read_dir(root)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()).collect())
            .unwrap_or_default();
        v.sort();
        v
    } else {
        files.clone()
    };
    let mut report = ImportReport::default();
    // A big account's history is split over tweets.js, tweets-part1.js, ...:
    // passing tweets.js alone must not silently drop the older parts.
    if single && is_tweets_file(&base_name(path)) {
        for f in context_files.iter().filter(|f| is_tweets_file(&base_name(f)) && base_name(f) != base_name(path)) {
            report.also_read.push(base_name(f));
            files.push(f.clone());
        }
    }
    let account = context_files
        .iter()
        .find(|f| base_name(f) == "account.js")
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|raw| parse_account(&raw));
    let notes: Vec<Note> = context_files
        .iter()
        .filter(|f| is_note_file(&base_name(f)))
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|raw| parse_notes(&raw))
        .collect();

    let has_dm_files = files.iter().any(|f| is_dm_file(&base_name(f)));
    let want_dms = has_dm_files && (register == Some(Register::Message) || (single && is_dm_file(&base_name(path))));
    let short_form = register.is_some_and(Register::is_short_form);

    report.account = account.as_ref().and_then(|a| a.username.as_ref().map(|u| format!("@{u}")));
    let mut docs: Vec<Doc> = Vec::new();
    // Structured files are read first and filtered across files at the end
    // (dominant author, duplicate ids, self-replies need the whole corpus).
    let mut structured: Vec<Export> = Vec::new();
    let mut archive_parts: HashMap<(Option<PathBuf>, &'static str), usize> = HashMap::new();
    let mut note_pool = NotePool::new(notes);
    // Exports in a folder that could not be read, kept for the error when
    // nothing else was readable.
    let mut failures: Vec<(String, AppError)> = Vec::new();
    let mut dm_skipped: Vec<&PathBuf> = Vec::new();
    let mut posts_read = false;
    for f in &files {
        let name = label(path, f);
        let base = base_name(f);
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let parsed: Result<Option<(Parsed, Register)>, AppError> = (|| {
            Ok(match ext.as_str() {
                "md" | "txt" | "markdown" | "text" => {
                    let raw = std::fs::read_to_string(f)?;
                    docs.push(Doc {
                        name: name.clone(),
                        text: clean_plain(&raw, short_form),
                        items: Vec::new(),
                    });
                    None
                }
                "js" if is_tweets_file(&base) => {
                    if want_dms {
                        report.files_skipped.push(format!("{name}: posts (build a social profile from them)"));
                        None
                    } else {
                        let raw = std::fs::read_to_string(f)?;
                        posts_read = true;
                        Some((x_archive(&raw, account.as_ref(), &mut note_pool)?, Register::Social))
                    }
                }
                "js" if is_dm_file(&base) => {
                    if want_dms {
                        let raw = std::fs::read_to_string(f)?;
                        let (p, others) = x_direct_messages(&raw, account.as_ref())?;
                        report.skipped.other_senders += others;
                        Some((p, Register::Message))
                    } else {
                        report.files_skipped.push(format!(
                            "{name}: direct messages, left out (profile them with --corpus {})",
                            f.display()
                        ));
                        dm_skipped.push(f);
                        None
                    }
                }
                "js" => {
                    if let Some(why) = skip_reason(&base) {
                        report.files_skipped.push(format!("{name}: {why}"));
                    }
                    None
                }
                "json" => Some((json_items(&std::fs::read_to_string(f)?)?, Register::Social)),
                "jsonl" | "ndjson" => Some((jsonl_items(&std::fs::read_to_string(f)?), Register::Social)),
                "csv" => Some((csv_items(f)?, Register::Social)),
                "eml" => Some((
                    Parsed {
                        objects: 1,
                        items: vec![Raw {
                            item: eml_item(&std::fs::read_to_string(f)?),
                            author: None,
                        }],
                        ..Parsed::default()
                    },
                    Register::Email,
                )),
                _ => None,
            })
        })();
        let export_ext = matches!(ext.as_str(), "json" | "jsonl" | "ndjson" | "csv");
        match parsed {
            // Records but no text field: an export whose column has another
            // name, not an empty export.
            Ok(Some((p, _))) if export_ext && p.items.is_empty() && p.retweets == 0 && p.objects > 0 => {
                let e = AppError::input(
                    format!("{name} has {} record(s) but none with a text field (looked for {})", p.objects, TEXT_FIELDS.join(", ")),
                    "Rename the field holding the writing to \"text\"",
                );
                if single {
                    return Err(e);
                }
                report.warnings.push(format!("{name} left out: {}", error_text(&e)));
                failures.push((name, e));
            }
            Ok(Some((p, reg))) => {
                report.skipped.retweets += p.retweets;
                // An archive splits one account's posts over tweets.js,
                // tweets-part1.js, ...: one source, sorted by time later.
                let key = (ext == "js").then(|| (f.parent().map(|d| d.canonicalize().unwrap_or_else(|_| d.to_path_buf())), reg.as_str()));
                if let Some(&i) = key.as_ref().and_then(|k| archive_parts.get(k)) {
                    structured[i].items.extend(p.items);
                } else {
                    if let Some(k) = key {
                        archive_parts.insert(k, structured.len());
                    }
                    structured.push(Export {
                        name,
                        items: p.items,
                        register: reg,
                    });
                }
            }
            Ok(None) => {}
            // In a folder, one unreadable export shouldn't sink the rest,
            // but the agent must hear that it was left out.
            Err(e) if !single && (export_ext || ext == "js") => {
                report.files_skipped.push(format!("{name}: unreadable ({})", error_text(&e)));
                report.warnings.push(format!("{name} could not be read and was left out: {}", error_text(&e)));
                failures.push((name, e));
            }
            Err(e) => return Err(e),
        }
    }
    report.note_tweets_merged = note_pool.merged;
    if register == Some(Register::Message) && posts_read {
        report.warnings.push(
            "--register message on X posts: there is no direct-messages.js here, so this profile is built from public posts".into(),
        );
    }

    let mut out = Corpus {
        docs,
        format_register: None,
        skipped: 0,
        report,
    };
    finish_structured(&mut out, structured, account.as_ref());
    out.skipped = out.report.skipped.total();
    recount(&mut out);

    if out.docs.is_empty() {
        return Err(nothing_readable(path, &out, failures, &dm_skipped));
    }
    let long_form = out.docs.iter().any(|d| d.items.is_empty());
    if long_form && out.format_register.is_some() {
        // Mixed folders: the structured items decide only if they dominate.
        let item_words: usize = out.docs.iter().flat_map(|d| &d.items).map(|i| text::count_words(&i.text)).sum();
        let prose_words: usize = out.docs.iter().map(|d| text::count_words(&d.text)).sum();
        if prose_words > item_words {
            out.format_register = None;
        }
    }
    Ok(out)
}

/// The error for a corpus with nothing to profile, naming the actual cause:
/// exports that failed to read (as if each had been passed directly), DMs
/// left out on purpose, or items all filtered away.
fn nothing_readable(path: &Path, c: &Corpus, mut failures: Vec<(String, AppError)>, dm_skipped: &[&PathBuf]) -> AppError {
    if failures.len() == 1 {
        return failures.remove(0).1;
    }
    if let Some((_, first)) = failures.first() {
        let list: Vec<String> = failures.iter().map(|(n, e)| format!("{n} ({})", error_text(e))).collect();
        let fix = match first {
            AppError::InvalidArguments { suggestion, .. } => suggestion.clone(),
            _ => "Fix or remove the files listed, then build again".into(),
        };
        return AppError::input(format!("no export under {} could be read: {}", path.display(), list.join("; ")), fix);
    }
    if let Some(dm) = dm_skipped.first() {
        return AppError::input(
            format!(
                "no posts under {}: it holds direct messages ({}), which are profiled only when asked for",
                path.display(),
                dm.display()
            ),
            format!("Profile the DMs: {BIN} profile build <name> --corpus {}", dm.display()),
        );
    }
    let mut why = format!("no readable text found under {}", path.display());
    if c.skipped > 0 {
        why.push_str(&format!(" ({} item(s) skipped: retweets, other accounts, or under 3 words)", c.skipped));
    }
    AppError::input(
        why,
        "Supported: .md/.txt prose, an unzipped X archive (pass its data/ folder), xmaster/JSON/JSONL/CSV exports with a text column, .eml",
    )
}

/// An error's message without the "Invalid input:" prefix, for warnings.
fn error_text(e: &AppError) -> String {
    match e {
        AppError::InvalidArguments { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Item counts per kind, recomputed after every filter.
fn recount(c: &mut Corpus) {
    c.report.items = 0;
    c.report.kinds.clear();
    for i in c.docs.iter().flat_map(|d| &d.items) {
        c.report.items += 1;
        *c.report.kinds.entry(kind_name(i.meta.kind)).or_insert(0) += 1;
    }
}

/// One structured file's parse: its items and what was dropped on the way.
#[derive(Default)]
struct Parsed {
    items: Vec<Raw>,
    retweets: usize,
    /// Records seen, with or without text.
    objects: usize,
}

/// A structured file after parsing, before the cross-file filters.
struct Export {
    name: String,
    items: Vec<Raw>,
    register: Register,
}

/// One parsed item plus what is needed to filter it across files.
struct Raw {
    item: Item,
    /// The posting account, when the export names it (xmaster `author`).
    author: Option<String>,
}

/// Cross-file filters: keep the owner's account, drop repeated ids, mark
/// replies to the owner's own posts as thread parts, drop items too short
/// to measure; then sort each file's items by date.
fn finish_structured(out: &mut Corpus, structured: Vec<Export>, account: Option<&Account>) {
    let mut authors: HashMap<String, usize> = HashMap::new();
    for e in &structured {
        for r in &e.items {
            if let Some(a) = &r.author {
                *authors.entry(norm_handle(a)).or_insert(0) += 1;
            }
        }
    }
    let owner: Option<String> = account
        .and_then(|a| a.username.as_deref().map(norm_handle))
        .filter(|u| authors.contains_key(u))
        .or_else(|| authors.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(a, _)| a.clone()));
    if out.report.account.is_none() {
        out.report.account = owner.as_ref().map(|o| format!("@{o}"));
    }

    let own_ids: HashSet<String> = structured
        .iter()
        .flat_map(|e| &e.items)
        .filter(|r| r.author.as_deref().is_none_or(|a| Some(norm_handle(a)) == owner))
        .filter_map(|r| r.item.meta.id.clone())
        .collect();

    let mut seen: HashSet<String> = HashSet::new();
    for Export { name, items, register: reg } in structured {
        let mut kept: Vec<Item> = Vec::with_capacity(items.len());
        for Raw { mut item, author } in items {
            if let (Some(a), Some(o)) = (&author, &owner) {
                if norm_handle(a) != *o {
                    out.report.skipped.other_authors += 1;
                    continue;
                }
            }
            if let Some(id) = &item.meta.id {
                if !seen.insert(id.clone()) {
                    out.report.skipped.duplicates += 1;
                    continue;
                }
            }
            if item.meta.kind == ItemKind::Reply && item.meta.parent_id.as_ref().is_some_and(|p| own_ids.contains(p)) {
                item.meta.kind = ItemKind::ThreadPart;
            }
            if text::count_words(&text::normalize_web(&item.text)) < MIN_ITEM_WORDS {
                out.report.skipped.too_short += 1;
                continue;
            }
            kept.push(item);
        }
        if kept.is_empty() {
            continue;
        }
        // Time order, so windows pool neighbouring posts (archives are
        // newest-first). Undated items keep their order, after dated ones.
        kept.sort_by(|a, b| match (&a.meta.date, &b.meta.date) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        // JSON/CSV exports of long items (newsletters, blog posts) are not social.
        let is_archive = name.ends_with(".js");
        let reg = if reg == Register::Social && !is_archive && median_words(&kept) > 60 {
            Register::Blog
        } else {
            reg
        };
        out.format_register.get_or_insert(reg);
        out.docs.push(Doc {
            name,
            text: String::new(),
            items: kept,
        });
    }
    // A home-timeline dump is cut to one account: say whose.
    if let (Some(o), n) = (&owner, out.report.skipped.other_authors) {
        if n > 0 {
            let others = authors.len().saturating_sub(1);
            out.report.warnings.push(format!(
                "kept only @{o}'s posts; left out {n} post(s) by {others} other account(s) (pass a dump of a single account if @{o} is not the writer)"
            ));
        }
    }
}

fn kind_name(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Post => "post",
        ItemKind::Reply => "reply",
        ItemKind::ThreadPart => "thread_part",
        ItemKind::Quote => "quote",
    }
}

fn norm_handle(h: &str) -> String {
    h.trim().trim_start_matches('@').to_lowercase()
}

fn list_files(path: &Path) -> Result<Vec<PathBuf>, AppError> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(AppError::input(
            format!("path does not exist: {}", path.display()),
            "Pass a file or folder: .md/.txt prose, an unzipped X archive, xmaster/JSON/JSONL/CSV exports, or .eml emails",
        ));
    }
    Ok(WalkDir::new(path)
        .sort_by_file_name()
        .into_iter()
        // macOS zip debris: AppleDouble `._x` files and `__MACOSX/` copies.
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !n.starts_with("._") && n != "__MACOSX"
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect())
}

fn label(root: &Path, f: &Path) -> String {
    f.strip_prefix(root)
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .map(|r| r.display().to_string())
        .unwrap_or_else(|| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn base_name(f: &Path) -> String {
    f.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// `stem` optionally followed by `-partN`, then `.js`.
fn matches_part(name: &str, stems: &[&str]) -> bool {
    let Some(stem) = name.strip_suffix(".js") else {
        return false;
    };
    let stem = match stem.rsplit_once("-part") {
        Some((s, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => s,
        _ => stem,
    };
    stems.contains(&stem)
}

/// `tweets.js`, `tweet.js` (older archives), `tweets-part1.js` (big accounts).
fn is_tweets_file(name: &str) -> bool {
    matches_part(name, &["tweets", "tweet"])
}

fn is_note_file(name: &str) -> bool {
    matches_part(name, &["note-tweet", "note-tweets"])
}

fn is_dm_file(name: &str) -> bool {
    matches_part(name, &["direct-messages", "direct-messages-group"])
}

/// Archive files that hold someone's writing but must not be profiled.
fn skip_reason(name: &str) -> Option<&'static str> {
    if matches_part(name, &["deleted-tweets", "deleted-tweet", "deleted-note-tweet"]) {
        Some("deleted posts (left out)")
    } else if matches_part(name, &["like", "likes"]) {
        Some("liked posts are other people's writing")
    } else {
        None
    }
}

/// Drop dated items before `day` (YYYY-MM-DD). Undated items are kept.
pub fn keep_since(c: &mut Corpus, day: &str) -> Result<(), AppError> {
    if day.len() != 10 || day.as_bytes()[4] != b'-' || day.as_bytes()[7] != b'-' {
        return Err(AppError::input(format!("--since '{day}' is not YYYY-MM-DD"), "Example: --since 2025-01-01"));
    }
    for d in &mut c.docs {
        d.items.retain(|i| i.meta.date.as_deref().is_none_or(|dt| dt.get(..10).is_none_or(|x| x >= day)));
    }
    c.docs.retain(|d| !d.items.is_empty() || !d.text.is_empty());
    recount(c);
    Ok(())
}

fn median_words(items: &[Item]) -> usize {
    let mut w: Vec<usize> = items.iter().map(|i| text::count_words(&i.text)).collect();
    w.sort_unstable();
    w.get(w.len() / 2).copied().unwrap_or(0)
}

// ── Splitting plain text into items ─────────────────────────────────────

/// Clean a plain-text file without letting paragraph repair cross an item
/// boundary. `text::clean` rejoins a block to the one before when that one
/// lacks final punctuation and this one starts in lowercase (a conversion's
/// page break). Two boundaries must survive it. Separator lines (`---`,
/// `***`, `===`) always mark items or scene breaks, so each piece between
/// them is cleaned on its own and the line itself is kept. For a short-form
/// register, blank lines too: a casual post with no final period followed
/// by one starting in lowercase is two posts, not a broken paragraph.
pub fn clean_plain(raw: &str, short_form: bool) -> String {
    let clean_piece = |piece: &str| -> String {
        if short_form {
            raw_blocks(piece)
                .iter()
                .map(|b| text::clean(b).trim().to_string())
                .filter(|b| !b.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            text::clean(piece).trim().to_string()
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut piece: Vec<&str> = Vec::new();
    for line in raw.lines() {
        if is_separator_line(line) {
            out.push(clean_piece(&piece.join("\n")));
            out.push(line.trim().to_string());
            piece.clear();
        } else {
            piece.push(line);
        }
    }
    out.push(clean_piece(&piece.join("\n")));
    out.retain(|p| !p.is_empty());
    out.join("\n\n") + "\n"
}

/// Blocks of raw text between blank (or whitespace-only) lines.
fn raw_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line);
        }
    }
    out
}

/// A line of three or more `-`, `*` or `=`: an explicit item boundary.
pub fn is_separator_line(l: &str) -> bool {
    let t = l.trim();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '='))
}

/// Split a plain-text document into items for a short-form register.
/// Separator lines (`---`, `***`, `===`) always win. Without them, social
/// text falls back to blank lines (one post per block); an email or message
/// file is one item, because greeting, body and sign-off are blocks of the
/// same email and splitting them loses all three.
pub fn split_items_for(text: &str, register: Register) -> Vec<Item> {
    if text.lines().any(is_separator_line) || register != Register::Social {
        return split_on_separators(text);
    }
    blocks(text, |l| l.trim().is_empty())
}

/// Split only on separator lines; text without them is one item. This is
/// the rule for a `check` draft: one post unless the agent separated several.
pub fn split_on_separators(text: &str) -> Vec<Item> {
    blocks(text, is_separator_line)
}

/// The register-blind split `profile.rs` used before registers reached it:
/// separators, else blank lines. Prefer `split_items_for`.
pub fn split_items(text: &str) -> Vec<Item> {
    split_items_for(text, Register::Social)
}

fn blocks(text: &str, boundary: impl Fn(&str) -> bool) -> Vec<Item> {
    let mut items = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut flush = |cur: &mut Vec<&str>| {
        let t = cur.join("\n").trim().to_string();
        cur.clear();
        if !t.is_empty() {
            items.push(Item {
                text: t,
                meta: ItemMeta::default(),
            });
        }
    };
    for line in text.lines() {
        if boundary(line) {
            flush(&mut cur);
        } else {
            cur.push(line);
        }
    }
    flush(&mut cur);
    items
}

// ── Value helpers ───────────────────────────────────────────────────────

/// Decode HTML entities in one pass (X archives store `&amp;`, `&lt;`,
/// `&gt;`), so "&amp;lt;" becomes "&lt;", never "<".
pub fn unescape_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&j| j <= 10).and_then(|j| {
            let ent = &rest[1..j];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let n = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X"));
                    match n {
                        Some(h) => u32::from_str_radix(h, 16).ok().and_then(char::from_u32),
                        None => ent.strip_prefix('#').and_then(|d| d.parse().ok()).and_then(char::from_u32),
                    }
                }
            };
            c.map(|c| (c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace([',', '_'], "").parse().ok(),
        _ => None,
    }
}

fn first_num(o: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| num(o.get(*k)))
}

fn first_str<'a>(o: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| o.get(*k).and_then(|v| v.as_str()))
}

/// A string or integer id; empty strings, nulls and "0" are no id (CSV
/// exports write every column, filled or not).
fn id_of(v: Option<&Value>) -> Option<String> {
    let s = match v? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty() && s != "0" && s != "null").then_some(s)
}

fn first_id(o: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| id_of(o.get(*k)))
}

/// A boolean, also as CSV text ("true", "1", "yes").
fn flag(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => matches!(s.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "y"),
        _ => false,
    }
}

fn non_empty_array(v: Option<&Value>) -> bool {
    v.and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty())
}

/// Parse an X archive `.js` file: `window.YTD.<name>.part0 = [...]`, or
/// plain JSON when the prefix is missing.
fn ytd_json(raw: &str, what: &str) -> Result<Value, AppError> {
    let t = raw.trim_start_matches('\u{feff}').trim();
    let body = if t.starts_with('[') || t.starts_with('{') {
        t
    } else {
        t.split_once('=').map(|(_, b)| b.trim()).unwrap_or(t)
    };
    serde_json::from_str(body.trim_end_matches(';')).map_err(|e| {
        AppError::input(
            format!("{what} is not valid archive JSON: {e}"),
            "Pass the data/ folder of an unzipped X archive (Settings > Your account > Download an archive)",
        )
    })
}

// ── Dates ───────────────────────────────────────────────────────────────

/// ISO-8601 for the date shapes exports use: X archive ("Wed Oct 10
/// 20:19:24 +0000 2018"), RFC 2822 email dates, Unix seconds or
/// milliseconds, and ISO itself. Anything else is kept as given.
pub fn iso_date(s: &str) -> String {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() >= 10 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' {
        return t.to_string();
    }
    if !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = t.parse::<i64>() {
            return epoch_iso(if t.len() >= 13 { n / 1000 } else { n });
        }
    }
    parse_wordy_date(t).unwrap_or_else(|| t.to_string())
}

fn month_num(m: &str) -> Option<u32> {
    let m = m.get(..3)?.to_lowercase();
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|x| *x == m)
        .map(|i| i as u32 + 1)
}

/// "Wed Oct 10 20:19:24 +0000 2018" and "Tue, 21 Oct 2026 10:00:00 +0000".
fn parse_wordy_date(t: &str) -> Option<String> {
    let parts: Vec<&str> = t.split_whitespace().map(|p| p.trim_end_matches(',')).collect();
    let is_time = |p: &str| p.contains(':') && p.split(':').all(|x| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()));
    let is_off = |p: &str| (p.starts_with('+') || p.starts_with('-')) && p.len() == 5 && p[1..].bytes().all(|c| c.is_ascii_digit());
    let mut month = None;
    let mut day = None;
    let mut year = None;
    let mut time = None;
    let mut off = None;
    for p in &parts {
        if let Some(m) = month_num(p).filter(|_| month.is_none() && p.chars().all(char::is_alphabetic) && p.len() >= 3) {
            month = Some(m);
        } else if is_time(p) {
            time = Some(*p);
        } else if is_off(p) || *p == "GMT" || *p == "UTC" || *p == "Z" {
            off = Some(*p);
        } else if let Ok(n) = p.parse::<u32>() {
            if p.len() == 4 {
                year = Some(n);
            } else if p.len() <= 2 && day.is_none() {
                day = Some(n);
            }
        }
    }
    let (y, m, d) = (year?, month?, day?);
    if !(1..=31).contains(&d) {
        return None;
    }
    let date = format!("{y:04}-{m:02}-{d:02}");
    let Some(time) = time else {
        return Some(date);
    };
    let mut hms: Vec<&str> = time.split(':').collect();
    hms.resize(3, "00");
    let zone = match off {
        Some(o) if o.len() == 5 && &o[1..] != "0000" => format!("{}:{}", &o[..3], &o[3..]),
        Some(_) => "Z".to_string(),
        None => String::new(),
    };
    Some(format!("{date}T{:0>2}:{:0>2}:{:0>2}{zone}", hms[0], hms[1], hms[2]))
}

/// Unix seconds to ISO UTC (days-to-civil, Howard Hinnant's algorithm).
fn epoch_iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn date_of(o: &Value) -> Option<String> {
    ["created_at", "createdAt", "date", "timestamp", "time", "sent_at"].iter().find_map(|k| match o.get(*k)? {
        Value::String(s) if !s.trim().is_empty() => Some(iso_date(s)),
        Value::Number(n) => n.as_i64().map(|n| epoch_iso(if n > 100_000_000_000 { n / 1000 } else { n })),
        _ => None,
    })
}

// ── Handles and links ───────────────────────────────────────────────────

fn is_handle(tok: &str) -> bool {
    tok.strip_prefix('@')
        .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// Remove the run of @handles X puts in front of a reply ("@alice @bob
/// thanks" → "thanks"), so mentions, the hook and casing are measured on
/// what the person typed. A handle that is part of the sentence ("@alice's
/// idea") stops the run.
pub fn strip_leading_handles(s: &str) -> &str {
    let mut rest = s.trim_start();
    loop {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if !is_handle(&rest[..end]) {
            return rest;
        }
        rest = rest[end..].trim_start();
    }
}

/// The reply's own text by `display_text_range[0]` (code points), when that
/// prefix is only handles; otherwise the handle-run rule.
fn reply_body(text: &str, range_start: Option<usize>) -> String {
    if let Some(start) = range_start.filter(|&s| s > 0) {
        let byte = text.char_indices().nth(start).map(|(b, _)| b).unwrap_or(text.len());
        let (pre, body) = text.split_at(byte);
        let on_boundary = pre.ends_with(char::is_whitespace) || body.starts_with(char::is_whitespace);
        if on_boundary && pre.split_whitespace().all(is_handle) {
            return body.trim_start().to_string();
        }
    }
    strip_leading_handles(text).to_string()
}

/// Remove each link in `urls` from the text and tidy the whitespace left.
fn drop_links(text: &str, urls: &[String]) -> String {
    let mut t = text.to_string();
    for u in urls.iter().filter(|u| !u.is_empty()) {
        t = t.replace(u.as_str(), "");
    }
    let lines: Vec<&str> = t.lines().map(str::trim_end).collect();
    lines.join("\n").trim().to_string()
}

/// `https://x.com/<user>/status/<id>` or twitter.com: a quoted post's link.
fn is_status_url(u: &str) -> bool {
    let u = u.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    let u = u.trim_start_matches("mobile.");
    (u.starts_with("twitter.com/") || u.starts_with("x.com/")) && u.contains("/status/")
}

// ── X archive ───────────────────────────────────────────────────────────

/// The archive owner, from `account.js`. Its email is never read out.
struct Account {
    id: Option<String>,
    username: Option<String>,
}

fn parse_account(raw: &str) -> Option<Account> {
    let v = ytd_json(raw, "account.js").ok()?;
    let a = v.as_array()?.first()?;
    let a = a.get("account").unwrap_or(a);
    Some(Account {
        id: first_id(a, &["accountId", "account_id", "id"]),
        username: first_str(a, &["username", "screen_name"]).map(str::to_string),
    })
}

/// A long post's full text from `note-tweet.js`.
struct Note {
    created: Option<String>,
    text: String,
}

fn parse_notes(raw: &str) -> Vec<Note> {
    let Ok(v) = ytd_json(raw, "note-tweet.js") else {
        return Vec::new();
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let n = n.get("noteTweet").unwrap_or(n);
            let text = n.get("core").and_then(|c| c.get("text")).or_else(|| n.get("text"))?.as_str()?;
            Some(Note {
                created: first_str(n, &["createdAt", "created_at"]).map(iso_date),
                text: unescape_html(text),
            })
        })
        .collect()
}

/// Notes waiting to be matched to the truncated posts they belong to.
struct NotePool {
    notes: Vec<Note>,
    used: Vec<bool>,
    merged: usize,
}

impl NotePool {
    fn new(notes: Vec<Note>) -> Self {
        let used = vec![false; notes.len()];
        NotePool { notes, used, merged: 0 }
    }

    /// The full text for a post whose archive text is a truncated prefix of
    /// a note (matched on the first 40+ characters, ignoring whitespace,
    /// the trailing "…" and link; same day when both dates are known, the
    /// closest creation time wins).
    fn full_text(&mut self, shown: &str, date: Option<&str>) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut prefix = squash(shown);
        if let Some(last) = prefix.rsplit(' ').next().filter(|w| w.starts_with("https://t.co/")) {
            prefix.truncate(prefix.len() - last.len());
        }
        let prefix = prefix.trim_end().trim_end_matches(['…', '.']).trim_end().to_string();
        if prefix.chars().count() < 40 {
            return None;
        }
        let gap = |n: &Note| match (date, n.created.as_deref()) {
            (Some(a), Some(b)) => if a.get(..16) == b.get(..16) { 0 } else if a.get(..10) == b.get(..10) { 1 } else { 2 },
            _ => 1,
        };
        let best = (0..self.notes.len())
            .filter(|&i| !self.used[i])
            // A note is posted with its tweet: a match from another day is a
            // different post that happens to open the same way.
            .filter(|&i| gap(&self.notes[i]) < 2)
            .filter(|&i| {
                let full = squash(strip_leading_handles(&self.notes[i].text));
                full.len() > prefix.len() && full.starts_with(&prefix)
            })
            .min_by_key(|&i| gap(&self.notes[i]))?;
        self.used[best] = true;
        self.merged += 1;
        Some(self.notes[best].text.clone())
    }
}

/// X archive `tweets.js` (or `tweet.js`, `tweets-partN.js`):
/// `window.YTD.tweets.part0 = [ {"tweet": {...}}, ... ]`.
fn x_archive(raw: &str, account: Option<&Account>, notes: &mut NotePool) -> Result<Parsed, AppError> {
    let v = ytd_json(raw, "tweets.js")?;
    let mut items = Vec::new();
    let mut retweets = 0;
    let mut objects = 0;
    for e in v.as_array().into_iter().flatten() {
        objects += 1;
        let o = e.get("tweet").filter(|t| t.is_object()).unwrap_or(e);
        let Some(full) = first_str(o, &["full_text", "text"]) else {
            continue;
        };
        if full.starts_with("RT @") || flag(o.get("retweeted")) && full.starts_with("RT ") {
            retweets += 1;
            continue;
        }
        let entities = |k: &str| -> Vec<&Value> {
            ["entities", "extended_entities"]
                .iter()
                .filter_map(|e| o.get(*e).and_then(|e| e.get(k)).and_then(|a| a.as_array()))
                .flatten()
                .collect()
        };
        let media_links: Vec<String> = entities("media")
            .iter()
            .filter_map(|m| m.get("url").and_then(|u| u.as_str()).map(str::to_string))
            .collect();
        let quote_links: Vec<String> = entities("urls")
            .iter()
            .filter(|u| u.get("expanded_url").and_then(|x| x.as_str()).is_some_and(is_status_url))
            .filter_map(|u| u.get("url").and_then(|x| x.as_str()).map(str::to_string))
            .filter(|u| full.trim_end().ends_with(u.as_str()))
            .collect();

        let parent_id = first_id(o, &["in_reply_to_status_id_str", "in_reply_to_status_id"]);
        let to_user = first_id(o, &["in_reply_to_user_id_str", "in_reply_to_user_id"]);
        let to_name = first_str(o, &["in_reply_to_screen_name"]);
        let own = account.is_some_and(|a| {
            (a.id.is_some() && a.id == to_user)
                || a.username.as_deref().zip(to_name).is_some_and(|(u, n)| u.eq_ignore_ascii_case(n))
        });
        let quoted = !quote_links.is_empty() || flag(o.get("is_quote_status")) || first_id(o, &["quoted_status_id_str"]).is_some();
        let kind = match (parent_id.is_some() || to_user.is_some(), own, quoted) {
            (true, true, _) => ItemKind::ThreadPart,
            (true, false, _) => ItemKind::Reply,
            (false, _, true) => ItemKind::Quote,
            _ => ItemKind::Post,
        };

        let range_start = o
            .get("display_text_range")
            .and_then(|r| r.as_array())
            .and_then(|r| num(r.first()))
            .map(|n| n as usize);
        let mut body = if matches!(kind, ItemKind::Reply | ItemKind::ThreadPart) {
            reply_body(full, range_start)
        } else {
            full.to_string()
        };
        body = unescape_html(&drop_links(&body, &[media_links.clone(), quote_links].concat()));
        let date = first_str(o, &["created_at"]).map(iso_date);
        if let Some(t) = notes.full_text(&body, date.as_deref()) {
            let t = if matches!(kind, ItemKind::Reply | ItemKind::ThreadPart) { strip_leading_handles(&t) } else { &t };
            body = drop_links(t, &media_links);
        }
        items.push(Raw {
            item: Item {
                text: body,
                meta: ItemMeta {
                    id: first_id(o, &["id_str", "id"]),
                    parent_id,
                    kind,
                    likes: first_num(o, &["favorite_count"]),
                    reposts: first_num(o, &["retweet_count"]),
                    impressions: None,
                    date,
                    media: !media_links.is_empty() || !entities("media").is_empty(),
                },
            },
            author: None,
        });
    }
    Ok(Parsed {
        items,
        retweets,
        objects,
        ..Parsed::default()
    })
}

/// `direct-messages.js`: the owner's own messages only (the other side's
/// writing would leak into the profile). The owner is `account.js`'s id, or
/// failing that the one sender present in every conversation.
fn x_direct_messages(raw: &str, account: Option<&Account>) -> Result<(Parsed, usize), AppError> {
    let v = ytd_json(raw, "direct-messages.js")?;
    let convs: Vec<&Value> = v
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c.get("dmConversation").unwrap_or(c))
        .collect();
    let messages = |c: &Value| -> Vec<Value> {
        c.get("messages")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| m.get("messageCreate").cloned())
            .collect()
    };
    let owner = match account.and_then(|a| a.id.clone()) {
        Some(id) => id,
        None => {
            let mut presence: HashMap<String, usize> = HashMap::new();
            for c in &convs {
                let senders: HashSet<String> = messages(c).iter().filter_map(|m| first_id(m, &["senderId"])).collect();
                for s in senders {
                    *presence.entry(s).or_insert(0) += 1;
                }
            }
            let mut ranked: Vec<(String, usize)> = presence.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            match ranked.as_slice() {
                [(id, n), rest @ ..] if *n >= 2 && rest.first().is_none_or(|r| r.1 < *n) => id.clone(),
                _ => {
                    return Err(AppError::input(
                        "can't tell which side of these DMs is the archive owner",
                        "Put account.js (from the same archive's data/ folder) next to direct-messages.js",
                    ));
                }
            }
        }
    };
    let mut items = Vec::new();
    let mut others = 0;
    let mut objects = 0;
    for c in &convs {
        for m in messages(c) {
            objects += 1;
            if first_id(&m, &["senderId"]).as_deref() != Some(owner.as_str()) {
                others += 1;
                continue;
            }
            let Some(t) = first_str(&m, &["text"]) else {
                continue;
            };
            let media = non_empty_array(m.get("mediaUrls"));
            let media_links: Vec<String> = if media {
                // The archive appends the media's t.co link to the text.
                t.split_whitespace().rev().take(1).filter(|w| w.starts_with("https://t.co/")).map(str::to_string).collect()
            } else {
                Vec::new()
            };
            items.push(Raw {
                item: Item {
                    text: unescape_html(&drop_links(t, &media_links)),
                    meta: ItemMeta {
                        id: first_id(&m, &["id"]),
                        date: date_of(&m),
                        media,
                        ..ItemMeta::default()
                    },
                },
                author: None,
            });
        }
    }
    let parsed = Parsed {
        items,
        objects,
        ..Parsed::default()
    };
    Ok((parsed, others))
}

// ── JSON / JSONL / CSV ──────────────────────────────────────────────────

/// An item from one JSON object of any supported export shape, or why not:
/// `Err(true)` for a retweet, `Err(false)` for an object without text.
fn item_of(o: &Value) -> Result<Raw, bool> {
    let o = o.get("tweet").filter(|t| t.is_object()).unwrap_or(o);
    let text = first_str(o, &TEXT_FIELDS).ok_or(false)?;
    if text.starts_with("RT @") {
        return Err(true);
    }
    let metrics = o.get("public_metrics").or_else(|| o.get("metrics")).unwrap_or(o);
    let parent_id = first_id(o, &PARENT_FIELDS);
    let explicit = first_str(o, &["kind"]).and_then(|k| match k.trim().to_lowercase().as_str() {
        "post" | "tweet" | "original" | "status" => Some(ItemKind::Post),
        "reply" => Some(ItemKind::Reply),
        "thread_part" | "thread" | "self_reply" => Some(ItemKind::ThreadPart),
        "quote" => Some(ItemKind::Quote),
        _ => None,
    });
    let unescaped = unescape_html(text);
    let leading_handle = unescaped.split_whitespace().next().is_some_and(is_handle);
    let kind = explicit.unwrap_or(
        if parent_id.is_some() || flag(o.get("is_reply")) || leading_handle {
            ItemKind::Reply
        } else if flag(o.get("is_quote_status")) || flag(o.get("quoted")) || first_id(o, &["quoted_status_id_str", "quoted_status_id", "quote_of"]).is_some() {
            ItemKind::Quote
        } else {
            ItemKind::Post
        },
    );
    let body = if matches!(kind, ItemKind::Reply | ItemKind::ThreadPart) {
        strip_leading_handles(&unescaped).to_string()
    } else {
        unescaped
    };
    let media = flag(o.get("media"))
        || flag(o.get("has_media"))
        || non_empty_array(o.get("media"))
        || non_empty_array(o.get("attachments"))
        || o.get("entities").and_then(|e| e.get("media")).is_some_and(|m| non_empty_array(Some(m)));
    Ok(Raw {
        item: Item {
            text: body,
            meta: ItemMeta {
                id: first_id(o, &["id_str", "id"]),
                parent_id,
                kind,
                likes: first_num(metrics, &["likes", "like_count", "favorite_count", "reactions"]),
                reposts: first_num(metrics, &["retweets", "reposts", "retweet_count", "repost_count", "shares"]),
                // xmaster writes 0 when it doesn't know the impressions.
                impressions: first_num(metrics, &["impressions", "impression_count", "views"]).filter(|&n| n > 0.0),
                date: date_of(o),
                media,
            },
        },
        author: first_str(o, &["author", "username", "screen_name", "handle"])
            .filter(|a| !a.trim().is_empty())
            .map(str::to_string),
    })
}

/// Parse one file's objects.
fn items_of(objs: &[Value]) -> Parsed {
    let unwrap = |o: &'_ Value| -> Value { o.get("tweet").filter(|t| t.is_object()).unwrap_or(o).clone() };
    let rows: Vec<Value> = objs.iter().map(unwrap).collect();
    let mut p = Parsed {
        objects: rows.iter().filter(|o| o.is_object()).count(),
        ..Parsed::default()
    };
    for o in &rows {
        match item_of(o) {
            Ok(r) => p.items.push(r),
            Err(true) => p.retweets += 1,
            Err(false) => {}
        }
    }
    p
}

/// A JSON file: an array of items, an xmaster envelope (`data.tweets`), or
/// any object holding one array of items.
fn json_items(raw: &str) -> Result<Parsed, AppError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| AppError::input(format!("invalid JSON: {e}"), "Pass a JSON array of objects with a \"text\" field, or xmaster --json output"))?;
    fn find_array(v: &Value) -> Option<&Vec<Value>> {
        match v {
            Value::Array(a) => Some(a),
            Value::Object(o) => {
                for k in ["tweets", "posts", "items", "messages", "data", "results"] {
                    if let Some(a) = o.get(k).and_then(find_array) {
                        return Some(a);
                    }
                }
                o.values().find_map(find_array)
            }
            _ => None,
        }
    }
    Ok(find_array(&v).map(|a| items_of(a)).unwrap_or_default())
}

fn jsonl_items(raw: &str) -> Parsed {
    let objs: Vec<Value> = raw.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).collect();
    items_of(&objs)
}

fn csv_items(path: &Path) -> Result<Parsed, AppError> {
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_path(path).map_err(|e| {
        AppError::input(format!("cannot read CSV {}: {e}", path.display()), "Pass a CSV with a header row and a text column")
    })?;
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| AppError::input(format!("CSV header: {e}"), "Add a header row with a text column"))?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    if !TEXT_FIELDS.iter().any(|f| headers.iter().any(|h| h == f)) {
        return Err(AppError::input(
            format!("CSV {} has no text column (looked for {})", path.display(), TEXT_FIELDS.join(", ")),
            "Rename the column holding the writing to \"text\"",
        ));
    }
    let mut objs = Vec::new();
    for rec in rdr.records().flatten() {
        let mut o = serde_json::Map::new();
        for (h, v) in headers.iter().zip(rec.iter()) {
            // Empty cells are absent values, not empty ids or "false".
            if !v.trim().is_empty() {
                o.insert(h.clone(), Value::String(v.to_string()));
            }
        }
        objs.push(Value::Object(o));
    }
    Ok(items_of(&objs))
}

/// The body of an .eml message: headers dropped, quoted replies and the
/// signature (after "-- ") removed, so only what the person wrote remains.
fn eml_item(raw: &str) -> Item {
    let body = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
        .map(|(_, b)| b)
        .unwrap_or(raw);
    let mut lines = Vec::new();
    for line in body.lines() {
        let t = line.trim_end();
        if t == "--" || t == "-- " || (t.starts_with("On ") && t.ends_with("wrote:")) {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        lines.push(t);
    }
    let date = raw
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .find_map(|l| l.strip_prefix("Date:").map(|d| iso_date(d.trim())));
    Item {
        text: lines.join("\n").trim().to_string(),
        meta: ItemMeta {
            date,
            ..ItemMeta::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> NotePool {
        NotePool::new(Vec::new())
    }

    fn texts(items: &[Raw]) -> Vec<&str> {
        items.iter().map(|r| r.item.text.as_str()).collect()
    }

    #[test]
    fn reads_x_archive_and_skips_retweets() {
        let raw = r#"window.YTD.tweets.part0 = [
          {"tweet": {"id_str": "10", "full_text": "Shipping the new build today &amp; it's fast", "favorite_count": "12", "retweet_count": "3", "created_at": "Mon Oct 20 10:00:00 +0000 2026"}},
          {"tweet": {"full_text": "RT @someone: not mine at all here", "favorite_count": "0", "retweet_count": "0"}},
          {"tweet": {"id_str": "11", "full_text": "@friend thanks, that helps a lot", "in_reply_to_status_id_str": "1", "in_reply_to_user_id_str": "99", "display_text_range": ["0", "32"], "favorite_count": 1, "retweet_count": "0"}}
        ]"#;
        let Parsed { items, retweets, .. } = x_archive(raw, None, &mut pool()).unwrap();
        assert_eq!(retweets, 1);
        assert_eq!(texts(&items), ["Shipping the new build today & it's fast", "thanks, that helps a lot"]);
        let m = &items[0].item.meta;
        assert_eq!((m.likes, m.reposts, m.impressions), (Some(12.0), Some(3.0), None));
        assert_eq!(m.date.as_deref(), Some("2026-10-20T10:00:00Z"));
        assert_eq!(m.id.as_deref(), Some("10"));
        assert_eq!(items[1].item.meta.kind, ItemKind::Reply);
        assert_eq!(items[1].item.meta.parent_id.as_deref(), Some("1"));
    }

    #[test]
    fn archive_self_replies_are_thread_parts() {
        let acct = Account {
            id: Some("42".into()),
            username: Some("me".into()),
        };
        let raw = r#"[{"tweet": {"id_str": "2", "full_text": "and the second part of the thread", "in_reply_to_status_id_str": "1", "in_reply_to_user_id_str": "42"}}]"#;
        let items = x_archive(raw, Some(&acct), &mut pool()).unwrap().items;
        assert_eq!(items[0].item.meta.kind, ItemKind::ThreadPart);
    }

    #[test]
    fn display_text_range_strips_only_the_handle_prefix() {
        // "@bob" inside the body is the author's own mention and stays.
        assert_eq!(reply_body("@alice @carol @bob is right about this", Some(14)), "@bob is right about this");
        // A range that doesn't land after handles falls back to the run rule.
        assert_eq!(reply_body("@alice hello there friend", Some(3)), "hello there friend");
        assert_eq!(strip_leading_handles("@alice's idea is good"), "@alice's idea is good");
        assert_eq!(strip_leading_handles("@a @b_2  ok then"), "ok then");
    }

    #[test]
    fn media_and_quote_links_leave_the_text() {
        let raw = r#"[
          {"tweet": {"id_str": "1", "full_text": "Look at this view from the office https://t.co/IMG1", "entities": {"media": [{"url": "https://t.co/IMG1"}]}, "extended_entities": {"media": [{"url": "https://t.co/IMG1"}]}}},
          {"tweet": {"id_str": "2", "full_text": "This is exactly what I meant earlier https://t.co/Q1", "entities": {"urls": [{"url": "https://t.co/Q1", "expanded_url": "https://twitter.com/x/status/5"}]}}},
          {"tweet": {"id_str": "3", "full_text": "Read my post https://t.co/L1 about it", "entities": {"urls": [{"url": "https://t.co/L1", "expanded_url": "https://example.com/a"}]}}}
        ]"#;
        let items = x_archive(raw, None, &mut pool()).unwrap().items;
        assert_eq!(texts(&items), ["Look at this view from the office", "This is exactly what I meant earlier", "Read my post https://t.co/L1 about it"]);
        assert!(items[0].item.meta.media);
        assert_eq!(items[1].item.meta.kind, ItemKind::Quote);
        assert_eq!(items[2].item.meta.kind, ItemKind::Post);
    }

    #[test]
    fn note_tweet_replaces_truncated_text() {
        let full = "This is a long premium post that goes on well past the old limit, and keeps going with more detail at the end.";
        let mut notes = NotePool::new(vec![Note {
            created: Some("2024-03-01T10:00:00.000Z".into()),
            text: full.into(),
        }]);
        let raw = r#"[{"tweet": {"id_str": "7", "full_text": "This is a long premium post that goes on well past the old limit, and… https://t.co/N1", "created_at": "Fri Mar 01 10:00:00 +0000 2024"}}]"#;
        let items = x_archive(raw, None, &mut notes).unwrap().items;
        assert_eq!(items[0].item.text, full);
        assert_eq!(notes.merged, 1);

        // A short post from another day that opens the same way keeps its text.
        let mut notes = NotePool::new(vec![Note {
            created: Some("2024-03-09T10:00:00.000Z".into()),
            text: full.into(),
        }]);
        let items = x_archive(raw, None, &mut notes).unwrap().items;
        assert!(items[0].item.text.ends_with("https://t.co/N1"));
        assert_eq!(notes.merged, 0);
    }

    #[test]
    fn archive_files_are_recognised_by_name() {
        for n in ["tweets.js", "tweet.js", "tweets-part1.js", "tweets-part12.js"] {
            assert!(is_tweets_file(n), "{n}");
        }
        for n in ["tweet-headers.js", "deleted-tweets.js", "note-tweet.js", "community-tweet.js", "tweets-partx.js", "._tweets.js"] {
            assert!(!is_tweets_file(n), "{n}");
        }
        assert!(is_dm_file("direct-messages-group.js"));
        assert!(skip_reason("deleted-tweets-part1.js").is_some());
        assert!(skip_reason("like.js").is_some());
    }

    #[test]
    fn ytd_prefix_is_stripped_up_to_the_first_equals() {
        let v = ytd_json("window.YTD.tweet.part0 = [{\"tweet\": {\"full_text\": \"a = b\"}}];", "t").unwrap();
        assert_eq!(v[0]["tweet"]["full_text"], "a = b");
        assert!(ytd_json("[1, 2]", "t").is_ok());
    }

    #[test]
    fn dates_become_iso() {
        assert_eq!(iso_date("Wed Oct 10 20:19:24 +0000 2018"), "2018-10-10T20:19:24Z");
        assert_eq!(iso_date("Wed Oct 10 20:19:24 +0200 2018"), "2018-10-10T20:19:24+02:00");
        assert_eq!(iso_date("Tue, 21 Oct 2026 10:00:00 +0000"), "2026-10-21T10:00:00Z");
        assert_eq!(iso_date("Tue, 21 Oct 2026"), "2026-10-21");
        assert_eq!(iso_date("2026-10-19T14:49:58.000Z"), "2026-10-19T14:49:58.000Z");
        assert_eq!(iso_date("1539202764"), "2018-10-10T20:19:24Z");
        assert_eq!(iso_date("1539202764000"), "2018-10-10T20:19:24Z");
        assert_eq!(iso_date("last tuesday"), "last tuesday");
    }

    #[test]
    fn html_entities_decode_once() {
        assert_eq!(unescape_html("a &amp; b &lt;3 &#39;x&#39; &#x1F680; &amp;lt;"), "a & b <3 'x' 🚀 &lt;");
        assert_eq!(unescape_html("AT&T & co; fine"), "AT&T & co; fine");
    }

    #[test]
    fn xmaster_rows_keep_meta_and_drop_zero_impressions() {
        let raw = r#"{"status":"success","data":{"tweets":[
          {"id":"1","author":"@me","text":"Your agent needs keys.\n\nIt shouldn't see them.","likes":6,"retweets":1,"impressions":129,"date":"2026-10-19T14:49:58.000Z"},
          {"id":"2","author":"@me","text":"@pal @other Thanks, that's from my own usage","likes":1,"retweets":0,"impressions":0,"date":"2026-10-19T19:44:56.000Z"}
        ]}}"#;
        let items = json_items(raw).unwrap().items;
        assert!(items[0].item.text.contains("\n\n"));
        assert_eq!(items[0].item.meta.impressions, Some(129.0));
        assert_eq!(items[0].item.meta.reposts, Some(1.0));
        assert_eq!(items[1].item.meta.impressions, None);
        assert_eq!(items[1].item.meta.kind, ItemKind::Reply);
        assert_eq!(items[1].item.text, "Thanks, that's from my own usage");
        assert_eq!(items[1].author.as_deref(), Some("@me"));
    }

    #[test]
    fn explicit_kind_and_parent_columns_win() {
        let o: Value = serde_json::json!({"id": 5, "text": "@x not a reply, an opener", "kind": "post"});
        let r = item_of(&o).ok().unwrap();
        assert_eq!(r.item.meta.kind, ItemKind::Post);
        assert_eq!(r.item.text, "@x not a reply, an opener");
        assert_eq!(r.item.meta.id.as_deref(), Some("5"));
        let o: Value = serde_json::json!({"text": "fine by me then", "is_reply": "true", "parent_id": ""});
        let r = item_of(&o).ok().unwrap();
        assert_eq!(r.item.meta.kind, ItemKind::Reply);
        assert_eq!(r.item.meta.parent_id, None);
    }

    #[test]
    fn dms_keep_only_the_owners_messages() {
        let raw = r#"window.YTD.direct_messages.part0 = [
          {"dmConversation": {"conversationId": "1-2", "messages": [
            {"messageCreate": {"senderId": "1", "id": "a", "text": "hey are you around later today", "createdAt": "2024-01-01T10:00:00.000Z", "mediaUrls": []}},
            {"messageCreate": {"senderId": "2", "id": "b", "text": "yes after five works for me", "createdAt": "2024-01-01T10:01:00.000Z", "mediaUrls": []}}
          ]}},
          {"dmConversation": {"conversationId": "1-3", "messages": [
            {"messageCreate": {"senderId": "3", "id": "c", "text": "did you see the thing I sent", "createdAt": "2024-02-01T10:00:00.000Z"}},
            {"messageCreate": {"senderId": "1", "id": "d", "text": "not yet, will look tonight &amp; reply", "createdAt": "2024-02-01T10:05:00.000Z"}}
          ]}}
        ]"#;
        let (p, others) = x_direct_messages(raw, None).unwrap();
        assert_eq!(others, 2);
        assert_eq!(texts(&p.items), ["hey are you around later today", "not yet, will look tonight & reply"]);
        // One conversation can't say who the owner is without account.js.
        let one = r#"[{"dmConversation": {"messages": [{"messageCreate": {"senderId": "1", "text": "hi there you"}}]}}]"#;
        assert!(x_direct_messages(one, None).is_err());
        let acct = Account { id: Some("1".into()), username: None };
        assert_eq!(x_direct_messages(one, Some(&acct)).unwrap().0.items.len(), 1);
    }

    #[test]
    fn eml_keeps_only_what_the_sender_wrote() {
        let raw = "From: a@b.c\nDate: Tue, 21 Oct 2026 09:15:00 +0000\nSubject: hi\n\nHey Sam,\n\nSounds good, see you Friday.\n\nCheers,\nB\n-- \nB, CEO\n> earlier text\n";
        let i = eml_item(raw);
        assert!(i.text.starts_with("Hey Sam,"));
        assert!(i.text.ends_with("B"));
        assert!(!i.text.contains("CEO"));
        assert_eq!(i.meta.date.as_deref(), Some("2026-10-21T09:15:00Z"));
    }

    #[test]
    fn splitting_policy_depends_on_register() {
        let sep = "one post here now\n\nsecond block\n---\nanother post here\n";
        for r in [Register::Social, Register::Email, Register::Message] {
            let s = split_items_for(sep, r);
            assert_eq!(s.len(), 2, "{r:?}");
            assert!(s[0].text.contains("\n\n"), "separators keep blank lines inside an item");
        }
        let blank = "first post words\n\nsecond post words\n";
        assert_eq!(split_items_for(blank, Register::Social).len(), 2);
        let email = "Hi Tom,\n\nThe draft is attached, see page two.\n\nBest,\nB\n";
        let e = split_items_for(email, Register::Email);
        assert_eq!(e.len(), 1);
        assert!(e[0].text.starts_with("Hi Tom,") && e[0].text.ends_with("Best,\nB"));
        assert_eq!(split_items_for(email, Register::Message).len(), 1);
        assert_eq!(split_on_separators(blank).len(), 1);
        assert_eq!(split_items(blank).len(), 2, "the register-blind wrapper keeps the old behaviour");
    }

    /// Casual posts: lowercase start, no final period. `text::clean` alone
    /// would glue each one onto the one before as a "broken paragraph".
    fn casual_posts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("day {i} of shipping small tools and honestly it keeps teaching me stuff 🚀")).collect()
    }

    #[test]
    fn separators_are_hard_boundaries_for_paragraph_repair() {
        let t = clean_plain(&casual_posts(4).join("\n\n---\n\n"), false);
        let items = split_items_for(&t, Register::Social);
        assert_eq!(items.len(), 4, "{t}");
        assert!(items.iter().all(|i| i.text.starts_with("day ") && !i.text.contains("---")), "{items:?}", items = texts_of(&items));
        // An email separated by --- whose next one starts in lowercase.
        let mail = "Hi Tom,\n\nThe draft is attached\n\n---\n\nthanks again for the call, B\n";
        assert_eq!(split_items_for(&clean_plain(mail, false), Register::Email).len(), 2);
    }

    #[test]
    fn short_form_keeps_blank_line_boundaries_long_form_repairs_them() {
        let raw = casual_posts(24).join("\n\n");
        assert_eq!(split_items_for(&clean_plain(&raw, true), Register::Social).len(), 24);
        // Long-form keeps the conversion repair inside each piece.
        let book = "It was a dark and stormy\n\nnight, and the rain fell.\n\n***\n\nmorning came late.\n";
        let t = clean_plain(book, false);
        assert!(t.contains("It was a dark and stormy night, and the rain fell."), "{t}");
        assert!(t.contains("***\n\nmorning came late."), "a scene break is never glued over: {t}");
        // Debris is still cleaned in short-form blocks.
        assert_eq!(clean_plain("it\\'s fine {#id}\n\nok then friend\n", true), "it's fine\n\nok then friend\n");
    }

    fn texts_of(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.text.as_str()).collect()
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn requested_short_form_register_reaches_txt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f = write(tmp.path(), "posts.txt", &casual_posts(24).join("\n\n"));
        let c = read_corpus_as(&f, Some(Register::Social)).unwrap();
        assert_eq!(split_items_for(&c.docs[0].text, Register::Social).len(), 24);
        // Without a register a .txt is prose, and broken paragraphs are repaired.
        let c = read_corpus(&f).unwrap();
        assert_eq!(text::paragraphs(&c.docs[0].text).len(), 1);
    }

    fn ytd(name: &str, v: &Value) -> String {
        format!("window.YTD.{name}.part0 = {v}")
    }

    fn dm_archive(dir: &Path) {
        write(dir, "data/account.js", &ytd("account", &serde_json::json!([{"account": {"username": "owner", "accountId": "4242"}}])));
        let msgs: Vec<Value> = (0..6)
            .flat_map(|i| {
                [
                    serde_json::json!({"messageCreate": {"senderId": "4242", "id": format!("m{i}"), "text": format!("OWN sure, plan {i} sounds good to me"), "createdAt": "2020-01-01T10:00:00.000Z"}}),
                    serde_json::json!({"messageCreate": {"senderId": "77", "id": format!("o{i}"), "text": format!("OTHER what about plan {i} this week"), "createdAt": "2020-01-01T10:01:00.000Z"}}),
                ]
            })
            .collect();
        write(dir, "data/direct-messages.js", &ytd("direct_messages", &serde_json::json!([{"dmConversation": {"messages": msgs}}])));
    }

    #[test]
    fn dm_only_folder_names_the_file_and_the_command_that_works() {
        let tmp = tempfile::tempdir().unwrap();
        dm_archive(tmp.path());
        let Err(AppError::InvalidArguments { message, suggestion }) = read_corpus(tmp.path()) else {
            panic!("a DM-only folder must be an input error");
        };
        assert!(message.contains("direct messages") && message.contains("direct-messages.js"), "{message}");
        let dm = tmp.path().join("data/direct-messages.js");
        assert!(suggestion.contains(&format!("--corpus {}", dm.display())), "{suggestion}");
        // The suggested path works today; a folder with --register message
        // works once profile build passes the register through.
        for c in [read_corpus(&dm).unwrap(), read_corpus_as(tmp.path(), Some(Register::Message)).unwrap()] {
            let all: Vec<&Item> = c.docs.iter().flat_map(|d| &d.items).collect();
            assert_eq!(all.len(), 6);
            assert!(all.iter().all(|i| i.text.starts_with("OWN ")));
        }
    }

    #[test]
    fn skipped_dm_hint_points_at_the_file_not_a_flag_that_reads_posts() {
        let tmp = tempfile::tempdir().unwrap();
        dm_archive(tmp.path());
        let rows: Vec<Value> = (0..5)
            .map(|i| serde_json::json!({"tweet": {"id_str": i.to_string(), "full_text": format!("a public post about plan {i} today")}}))
            .collect();
        write(tmp.path(), "data/tweets.js", &ytd("tweets", &Value::Array(rows)));
        let c = read_corpus(tmp.path()).unwrap();
        let hint = c.report.files_skipped.iter().find(|s| s.contains("direct messages")).unwrap();
        assert!(hint.contains("--corpus") && !hint.contains("--register"), "{hint}");
        // Asking for messages on posts alone says so.
        let f = tmp.path().join("data/tweets.js");
        let c = read_corpus_as(&f, Some(Register::Message)).unwrap();
        assert!(c.report.warnings.iter().any(|w| w.contains("public posts")), "{:?}", c.report.warnings);
    }

    fn xm(rows: &[(usize, &str, &str)]) -> String {
        let rows: Vec<Value> = rows
            .iter()
            .map(|(id, a, t)| serde_json::json!({"id": id.to_string(), "author": a, "text": t, "impressions": 10, "likes": 1, "retweets": 0, "date": "2026-10-01T10:00:00.000Z"}))
            .collect();
        serde_json::json!({"data": {"tweets": rows}}).to_string()
    }

    #[test]
    fn left_out_exports_and_other_authors_become_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rows: Vec<(usize, &str, &str)> = (1..6).map(|i| (i, "@me", "a post of mine about the garden fence")).collect();
        rows.push((9, "@stranger", "someone else's post on the timeline"));
        write(tmp.path(), "good.json", &xm(&rows));
        let big = xm(&rows);
        write(tmp.path(), "big-2025.json", &big[..big.len() / 2]);
        write(tmp.path(), "notes.csv", "title,words\nfoo,12\n");
        write(tmp.path(), "named.json", r#"[{"tweet_text": "the column has another name here"}]"#);
        let c = read_corpus(tmp.path()).unwrap();
        let w = &c.report.warnings;
        for name in ["big-2025.json", "notes.csv", "named.json"] {
            assert!(w.iter().any(|x| x.starts_with(name) && x.contains("left out")), "{name}: {w:?}");
        }
        assert!(w.iter().any(|x| x.contains("kept only @me's posts") && x.contains("1 post(s) by 1 other account")), "{w:?}");
        assert_eq!(c.report.items, 5);
        assert_eq!(c.report.kinds.get("post"), Some(&5));
        let json = serde_json::to_value(&c.report).unwrap();
        assert!(json["warnings"].is_array() && json["skipped"]["other_authors"] == 1, "{json}");
    }

    #[test]
    fn records_without_text_are_named_but_strings_and_dms_are_not_records() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "book/ch1.md", "It was late when she left the house, and the rain had not stopped.\n");
        write(tmp.path(), "book/package.json", r#"{"keywords": ["a", "b"]}"#);
        let c = read_corpus(&tmp.path().join("book")).unwrap();
        assert!(c.report.warnings.is_empty(), "{:?}", c.report.warnings);
        // Only the other side wrote in this conversation: no owner's items,
        // which is not a "missing text field".
        write(tmp.path(), "dm/data/account.js", &ytd("account", &serde_json::json!([{"account": {"accountId": "4242"}}])));
        let msgs = serde_json::json!([{"dmConversation": {"messages": [{"messageCreate": {"senderId": "77", "id": "o1", "text": "are you around later today"}}]}}]);
        let dm = write(tmp.path(), "dm/data/direct-messages.js", &ytd("direct_messages", &msgs));
        let Err(AppError::InvalidArguments { message, .. }) = read_corpus(&dm) else {
            panic!("expected an input error");
        };
        assert!(!message.contains("text field") && message.contains("1 item(s) skipped"), "{message}");
    }

    #[test]
    fn a_folder_where_every_export_failed_reports_why() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "notes.csv", "title,words\nfoo,12\n");
        let Err(AppError::InvalidArguments { message, suggestion }) = read_corpus(tmp.path()) else {
            panic!("expected an input error");
        };
        assert!(message.contains("no text column"), "{message}");
        assert!(suggestion.contains("text"), "{suggestion}");
        write(tmp.path(), "broken.json", "{ not json");
        let Err(AppError::InvalidArguments { message, .. }) = read_corpus(tmp.path()) else {
            panic!("expected an input error");
        };
        assert!(message.contains("broken.json") && message.contains("notes.csv"), "{message}");
    }

    #[test]
    fn lone_tweets_file_reads_its_sibling_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let part = |base: usize, year: u32| -> String {
            let rows: Vec<Value> = (0..3)
                .map(|i| serde_json::json!({"tweet": {"id_str": (base + i).to_string(), "full_text": format!("post {} from {year} about the fence", base + i), "created_at": format!("Wed Oct 10 20:19:24 +0000 {year}")}}))
                .collect();
            ytd("tweets", &Value::Array(rows))
        };
        let f = write(tmp.path(), "data/tweets.js", &part(100, 2024));
        write(tmp.path(), "data/tweets-part1.js", &part(200, 2019));
        write(tmp.path(), "data/tweets-part2.js", &part(300, 2016));
        let c = read_corpus(&f).unwrap();
        assert_eq!(c.report.items, 9);
        assert_eq!(c.report.also_read, ["tweets-part1.js", "tweets-part2.js"]);
        assert_eq!(c.docs.len(), 1, "the parts are one source");
        assert!(c.docs[0].items[0].meta.date.as_deref().unwrap().starts_with("2016"), "time order across parts");
    }
}
