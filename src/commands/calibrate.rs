//! `calibrate`: fit P(same author) per length band.
//!
//! FOUNDATION STUB: implemented by a build lane.

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String) -> Result<(), AppError> {
    let _ = ctx;
    Err(AppError::InvalidInput("`calibrate` is not implemented yet".into()))
}
