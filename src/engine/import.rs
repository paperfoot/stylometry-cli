//! Corpus importers. Long-form text (.md/.txt) is read by `text::read_docs`;
//! this module adds the short-form shapes writers actually have: an X
//! archive (`tweets.js`), xmaster JSON, JSON/JSONL exports, CSV, and .eml
//! emails. Each item keeps its text verbatim (line breaks matter in a post)
//! plus whatever engagement and date metadata the source carries.

use std::path::Path;

use serde_json::Value;
use walkdir::WalkDir;

use crate::engine::profile::{ItemKind, ItemMeta, Register};
use crate::engine::text::{self, Doc, Item};
use crate::error::AppError;

/// Field names that hold an item's text, in priority order.
const TEXT_FIELDS: [&str; 6] = ["full_text", "text", "content", "body", "message", "tweet"];

/// What a corpus path contained: the documents, and the register the source
/// format implies (an X archive is social whatever its length), if any.
pub struct Corpus {
    pub docs: Vec<Doc>,
    pub format_register: Option<Register>,
    /// Items skipped (retweets, empty, under 3 words), for the build report.
    pub skipped: usize,
}

/// Read a file or folder of any supported format.
pub fn read_corpus(path: &Path) -> Result<Corpus, AppError> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        for e in WalkDir::new(path)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if e.file_type().is_file() {
                files.push(e.path().to_path_buf());
            }
        }
    } else {
        return Err(AppError::input(
            format!("path does not exist: {}", path.display()),
            "Pass a file or folder: .md/.txt prose, an X archive tweets.js, xmaster/JSON/JSONL/CSV exports, or .eml emails",
        ));
    }

    let mut out = Corpus {
        docs: Vec::new(),
        format_register: None,
        skipped: 0,
    };
    let mut long_form = false;
    for f in &files {
        let name = f
            .strip_prefix(path)
            .ok()
            .filter(|r| !r.as_os_str().is_empty())
            .map(|r| r.display().to_string())
            .unwrap_or_else(|| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let lower = name.to_lowercase();
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let (items, reg) = match ext.as_str() {
            "md" | "txt" | "markdown" | "text" => {
                let raw = std::fs::read_to_string(f)?;
                out.docs.push(Doc {
                    name,
                    text: text::clean(&raw),
                    items: Vec::new(),
                });
                long_form = true;
                continue;
            }
            "js" if lower.contains("tweet") => (x_archive(&std::fs::read_to_string(f)?)?, Register::Social),
            "json" => json_items(&std::fs::read_to_string(f)?)?,
            "jsonl" | "ndjson" => (jsonl_items(&std::fs::read_to_string(f)?), Register::Social),
            "csv" => (csv_items(f)?, Register::Social),
            "eml" => (vec![eml_item(&std::fs::read_to_string(f)?)], Register::Email),
            _ => continue,
        };
        let before = items.len();
        let items: Vec<Item> = items
            .into_iter()
            .filter(|i| text::count_words(&i.text) >= 3)
            .collect();
        out.skipped += before - items.len();
        if items.is_empty() {
            continue;
        }
        // JSON/CSV exports of long items (newsletters, blog posts) are not social.
        let reg = if reg == Register::Social && ext != "js" && median_words(&items) > 60 {
            Register::Blog
        } else {
            reg
        };
        out.format_register.get_or_insert(reg);
        out.docs.push(Doc {
            name,
            text: String::new(),
            items,
        });
    }
    if out.docs.is_empty() {
        return Err(AppError::input(
            format!("no readable text found under {}", path.display()),
            "Supported: .md/.txt prose, tweets.js (X archive), xmaster/JSON/JSONL/CSV exports with a text column, .eml",
        ));
    }
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

/// Drop dated items before `day` (YYYY-MM-DD). Undated items are kept.
pub fn keep_since(c: &mut Corpus, day: &str) -> Result<(), AppError> {
    if day.len() != 10 || day.as_bytes()[4] != b'-' || day.as_bytes()[7] != b'-' {
        return Err(AppError::input(format!("--since '{day}' is not YYYY-MM-DD"), "Example: --since 2025-01-01"));
    }
    for d in &mut c.docs {
        d.items.retain(|i| i.meta.date.as_deref().is_none_or(|dt| dt.get(..10).is_none_or(|x| x >= day)));
    }
    c.docs.retain(|d| !d.items.is_empty() || !d.text.is_empty());
    Ok(())
}

fn median_words(items: &[Item]) -> usize {
    let mut w: Vec<usize> = items.iter().map(|i| text::count_words(&i.text)).collect();
    w.sort_unstable();
    w.get(w.len() / 2).copied().unwrap_or(0)
}

/// Split a long-form document into items for a short-form register: on
/// separator lines (`---`, `***`, `===`) when present, else on blank lines.
pub fn split_items(text: &str) -> Vec<Item> {
    let is_sep = |l: &str| {
        let t = l.trim();
        t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '='))
    };
    let has_sep = text.lines().any(is_sep);
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
        let boundary = if has_sep { is_sep(line) } else { line.trim().is_empty() };
        if boundary {
            flush(&mut cur);
        } else {
            cur.push(line);
        }
    }
    flush(&mut cur);
    items
}

fn unescape_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn first_num(o: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| num(o.get(*k)))
}

fn first_str<'a>(o: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| o.get(*k).and_then(|v| v.as_str()))
}

