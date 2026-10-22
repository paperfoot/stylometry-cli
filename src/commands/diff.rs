//! `diff`: contrast two voices.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, a: String, b: String, limit: usize) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`diff` is not implemented yet".into()))
}
