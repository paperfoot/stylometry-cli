//! Text ingestion: corpus reading, light cleaning, tokenization, chunking,
//! and character n-gram extraction.
//!
//! All features downstream are derived from two views of a text:
//!   - word tokens (lowercased) -> most-frequent-word features
//!   - character trigrams       -> character n-gram features (PAN's second
//!     strongest authorship signal, robust to topic)

use std::path::Path;

use unicode_segmentation::UnicodeSegmentation;
use walkdir::WalkDir;

use crate::error::AppError;

/// Read a corpus from a file or a directory (recursively concatenating
/// `.md`, `.txt`, and `.markdown` files). Applies light cleaning to strip
/// epub/markdown conversion cruft that would pollute the fingerprint.
pub fn read_corpus(path: &Path) -> Result<String, AppError> {
    if path.is_file() {
        let raw = std::fs::read_to_string(path)?;
        return Ok(clean(&raw));
    }
    if path.is_dir() {
        let mut buf = String::new();
        let mut files = 0usize;
        // Sorted traversal so multi-file profile chunk boundaries are
        // reproducible regardless of filesystem iteration order.
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
                    buf.push_str(&clean(&raw));
                    buf.push('\n');
                    files += 1;
                }
            }
        }
        if files == 0 {
            return Err(AppError::InvalidInput(format!(
                "no .md/.txt files found under {}",
                path.display()
            )));
        }
        return Ok(buf);
    }
    Err(AppError::InvalidInput(format!(
        "corpus path does not exist: {}",
        path.display()
    )))
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

/// Lowercased word tokens (Unicode word segmentation). Keeps intra-word
/// apostrophes that `unicode_words` already folds in (e.g. "don't").
pub fn word_tokens(text: &str) -> Vec<String> {
    text.unicode_words().map(|w| w.to_lowercase()).collect()
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

/// Split a text into chunks of approximately `chunk_words` words each, where
/// "word" means a Unicode word (same segmentation as the features), not a
/// whitespace token — punctuation-only tokens don't count toward chunk size.
/// Chunk boundaries fall on whitespace so the raw text (punctuation included)
/// is preserved for trigram extraction. The trailing remainder is merged into
/// the previous chunk if it is shorter than half a chunk, so we never produce
/// a tiny, high-variance final sample.
pub fn chunk_by_words(text: &str, chunk_words: usize) -> Vec<String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let chunk_words = chunk_words.max(50);
    let mut chunks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut cur_words = 0usize;
    for tok in tokens {
        cur.push(tok);
        cur_words += tok.unicode_words().count();
        if cur_words >= chunk_words {
            chunks.push(cur.join(" "));
            cur.clear();
            cur_words = 0;
        }
    }
    if !cur.is_empty() {
        if !chunks.is_empty() && cur_words < chunk_words / 2 {
            let prev = chunks.last_mut().unwrap();
            prev.push(' ');
            prev.push_str(&cur.join(" "));
        } else {
            chunks.push(cur.join(" "));
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_counts_unicode_words_not_whitespace_tokens() {
        // 120 words + 120 punctuation-only tokens. With whitespace counting
        // this would split at token 100; with word counting the first chunk
        // must contain >= 100 actual words.
        let text = "word — ".repeat(120);
        let chunks = chunk_by_words(&text, 100);
        assert!(!chunks.is_empty());
        assert!(
            count_words(&chunks[0]) >= 100,
            "first chunk has {} words, expected >= 100",
            count_words(&chunks[0])
        );
    }

    #[test]
    fn tiny_trailing_chunk_merges_into_predecessor() {
        let text = "alpha beta ".repeat(130); // 260 words at chunk 100 -> 2 full + 60 tail
        let chunks = chunk_by_words(&text, 100);
        assert_eq!(chunks.len(), 3); // 60 >= 100/2, kept separate
        let text = "alpha beta ".repeat(110); // 220 words -> 20-word tail merges
        let chunks = chunk_by_words(&text, 100);
        assert_eq!(chunks.len(), 2);
        assert!(count_words(chunks.last().unwrap()) >= 100);
    }

    #[test]
    fn count_words_ignores_punctuation_tokens() {
        assert_eq!(count_words("hello — world ... !"), 2);
    }
}
