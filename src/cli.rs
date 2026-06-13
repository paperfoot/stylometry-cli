use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::engine;

#[derive(Parser)]
#[command(
    name = "stylometry",
    version,
    about = "Forensic-grade stylometry: author profiles + calibrated authorship verification"
)]
pub struct Cli {
    /// Force JSON output even in a terminal
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress informational output
    #[arg(long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Build, list, show, and remove author profiles
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Compare a text against a profile and return a verdict
    Compare {
        /// Profile to compare against
        #[arg(long)]
        profile: String,
        /// Path to a text file to score
        file: Option<PathBuf>,
        /// Inline text instead of a file
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
    },
    /// Calibrate a profile's verifier against the other profiles (imposters)
    Calibrate {
        /// Profile to calibrate
        name: String,
    },
    /// Machine-readable capability manifest
    #[command(visible_alias = "info")]
    AgentInfo,
    /// Manage skill file installation
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Distribution-aware update check/apply
    Update {
        /// Check only, don't install
        #[arg(long)]
        check: bool,
    },
    /// Hidden: deterministic exit-code trigger for contract tests
    #[command(hide = true)]
    Contract {
        /// Exit code to trigger (0-4)
        code: i32,
    },
}

#[derive(Subcommand)]
pub enum ProfileAction {
    /// Build a profile by analysing a corpus (a file or a directory of .md/.txt)
    Build {
        /// Profile name (letters, digits, '-', '_')
        name: String,
        /// Corpus path: a file or a directory
        #[arg(long)]
        corpus: PathBuf,
        /// Words per analysis chunk
        #[arg(long, default_value_t = engine::DEFAULT_CHUNK_WORDS)]
        chunk_size: usize,
        /// Overwrite an existing profile
        #[arg(long)]
        force: bool,
    },
    /// List all profiles
    List,
    /// Show a profile's fingerprint summary
    Show {
        /// Profile name
        name: String,
    },
    /// Remove a profile
    Remove {
        /// Profile name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum SkillAction {
    /// Write skill file to all detected agent platforms
    Install,
    /// Check which platforms have the skill installed
    Status,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Display effective merged configuration
    Show,
    /// Print configuration file path
    Path,
}
