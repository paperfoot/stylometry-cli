//! Prose texture, judged by JEV passage by passage: how rough or polished
//! the writing is, whether passages end on crafted lines, how figurative it
//! is, and which kinds of roughness it has. Measured on the author's own
//! passages to give bands, then on a draft, so every judgment is relative
//! to the writer (a clean essayist and a loose poster differ a lot).
//!
//! Why: blind readers told imitations from the real writer by polish that
//! counts cannot see (aphorisms, kickers, no rough edges). On 2026-10-22
//! the `rough` question separated genuine from imitation at AUC 0.90-1.00
//! for novels, essays and X posts, where the stylometric score managed
//! 0.00-0.78. Used as a target alone it is gamed (agents add filler
//! hedges), so the kinds are bounded on both sides.

use crate::engine::text;

/// Which side of the author's band is a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The draft has too little of it (below the band).
    Low,
    /// The draft has too much of it (above the band).
    High,
}

pub struct TextureKey {
    /// Question id in `data/jev_texture.json`.
    pub key: &'static str,
    pub label: &'static str,
    pub side: Side,
    /// Gate keys decide `texture.pass`; the rest are advice.
    pub gate: bool,
    /// What to do when the draft is off on this side.
    pub advice: &'static str,
}

/// The measured keys, gate keys first. Gate: the five that separated
/// genuine from imitation and resisted gaming in rounds 3-4.
pub const KEYS: [TextureKey; 9] = [
    TextureKey { key: "rough", label: "rough edges", side: Side::Low, gate: true,
        advice: "loosen it in the author's own ways (the kinds below), not with filler" },
    TextureKey { key: "polish", label: "polish (0 plain to 3 highly crafted)", side: Side::High, gate: true,
        advice: "write plainer: say what happens and what is thought, directly" },
    TextureKey { key: "button", label: "crafted closing lines", side: Side::High, gate: true,
        advice: "end passages on ordinary sentences, not kickers, reveals or callbacks" },
    TextureKey { key: "figurative", label: "fresh figures of speech", side: Side::High, gate: true,
        advice: "cut inventive metaphors and similes" },
    TextureKey { key: "hedge", label: "filler hedges (I guess, kind of)", side: Side::High, gate: true,
        advice: "cut filler hedges; the author rarely uses them" },
    TextureKey { key: "overexplain", label: "over-explaining", side: Side::Low, gate: false,
        advice: "spell out reasons, logistics and feelings a little more fully, as the author does" },
    TextureKey { key: "repeat", label: "repeating a noun instead of a pronoun", side: Side::Low, gate: false,
        advice: "repeat the key noun now and then where the author would" },
    TextureKey { key: "splice", label: "loosely joined, run-on sentences", side: Side::Low, gate: false,
        advice: "let a few sentences run on, joined loosely with commas or \"and\"" },
    TextureKey { key: "oneliner", label: "dramatic one-line paragraphs", side: Side::High, gate: false,
        advice: "fold some standalone beats back into their paragraphs" },
];

/// About this many words per judged passage (paragraph-aligned), as in the
/// eval that set the bands.
pub const PASSAGE_WORDS: usize = 150;
/// Shorter trailing groups are dropped; shorter whole texts are one passage.
pub const MIN_PASSAGE_WORDS: usize = 60;
/// Short-form batches are judged this many posts at a time: single posts
/// are too short to judge (rough AUC 0.79 per post vs 0.92 per batch).
pub const POSTS_PER_PASSAGE: usize = 4;

