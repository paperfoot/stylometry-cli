use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "stylometry",
    version,
    about = "Analyse a writer's voice, brief an agent to write in it, and check drafts against it"
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
    /// Build, list, show, rebuild, and remove voice profiles
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Voice report: signature and avoided words, synonym habits, rhythm, punctuation
    Analyze {
        /// Profile name, or a path to a file/folder to analyse without saving a profile
        source: String,
        /// Items per list (signature words, phrases, ...)
        #[arg(short = 'n', long, default_value_t = 25)]
        limit: usize,
        /// Passage length (words) the target ranges are measured at, e.g. a scene or a chapter
        #[arg(long, default_value_t = 1000)]
        window: usize,
    },
    /// Style brief (Markdown) an agent can follow to write in a profile's voice
    Guide {
        /// Profile name
        profile: String,
        /// Exemplar passages from the author's own text to include
        #[arg(long, default_value_t = 3)]
        exemplars: usize,
        /// Items per word list
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
        /// Passage length (words) the target ranges are measured at, e.g. a scene or a chapter
        #[arg(long, default_value_t = 1000)]
        window: usize,
        /// Write the brief to this file instead of only returning it
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
    /// Check a draft against a voice: off-voice words with the author's preferred alternative, LLM tells, rhythm drift, and a voice-match score
    Check {
        /// Draft file ('-' reads stdin)
        file: Option<PathBuf>,
        /// Profile to check against
        #[arg(short = 'p', long)]
        profile: String,
        /// Inline text instead of a file
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        /// Maximum findings to return, most important first
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
        /// File of banned words/phrases (one per line); any hit fails the check
        #[arg(long, value_name = "FILE")]
        avoid: Vec<PathBuf>,
        /// Extra profile whose text the draft must not copy (the target's own text is always guarded)
        #[arg(long, value_name = "PROFILE")]
        guard: Vec<String>,
        /// Gate mode: exit 5 with the report in error.details when the check fails
        #[arg(long)]
        strict: bool,
    },
    /// Contrast two voices: what each uses that the other doesn't
    Diff {
        /// First profile name or path
        a: String,
        /// Second profile name or path
        b: String,
        /// Items per list
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Export a profile's passages as fine-tuning data (JSONL continuation pairs)
    Export {
        /// Profile name
        profile: String,
        /// Output file (JSONL, one example per line)
        #[arg(short = 'o', long)]
        out: PathBuf,
        /// Example shape: chat messages or prompt/completion
        #[arg(long, value_enum, default_value = "chat")]
        format: ExportFormat,
    },
    /// Authorship verification: was this text written by the profile's author?
    Compare {
        /// Text file to score ('-' reads stdin)
        file: Option<PathBuf>,
        /// Profile to compare against
        #[arg(short = 'p', long)]
        profile: String,
        /// Inline text instead of a file
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
    },
    /// Fit a profile's verifier (P(same author) per text length) against background and other profiles
    Calibrate {
        /// Profile to calibrate
        profile: String,
    },
    /// Manage the background of reference authors (public-domain books) used for calibration and comparison
    Background {
        #[command(subcommand)]
        action: BackgroundAction,
    },
    /// Machine-readable capability manifest
    #[command(visible_alias = "info")]
    AgentInfo {
        /// Inspect a canonical command or group, e.g. "check" or "profile build"
        #[arg(long, value_name = "PATH")]
        command: Option<String>,
    },
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
    /// Diagnose setup: data dir, profiles, background, calibration state
    Doctor,
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
    /// Build a profile from a corpus (a file, or a folder of .md/.txt; each file is one work)
    Build {
        /// Profile name (letters, digits, '-', '_')
        name: String,
        /// Corpus path: a file or a directory
        #[arg(long)]
        corpus: PathBuf,
        /// Mark as a background (reference) author rather than a voice to write in
        #[arg(long)]
        background: bool,
        /// Overwrite an existing profile
        #[arg(long)]
        force: bool,
    },
    /// Blend weighted profiles into a new target voice, e.g. --from hoover=0.6 --from fisher=0.4
    Blend {
        /// Name for the blended voice
        name: String,
        /// Component as PROFILE=SHARE (repeat; shares are normalized)
        #[arg(long = "from", value_name = "PROFILE=SHARE", required = true)]
        from: Vec<String>,
        /// Overwrite an existing profile
        #[arg(long)]
        force: bool,
    },
    /// List voice profiles
    List {
        /// Include background (reference) profiles
        #[arg(long)]
        all: bool,
    },
    /// Show a profile's summary: size, works, calibration
    Show {
        /// Profile name
        name: String,
    },
    /// Re-analyse a profile from its stored text (after an upgrade); drops calibration
    Rebuild {
        /// Profile name
        name: String,
    },
    /// Remove a profile
    Remove {
        /// Profile name
        name: String,
        /// Confirm the deletion
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Subcommand)]
pub enum BackgroundAction {
    /// Download and build the reference authors from Project Gutenberg (one-time, ~40 books)
    Fetch {
        /// Rebuild background profiles that already exist
        #[arg(long)]
        force: bool,
    },
    /// Show which reference authors are installed
    Status,
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

#[derive(Clone, Copy, ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// {"messages": [system, user (preceding text), assistant (continuation)]}
    Chat,
    /// {"prompt": preceding text, "completion": continuation}
    Completion,
}
