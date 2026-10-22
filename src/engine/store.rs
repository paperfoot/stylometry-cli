//! On-disk profile store. Profiles live as JSON under the platform data dir,
//! e.g. ~/Library/Application Support/stylometry/profiles/<name>.json.
//! Author and background profiles share one directory; `Profile::kind` tells
//! them apart.

use std::path::PathBuf;

use crate::engine::profile::{self, Profile, ProfileKind};
use crate::engine::text::Doc;
use crate::error::AppError;

/// Data root. Overridable via STYLOMETRY_DATA_DIR (used by tests and for
/// isolated environments).
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STYLOMETRY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", env!("CARGO_BIN_NAME"))
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory holding all profiles.
pub fn profiles_dir() -> PathBuf {
    data_dir().join("profiles")
}

fn sanitize(name: &str) -> Result<String, AppError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(name.to_string())
    } else {
        Err(AppError::InvalidInput(format!(
            "invalid profile name '{name}': use letters, digits, '-' and '_' only"
        )))
    }
}

pub fn path_for(name: &str) -> Result<PathBuf, AppError> {
    Ok(profiles_dir().join(format!("{}.json", sanitize(name)?)))
}

pub fn save(profile: &Profile) -> Result<PathBuf, AppError> {
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir)?;
    let path = path_for(&profile.name)?;
    let json = serde_json::to_vec(profile)
        .map_err(|e| AppError::Transient(format!("serialize profile: {e}")))?;
    // Write-then-rename so a crash never leaves a half-written profile.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn load(name: &str) -> Result<Profile, AppError> {
    let path = path_for(name)?;
    if !path.exists() {
        return Err(AppError::InvalidInput(format!(
            "no profile named '{name}'. List with: {} profile list --all",
            env!("CARGO_BIN_NAME")
        )));
    }
    let raw = std::fs::read(&path)?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| AppError::Config(format!("corrupt profile '{name}': {e}")))?;
    let schema = value.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema < profile::SCHEMA as u64 {
        return Err(AppError::Config(format!(
            "profile '{name}' was built by an older version (schema {schema}); rebuild it: \
             {} profile build {name} --corpus <path> --force",
            env!("CARGO_BIN_NAME")
        )));
    }
    let had_register = value.get("register").is_some();
    let mut p: Profile = serde_json::from_value(value)
        .map_err(|e| AppError::Config(format!("corrupt profile '{name}': {e}")))?;
    if !had_register {
        p.register = profile::detect_from_chunks(&p);
        p.register_reason = "detected on load (profile predates registers)".into();
    }
    Ok(p)
}

/// The stored text of a profile of any schema >= 3, grouped back into its
/// source works: what `profile rebuild` re-analyses after an upgrade.
pub fn load_text(name: &str) -> Result<(ProfileKind, Vec<Doc>), AppError> {
    let path = path_for(name)?;
    if !path.exists() {
        return load(name).map(|_| unreachable!());
    }
    let raw = std::fs::read(&path)?;
    let v: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| AppError::Config(format!("corrupt profile '{name}': {e}")))?;
    let outdated = || {
        AppError::setup(
            format!("profile '{name}' predates stored text and can't be rebuilt in place"),
            format!(
                "Rebuild it from its corpus: {} profile build {name} --corpus <path> --force",
                env!("CARGO_BIN_NAME")
            ),
        )
    };
    let kind = match v.get("kind").and_then(|k| k.as_str()) {
        Some("background") => ProfileKind::Background,
        _ => ProfileKind::Author,
    };
    let sources = v.get("sources").and_then(|s| s.as_array()).ok_or_else(outdated)?;
    let mut docs: Vec<Doc> = sources
        .iter()
        .map(|s| Doc {
            name: s.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
            ..Doc::default()
        })
        .collect();
    let chunks = v.get("chunks").and_then(|c| c.as_array()).ok_or_else(outdated)?;
    for c in chunks {
        let src = c.get("source").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
        let paras = c.get("paragraphs").and_then(|p| p.as_array()).ok_or_else(outdated)?;
        let doc = docs.get_mut(src).ok_or_else(outdated)?;
        let text: Vec<&str> = paras.iter().filter_map(|p| p.as_str()).collect();
        // A short-form chunk is one item: restore it whole, with its meta,
        // so a rebuild neither splits posts at inner blank lines nor loses
        // kind, date and engagement.
        if let Some(meta) = c.get("meta").filter(|m| !m.is_null()) {
            doc.items.push(crate::engine::text::Item {
                text: text.join("\n\n"),
                meta: serde_json::from_value(meta.clone()).unwrap_or_default(),
            });
            continue;
        }
        for p in text {
            doc.text.push_str(p);
            doc.text.push_str("\n\n");
        }
    }
    Ok((kind, docs))
}

pub fn exists(name: &str) -> bool {
    path_for(name).map(|p| p.exists()).unwrap_or(false)
}

pub fn list_names() -> Vec<String> {
    let dir = profiles_dir();
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("json") {
                // `<name>.texture.json` sidecars are not profiles.
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()).filter(|s| !s.contains('.')) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// Load every readable current-schema profile, authors and background alike
/// (the shared reference model and the imposter pool use all of them).
/// Unreadable or outdated profiles are skipped; `profile list` reports them.
pub fn load_all() -> Vec<Profile> {
    list_names().iter().filter_map(|n| load(n).ok()).collect()
}

pub fn load_kind(kind: ProfileKind) -> Vec<Profile> {
    load_all().into_iter().filter(|p| p.kind == kind).collect()
}

pub fn remove(name: &str) -> Result<(), AppError> {
    let path = path_for(name)?;
    if !path.exists() {
        return Err(AppError::InvalidInput(format!("no profile named '{name}'")));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}