/// One judged unit of text, with the 1-based line range it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// The text a passage is judged on: typography evened out (texture is not
/// about marks; `check` holds those to the author separately), links and
/// footnote markers dropped, hard wraps joined.
pub fn normalize(t: &str) -> String {
    let t = t
        .replace(['\u{201C}', '\u{201D}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace('\u{2026}', "...");
    let mut out = String::with_capacity(t.len());
    for word in t.split_inclusive(char::is_whitespace) {
        let w = word.trim_end();
        if w.starts_with("http://") || w.starts_with("https://") {
            out.push_str(&word[w.len()..]);
            continue;
        }
        out.push_str(word);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a draft or an author's text into judged passages: long-form text
/// into paragraph-aligned groups of about `PASSAGE_WORDS`, short-form text
/// (posts separated by `---` lines) into batches of `POSTS_PER_PASSAGE`.
pub fn passages(raw: &str, short_form: bool) -> Vec<Passage> {
    // Blocks: blank-line separated runs of lines, with their line ranges;
    // `---` lines separate posts and scene breaks, and belong to no block.
    let mut blocks: Vec<(String, usize, usize, bool)> = Vec::new(); // (text, start, end, starts_post)
    let mut cur: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut new_post = true;
    let lines: Vec<&str> = raw.lines().collect();
    let flush = |cur: &mut Vec<&str>, start: usize, end: usize, new_post: &mut bool, blocks: &mut Vec<(String, usize, usize, bool)>| {
        if !cur.is_empty() {
            blocks.push((cur.join("\n"), start, end, *new_post));
            cur.clear();
            *new_post = false;
        }
    };
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let is_break = t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | '*' | '•' | '#' | '~' | '=' | '_' | '·' | ' '));
        if t.is_empty() || is_break || text::is_heading(t) {
            flush(&mut cur, start, i, &mut new_post, &mut blocks);
            if is_break {
                new_post = true;
            }
            continue;
        }
        if cur.is_empty() {
            start = i + 1;
        }
        cur.push(line);
    }
    flush(&mut cur, start, lines.len(), &mut new_post, &mut blocks);

    let words = |s: &str| s.split_whitespace().count();
    let mut out = Vec::new();
    let mut group: Vec<&(String, usize, usize, bool)> = Vec::new();
    let emit = |group: &mut Vec<&(String, usize, usize, bool)>, out: &mut Vec<Passage>| {
        if let (Some(first), Some(last)) = (group.first(), group.last()) {
            let text = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
            out.push(Passage { text, start_line: first.1, end_line: last.2 });
        }
        group.clear();
    };
    if short_form {
        let mut posts = 0;
        for b in &blocks {
            if b.3 {
                if posts == POSTS_PER_PASSAGE {
                    emit(&mut group, &mut out);
                    posts = 0;
                }
                posts += 1;
            }
            group.push(b);
        }
        // A short trailing batch joins the previous one.
        if !group.is_empty() && posts < POSTS_PER_PASSAGE / 2 && !out.is_empty() {
            let last = out.pop().unwrap();
            let tail = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
            out.push(Passage { text: format!("{}\n\n{}", last.text, tail), start_line: last.start_line, end_line: group.last().unwrap().2 });
            group.clear();
        }
        emit(&mut group, &mut out);
        return out;
    }
    let mut n = 0;
    for b in &blocks {
        group.push(b);
        n += words(&b.0);
        if n >= PASSAGE_WORDS {
            emit(&mut group, &mut out);
            n = 0;
        }
    }
    if n >= MIN_PASSAGE_WORDS || out.is_empty() {
        emit(&mut group, &mut out);
    } else if let (Some(last), false) = (out.last_mut(), group.is_empty()) {
        // A short tail joins the passage before it.
        let tail = group.iter().map(|b| normalize(&b.0)).collect::<Vec<_>>().join("\n\n");
        last.text = format!("{}\n\n{}", last.text, tail);
        last.end_line = group.last().unwrap().2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(n: usize, w: &str) -> String {
        vec![w; n].join(" ") + "."
    }

    #[test]
    fn long_form_groups_paragraphs_to_about_150_words_with_line_ranges() {
        let text = [para(80, "a"), para(80, "b"), para(80, "c"), para(30, "d")].join("\n\n");
        let p = passages(&text, false);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!((p[0].start_line, p[0].end_line), (1, 3));
        assert_eq!((p[1].start_line, p[1].end_line), (5, 7), "the 30-word tail joins the last passage");
        assert!(p[1].text.contains("d d"));
    }

    #[test]
    fn short_form_judges_posts_four_at_a_time_and_scene_breaks_split_nothing_else() {
        let posts: Vec<String> = (0..9).map(|i| format!("post {i} says something plain here")).collect();
        let p = passages(&posts.join("\n---\n"), true);
        assert_eq!(p.len(), 2, "9 posts: 4 + 5 (a lone trailing post joins the batch before)");
        assert!(p[0].text.contains("post 0") && p[0].text.contains("post 3") && !p[0].text.contains("post 4"));
        assert!(p[1].text.contains("post 8"));
    }

    #[test]
    fn normalize_evens_out_marks_and_drops_links() {
        let n = normalize("\u{201C}It\u{2019}s fine\u{2026}\u{201D} see https://t.co/x1 now\nand then");
        assert_eq!(n, "\"It's fine...\" see now and then");
    }
}
