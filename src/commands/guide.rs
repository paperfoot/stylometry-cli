//! `guide`: agent-ready style brief (Markdown).
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String, exemplars: usize, limit: usize, out: Option<std::path::PathBuf>) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`guide` is not implemented yet".into()))
}
