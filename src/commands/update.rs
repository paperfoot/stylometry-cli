//! `update`: distribution-aware update check (docs/update-standard.md of the
//! agent-cli-framework). Package managers own their binaries; a standalone
//! install gets a real release check and the release URL. Neither form ever
//! replaces the binary: there is no verified (checksummed, staged)
//! self-replacement yet.
use serde::Serialize;
use std::path::Path;

use crate::config::AppConfig;
use crate::error::AppError;
use crate::output::{self, Ctx};

#[derive(Serialize)]
struct UpdateResult {
    current_version: String,
    /// Verified latest release, or null when not checked.
    latest_version: Option<String>,
    status: String,
    install_source: String,
    update_mode: String,
    /// A literal command for the owning package manager, or null.
    upgrade_command: Option<String>,
    release_url: Option<String>,
    requires_skill_reinstall: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallSource {
    Auto,
    Standalone,
    Homebrew,
    Cargo,
    CargoBinstall,
    Npm,
    Bun,
    UvTool,
    Pipx,
    Winget,
    Scoop,
    Apt,
    Managed,
    Unknown,
}

impl InstallSource {
    fn parse(raw: &str) -> Result<Self, AppError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "standalone" => Ok(Self::Standalone),
            "homebrew" | "brew" => Ok(Self::Homebrew),
            "cargo" => Ok(Self::Cargo),
            "cargo_binstall" | "cargo-binstall" | "binstall" => Ok(Self::CargoBinstall),
            "npm" => Ok(Self::Npm),
            "bun" => Ok(Self::Bun),
            "uv_tool" | "uv-tool" | "uv" => Ok(Self::UvTool),
            "pipx" => Ok(Self::Pipx),
            "winget" => Ok(Self::Winget),
            "scoop" => Ok(Self::Scoop),
            "apt" => Ok(Self::Apt),
            "managed" => Ok(Self::Managed),
            "unknown" => Ok(Self::Unknown),
            other => Err(AppError::Config(format!(
                "invalid update.install_source '{other}' (expected auto, standalone, homebrew, cargo, cargo_binstall, npm, bun, uv_tool, pipx, winget, scoop, apt, managed, or unknown)"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Standalone => "standalone",
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::CargoBinstall => "cargo_binstall",
            Self::Npm => "npm",
            Self::Bun => "bun",
            Self::UvTool => "uv_tool",
            Self::Pipx => "pipx",
            Self::Winget => "winget",
            Self::Scoop => "scoop",
            Self::Apt => "apt",
            Self::Managed => "managed",
            Self::Unknown => "unknown",
        }
    }
}

fn detect_install_source(config: &AppConfig) -> Result<InstallSource, AppError> {
    let configured = InstallSource::parse(&config.update.install_source)?;
    if configured != InstallSource::Auto {
        return Ok(configured);
    }

    if let Some(source) = option_env!("ACF_INSTALL_SOURCE") {
        let source = InstallSource::parse(source)?;
        if source != InstallSource::Auto {
            return Ok(source);
        }
    }

    let exe = std::env::current_exe().map_err(AppError::Io)?;
    let path = exe.to_string_lossy();

    if path.contains("/Cellar/") || path.starts_with("/opt/homebrew/bin/") {
        return Ok(InstallSource::Homebrew);
    }

    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(|p| Path::new(&p).join("bin"))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo").join("bin"))
        });
    if let Some(cargo_bin) = cargo_bin {
        if exe.starts_with(cargo_bin) {
            return Ok(InstallSource::Cargo);
        }
    }

    if path.contains("/node_modules/.bin/") {
        return Ok(InstallSource::Npm);
    }

    if path.contains("/.bun/bin/") {
        return Ok(InstallSource::Bun);
    }

    if path.contains("/uv/tools/") || path.contains("/.local/share/uv/tools/") {
        return Ok(InstallSource::UvTool);
    }

    // A development build under target/ proves nothing about how the tool is
    // installed: uncertain ownership is `unknown` (instructions only).
    Ok(InstallSource::Unknown)
}

