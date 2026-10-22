//! `compare`: authorship verification verdict.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String, file: Option<std::path::PathBuf>, text: Option<String>) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`compare` is not implemented yet".into()))
}
