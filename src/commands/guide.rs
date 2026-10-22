//! `guide`: agent-ready style brief (Markdown).
//!
//! FOUNDATION STUB: implemented by a build lane.

use std::path::PathBuf;

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(
    ctx: Ctx,
    profile: String,
    exemplars: usize,
    limit: usize,
    window: usize,
    out: Option<PathBuf>,
) -> Result<(), AppError> {
    Err(AppError::InvalidInput("`guide` is not implemented yet".into()))
}
