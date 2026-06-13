//! Robustness tests: recovery from bad state.
//!
//! Discovery and diagnostic commands must work even when configuration is
//! malformed, and enforced constraints must match agent-info.

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

fn write_bad_config(home: &std::path::Path) {
    let config_dir = home.join("Library/Application Support/stylometry");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "{{invalid toml").unwrap();
}

// ── Malformed config resilience ────────────────────────────────────────────

#[test]
fn agent_info_works_with_malformed_config() {
    let tmp = tempfile::tempdir().unwrap();
    write_bad_config(tmp.path());
    bin()
        .env("HOME", tmp.path())
        .arg("agent-info")
        .assert()
        .code(0);
}

#[test]
fn config_path_works_with_malformed_config() {
    let tmp = tempfile::tempdir().unwrap();
    write_bad_config(tmp.path());
    bin()
        .env("HOME", tmp.path())
        .args(["config", "path"])
        .assert()
        .code(0);
}

#[test]
fn config_show_fails_with_malformed_config() {
    let tmp = tempfile::tempdir().unwrap();
    write_bad_config(tmp.path());
    bin()
        .env("HOME", tmp.path())
        .args(["config", "show"])
        .assert()
        .code(2);
}

// ── Constraint enforcement ─────────────────────────────────────────────────

#[test]
fn invalid_chunk_size_rejected() {
    // --chunk-size expects an integer; a non-int is a parse error (exit 3).
    bin()
        .args(["profile", "build", "x", "--corpus", "/tmp", "--chunk-size", "abc"])
        .assert()
        .code(3);
}

#[test]
fn profile_list_works_with_temp_home() {
    let tmp = tempfile::tempdir().unwrap();
    bin()
        .env("HOME", tmp.path())
        .env("STYLOMETRY_DATA_DIR", tmp.path())
        .args(["profile", "list"])
        .assert()
        .code(0);
}
