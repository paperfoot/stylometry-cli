//! Shared input handling: the text to judge (file, explicit stdin, or
//! inline), and "profile name or path" resolution for read-only analysis.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::engine::profile::{Profile, ProfileKind};
use crate::engine::{store, text};
use crate::error::AppError;

/// Read the text to score. Stdin is read only when asked for with `-`:
/// an agent's stdin may be an open pipe that never closes, and an implicit
/// read would hang it.
pub fn read_text(file: Option<PathBuf>, inline: Option<String>, cmd: &str) -> Result<String, AppError> {
    let bin = env!("CARGO_BIN_NAME");
    match (file, inline) {
        (Some(p), _) if p.as_os_str() == "-" => read_stdin(),
        (Some(p), _) => {
            let docs = text::read_docs(&p)?;
            Ok(docs
                .into_iter()
                .map(|d| d.text)
                .collect::<Vec<_>>()
                .join("\n\n"))
        }
        (None, Some(t)) if !t.trim().is_empty() => Ok(t),
        _ => Err(AppError::input(
            "no text given",
            format!(
                "Pass a file path, '-' to read stdin, or --text \"...\": {bin} {cmd} draft.md --profile <name>"
            ),
        )),
    }
}

fn read_stdin() -> Result<String, AppError> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| AppError::InvalidInput(format!("failed to read stdin: {e}")))?;
    if buf.trim().is_empty() {
        return Err(AppError::input(
            "stdin was empty",
            "Pipe the draft in (cat draft.md | ... -) or pass a file path",
        ));
    }
    Ok(buf)
}

/// A saved profile by name, or — when `source` is an existing file/folder and
/// no profile has that name — an unsaved profile built on the fly from it.
/// Ad-hoc profiles accept a single passage: they are for describing a text,
/// never for verification.
pub fn resolve_profile(source: &str) -> Result<Profile, AppError> {
    let path = Path::new(source);
    let is_name = !source.contains(['/', '\\', '.']);
    if is_name && store::exists(source) {
        return store::load(source);
    }
    if path.exists() {
        let docs = text::read_docs(path)?;
        let label = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string());
        return Profile::build_min(&label, &docs, ProfileKind::Author, 1);
    }
    if is_name {
        return store::load(source); // the store's "no profile named" error
    }
    Err(AppError::input(
        format!("'{source}' is neither a profile nor an existing path"),
        format!("List profiles with: {} profile list", env!("CARGO_BIN_NAME")),
    ))
}
