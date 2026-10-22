//! `check`: draft feedback against a voice.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String, file: Option<std::path::PathBuf>, text: Option<String>, limit: usize) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`check` is not implemented yet".into()))
}
