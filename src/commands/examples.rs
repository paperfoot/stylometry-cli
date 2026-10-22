//! `examples`: the author's own passages or posts most like what is being
//! written, as few-shot examples for an agent.
//!
//! FOUNDATION STUB: implemented by a build lane.

use std::path::PathBuf;

use crate::error::AppError;
use crate::output::Ctx;

#[allow(unused_variables)]
pub fn run(ctx: Ctx, profile: String, like: Option<PathBuf>, about: Option<String>, limit: usize) -> Result<(), AppError> {
    Err(AppError::InvalidInput("`examples` is not implemented yet".into()))
}
