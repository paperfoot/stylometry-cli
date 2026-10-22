//! Background authors: a curated set of public-domain books (Project
//! Gutenberg, 1850-1930, British and American, fiction, non-fiction, essays
//! and letters) that stand in for "writers in general".
//!
//! They do two jobs. As imposters they are the negatives calibration fits
//! against: without many varied authors, "not the three neighbours I was
//! calibrated on" passes for "same author". And as part of the reference set
//! they make Delta's z-scores mean "unusual for writers", not "unusual among
//! the few profiles you happen to have".

use std::path::PathBuf;

use crate::engine::profile::{Profile, ProfileKind};
use crate::engine::store;
use crate::engine::text::{self, Doc};
use crate::error::AppError;

const CATALOG: &str = include_str!("data/background.tsv");

/// Words kept per book: a contiguous excerpt. Enough for every calibration
/// band (7 passages of 4,000 words) while keeping 40 profiles fast to load.
pub const MAX_WORDS: usize = 30_000;

/// Env var naming a local folder of `<id>.txt` Gutenberg files to use
/// instead of the network (offline installs and tests).
pub const SOURCE_ENV: &str = "STYLOMETRY_BACKGROUND_SOURCE";

/// One catalog entry.
#[derive(Debug, Clone)]
pub struct Work {
    pub id: u32,
    pub slug: String,
    pub author: String,
    pub title: String,
    pub year: u16,
}

impl Work {
    pub fn profile_name(&self) -> String {
        format!("bg-{}", self.slug)
    }
}

/// The curated catalog (tab-separated: id, slug, author, title, year).
pub fn catalog() -> Vec<Work> {
    CATALOG
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some(Work {
                id: f.first()?.trim().parse().ok()?,
                slug: f.get(1)?.trim().to_string(),
                author: f.get(2)?.trim().to_string(),
                title: f.get(3)?.trim().to_string(),
                year: f.get(4)?.trim().parse().ok()?,
            })
        })
        .collect()
}

/// Raw downloads are cached here so `--force` rebuilds never re-download.
pub fn cache_dir() -> PathBuf {
    store::data_dir().join("background-src")
}

/// The book's raw Gutenberg text: from `STYLOMETRY_BACKGROUND_SOURCE` when
/// set, else the download cache, else the network (then cached).
pub fn source_text(w: &Work) -> Result<String, AppError> {
    if let Ok(dir) = std::env::var(SOURCE_ENV) {
        let p = PathBuf::from(dir).join(format!("{}.txt", w.id));
        return std::fs::read_to_string(&p)
            .map_err(|e| AppError::InvalidInput(format!("{}: {e}", p.display())));
    }
    let cached = cache_dir().join(format!("{}.txt", w.id));
    if let Ok(t) = std::fs::read_to_string(&cached) {
        return Ok(t);
    }
    let url = format!("https://www.gutenberg.org/cache/epub/{0}/pg{0}.txt", w.id);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .build()
        .into();
    let body = agent
        .get(&url)
        .header(
            "User-Agent",
            concat!(
                env!("CARGO_BIN_NAME"),
                "/",
                env!("CARGO_PKG_VERSION"),
                " (background corpus; https://github.com/paperfoot/stylometry-cli)"
            ),
        )
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(32 << 20).read_to_string())
        .map_err(|e| AppError::Transient(format!("download {url}: {e}")))?;
    std::fs::create_dir_all(cache_dir())?;
    let tmp = cached.with_extension("txt.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &cached)?;
    Ok(body)
}

/// The book's body without the Project Gutenberg header and licence footer.
pub fn strip_gutenberg(raw: &str) -> &str {
    let start = raw
        .find("*** START OF")
        .and_then(|i| raw[i..].find('\n').map(|j| i + j + 1))
        .unwrap_or(0);
    let end = raw[start..]
        .find("*** END OF")
        .map(|i| start + i)
        .unwrap_or(raw.len());
    &raw[start..end]
}

/// A contiguous prose excerpt of up to `MAX_WORDS` words. Skips the first
/// tenth of the book (title page, contents, an editor's introduction), and
/// drops non-prose blocks: illustrations, and title-case lines such as
/// contents entries.
pub fn excerpt(body: &str) -> String {
    let paras: Vec<String> = text::paragraphs(&text::clean(body))
        .into_iter()
        .filter(|p| !p.starts_with('[') && !is_title_case(p))
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut words = 0usize;
    for p in paras.iter().skip(paras.len() / 10) {
        if words >= MAX_WORDS {
            break;
        }
        words += text::count_words(p);
        out.push(p.clone());
    }
    out.join("\n\n")
}

fn is_title_case(p: &str) -> bool {
    let w = text::word_tokens_cased(p);
    let caps = w
        .iter()
        .filter(|t| t.chars().next().is_some_and(|c| c.is_uppercase()))
        .count();
    w.len() >= 3 && caps * 2 > w.len()
}

/// Build the background profile for one book from its raw Gutenberg text.
pub fn build_profile(w: &Work, raw: &str) -> Result<Profile, AppError> {
    let doc = Doc {
        name: format!("{} ({}, {})", w.title, w.author, w.year),
        text: excerpt(strip_gutenberg(raw)),
    };
    Profile::build(&w.profile_name(), &[doc], ProfileKind::Background)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_curated() {
        let c = catalog();
        assert!(c.len() >= 35, "{} works", c.len());
        let mut per_author: std::collections::HashMap<&str, usize> = Default::default();
        for w in &c {
            *per_author.entry(w.author.as_str()).or_default() += 1;
            assert!((1850..=1930).contains(&w.year), "{} {}", w.slug, w.year);
            assert!(w.slug.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'));
        }
        assert!(per_author.values().all(|&n| n <= 2), "{per_author:?}");
        let mut ids: Vec<u32> = c.iter().map(|w| w.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.len(), "duplicate ids");
    }

    #[test]
    fn strips_header_and_footer() {
        let raw = "Title: X\n*** START OF THE PROJECT GUTENBERG EBOOK X ***\nBody text.\n*** END OF THE PROJECT GUTENBERG EBOOK X ***\nLicence";
        assert_eq!(strip_gutenberg(raw).trim(), "Body text.");
        assert_eq!(strip_gutenberg("no markers"), "no markers");
    }

    #[test]
    fn excerpt_drops_contents_lines() {
        let mut body = String::from("CONTENTS\n\nChapter One The Beginning Of It All\n\n");
        for _ in 0..40 {
            body.push_str("It was a quiet evening and the rain had stopped at last, so we went out.\n\n");
        }
        let e = excerpt(&body);
        assert!(!e.contains("Chapter One"));
        assert!(e.contains("quiet evening"));
    }
}
