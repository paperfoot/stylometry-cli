//! `doctor`: diagnose setup.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

pub fn run(ctx: Ctx) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`doctor` is not implemented yet".into()))
}
