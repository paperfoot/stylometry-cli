use std::borrow::Cow;

/// Error types with semantic exit codes.
///
/// Every error maps to an exit code (1-4, plus the documented domain code 5
/// for a failed `check --strict`), a machine-readable code, and a recovery
/// suggestion that agents can follow literally. Domain errors use
/// `AppError::input` / `AppError::setup` so the suggestion is the exact next
/// command, not a pointer to --help.

#[derive(thiserror::Error, Debug)]
#[allow(dead_code)] // Some variants demonstrate the full exit code contract (0-5)
pub enum AppError {
    #[error("Could not serialize command output")]
    Serialization,

    #[error("doctor found failing checks")]
    Diagnostics(serde_json::Value),

    /// `check --strict` gate failure; the full check report is the details.
    #[error("draft failed the voice check")]
    CheckFailed(serde_json::Value),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
    /// Bad input with a specific recovery (also used for parse errors).
    #[error("Invalid input: {message}")]
    InvalidArguments { message: String, suggestion: String },

    #[error("Configuration error: {0}")]
    Config(String),
    /// Missing or outdated setup (profiles, background) with a specific fix.
    #[error("{message}")]
    Setup { message: String, suggestion: String },

    #[error("{0}")]
    Transient(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Update failed: {0}")]
    Update(String),
}

impl AppError {
    /// Bad input (exit 3) whose suggestion is the exact fix.
    pub fn input(message: impl Into<String>, suggestion: impl Into<String>) -> Self {
        Self::InvalidArguments {
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }

    /// Missing setup (exit 2) whose suggestion is the exact fix.
    pub fn setup(message: impl Into<String>, suggestion: impl Into<String>) -> Self {
        Self::Setup {
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }

    pub fn details(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Diagnostics(report) | Self::CheckFailed(report) => Some(report),
            _ => None,
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidInput(_) | Self::InvalidArguments { .. } => 3,
            Self::CheckFailed(_) => 5,
            Self::Config(_) | Self::Setup { .. } | Self::Diagnostics(_) => 2,
            Self::RateLimited(_) => 4,
            Self::Transient(_) | Self::Io(_) | Self::Update(_) | Self::Serialization => 1,
        }
    }

    /// Whether repeating the same request unchanged can succeed; `None` when
    /// that depends on an outcome this error cannot know (an uncertain write).
    pub fn retryable(&self) -> Option<bool> {
        match self {
            Self::RateLimited(_) => Some(true),
            Self::InvalidInput(_)
            | Self::InvalidArguments { .. }
            | Self::Config(_)
            | Self::Setup { .. }
            | Self::Diagnostics(_)
            | Self::CheckFailed(_) => Some(false),
            _ => None,
        }
    }

    pub fn error_code(&self) -> &str {
        match self {
            Self::Serialization => "serialization_error",
            Self::CheckFailed(_) => "check_failed",
            Self::Diagnostics(_) | Self::Config(_) | Self::Setup { .. } => "config_error",
            Self::InvalidInput(_) | Self::InvalidArguments { .. } => "invalid_input",
            Self::Transient(_) => "transient_error",
            Self::RateLimited(_) => "rate_limited",
            Self::Io(_) => "io_error",
            Self::Update(_) => "update_error",
        }
    }

    pub fn suggestion(&self) -> Cow<'static, str> {
        match self {
            Self::Serialization => {
                "Report this output serialization failure to the tool maintainer".into()
            }
            Self::Diagnostics(_) => {
                "Fix the failed checks listed in error.details, then run doctor again".into()
            }
            Self::CheckFailed(_) => {
                "Revise the draft for the findings in error.details (hard failures first), then run check again".into()
            }
            Self::InvalidInput(_) => {
                concat!("Check arguments with: ", env!("CARGO_BIN_NAME"), " --help").into()
            }
            Self::InvalidArguments { suggestion, .. } | Self::Setup { suggestion, .. } => {
                suggestion.clone().into()
            }
            Self::Config(_) => {
                concat!("Check config with: ", env!("CARGO_BIN_NAME"), " config path").into()
            }
            Self::Transient(_) | Self::Io(_) => {
                "Inspect the operation result before retrying; a failed response does not prove a write failed".into()
            }
            Self::RateLimited(_) => {
                "Respect the provider backoff; retry only when the operation is safe to repeat".into()
            }
            Self::Update(_) => concat!(
                "Inspect the installed version and update instructions with: ",
                env!("CARGO_BIN_NAME"),
                " update --check"
            )
            .into(),
        }
    }
}
