use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Root `--help` footer. Agents read --help first: the loop, real
/// copy-pasteable examples, and where per-command detail lives.
const HELP_FOOTER: &str = "\
Tips:
  • The loop: profile build -> analyze -> guide (brief the writer) -> write -> check -> revise
  • `stylometry agent-info --command check` shows one command's flags, defaults and values
  • Piped output is one JSON envelope; suggested follow-up commands are in metadata.next
  • `check --strict` exits 5 when a draft fails (report in error.details): revise, don't rerun unchanged
  • Run `stylometry background fetch` once before calibrate/compare (authorship verification)
  • `stylometry doctor` diagnoses the data dir, outdated profiles and calibration

Examples:
  stylometry profile build ada --corpus ./ada-books/
    Build a voice profile from a file or folder of .md/.txt (each file is one work)

  stylometry guide ada -o ada-brief.md
    Write the style brief an agent follows to draft in Ada's voice

  stylometry check chapter-3.md --profile ada --strict
    Off-voice words with Ada's preferred alternative, LLM tells, rhythm; exit 5 on failure

  stylometry compare unknown.txt --profile ada
    Was this written by Ada? P(same author) once `stylometry calibrate ada` has run";

#[derive(Parser)]
#[command(
    name = "stylometry",
    version,
    about = "Analyse a writer's voice, brief an agent to write in it, and check drafts against it",
    after_long_help = HELP_FOOTER
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
        /// Exemplar passages (or posts) from the author's own text to include
        #[arg(long, default_value_t = 3)]
        exemplars: usize,
        /// The piece being written (a draft or outline file): exemplars are chosen to match it
        #[arg(long = "for", value_name = "FILE", conflicts_with = "about")]
        for_file: Option<PathBuf>,
        /// What is being written, in a few words: exemplars are chosen to match it
        #[arg(long, value_name = "TEXT")]
        about: Option<String>,
        /// Write the brief for this kind of writing (default: the profile's own register)
        #[arg(long, value_enum)]
        register: Option<RegisterArg>,
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
    /// The author's own passages or posts most like what you're writing: few-shot examples for an agent
    Examples {
        /// Profile name
        profile: String,
        /// A draft or outline file to match
        #[arg(long, value_name = "FILE", conflicts_with = "about")]
        like: Option<PathBuf>,
        /// What is being written, in a few words, to match
        #[arg(long, value_name = "TEXT")]
        about: Option<String>,
        /// How many examples
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: usize,
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
    /// Check for a newer release and show how to upgrade this install (never replaces the binary)
    Update {
        /// Check only (both forms are read-only)
        #[arg(long)]
        check: bool,
    },
    /// Hidden: deterministic exit-code trigger for contract tests
    #[command(hide = true)]
    Contract {
        /// Exit code to trigger (0-5)
        code: i32,
    },
}

