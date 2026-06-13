//! On-disk profile store. Profiles live as JSON under the platform data dir,
//! e.g. ~/Library/Application Support/stylometry/profiles/<name>.json.

use std::path::PathBuf;

use crate::engine::profile::Profile;
use crate::error::AppError;

/// Directory holding all profiles. Overridable via STYLOMETRY_DATA_DIR (used by
/// tests and for isolated environments).
pub fn profiles_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STYLOMETRY_DATA_DIR") {
        return PathBuf::from(dir).join("profiles");
    }
    directories::ProjectDirs::from("", "", env!("CARGO_PKG_NAME"))
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("profiles")
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

fn path_for(name: &str) -> Result<PathBuf, AppError> {
    Ok(profiles_dir().join(format!("{}.json", sanitize(name)?)))
}

pub fn save(profile: &Profile) -> Result<PathBuf, AppError> {
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir)?;
    let path = path_for(&profile.name)?;
    let json = serde_json::to_string(profile)
        .map_err(|e| AppError::Transient(format!("serialize profile: {e}")))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

pub fn load(name: &str) -> Result<Profile, AppError> {
    let path = path_for(name)?;
    if !path.exists() {
        return Err(AppError::InvalidInput(format!(
            "no profile named '{name}'. List with: {} profile list",
            env!("CARGO_PKG_NAME")
        )));
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| AppError::Config(format!("corrupt profile '{name}': {e}")))
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
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// Load every profile (used to build the shared reference model and the
/// imposter set for verification).
pub fn load_all() -> Vec<Profile> {
    list_names().iter().filter_map(|n| load(n).ok()).collect()
}

pub fn remove(name: &str) -> Result<(), AppError> {
    let path = path_for(name)?;
    if !path.exists() {
        return Err(AppError::InvalidInput(format!("no profile named '{name}'")));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}
