//! `check`: draft feedback against a voice.
//!
//! FOUNDATION STUB: implemented by a build lane.

use std::path::PathBuf;

use crate::error::AppError;
use crate::output::Ctx;

pub struct CheckArgs {
    pub profile: String,
    pub file: Option<PathBuf>,
    pub text: Option<String>,
    pub limit: usize,
    /// Banned word/phrase files (one per line); any hit is a hard failure.
    pub avoid: Vec<PathBuf>,
    /// Extra profiles whose stored text the draft must not copy.
    pub guard: Vec<String>,
    /// Gate mode: a failed check returns `AppError::CheckFailed(report)`.
    pub strict: bool,
}

#[allow(unused_variables)]
pub fn run(ctx: Ctx, args: CheckArgs) -> Result<(), AppError> {
    Err(AppError::InvalidInput("`check` is not implemented yet".into()))
}
