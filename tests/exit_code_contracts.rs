//! The semantic exit-code contract: 0-4 plus domain code 5 (`check --strict`
//! failed). The hidden `contract` command triggers each deterministically;
//! real commands cover the natural paths.

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

fn code(args: &[&str]) -> Option<i32> {
    let tmp = tempfile::tempdir().unwrap();
    bin(tmp.path()).args(args).output().unwrap().status.code()
}

// ── Contract command: every documented code ────────────────────────────────

#[test]
fn contract_triggers_every_documented_code() {
    for c in 0..=5 {
        assert_eq!(code(&["contract", &c.to_string()]), Some(c), "contract {c}");
    }
}

#[test]
fn exit_5_carries_the_report_and_says_not_to_retry_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(["contract", "5"]).output().unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert!(out.stdout.is_empty(), "no success envelope on failure");
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], "check_failed");
    assert_eq!(err["error"]["retryable"], false);
    assert!(err["error"]["details"].is_object());
    assert!(
        err["error"]["suggestion"]
            .as_str()
            .unwrap()
            .contains("Revise")
    );
}

#[test]
fn unknown_contract_code_is_bad_input() {
    assert_eq!(code(&["contract", "9"]), Some(3));
}

// ── Real commands: natural exit codes ──────────────────────────────────────

#[test]
fn read_only_framework_commands_exit_0() {
    for args in [
        &["--help"][..],
        &["--version"],
        &["agent-info"],
        &["config", "path"],
        &["config", "show"],
        &["profile", "list"],
        &["doctor"],
    ] {
        assert_eq!(code(args), Some(0), "{args:?}");
    }
}

#[test]
fn parse_errors_exit_3() {
    assert_eq!(code(&[]), Some(3), "missing subcommand");
    assert_eq!(
        code(&["compare", "some.txt"]),
        Some(3),
        "compare needs --profile"
    );
    assert_eq!(
        code(&["check", "draft.md"]),
        Some(3),
        "check needs --profile"
    );
    assert_eq!(
        code(&["export", "x", "-o", "x.jsonl", "--format", "csv"]),
        Some(3)
    );
    assert_eq!(code(&["definitely-not-a-command"]), Some(3));
}

#[test]
fn failing_doctor_exits_2_with_the_report_in_details() {
    let tmp = tempfile::tempdir().unwrap();
    let profiles = tmp.path().join("data/profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("old.json"), r#"{"schema":0,"name":"old"}"#).unwrap();

    let out = bin(tmp.path()).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], "config_error");
    let checks = err["error"]["details"]["checks"].as_array().unwrap();
    let old = checks.iter().find(|c| c["name"] == "profile old").unwrap();
    assert_eq!(old["status"], "fail");
    assert!(
        old["suggestion"]
            .as_str()
            .unwrap()
            .contains("stylometry profile build old --corpus <path> --force")
    );
    assert_eq!(err["error"]["details"]["summary"]["fail"], 1);
}
