//! Machine-readable capability manifest.
//!
//! Syntax comes from the same Clap tree used to execute commands, so
//! discovery cannot drift from the binary. Semantic annotations (effects,
//! examples, exit behaviour) stay explicit and are tested for coverage.
//!
//! The default manifest lists each command as one generated signature line
//! plus the intended workflow; `--command PATH` returns the selected
//! commands in full.
use std::fmt::Write;

use clap::{Arg, ArgAction, Command, CommandFactory};
use serde_json::{Map, Value, json};

use crate::cli::{Cli, own_args};
use crate::{error::AppError, output};

const BIN: &str = env!("CARGO_BIN_NAME");

fn argument(arg: &Arg) -> Value {
    let boolean = matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse);
    let mut value = json!({
        "name": arg.get_long().map(|s| format!("--{s}"))
            .unwrap_or_else(|| arg.get_id().to_string()),
        "type": if boolean { "bool" } else { "string" },
        "required": arg.is_required_set(),
        "description": arg.get_help().map(ToString::to_string).unwrap_or_default(),
    });
    if arg.is_positional() {
        value["kind"] = json!("positional");
    }
    if let Some(short) = arg.get_short() {
        value["short"] = json!(format!("-{short}"));
    }
    if matches!(arg.get_action(), ArgAction::Append) {
        value["repeatable"] = json!(true);
    }
    if let Some(default) = arg.get_default_values().first() {
        let default = default.to_string_lossy();
        value["default"] = if boolean {
            json!(default == "true")
        } else {
            json!(default)
        };
    }
    // SetTrue/SetFalse are switches, not value-taking boolean options.
    if let Some(values) = arg
        .get_value_parser()
        .possible_values()
        .filter(|_| !boolean)
    {
        value["values"] = values
            .filter(|v| !v.is_hide_set())
            .map(|v| json!(v.get_name()))
            .collect();
    }
    value
}

/// Every public leaf command by canonical path ("profile build"), in Clap order.
fn leaves<'a>(root: &'a Command, prefix: &str, result: &mut Vec<(String, &'a Command)>) {
    for command in root.get_subcommands().filter(|c| !c.is_hide_set()) {
        // Clap's generated help command is presentation, not a domain command.
        if command.get_name() == "help" {
            continue;
        }
        let path = format!("{prefix}{}", command.get_name());
        if command.has_subcommands() {
            leaves(command, &format!("{path} "), result);
        } else {
            result.push((path, command));
        }
    }
}

/// The command in full: description, args, options, defaults, values, aliases.
fn full(command: &Command) -> Value {
    let (args, options): (Vec<&Arg>, Vec<&Arg>) =
        own_args(command).partition(|a| a.is_positional());
    let mut value = json!({
        "description": command.get_about().map(ToString::to_string).unwrap_or_default(),
        "args": args.into_iter().map(argument).collect::<Vec<_>>(),
        "options": options.into_iter().map(argument).collect::<Vec<_>>(),
    });
    let aliases: Vec<_> = command.get_visible_aliases().collect();
    if !aliases.is_empty() {
        value["aliases"] = json!(aliases);
    }
    value
}

/// One line for the default manifest: `<name> --corpus=<corpus> --force: Build ...`.
/// `<x>` required, `[x]` optional, `...` repeats, `--flag=` takes a value,
/// `--flag=<flag>` is a required flag, `--flag` alone is a switch.
pub fn line(command: &Command) -> String {
    let mut line = String::new();
    for arg in own_args(command) {
        let id = arg.get_id().as_str();
        let many = if matches!(arg.get_action(), ArgAction::Append) {
            "..."
        } else {
            ""
        };
        if arg.is_positional() {
            let (open, close) = if arg.is_required_set() {
                ('<', '>')
            } else {
                ('[', ']')
            };
            let _ = write!(line, "{open}{id}{many}{close} ");
        } else if let Some(long) = arg.get_long() {
            let value = match (arg.get_action().takes_values(), arg.is_required_set()) {
                (false, _) => String::new(),
                (true, false) => "=".into(),
                (true, true) => format!("=<{id}>"),
            };
            let _ = write!(line, "--{long}{value}{many} ");
        }
    }
    let about = command
        .get_about()
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut line = match (line.trim_end(), about.as_str()) {
        ("", about) => about.to_string(),
        (syntax, "") => syntax.to_string(),
        (syntax, about) => format!("{syntax}: {about}"),
    };
    let aliases: Vec<_> = command.get_visible_aliases().collect();
    if !aliases.is_empty() {
        let _ = write!(line, " (alias: {})", aliases.join(", "));
    }
    line
}

