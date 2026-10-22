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

/// Strip common epub->markdown conversion artifacts (cover images, raw SVG/HTML,
/// calibre split anchors) while keeping prose. Conservative: only drops lines
/// that are clearly non-prose.
pub fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() {
            out.push('\n');
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
        out.push_str(line);
        out.push('\n');
    }
    out
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
