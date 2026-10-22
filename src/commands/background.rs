//! `background`: download and build reference authors.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn fetch(ctx: Ctx, force: bool) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`background` is not implemented yet".into()))
}

pub fn status(ctx: Ctx) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`background status` is not implemented yet".into()))
}
