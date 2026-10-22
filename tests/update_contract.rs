//! The distribution-aware update contract (agent-cli-framework update
//! standard). Tests force non-standalone channels through config or env, so
//! they never hit the network or mutate the machine running them.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn bin(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("stylometry").unwrap();
    cmd.env("HOME", home)
        .env("STYLOMETRY_DATA_DIR", home.join("data"));
    cmd
}

fn config_path_for_home(home: &Path) -> PathBuf {
    let out = bin(home)
        .args(["--json", "config", "path"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let json: Value = serde_json::from_slice(&out.stdout).expect("config path should be JSON");
    PathBuf::from(json["data"]["path"].as_str().unwrap())
}

fn write_config(home: &Path, contents: &str) {
    let path = config_path_for_home(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn update_check_with_config(config: &str) -> Value {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), config);
    let out = bin(tmp.path())
        .args(["--json", "update", "--check"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    serde_json::from_slice(&out.stdout).expect("update --check should emit JSON")
}

const FIELDS: &[&str] = &[
    "current_version",
    "latest_version",
    "status",
    "install_source",
    "update_mode",
    "upgrade_command",
    "release_url",
    "requires_skill_reinstall",
];

#[test]
fn payload_always_has_every_standard_field() {
    let json = update_check_with_config("[update]\ninstall_source = \"homebrew\"\n");
    for field in FIELDS {
        assert!(json["data"].get(*field).is_some(), "missing {field}");
    }
    assert_eq!(json["data"]["current_version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn disabled_update_returns_disabled_status_and_no_command() {
    let json = update_check_with_config("[update]\nenabled = false\ninstall_source = \"cargo\"\n");
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"]["status"], "disabled");
    assert_eq!(json["data"]["update_mode"], "disabled");
    assert!(json["data"]["upgrade_command"].is_null());
    assert!(json["data"]["latest_version"].is_null());
}

#[test]
fn managed_install_is_disabled_without_a_command() {
    let json = update_check_with_config("[update]\ninstall_source = \"managed\"\n");
    assert_eq!(json["data"]["status"], "disabled");
    assert_eq!(json["data"]["install_source"], "managed");
    assert!(json["data"]["upgrade_command"].is_null());
}

#[test]
fn unknown_install_returns_instructions_not_a_fake_command() {
    let json = update_check_with_config("[update]\ninstall_source = \"unknown\"\n");
    assert_eq!(json["data"]["status"], "not_checked");
    assert_eq!(json["data"]["update_mode"], "instructions_only");
    assert!(json["data"]["upgrade_command"].is_null());
    assert!(
        json["data"]["latest_version"].is_null(),
        "never copy the installed version"
    );
    assert!(
        json["data"]["release_url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/paperfoot/stylometry-cli/releases")
    );
    assert_eq!(json["data"]["requires_skill_reinstall"], false);
}

#[test]
fn package_managers_get_their_literal_upgrade_command() {
    for (config, command) in [
        (
            "install_source = \"homebrew\"\nformula = \"stylometry\"\ntap = \"paperfoot/tap\"",
            "brew upgrade paperfoot/tap/stylometry",
        ),
        (
            "install_source = \"cargo\"\ncrate_name = \"stylometry-cli\"",
            "cargo install --locked --force stylometry-cli",
        ),
        (
            "install_source = \"uv_tool\"\ncrate_name = \"stylometry\"",
            "uv tool upgrade stylometry",
        ),
        (
            "install_source = \"bun\"\ncrate_name = \"stylometry\"",
            "bun update --global stylometry",
        ),
    ] {
        let json = update_check_with_config(&format!("[update]\n{config}\n"));
        assert_eq!(json["data"]["status"], "managed_install", "{config}");
        assert_eq!(json["data"]["update_mode"], "package_manager");
        assert_eq!(json["data"]["upgrade_command"], command);
        assert!(json["data"]["latest_version"].is_null());
    }
}

#[test]
fn default_cargo_command_names_the_published_crate() {
    let json = update_check_with_config("[update]\ninstall_source = \"cargo\"\n");
    assert_eq!(
        json["data"]["upgrade_command"],
        "cargo install --locked --force stylometry-cli"
    );
}

#[test]
fn shell_syntax_in_a_package_name_is_a_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(
        tmp.path(),
        "[update]\ninstall_source = \"cargo\"\ncrate_name = \"x; rm -rf ~\"\n",
    );
    let out = bin(tmp.path())
        .args(["update", "--check"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn env_override_nests_with_double_underscore() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path())
        .env("STYLOMETRY_UPDATE__INSTALL_SOURCE", "pipx")
        .args(["update", "--check"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["install_source"], "pipx");
    assert_eq!(
        json["data"]["upgrade_command"],
        "pipx upgrade stylometry-cli"
    );
}

#[test]
fn invalid_update_source_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "[update]\ninstall_source = \"spaceship\"\n");
    let out = bin(tmp.path())
        .args(["--json", "update", "--check"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let json: Value = serde_json::from_slice(&out.stderr).expect("config error should be JSON");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "config_error");
}

#[test]
fn agent_info_documents_the_update_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path())
        .args(["agent-info", "--command", "update"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    let update = &info["commands"]["update"];
    assert!(
        update["effect_detail"]
            .as_str()
            .unwrap()
            .contains("Never replaces")
    );
    for src in ["homebrew", "uv_tool", "bun", "unknown"] {
        assert!(
            update["install_sources"]
                .as_array()
                .unwrap()
                .contains(&Value::from(src))
        );
    }
    let fields: Vec<&str> = update["data_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(fields, FIELDS);
}
