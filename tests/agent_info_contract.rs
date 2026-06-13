//! Verify the agent-info manifest matches reality.
//!
//! Every command listed in agent-info must be routable, and the schema must
//! contain the required fields for agent bootstrapping.

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

fn agent_info() -> serde_json::Value {
    let out = bin().arg("agent-info").output().unwrap();
    assert!(out.status.success());
    serde_json::from_slice(&out.stdout).expect("agent-info must be valid JSON")
}

// ── Required top-level fields ──────────────────────────────────────────────

#[test]
fn has_required_fields() {
    let info = agent_info();
    assert!(info["name"].is_string());
    assert!(info["version"].is_string());
    assert!(info["description"].is_string());
    assert!(info["commands"].is_object());
    assert!(info["exit_codes"].is_object());
    assert!(info["envelope"].is_object());
    assert!(info["auto_json_when_piped"].is_boolean());
}

#[test]
fn name_matches_binary() {
    let info = agent_info();
    assert_eq!(info["name"], "stylometry");
}

// ── Exit codes ─────────────────────────────────────────────────────────────

#[test]
fn exit_codes_cover_full_contract() {
    let info = agent_info();
    let codes = &info["exit_codes"];
    for code in ["0", "1", "2", "3", "4"] {
        assert!(codes[code].is_string(), "exit_codes must document code {code}");
    }
}

// ── Commands: every listed command is routable ─────────────────────────────

#[test]
fn profile_list_is_routable() {
    let tmp = tempfile::tempdir().unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", tmp.path())
        .args(["profile", "list"])
        .assert()
        .code(0);
}

#[test]
fn agent_info_is_routable() {
    bin().arg("agent-info").assert().code(0);
}

#[test]
fn agent_info_alias_is_routable() {
    bin().arg("info").assert().code(0);
}

#[test]
fn skill_install_is_routable() {
    let tmp = tempfile::tempdir().unwrap();
    bin()
        .env("HOME", tmp.path())
        .args(["skill", "install"])
        .assert()
        .code(0);
}

#[test]
fn skill_status_is_routable() {
    bin().args(["skill", "status"]).assert().code(0);
}

#[test]
fn config_show_is_routable() {
    bin().args(["config", "show"]).assert().code(0);
}

#[test]
fn config_path_is_routable() {
    bin().args(["config", "path"]).assert().code(0);
}

// ── Enriched schema ────────────────────────────────────────────────────────

#[test]
fn profile_build_has_arg_schema() {
    let info = agent_info();
    let cmd = &info["commands"]["profile build"];
    let args = cmd["args"].as_array().expect("profile build must have args");
    assert!(!args.is_empty());
    assert_eq!(args[0]["name"], "name");
    assert_eq!(args[0]["required"], true);
}

#[test]
fn compare_has_required_profile_option() {
    let info = agent_info();
    let opts = info["commands"]["compare"]["options"]
        .as_array()
        .expect("compare must have options");
    let profile_opt = opts
        .iter()
        .find(|o| o["name"] == "--profile")
        .expect("compare must document --profile");
    assert_eq!(profile_opt["required"], true);
}

#[test]
fn global_flags_documented() {
    let info = agent_info();
    let flags = &info["global_flags"];
    assert!(flags["--json"].is_object());
    assert!(flags["--quiet"].is_object());
}

#[test]
fn config_metadata_present() {
    let info = agent_info();
    let config = &info["config"];
    assert!(config["path"].is_string());
    assert!(config["env_prefix"].is_string());
    assert!(config["env_prefix"].as_str().unwrap().ends_with('_'));
}
