//! Text ingestion: corpus reading, light cleaning, tokenization, paragraph
//! and sentence segmentation, dialogue detection, and character n-grams.
//!
//! All features downstream are derived from a few views of a text:
//!   - word tokens (lowercased, curly apostrophes folded to ') -> vocabulary
//!   - character trigrams -> character n-gram features (robust to topic)
//!   - paragraphs and sentences -> rhythm, openers, punctuation, dialogue

use std::path::Path;

use unicode_segmentation::UnicodeSegmentation;
use walkdir::WalkDir;

use crate::error::AppError;

/// One source text: a file of the corpus, or an inline/stdin query.
/// Structured sources (an X archive, JSON/CSV exports, emails) carry their
/// writing as `items` instead of `text`.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub name: String,
    pub text: String,
    pub items: Vec<Item>,
}

/// One short-form piece (a post, message, email) with its metadata.
#[derive(Debug, Clone)]
pub struct Item {
    pub text: String,
    pub meta: crate::engine::profile::ItemMeta,
}

/// Read a corpus from a file or a directory (recursively collecting `.md`,
/// `.txt`, `.markdown` and `.text` files, sorted by path so chunking is
/// reproducible). Each file stays a separate `Doc`: chunks never straddle
/// files, and calibration can hold out whole works.
pub fn read_docs(path: &Path) -> Result<Vec<Doc>, AppError> {
    if path.is_file() {
        let raw = std::fs::read_to_string(path)?;
        return Ok(vec![Doc {
            name: file_label(path),
            text: clean(&raw),
            items: Vec::new(),
        }]);
    }
    if path.is_dir() {
        let mut docs = Vec::new();
        for entry in WalkDir::new(path)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(ext.as_str(), "md" | "txt" | "markdown" | "text") {
                if let Ok(raw) = std::fs::read_to_string(p) {
                    let name = p
                        .strip_prefix(path)
                        .map(|r| r.display().to_string())
                        .unwrap_or_else(|_| file_label(p));
                    docs.push(Doc {
                        name,
                        text: clean(&raw),
                        items: Vec::new(),
                    });
                }
            }
        }
        if docs.is_empty() {
            return Err(AppError::InvalidInput(format!(
                "no .md/.txt files found under {}",
                path.display()
            )));
        }
        return Ok(docs);
    }
    Err(AppError::InvalidInput(format!(
        "path does not exist: {}",
        path.display()
    )))
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// What `clean` removed: conversion debris that, left in, misreports a voice
/// (escaped apostrophes hide contractions; fence lines inflate colons;
/// mid-sentence paragraph breaks fake one-sentence paragraphs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Hygiene {
    pub escapes: usize,
    pub fences: usize,
    pub attributes: usize,
    pub broken_paragraphs: usize,
}

impl Hygiene {
    pub fn total(&self) -> usize {
        self.escapes + self.fences + self.attributes + self.broken_paragraphs
    }
}

/// Strip epub/markdown conversion artifacts (pandoc/calibre output: cover
/// images, raw SVG/HTML, `:::` fenced divs, `{#id .class}` attributes,
/// `[text]{.italic}` spans, empty `[]{#anchor}` anchors, backslash escapes)
/// while keeping prose, and rejoin paragraphs that a conversion split
/// mid-sentence. Conservative: only clearly non-prose is dropped.
pub fn clean(raw: &str) -> String {
    let (t, h) = clean_report(raw);
    if let Ok(mut acc) = HYGIENE.lock() {
        acc.escapes += h.escapes;
        acc.fences += h.fences;
        acc.attributes += h.attributes;
        acc.broken_paragraphs += h.broken_paragraphs;
    }
    t
}

/// Debris removed by every `clean` call in this process, so `profile build`
/// can report it without each importer threading counts through.
static HYGIENE: std::sync::Mutex<Hygiene> = std::sync::Mutex::new(Hygiene {
    escapes: 0,
    fences: 0,
    attributes: 0,
    broken_paragraphs: 0,
});

/// Take (and reset) the debris counts accumulated by `clean`.
pub fn take_hygiene() -> Hygiene {
    HYGIENE.lock().map(|mut h| std::mem::take(&mut *h)).unwrap_or_default()
}

