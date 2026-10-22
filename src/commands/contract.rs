/// Hidden command for integration tests.
///
/// Deterministically triggers each documented exit code (0-4, plus domain
/// code 5 = `check --strict` failure) so the test suite and the framework
/// conformance probe can verify the contract without network or profiles.
use crate::error::AppError;
use crate::output::{self, Ctx};

pub fn run(ctx: Ctx, code: i32) -> Result<(), AppError> {
    match code {
        0 => {
            let data = serde_json::json!({"contract": true, "exit_code": 0});
            output::print_success_or(ctx, &data, |_| {
                println!("contract: success");
            })
        }
        1 => Err(AppError::Transient("contract: transient error".into())),
        2 => Err(AppError::Config("contract: config error".into())),
        3 => Err(AppError::InvalidInput("contract: bad input".into())),
        4 => Err(AppError::RateLimited("contract: rate limited".into())),
        5 => Err(AppError::CheckFailed(
            serde_json::json!({"contract": true, "passed": false}),
        )),
        other => Err(AppError::InvalidInput(format!(
            "unknown contract code: {other}. Valid: 0-5"
        ))),
    }
}