/// An item from one JSON object of any supported export shape.
fn item_of(o: &Value) -> Option<Item> {
    let o = o.get("tweet").filter(|t| t.is_object()).unwrap_or(o);
    let text = first_str(o, &TEXT_FIELDS)?;
    if text.starts_with("RT @") {
        return None;
    }
    let metrics = o.get("public_metrics").or_else(|| o.get("metrics")).unwrap_or(o);
    let reply = o.get("in_reply_to_status_id_str").is_some_and(|v| !v.is_null())
        || o.get("in_reply_to_status_id").is_some_and(|v| !v.is_null())
        || o.get("reply_to").is_some_and(|v| !v.is_null())
        || o.get("is_reply").and_then(|v| v.as_bool()).unwrap_or(false)
        || text.trim_start().starts_with('@');
    Some(Item {
        text: unescape_html(text),
        meta: ItemMeta {
            likes: first_num(metrics, &["likes", "like_count", "favorite_count", "reactions"]),
            reposts: first_num(metrics, &["retweets", "reposts", "retweet_count", "repost_count", "shares"]),
            impressions: first_num(metrics, &["impressions", "impression_count", "views"]),
            date: first_str(o, &["created_at", "date", "timestamp"]).map(str::to_string),
            kind: if reply { ItemKind::Reply } else { ItemKind::Post },
            ..ItemMeta::default()
        },
    })
}

/// X archive `tweets.js`: `window.YTD.tweets.part0 = [ {"tweet": {...}}, ... ]`.
fn x_archive(raw: &str) -> Result<Vec<Item>, AppError> {
    let start = raw.find('[').ok_or_else(|| {
        AppError::input("tweets.js has no JSON array", "Pass data/tweets.js from an unzipped X archive")
    })?;
    let v: Value = serde_json::from_str(raw[start..].trim_end().trim_end_matches(';'))
        .map_err(|e| AppError::input(format!("tweets.js is not valid JSON: {e}"), "Pass data/tweets.js from an unzipped X archive"))?;
    Ok(v.as_array().map(|a| a.iter().filter_map(item_of).collect()).unwrap_or_default())
}

/// A JSON file: an array of items, an xmaster envelope (`data.tweets`), or
/// any object holding one array of items.
fn json_items(raw: &str) -> Result<(Vec<Item>, Register), AppError> {
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
    let items = find_array(&v).map(|a| a.iter().filter_map(item_of).collect()).unwrap_or_default();
    Ok((items, Register::Social))
}

fn jsonl_items(raw: &str) -> Vec<Item> {
    raw.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter_map(|v| item_of(&v))
        .collect()
}

fn csv_items(path: &Path) -> Result<Vec<Item>, AppError> {
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
    let mut items = Vec::new();
    for rec in rdr.records().flatten() {
        let mut o = serde_json::Map::new();
        for (h, v) in headers.iter().zip(rec.iter()) {
            o.insert(h.clone(), Value::String(v.to_string()));
        }
        if let Some(i) = item_of(&Value::Object(o)) {
            items.push(i);
        }
    }
    Ok(items)
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
        .find_map(|l| l.strip_prefix("Date:").map(|d| d.trim().to_string()));
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

    #[test]
    fn reads_x_archive_and_skips_retweets() {
        let raw = r#"window.YTD.tweets.part0 = [
          {"tweet": {"full_text": "Shipping the new build today &amp; it's fast", "favorite_count": "12", "retweet_count": "3", "created_at": "Mon Oct 20 10:00:00 +0000 2026"}},
          {"tweet": {"full_text": "RT @someone: not mine at all here", "favorite_count": "0", "retweet_count": "0"}},
          {"tweet": {"full_text": "@friend thanks, that helps a lot", "in_reply_to_status_id_str": "1", "favorite_count": "1", "retweet_count": "0"}}
        ]"#;
        let items = x_archive(raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "Shipping the new build today & it's fast");
        assert_eq!(items[0].meta.likes, Some(12.0));
        assert_eq!(items[0].meta.reposts, Some(3.0));
        assert!(items[1].meta.is_reply());
    }

    #[test]
    fn reads_xmaster_envelope() {
        let raw = r#"{"status":"success","data":{"tweets":[{"text":"Your agent needs keys.\n\nIt shouldn't see them.","likes":6,"retweets":1,"impressions":129,"date":"2026-10-19T14:49:58.000Z"}]}}"#;
        let (items, reg) = json_items(raw).unwrap();
        assert_eq!(reg, Register::Social);
        assert_eq!(items.len(), 1);
        assert!(items[0].text.contains("\n\n"));
        assert_eq!(items[0].meta.impressions, Some(129.0));
    }

    #[test]
    fn eml_keeps_only_what_the_sender_wrote() {
        let raw = "From: a@b.c\nDate: Tue, 21 Oct 2026\nSubject: hi\n\nHey Sam,\n\nSounds good, see you Friday.\n\nCheers,\nB\n-- \nB, CEO\n> earlier text\n";
        let i = eml_item(raw);
        assert!(i.text.starts_with("Hey Sam,"));
        assert!(i.text.ends_with("B"));
        assert!(!i.text.contains("CEO"));
        assert_eq!(i.meta.date.as_deref(), Some("Tue, 21 Oct 2026"));
    }

    #[test]
    fn splits_items_on_separators_else_blank_lines() {
        let s = split_items("one post here now\nsecond line\n---\nanother post here\n");
        assert_eq!(s.len(), 2);
        assert!(s[0].text.contains('\n'));
        let b = split_items("first post words\n\nsecond post words\n");
        assert_eq!(b.len(), 2);
    }
}