/// Claims about behaviour, not syntax: every public leaf command has an
/// `effect`; examples must parse against the real CLI (tested).
fn annotations() -> Value {
    let read = |examples: Value| json!({"effect": "read", "examples": examples});
    json!({
        "profile build": {
            "effect": "write",
            "effect_detail": "Saves the profile to the data dir; an existing name needs --force",
            "examples": [["profile", "build", "ada", "--corpus", "./ada-books/"]]
        },
        "profile blend": {
            "effect": "write",
            "effect_detail": "Saves a blended target voice; it can be analysed, guided and checked, never calibrated",
            "examples": [["profile", "blend", "house", "--from", "ada=0.6", "--from", "ben=0.4"]]
        },
        "profile list": read(json!([["profile", "list"], ["profile", "list", "--all"]])),
        "profile show": read(json!([["profile", "show", "ada"]])),
        "profile rebuild": {
            "effect": "write",
            "effect_detail": "Re-analyses the stored text after an upgrade; drops calibration (run calibrate again)",
            "examples": [["profile", "rebuild", "ada"]]
        },
        "profile remove": {
            "effect": "write",
            "effect_detail": "Deletes the profile file; requires --confirm",
            "examples": [["profile", "remove", "ada", "--confirm"]]
        },
        "analyze": read(json!([["analyze", "ada"], ["analyze", "./drafts/", "-n", "40"]])),
        "guide": {
            "effect": "mixed",
            "effect_detail": "Returns the Markdown brief; also writes it to --out when given",
            "examples": [["guide", "ada", "-o", "ada-brief.md"], ["guide", "ada", "--about", "a launch post for our new app"]]
        },
        "examples": {
            "effect": "read",
            "effect_detail": "Returns the author's passages or posts nearest the piece being written",
            "examples": [["examples", "ada", "--about", "a thread on hiring", "-n", "5"], ["examples", "ada", "--like", "draft.md"]]
        },
        "check": {
            "effect": "read",
            "examples": [
                ["check", "chapter-3.md", "--profile", "ada"],
                ["check", "chapter-3.md", "--profile", "ada", "--avoid", "banned.txt", "--strict"]
            ],
            "exit_behavior": "0 with the report; with --strict a failed check exits 5 with the report in error.details"
        },
        "diff": read(json!([["diff", "ada", "ben"]])),
        "export": {
            "effect": "write",
            "effect_detail": "Writes JSONL fine-tuning examples to --out",
            "examples": [["export", "ada", "-o", "ada.jsonl"]]
        },
        "compare": read(json!([["compare", "unknown.txt", "--profile", "ada"]])),
        "calibrate": {
            "effect": "write",
            "effect_detail": "Stores the fitted verifier in the profile; re-run after adding profiles",
            "examples": [["calibrate", "ada"]]
        },
        "background fetch": {
            "effect": "write",
            "effect_detail": "Downloads public-domain books from Project Gutenberg and saves background profiles; skips installed ones unless --force",
            "examples": [["background", "fetch"]]
        },
        "background status": read(json!([["background", "status"]])),
        "agent-info": read(json!([["agent-info", "--command", "check"], ["agent-info", "--command", "profile"]])),
        "skill install": {
            "effect": "write",
            "effect_detail": "Writes the embedded skill to the Claude Code, Codex and Gemini skill directories",
            "examples": [["skill", "install"]]
        },
        "skill status": read(json!([["skill", "status"]])),
        "config show": read(json!([["config", "show"]])),
        "config path": read(json!([["config", "path"]])),
        "doctor": {
            "effect": "read",
            "examples": [["doctor"]],
            "exit_behavior": "0 with no failed checks; 2 with the report in error.details"
        },
        "update": {
            "effect": "read",
            "effect_detail": "Standalone installs: reads the latest GitHub release. Package-managed: returns the manager's upgrade command. Never replaces the binary",
            "examples": [["update", "--check"]],
            "install_sources": ["standalone", "homebrew", "cargo", "cargo_binstall", "npm", "bun", "uv_tool", "pipx", "winget", "scoop", "apt", "managed", "unknown"],
            "data_fields": ["current_version", "latest_version", "status", "install_source", "update_mode", "upgrade_command", "release_url", "requires_skill_reinstall"]
        }
    })
}