/// `clean`, plus counts of what was removed.
pub fn clean_report(raw: &str) -> (String, Hygiene) {
    let mut h = Hygiene::default();
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() {
            lines.push(String::new());
            continue;
        }
        if t.starts_with(":::") {
            h.fences += 1;
            continue;
        }
        let is_cruft = t.starts_with("![")            // markdown image
            || t.starts_with("<svg")
            || t.starts_with("</svg")
            || t.contains("{=html}")
            || t.contains("xlink:")
            || t.contains("xmlns")
            || (t.starts_with("[]{#") && t.ends_with('}')) // calibre anchor
            || (t.starts_with('<') && t.ends_with('>') && !t.contains(' ')); // bare tag
        if is_cruft {
            continue;
        }
        let fixed = strip_markup(line, &mut h);
        if fixed.trim().is_empty() {
            continue;
        }
        lines.push(fixed);
    }
    let joined = lines.join("\n");
    let (merged, broken) = rejoin_broken_paragraphs(&joined);
    h.broken_paragraphs = broken;
    (merged, h)
}

/// Unwrap `[text]{.italic}` → `*text*` (`.bold` → `**text**`, other spans →
/// text), drop remaining `{#id .class}` attribute blocks and empty `[]`
/// anchors, and undo backslash escapes (`\'` → `'`).
fn strip_markup(line: &str, h: &mut Hygiene) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // [text]{attrs}
        if c == '[' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == ']') {
                if chars.get(close + 1) == Some(&'{') {
                    if let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == '}') {
                        let inner: String = chars[i + 1..close].iter().collect();
                        let attrs: String = chars[close + 2..end].iter().collect();
                        h.attributes += 1;
                        let wrap = if attrs.contains("bold") || attrs.contains("strong") {
                            "**"
                        } else if attrs.contains("italic") || attrs.contains("emph") {
                            "*"
                        } else {
                            ""
                        };
                        if !inner.is_empty() {
                            out.push_str(wrap);
                            out.push_str(&strip_markup(&inner, h));
                            out.push_str(wrap);
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        // {#id .class} attribute blocks
        if c == '{' && matches!(chars.get(i + 1), Some('#') | Some('.')) {
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '}') {
                h.attributes += 1;
                i = end + 1;
                continue;
            }
        }
        // backslash escapes
        if c == '\\' {
            if let Some(&n) = chars.get(i + 1) {
                if "'\"*_[]().!-#`".contains(n) {
                    h.escapes += 1;
                    out.push(n);
                    i += 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Rejoin a paragraph to the previous one when the previous lacks terminal
/// punctuation and this one starts in lowercase: a conversion's page or
/// file break, not the author's paragraphing. Returns the text and the
/// number of joins.
fn rejoin_broken_paragraphs(text: &str) -> (String, usize) {
    let mut blocks: Vec<String> = Vec::new();
    let mut joins = 0;
    for block in text.split("\n\n") {
        let b = block.trim_matches('\n');
        if b.trim().is_empty() {
            continue;
        }
        let starts_lower = b
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_lowercase());
        let prev_open = blocks.last().is_some_and(|p| {
            let t = p.trim_end().trim_end_matches(['*', '_']);
            !t.ends_with(['.', '!', '?', '\u{2026}', ':', ';', '"', '\u{201D}', '\u{2019}', '\'', ')'])
                && !t.trim_start().starts_with('#')
        });
        if starts_lower && prev_open {
            let last = blocks.last_mut().unwrap();
            last.push(' ');
            last.push_str(b.trim());
            joins += 1;
        } else {
            blocks.push(b.to_string());
        }
    }
    (blocks.join("\n\n") + "\n", joins)
}

/// Fold typographic apostrophes so "don’t" and "don't" are one word.
#[inline]
fn fold_apostrophes(w: &str) -> String {
    w.replace(['\u{2019}', '\u{02BC}'], "'")
}

/// Replace web debris that would pollute word and trigram counts: links
/// become the token `URL`, @handles become `@USER`. Posts are full of both,
/// and "https", "t.co" or a friend's handle say nothing about voice. The
/// rate of links and mentions is measured separately, from the raw text.
pub fn normalize_web(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("http") && !text.contains("www.") && !text.contains('@') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for tok in text.split_inclusive(char::is_whitespace) {
        let core = tok.trim_end();
        let tail = &tok[core.len()..];
        let bare = core.trim_start_matches(['(', '[', '"', '\u{201C}']);
        let lead = &core[..core.len() - bare.len()];
        if bare.starts_with("http://") || bare.starts_with("https://") || bare.starts_with("www.") {
            out.push_str(lead);
            out.push_str("URL");
        } else if let Some(h) = bare.strip_prefix('@').filter(|h| h.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')) {
            let end = h.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(h.len());
            out.push_str(lead);
            out.push_str("@USER");
            out.push_str(&h[end..]);
        } else {
            out.push_str(core);
        }
        out.push_str(tail);
    }
    std::borrow::Cow::Owned(out)
}

/// Lowercased word tokens (Unicode word segmentation). Keeps intra-word
/// apostrophes (e.g. "don't"), with curly apostrophes folded to '. Links and
/// handles are normalised first (see `normalize_web`).
pub fn word_tokens(text: &str) -> Vec<String> {
    normalize_web(text)
        .unicode_words()
        .map(|w| fold_apostrophes(&w.to_lowercase()))
        .collect()
}

/// Word tokens with their original case (apostrophes folded). Used where
/// capitalization matters: proper-noun detection.
pub fn word_tokens_cased(text: &str) -> Vec<String> {
    text.unicode_words().map(fold_apostrophes).collect()
}

/// Word count under the same Unicode segmentation the features use. This is
/// the single definition of "word" for chunk sizing, length gates, and length
/// warnings — do not mix with `split_whitespace` counts.
pub fn count_words(text: &str) -> usize {
    text.unicode_words().count()
}

/// Character trigrams over a normalized stream: lowercased, runs of whitespace
/// collapsed to a single space. Punctuation is retained -- it carries style.
pub fn char_trigrams(text: &str) -> Vec<String> {
    let text = normalize_web(text);
    let normalized: String = {
        let mut s = String::with_capacity(text.len());
        let mut prev_ws = false;
        for c in text.chars() {
            if c.is_whitespace() {
                if !prev_ws {
                    s.push(' ');
                }
                prev_ws = true;
            } else {
                s.extend(c.to_lowercase());
                prev_ws = false;
            }
        }
        s
    };
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Prose paragraphs: blocks separated by blank lines, internal line breaks
/// joined with spaces. Headings are dropped: markdown `#` lines, and short
/// blocks (under 5 words) with no sentence-final punctuation ("CHAPTER IV",
/// "Part Two"), since they are not prose and would distort rhythm stats.
pub fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut flush = |cur: &mut Vec<&str>, out: &mut Vec<String>| {
        if cur.is_empty() {
            return;
        }
        let joined = cur.join(" ");
        cur.clear();
        let t = joined.trim();
        if t.is_empty() || t.starts_with('#') {
            return;
        }
        let ends_like_prose = t
            .trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', '*', '_'])
            .ends_with(['.', '!', '?', ':', ';', ',', '\u{2026}', '\u{2014}', '-']);
        if count_words(t) < 5 && !ends_like_prose {
            return;
        }
        out.push(t.to_string());
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut cur, &mut out);
        } else {
            cur.push(line.trim());
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Abbreviations whose trailing period does not end a sentence.
const ABBREVIATIONS: [&str; 26] = [
    "mr", "mrs", "ms", "dr", "st", "mt", "jr", "sr", "prof", "capt", "col", "gen", "lt",
    "sgt", "rev", "hon", "no", "vs", "etc", "e.g", "i.e", "cf", "fig", "vol", "approx", "esq",
];

/// Split a paragraph into sentences. UAX#29 sentence boundaries, then two
/// repairs: a split after a known abbreviation or a single-letter initial is
/// undone, and a "sentence" starting in lowercase is glued back to its
/// predecessor (`"Why?" he asked.` is one sentence, not two).
pub fn sentences(paragraph: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in paragraph.unicode_sentences() {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let glue = match out.last() {
            None => false,
            Some(prev) => ends_with_abbreviation(prev) || starts_lowercase(s),
        };
        if glue {
            let prev = out.last_mut().unwrap();
            prev.push(' ');
            prev.push_str(s);
        } else {
            out.push(s.to_string());
        }
    }
    out
}

/// Sentences for a register: long-form uses `sentences`; short-form (posts,
/// messages) splits on every line break and on terminal punctuation followed
/// by whitespace whatever the next letter's case, because casual writers
/// type in lowercase ("i think so. tbh it works.") and UAX#29 would glue
/// those into one sentence. The `"Why?" he asked` repair is kept: a split
/// right after a closing quote is undone when the next word is lowercase.
pub fn sentences_for(paragraph: &str, short_form: bool) -> Vec<String> {
    if !short_form {
        return sentences(paragraph);
    }
    let mut out: Vec<String> = Vec::new();
    for line in paragraph.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut cur = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            cur.push(c);
            if matches!(c, '.' | '!' | '?' | '\u{2026}') {
                // absorb runs of terminal punctuation and closing quotes
                while i + 1 < chars.len() && matches!(chars[i + 1], '.' | '!' | '?' | '\u{2026}' | '"' | '\u{201D}' | '\u{2019}' | ')') {
                    i += 1;
                    cur.push(chars[i]);
                }
                let at_ws = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
                let after_quote = cur.ends_with(['"', '\u{201D}']);
                let next_lower = chars[i + 1..]
                    .iter()
                    .find(|ch| !ch.is_whitespace())
                    .is_some_and(|ch| ch.is_lowercase());
                if at_ws && !(after_quote && next_lower) && !ends_with_abbreviation(&cur) {
                    let t = cur.trim().to_string();
                    if !t.is_empty() {
                        out.push(t);
                    }
                    cur.clear();
                }
            }
            i += 1;
        }
        let t = cur.trim().to_string();
        if !t.is_empty() {
            out.push(t);
        }
    }
    out
}

fn ends_with_abbreviation(s: &str) -> bool {
    let t = s.trim_end();
    if !t.ends_with('.') {
        return false;
    }
    let last = t[..t.len() - 1]
        .rsplit(|c: char| c.is_whitespace() || c == '(' || c == '"' || c == '\u{201C}')
        .next()
        .unwrap_or("");
    let lw = last.to_lowercase();
    let single_initial = last.chars().count() == 1 && last.chars().all(|c| c.is_uppercase());
    single_initial || ABBREVIATIONS.contains(&lw.as_str())
}

fn starts_lowercase(s: &str) -> bool {
    s.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// The quoted (dialogue) spans of a paragraph. Handles “curly doubles”,
/// straight "doubles", and ‘curly singles’ (British convention), telling an
/// opening ‘ from an apostrophe by what precedes it. Quote state resets at
/// the paragraph end, matching the convention that a speech continuing into
/// the next paragraph omits its closing mark.
pub fn quoted_spans(paragraph: &str) -> Vec<String> {
    let chars: Vec<char> = paragraph.chars().collect();
    let mut spans = Vec::new();
    let mut open: Option<char> = None;
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let prev = if i == 0 { None } else { Some(chars[i - 1]) };
        let next = chars.get(i + 1).copied();
        let at_word_start = prev.is_none_or(|p| p.is_whitespace() || "([{\u{2014}-".contains(p));
        let at_word_end = next.is_none_or(|n| !n.is_alphanumeric());
        match open {
            None => {
                let opens = c == '\u{201C}'
                    || c == '"'
                    || (c == '\u{2018}' && at_word_start)
                    || (c == '\'' && at_word_start && next.is_some_and(|n| n.is_alphabetic()));
                if opens {
                    open = Some(c);
                }
            }
            Some(o) => {
                let closes = match o {
                    '\u{201C}' => c == '\u{201D}',
                    '"' => c == '"',
                    '\u{2018}' => c == '\u{2019}' && at_word_end,
                    _ => c == '\'' && at_word_end && prev.is_some_and(|p| !p.is_whitespace()),
                };
                if closes {
                    spans.push(std::mem::take(&mut cur));
                    open = None;
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if open.is_some() && !cur.trim().is_empty() {
        spans.push(cur);
    }
    spans
}

/// Words inside dialogue quotes in a paragraph.
pub fn dialogue_words(paragraph: &str) -> usize {
    quoted_spans(paragraph).iter().map(|s| count_words(s)).sum()
}

/// Paragraph-aligned chunks of roughly `target_words` words: paragraphs are
/// accumulated until the target is reached; a paragraph longer than twice
/// the target is split on sentence boundaries. A trailing remainder under
/// half the target is merged into the previous chunk. Chunks are therefore
/// coherent passages (usable as exemplars), never cut mid-sentence.
pub fn chunk_paragraphs(paragraphs: &[String], target_words: usize) -> Vec<Vec<String>> {
    let target = target_words.max(50);
    // Pieces: whole paragraphs, or sentence groups of an over-long paragraph.
    let mut pieces: Vec<(String, usize)> = Vec::new();
    for p in paragraphs {
        let n = count_words(p);
        if n <= 2 * target {
            pieces.push((p.clone(), n));
            continue;
        }
        let mut buf = String::new();
        let mut bn = 0usize;
        for s in sentences(p) {
            let sn = count_words(&s);
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(&s);
            bn += sn;
            if bn >= target {
                pieces.push((std::mem::take(&mut buf), bn));
                bn = 0;
            }
        }
        if !buf.is_empty() {
            pieces.push((buf, bn));
        }
    }

    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut cur_words = 0usize;
    for (text, n) in pieces {
        cur.push(text);
        cur_words += n;
        if cur_words >= target {
            chunks.push(std::mem::take(&mut cur));
            cur_words = 0;
        }
    }
    if !cur.is_empty() {
        if !chunks.is_empty() && cur_words < target / 2 {
            chunks.last_mut().unwrap().extend(cur);
        } else {
            chunks.push(cur);
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_words_ignores_punctuation_tokens() {
        assert_eq!(count_words("hello — world ... !"), 2);
    }

    #[test]
    fn clean_strips_pandoc_debris_and_rejoins_broken_paragraphs() {
        let raw = "::: {#split_001 .calibre2}\n# CHAPTER 1 {#index_split .calibre3}\n\nIt wasn\\'t [the Guide]{.italic} at all, and\n\nthen it was. []{#anchor}Fine.\n:::\n";
        let (t, h) = clean_report(raw);
        assert!(t.contains("It wasn't *the Guide* at all, and then it was. Fine."), "{t}");
        assert!(t.contains("# CHAPTER 1\n") || t.contains("# CHAPTER 1 \n") || t.contains("# CHAPTER 1"), "{t}");
        assert!(!t.contains(":::") && !t.contains("calibre"));
        assert_eq!(h.escapes, 1);
        assert_eq!(h.fences, 2);
        assert_eq!(h.broken_paragraphs, 1);
    }

    #[test]
    fn links_and_handles_are_normalised() {
        assert_eq!(
            word_tokens("thanks @sam_k! see https://t.co/x1 (www.a.io)"),
            vec!["thanks", "user", "see", "url", "url"]
        );
        assert_eq!(normalize_web("no web here"), "no web here");
    }

    #[test]
    fn curly_apostrophes_fold_into_one_word() {
        assert_eq!(word_tokens("Don’t"), vec!["don't".to_string()]);
    }

    #[test]
    fn paragraphs_drop_headings_and_join_lines() {
        let t = "# Title\n\nCHAPTER ONE\n\nIt was a dark\nand stormy night.\n\nShe left.";
        let p = paragraphs(t);
        assert_eq!(p, vec!["It was a dark and stormy night.", "She left."]);
    }

    #[test]
    fn sentences_repair_abbreviations_and_dialogue_tags() {
        let s = sentences("Mr. Smith arrived. \"Why?\" he asked. Nobody knew.");
        assert_eq!(s.len(), 3, "{s:?}");
        assert!(s[0].starts_with("Mr. Smith"));
        assert!(s[1].contains("he asked"));
    }

    #[test]
    fn short_form_sentences_split_lowercase_and_lines() {
        let s = sentences_for("i think so. tbh it works. ok.\nnew line here", true);
        assert_eq!(s, vec!["i think so.", "tbh it works.", "ok.", "new line here"]);
        let q = sentences_for("\"why?\" he asked. fine.", true);
        assert_eq!(q.len(), 2, "{q:?}");
    }

    #[test]
    fn dialogue_detects_curly_straight_and_single_quotes() {
        assert_eq!(dialogue_words("“Come here,” she said. “Now.”"), 3);
        assert_eq!(dialogue_words("\"Come here,\" she said."), 2);
        assert_eq!(dialogue_words("‘Don’t go,’ he said."), 2);
        assert_eq!(dialogue_words("It wasn't his fault, he thought."), 0);
    }

    #[test]
    fn chunks_are_paragraph_aligned_and_merge_small_tails() {
        let para = "word ".repeat(60).trim().to_string() + ".";
        let paras: Vec<String> = (0..7).map(|_| para.clone()).collect(); // 420 words
        let chunks = chunk_paragraphs(&paras, 100);
        // 60+60 = 120 >= 100 -> chunks of 2 paragraphs; 7th (60 >= 50) stands alone.
        assert_eq!(chunks.len(), 4);
        assert!(chunks.iter().all(|c| c.iter().all(|p| p.ends_with('.'))));
    }

    #[test]
    fn long_paragraph_splits_on_sentences() {
        let sentence = "One two three four five six seven eight nine ten. ";
        let para = sentence.repeat(30); // 300 words in one paragraph
        let chunks = chunk_paragraphs(&[para], 100);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c[0].trim_end().ends_with('.')));
    }
}
