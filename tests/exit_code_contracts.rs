//! Verify the semantic exit-code contract (0-4).
//!
//! Uses the hidden `contract` command for deterministic triggers and real
//! commands for natural exit-code coverage.

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

// ── Contract command: deterministic 0-4 ────────────────────────────────────

#[test]
fn contract_exit_0() {
    bin().args(["contract", "0"]).assert().code(0);
}

#[test]
fn contract_exit_1_transient() {
    bin().args(["contract", "1"]).assert().code(1);
}

#[test]
fn contract_exit_2_config() {
    bin().args(["contract", "2"]).assert().code(2);
}

#[test]
fn contract_exit_3_bad_input() {
    bin().args(["contract", "3"]).assert().code(3);
}

#[test]
fn contract_exit_4_rate_limited() {
    bin().args(["contract", "4"]).assert().code(4);
}

// ── Real commands: natural exit codes ──────────────────────────────────────

#[test]
fn profile_list_success_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", tmp.path())
        .args(["profile", "list"])
        .assert()
        .code(0);
}

#[test]
fn help_exits_0() {
    bin().arg("--help").assert().code(0);
}

#[test]
fn version_exits_0() {
    bin().arg("--version").assert().code(0);
}

#[test]
fn agent_info_exits_0() {
    bin().arg("agent-info").assert().code(0);
}

#[test]
fn config_path_exits_0() {
    bin().args(["config", "path"]).assert().code(0);
}

#[test]
fn config_show_exits_0() {
    bin().args(["config", "show"]).assert().code(0);
}

#[test]
fn missing_subcommand_exits_3() {
    bin().assert().code(3);
}

#[test]
fn compare_missing_profile_exits_3() {
    // `compare` requires --profile.
    bin().args(["compare", "some.txt"]).assert().code(3);
}