/// The intended loop, in order. Short on purpose: the full manifest is read
/// once per session.
fn workflow() -> Value {
    json!([
        format!(
            "{BIN} profile build <name> --corpus <path>  (once per voice: .md/.txt books/essays/blog posts, an X archive or xmaster JSON, JSON/CSV exports, .eml; --register fiction|nonfiction|blog|social|email|message when detection is wrong; blend voices with profile blend)"
        ),
        format!("{BIN} analyze <name>  (signature/avoided words, word choices, rhythm targets; per-item habits for posts)"),
        format!("{BIN} guide <name> --about \"<what you're writing>\" [--as post|reply|thread]  (the brief: targets, word choices, ban list, matched examples)"),
        format!("{BIN} examples <name> --about \"<topic>\"  (more of the author's own pieces like the one you're writing; imitate, never copy)"),
        "write the draft (separate several posts with --- lines)".to_string(),
        format!(
            "{BIN} check draft.md --profile <name> [--as post|reply|thread]  (follow `plan`, fix hard findings first, re-check until pass; add --verify on the final pass, --strict in a gate)"
        ),
        format!(
            "authorship: {BIN} background fetch (once; books), {BIN} calibrate <name>, {BIN} compare text.txt --profile <name>; posts verify only as a --- batch of 10+ against 3+ other posters built with --background"
        ),
    ])
}

/// Every public leaf command in full, with its semantic annotations.
fn entries(root: &Command) -> Result<Map<String, Value>, AppError> {
    let mut paths = Vec::new();
    leaves(root, "", &mut paths);
    let mut entries: Map<String, Value> = paths
        .into_iter()
        .map(|(path, command)| (path, full(command)))
        .collect();
    for (path, value) in annotations().as_object().expect("static annotation object") {
        let entry = entries
            .get_mut(path)
            .ok_or_else(|| AppError::Config(format!("metadata names missing command: {path}")))?;
        entry
            .as_object_mut()
            .expect("command entries are objects")
            .extend(value.as_object().expect("annotations are objects").clone());
    }
    if entries.values().any(|entry| entry.get("effect").is_none()) {
        return Err(AppError::Config(
            "add effect metadata for every public command".into(),
        ));
    }
    Ok(entries)
}

fn root() -> Command {
    let mut root = Cli::command();
    root.build();
    root
}

/// `agent-info --command PATH`: name, version, and the selected commands in
/// full. A leaf path selects itself; a group selects its descendants.
pub fn scoped(filter: &str) -> Result<Value, AppError> {
    let path = filter.split_whitespace().collect::<Vec<_>>().join(" ");
    if path.is_empty() {
        return Err(AppError::input(
            "command path cannot be empty",
            format!("Name a command, e.g. {BIN} agent-info --command check"),
        ));
    }
    let mut entries = entries(&root())?;
    let prefix = format!("{path} ");
    entries.retain(|key, _| key == &path || key.starts_with(&prefix));
    if entries.is_empty() {
        return Err(AppError::input(
            format!("unknown command path: {path}"),
            format!("Use a key from `{BIN} agent-info`, e.g. {BIN} agent-info --command check"),
        ));
    }
    Ok(json!({
        "name": BIN,
        "version": env!("CARGO_PKG_VERSION"),
        "commands": entries,
    }))
}