/// A literal upgrade command for the owning package manager, or None. Never
/// prose disguised as a command.
fn upgrade_command(source: InstallSource, config: &AppConfig) -> Option<String> {
    let crate_name = &config.update.crate_name;
    let formula = &config.update.formula;
    match source {
        InstallSource::Homebrew => {
            if config.update.tap.trim().is_empty() {
                Some(format!("brew upgrade {formula}"))
            } else {
                Some(format!("brew upgrade {}/{formula}", config.update.tap))
            }
        }
        InstallSource::Cargo => Some(format!("cargo install --locked --force {crate_name}")),
        InstallSource::CargoBinstall => Some(format!("cargo binstall --no-confirm {crate_name}")),
        InstallSource::Npm => Some(format!("npm update -g {crate_name}")),
        InstallSource::Bun => Some(format!("bun update --global {crate_name}")),
        InstallSource::UvTool => Some(format!("uv tool upgrade {crate_name}")),
        InstallSource::Pipx => Some(format!("pipx upgrade {crate_name}")),
        InstallSource::Winget => Some(format!("winget upgrade --id {crate_name}")),
        InstallSource::Scoop => Some(format!("scoop update {crate_name}")),
        InstallSource::Apt => Some(format!(
            "sudo apt update && sudo apt install --only-upgrade {crate_name}"
        )),
        InstallSource::Auto
        | InstallSource::Standalone
        | InstallSource::Managed
        | InstallSource::Unknown => None,
    }
}

/// Configured identifiers end up in a shell command: reject anything that
/// isn't a plain package name.
fn validate_package_identifier(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@/._+-".contains(c))
    {
        return Err(AppError::Config(
            "invalid update package identifier; use a package name without whitespace, shell syntax, or leading '-'".into(),
        ));
    }
    Ok(())
}

/// The latest published release version (without a leading `v`), read from
/// GitHub. Read-only: nothing is downloaded or replaced.
fn latest_release(config: &AppConfig) -> Result<String, AppError> {
    let release = self_update::backends::github::Update::configure()
        .repo_owner(&config.update.owner)
        .repo_name(&config.update.repo)
        .bin_name(env!("CARGO_BIN_NAME"))
        .current_version(env!("CARGO_PKG_VERSION"))
        .build()
        .and_then(|u| u.get_latest_release())
        .map_err(|e| AppError::Update(format!("could not read the latest release: {e}")))?;
    Ok(release.version.trim_start_matches('v').to_string())
}

pub fn run(ctx: Ctx, _check: bool, config: &AppConfig) -> Result<(), AppError> {
    let current = env!("CARGO_PKG_VERSION");
    let source = detect_install_source(config)?;
    let mut result = UpdateResult {
        current_version: current.into(),
        latest_version: None,
        status: "not_checked".into(),
        install_source: source.as_str().into(),
        update_mode: "instructions_only".into(),
        upgrade_command: None,
        release_url: Some(format!(
            "https://github.com/{}/{}/releases/latest",
            config.update.owner, config.update.repo
        )),
        requires_skill_reinstall: false,
    };

    match source {
        _ if !config.update.enabled => {
            result.status = "disabled".into();
            result.update_mode = "disabled".into();
        }
        InstallSource::Managed => {
            result.status = "disabled".into();
            result.update_mode = "disabled".into();
        }
        InstallSource::Unknown | InstallSource::Auto => {}
        InstallSource::Standalone => {
            let latest = latest_release(config)?;
            let newer = self_update::version::bump_is_greater(current, &latest).unwrap_or(false);
            result.status = if newer {
                "update_available"
            } else {
                "up_to_date"
            }
            .into();
            result.release_url = Some(format!(
                "https://github.com/{}/{}/releases/tag/v{latest}",
                config.update.owner, config.update.repo
            ));
            result.latest_version = Some(latest);
            result.requires_skill_reinstall = newer;
        }
        InstallSource::Homebrew => {
            validate_package_identifier(&config.update.formula)?;
            if !config.update.tap.is_empty() {
                validate_package_identifier(&config.update.tap)?;
            }
            result.status = "managed_install".into();
            result.update_mode = "package_manager".into();
            result.upgrade_command = upgrade_command(source, config);
        }
        _ => {
            validate_package_identifier(&config.update.crate_name)?;
            result.status = "managed_install".into();
            result.update_mode = "package_manager".into();
            result.upgrade_command = upgrade_command(source, config);
        }
    }

    let name = env!("CARGO_BIN_NAME");
    output::print_success_or(ctx, &result, |r| match r.status.as_str() {
        "disabled" => println!("Updates are disabled by configuration or the installation owner"),
        "up_to_date" => println!("Up to date (v{})", r.current_version),
        "update_available" => {
            let latest = r.latest_version.as_deref().unwrap_or("?");
            println!("Update available: v{} -> v{latest}", r.current_version);
            if let Some(url) = &r.release_url {
                println!("Download: {url}");
            }
            println!("Then run `{name} skill install` to refresh agent skills");
        }
        _ => {
            println!("Installed via {}", r.install_source);
            if let Some(command) = &r.upgrade_command {
                println!("Update with: {command}");
            } else if let Some(url) = &r.release_url {
                println!("Release instructions: {url}");
            }
        }
    })
}
