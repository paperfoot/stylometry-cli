//! Verify JSON envelope structure on stdout/stderr.
//!
//! assert_cmd runs the binary in a pipe (not a TTY), so JSON auto-detection
//! kicks in -- no need for `--json`.

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

// ── Success envelope ───────────────────────────────────────────────────────

#[test]
fn success_envelope_shape() {
    let out = bin().args(["config", "path"]).output().unwrap();
    assert!(out.status.success());

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout should be valid JSON");

    assert_eq!(json["version"], "1");
    assert_eq!(json["status"], "success");
    assert!(json["data"].is_object(), "envelope must have a 'data' field");
    assert!(json["data"]["path"].is_string());
}

#[test]
fn success_envelope_with_json_flag() {
    let out = bin().args(["config", "path", "--json"]).output().unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json stdout should be valid JSON");

    assert_eq!(json["status"], "success");
    assert!(json["data"]["exists"].is_boolean());
}

// ── Error envelope ─────────────────────────────────────────────────────────

#[test]
fn error_envelope_shape() {
    let out = bin().args(["contract", "3"]).output().unwrap();
    assert!(!out.status.success());

    let json: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("stderr should be valid JSON");

    assert_eq!(json["version"], "1");
    assert_eq!(json["status"], "error");
    assert!(json["error"].is_object());
    assert!(json["error"]["code"].is_string());
    assert!(json["error"]["message"].is_string());
    assert!(json["error"]["suggestion"].is_string());
}

#[test]
fn error_code_matches_variant() {
    let out = bin().args(["contract", "2"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(json["error"]["code"], "config_error");

    let out = bin().args(["contract", "4"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(json["error"]["code"], "rate_limited");
}

// ── Help/version wrapping ──────────────────────────────────────────────────

#[test]
fn help_wrapped_in_envelope_when_piped() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("piped --help should be JSON");

    assert_eq!(json["status"], "success");
    assert!(json["data"]["usage"].is_string());
}

#[test]
fn version_wrapped_in_envelope_when_piped() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("piped --version should be JSON");

    assert_eq!(json["status"], "success");
}

// ── Quiet flag ─────────────────────────────────────────────────────────────

#[test]
fn quiet_still_emits_json() {
    let out = bin()
        .args(["config", "path", "--json", "--quiet"])
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--quiet should not suppress JSON");

    assert_eq!(json["status"], "success");
}

// ── Parse error envelope ───────────────────────────────────────────────────

#[test]
fn parse_error_wrapped_in_envelope() {
    let out = bin().args(["profile", "show"]).output().unwrap(); // missing <name>
    assert_eq!(out.status.code(), Some(3));

    let json: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("parse error should be JSON on stderr");

    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "invalid_input");
    assert!(
        json["error"]["suggestion"]
            .as_str()
            .unwrap()
            .contains("--help")
    );
}