#[derive(Subcommand)]
pub enum ProfileAction {
    /// Build a profile from a corpus: .md/.txt prose (each file one work), an X archive tweets.js, xmaster/JSON/JSONL/CSV exports, or .eml emails
    Build {
        /// Profile name (letters, digits, '-', '_')
        name: String,
        /// Corpus path: a file or a directory
        #[arg(long)]
        corpus: PathBuf,
        /// Kind of writing (default: detected from the corpus format and content)
        #[arg(long, value_enum)]
        register: Option<RegisterArg>,
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

/// A command's own arguments: no globals, hidden args, or help/version.
pub fn own_args(command: &clap::Command) -> impl Iterator<Item = &clap::Arg> {
    use clap::ArgAction;
    command.get_arguments().filter(|a| {
        !a.is_hide_set()
            && !a.is_global_set()
            && !matches!(
                a.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            )
    })
}

/// Longest flag or subcommand list a parse-error hint spells out; past it,
/// the hint names the scoped `agent-info` lookup instead.
const INLINE_LIMIT: usize = 12;

/// What the command at `path` accepts, in one sentence: its flags, or the
/// subcommands of a group, so a retry needs no lookup.
pub fn accepts(command: &clap::Command, path: &str) -> String {
    let bin = env!("CARGO_BIN_NAME");
    let shown = format!("{bin} {path}").trim_end().to_string();
    let (kind, names): (&str, Vec<String>) = if command.has_subcommands() {
        let names = command
            .get_subcommands()
            .filter(|c| !c.is_hide_set() && c.get_name() != "help")
            .map(|c| c.get_name().to_string());
        ("Commands", names.collect())
    } else {
        let names = own_args(command).filter_map(|a| {
            let long = a.get_long()?;
            Some(match a.get_short() {
                Some(short) => format!("-{short}/--{long}"),
                None => format!("--{long}"),
            })
        });
        ("Flags", names.collect())
    };
    match names.len() {
        0 if kind == "Flags" => format!("`{shown}` takes no flags."),
        n if n <= INLINE_LIMIT => format!("{kind} for `{shown}`: {}.", names.join(", ")),
        _ if path.is_empty() => format!("List commands with: {bin} agent-info"),
        _ => format!(
            "See all {}: {bin} agent-info --command \"{path}\"",
            kind.to_lowercase()
        ),
    }
}

/// A parse error as `(message, suggestion)` for the error envelope. The
/// message is clap's text without its `error:` prefix, usage block and
/// `--help` footer; the suggestion names what the deepest command in `args`
/// accepts, plus its usage line. Aliases resolve to canonical names.
pub fn explain_parse_error(err: &clap::Error, args: &[String]) -> (String, String) {
    use clap::CommandFactory;
    let mut root = Cli::command();
    root.build();
    let mut command = &root;
    let mut path = Vec::new();
    for arg in args {
        if arg == "--" || !command.has_subcommands() {
            break;
        }
        if let Some(sub) = command.find_subcommand(arg) {
            path.push(sub.get_name());
            command = sub;
        }
    }
    let path = path.join(" ");

    let text = err.to_string();
    let message = if err.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    {
        // clap's text here is the whole help page; say what is missing.
        let shown = format!("{} {path}", env!("CARGO_BIN_NAME"));
        format!("`{}` needs a subcommand", shown.trim_end())
    } else {
        let mut message = String::new();
        for line in text
            .lines()
            .map(str::trim)
            .take_while(|l| !l.starts_with("Usage:"))
            .filter(|l| !l.is_empty() && !l.starts_with("For more information"))
        {
            if !message.is_empty() {
                message.push_str(if message.ends_with(':') { " " } else { "; " });
            }
            message.push_str(line.strip_prefix("error: ").unwrap_or(line));
        }
        message
    };

    let mut suggestion = accepts(command, &path);
    let usage = text.lines().find_map(|l| l.trim().strip_prefix("Usage: "));
    if let Some(usage) = usage.filter(|_| !command.has_subcommands()) {
        suggestion.push_str(&format!(" Usage: {usage}"));
    }
    (message, suggestion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, ArgAction, Command};

    #[test]
    fn long_flag_lists_point_at_scoped_discovery() {
        let mut wide = Command::new("wide");
        for i in 0..=INLINE_LIMIT {
            let name: &'static str = format!("f{i}").leak();
            wide = wide.arg(Arg::new(name).long(name).action(ArgAction::SetTrue));
        }
        assert_eq!(
            accepts(&wide, "wide"),
            "See all flags: stylometry agent-info --command \"wide\""
        );
        let narrow = Command::new("narrow").arg(Arg::new("n").long("limit").short('n'));
        assert_eq!(
            accepts(&narrow, "narrow"),
            "Flags for `stylometry narrow`: -n/--limit."
        );
        assert_eq!(
            accepts(&Command::new("bare"), "bare"),
            "`stylometry bare` takes no flags."
        );
    }
}

/// Kind of writing a profile holds or a brief targets.
#[derive(Clone, Copy, ValueEnum)]
pub enum RegisterArg {
    Fiction,
    Nonfiction,
    Blog,
    /// X/Threads/LinkedIn posts
    Social,
    Email,
    /// Chat and text messages
    Message,
}

impl From<RegisterArg> for crate::engine::profile::Register {
    fn from(r: RegisterArg) -> Self {
        use crate::engine::profile::Register as R;
        match r {
            RegisterArg::Fiction => R::Fiction,
            RegisterArg::Nonfiction => R::Nonfiction,
            RegisterArg::Blog => R::Blog,
            RegisterArg::Social => R::Social,
            RegisterArg::Email => R::Email,
            RegisterArg::Message => R::Message,
        }
    }
}
