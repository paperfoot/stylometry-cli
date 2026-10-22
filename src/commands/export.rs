//! `export`: a profile's passages as fine-tuning data.
//!
//! FOUNDATION STUB: implemented by a build lane.

use std::path::PathBuf;

use crate::cli::ExportFormat;
use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String, out: PathBuf, format: ExportFormat) -> Result<(), AppError> {
    Err(AppError::InvalidInput("`export` is not implemented yet".into()))
}
