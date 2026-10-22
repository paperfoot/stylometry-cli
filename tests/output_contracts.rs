//! Output contracts: compact JSON envelopes on pipes, plain-text help, parse
//! errors that name the failing command's flags, `retryable`, `metadata.next`.
//!
//! assert_cmd runs the binary in a pipe (not a TTY), so JSON auto-detection
//! applies without `--json`.

use assert_cmd::Command;
use serde_json::Value;

fn bin(home: &std::path::Path) -> Command {
    let mut cmd = {
        // Offline and deterministic: never call JEV from tests.
        let mut c = Command::cargo_bin("stylometry").unwrap();
        c.env("STYLOMETRY_JEV__ENABLED", "false");
        c
    };
    cmd.env("HOME", home)
        .env("STYLOMETRY_DATA_DIR", home.join("data"));
    cmd
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(args).output().unwrap();
    Out {
        code: out.status.code(),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

fn error_of(args: &[&str]) -> Value {
    let out = run(args);
    assert!(out.stdout.is_empty(), "{args:?}: errors never write stdout");
    serde_json::from_str(&out.stderr).expect("stderr should be one JSON error envelope")
}

// ── Envelopes ──────────────────────────────────────────────────────────────

#[test]
fn success_envelope_is_one_compact_line() {
    let out = run(&["config", "path"]);
    assert_eq!(out.code, Some(0));
    assert_eq!(out.stdout.lines().count(), 1, "compact JSON");
    let json: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(json["version"], "1");
    assert_eq!(json["status"], "success");
    assert!(json["data"]["path"].is_string());
    assert!(json["data"]["exists"].is_boolean());
}

#[test]
fn quiet_never_suppresses_json() {
    let out = run(&["config", "path", "--json", "--quiet"]);
    let json: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(json["status"], "success");
}

#[test]
fn error_envelope_shape_and_codes() {
    let json = error_of(&["contract", "3"]);
    assert_eq!(json["version"], "1");
    assert_eq!(json["status"], "error");
    for field in ["code", "message", "suggestion"] {
        assert!(json["error"][field].is_string(), "{field}");
    }
    assert_eq!(
        error_of(&["contract", "2"])["error"]["code"],
        "config_error"
    );
    assert_eq!(
        error_of(&["contract", "4"])["error"]["code"],
        "rate_limited"
    );
}

#[test]
fn retryable_says_whether_an_unchanged_retry_can_succeed() {
    assert_eq!(error_of(&["contract", "3"])["error"]["retryable"], false);
    assert_eq!(error_of(&["contract", "2"])["error"]["retryable"], false);
    assert_eq!(error_of(&["contract", "4"])["error"]["retryable"], true);
    // A transient failure may have written something: no blanket answer.
    assert!(
        error_of(&["contract", "1"])["error"]
            .get("retryable")
            .is_none()
    );
}

#[test]
fn success_with_follow_ups_puts_them_in_metadata_next() {
    let out = run(&["doctor"]);
    assert_eq!(out.code, Some(0));
    let json: Value = serde_json::from_str(&out.stdout).unwrap();
    let next = json["metadata"]["next"].as_array().unwrap();
    assert!(next.iter().any(|n| n == "stylometry background fetch"));
    assert!(json["data"]["checks"].is_array());
}

// ── Help and version ───────────────────────────────────────────────────────

#[test]
fn piped_help_is_plain_text_with_tips_and_real_examples() {
    let out = run(&["--help"]);
    assert_eq!(out.code, Some(0));
    assert!(
        serde_json::from_str::<Value>(&out.stdout).is_err(),
        "help is text"
    );
    let tips = out.stdout.find("Tips:").expect("Tips section");
    let examples = out.stdout.find("Examples:").expect("Examples section");
    assert!(tips < examples);
    let examples = &out.stdout[examples..];
    for step in [
        "stylometry profile build",
        "stylometry guide",
        "stylometry check",
    ] {
        assert!(examples.contains(step), "examples should show {step}");
    }
}

#[test]
fn json_flag_wraps_help_and_version_in_an_envelope() {
    let json: Value = serde_json::from_str(&run(&["--json", "--help"]).stdout).unwrap();
    assert_eq!(json["status"], "success");
    assert!(json["data"]["usage"].as_str().unwrap().contains("Tips:"));
    let json: Value = serde_json::from_str(&run(&["--json", "--version"]).stdout).unwrap();
    assert_eq!(json["status"], "success");
}

#[test]
fn piped_version_is_plain_text() {
    let out = run(&["--version"]);
    assert_eq!(out.code, Some(0));
    assert_eq!(
        out.stdout.trim(),
        format!("stylometry {}", env!("CARGO_PKG_VERSION"))
    );
}

// ── Parse errors name what the failing command accepts ─────────────────────

#[test]
fn parse_error_inside_check_lists_its_flags_and_usage() {
    let json = error_of(&["check", "draft.md", "--bogus"]);
    assert_eq!(json["error"]["code"], "invalid_input");
    assert_eq!(json["error"]["retryable"], false);
    let message = json["error"]["message"].as_str().unwrap();
    assert!(message.contains("--bogus"));
    assert!(!message.contains("error:"), "clap's prefix is dropped");
    let suggestion = json["error"]["suggestion"].as_str().unwrap();
    for flag in [
        "-p/--profile",
        "--text",
        "-n/--limit",
        "--avoid",
        "--guard",
        "--strict",
    ] {
        assert!(
            suggestion.contains(flag),
            "{flag} missing from: {suggestion}"
        );
    }
    assert!(suggestion.contains("Usage: stylometry check"));
}

#[test]
fn missing_required_argument_names_the_command() {
    let json = error_of(&["profile", "show"]);
    let suggestion = json["error"]["suggestion"].as_str().unwrap();
    assert!(
        suggestion.contains("stylometry profile show"),
        "{suggestion}"
    );
}

#[test]
fn group_without_subcommand_lists_the_subcommands() {
    let json = error_of(&["profile"]);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs a subcommand")
    );
    let suggestion = json["error"]["suggestion"].as_str().unwrap();
    for sub in ["build", "blend", "list", "show", "rebuild", "remove"] {
        assert!(suggestion.contains(sub), "{sub} missing from: {suggestion}");
    }
}

#[test]
fn unknown_root_command_lists_the_commands() {
    let json = error_of(&["definitely-not-a-command"]);
    let suggestion = json["error"]["suggestion"].as_str().unwrap();
    assert!(
        suggestion.contains("agent-info"),
        "past 12 commands the hint names discovery: {suggestion}"
    );
}