/// The default manifest: everything an agent needs to start, with one
/// generated line per command.
pub fn manifest() -> Result<Value, AppError> {
    let root = root();
    entries(&root)?; // annotation coverage holds in both views
    let mut paths = Vec::new();
    leaves(&root, "", &mut paths);
    let lines: Map<String, Value> = paths
        .into_iter()
        .map(|(path, command)| (path, json!(line(command))))
        .collect();
    let globals: Map<String, Value> = root
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| {
            let mut value = argument(a);
            let name = value
                .as_object_mut()
                .and_then(|o| o.remove("name"))
                .and_then(|n| n.as_str().map(String::from))
                .unwrap_or_default();
            (name, value)
        })
        .collect();
    Ok(json!({
        "name": BIN,
        "version": env!("CARGO_PKG_VERSION"),
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "workflow": workflow(),
        "commands": lines,
        "global_flags": globals,
        "exit_codes": {
            "0": "Success",
            "1": "Runtime/transient failure; inspect outcome before retrying writes",
            "2": "Setup/configuration (missing or outdated profile, no background); run the suggested fix",
            "3": "Invalid input; fix the request",
            "4": "Rate limited; respect backoff",
            "5": "check --strict: the draft failed the voice check (report in error.details); revise it and re-run, don't retry unchanged"
        },
        "envelope": {
            "version": "1",
            "success": "{ version, status, data, metadata?: { next: [command] } }",
            "error": "{ version, status, error: { code, message, suggestion, retryable?, details? } }"
        },
        "config": {
            "path": crate::config::config_path().display().to_string(),
            "env_prefix": format!("{}_", BIN.to_uppercase()),
            "data_dir": crate::engine::store::data_dir().display().to_string(),
            "data_dir_env": "STYLOMETRY_DATA_DIR"
        },
        "auto_json_when_piped": true
    }))
}

pub fn run(filter: Option<&str>) -> Result<(), AppError> {
    match filter {
        Some(path) => output::print_json(&scoped(path)?),
        None => output::print_json(&manifest()?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn every_example_parses_against_the_real_cli() {
        for (path, entry) in entries(&root()).unwrap() {
            let examples = entry["examples"].as_array();
            let examples = examples.unwrap_or_else(|| panic!("{path}: add examples"));
            for example in examples {
                let argv = std::iter::once(BIN).chain(
                    example
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap()),
                );
                assert!(Cli::try_parse_from(argv).is_ok(), "bad example: {example}");
            }
        }
    }

    #[test]
    fn effects_use_the_schema_vocabulary() {
        for (path, entry) in entries(&root()).unwrap() {
            let effect = entry["effect"].as_str().unwrap();
            assert!(
                ["read", "write", "mixed"].contains(&effect),
                "{path}: {effect}"
            );
            if effect == "mixed" {
                assert!(entry["effect_detail"].is_string(), "{path}: explain mixed");
            }
        }
    }

    #[test]
    fn boolean_switches_do_not_advertise_value_syntax() {
        let value = manifest().unwrap();
        for flag in value["global_flags"].as_object().unwrap().values() {
            assert!(flag.get("values").is_none());
        }
        assert_eq!(
            value["commands"]["update"],
            "--check: Check for a newer release and show how to upgrade this install (never replaces the binary)"
        );
        assert!(Cli::try_parse_from([BIN, "update", "--check"]).is_ok());
        assert!(Cli::try_parse_from([BIN, "update", "--check=true"]).is_err());
    }

    #[test]
    fn lines_render_required_optional_repeated_and_switch_syntax() {
        let command = Command::new("send")
            .about("Send a message")
            .visible_alias("s")
            .arg(Arg::new("to").required(true))
            .arg(Arg::new("files").action(ArgAction::Append))
            .arg(Arg::new("channel").long("channel").required(true))
            .arg(Arg::new("tag").long("tag").action(ArgAction::Append))
            .arg(Arg::new("limit").long("limit").short('n'))
            .arg(Arg::new("dry").long("dry-run").action(ArgAction::SetTrue));
        assert_eq!(
            line(&command),
            "<to> [files...] --channel=<channel> --tag=... --limit= --dry-run: Send a message (alias: s)"
        );
    }
}
