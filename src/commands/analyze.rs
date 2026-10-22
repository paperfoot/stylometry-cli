//! `analyze`: voice report for a profile or a path.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, source: String, limit: usize, window: usize) -> Result<(), AppError> {
    Err(AppError::InvalidInput("`analyze` is not implemented yet".into()))
}
