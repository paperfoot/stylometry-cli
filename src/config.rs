//! Configuration loading with 3-tier precedence:
//!   1. Compiled defaults
//!   2. TOML config file (platform config dir / config.toml)
//!   3. Environment variables (STYLOMETRY_*, `__` between nesting levels)
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::AppError;

// ── Config structs ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// Update / distribution settings.
    pub update: UpdateConfig,
    /// JEV (TypeSafe System One) judgments: prose texture and example
    /// re-ranking. On whenever a key is found; fails open to local-only.
    #[serde(default)]
    pub jev: JevConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JevConfig {
    /// Use JEV when a key is available. Sends passages of drafts and of
    /// profiled text to api.typesafe.ai; set false to stay fully local.
    pub enabled: bool,
    /// Pinned model; thresholds were measured against this version.
    pub model: String,
    /// API endpoint (tests point this at a local mock server).
    pub endpoint: String,
    /// Environment variable holding the API key.
    pub key_env: String,
    /// Command whose output is the key, when the variable is unset (run
    /// with `sh -c`; plain text, or JSON with `data.value` as `akm` prints).
    pub key_command: String,
    /// Parallel requests.
    pub workers: usize,
    /// Per-request deadline, milliseconds.
    pub timeout_ms: u64,
    /// Time budget for one command's JEV work, milliseconds (`check` and
    /// `guide`; `profile build` allows four times this). Past it nothing
    /// more is sent and texture reports unavailable, so a slow server
    /// cannot stall a check.
    pub budget_ms: u64,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "jev-1.13.0".into(),
            endpoint: "https://api.typesafe.ai/v1/systemone".into(),
            key_env: "TYPESAFE_API_KEY".into(),
            key_command: String::new(),
            workers: 8,
            timeout_ms: 8000,
            budget_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Enable or disable update checks.
    pub enabled: bool,

    /// Install source: auto, standalone, homebrew, cargo, cargo_binstall,
    /// npm, bun, uv_tool, pipx, winget, scoop, apt, managed, or unknown.
    #[serde(alias = "source")]
    pub install_source: String,

    /// GitHub repository owner
    pub owner: String,

    /// GitHub repository name
    pub repo: String,

    /// crates.io package name
    pub crate_name: String,

    /// Homebrew formula name
    pub formula: String,

    /// Optional Homebrew tap, for example owner/tap
    pub tap: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            install_source: "auto".into(),
            owner: "paperfoot".into(),
            repo: "stylometry-cli".into(),
            // The crates.io package is `stylometry-cli` (the bare name was
            // taken); the installed binary and formula stay `stylometry`.
            crate_name: env!("CARGO_PKG_NAME").into(),
            formula: env!("CARGO_BIN_NAME").into(),
            tap: "paperfoot/tap".into(),
        }
    }
}

// ── Paths ──────────────────────────────────────────────────────────────────

pub fn config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", env!("CARGO_BIN_NAME"))
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("config.toml")
}

// ── Loading ────────────────────────────────────────────────────────────────

pub fn load() -> Result<AppConfig, AppError> {
    use figment::Figment;
    use figment::providers::{Env, Format as _, Serialized, Toml};

    let prefix = format!("{}_", env!("CARGO_BIN_NAME").to_uppercase());

    Figment::from(Serialized::defaults(AppConfig::default()))
        .merge(Toml::file(config_path()))
        // Double underscores separate nesting; single underscores belong to
        // field names, e.g. STYLOMETRY_UPDATE__INSTALL_SOURCE.
        .merge(Env::prefixed(&prefix).split("__"))
        .extract()
        .map_err(|e| AppError::Config(e.to_string()))
}
