//! Discovery matches execution: the agent-info manifest lists exactly the
//! commands clap routes, one signature line each; `--command PATH` returns
//! the full object; exit codes (including domain code 5) are documented.

use std::collections::BTreeSet;

use assert_cmd::Command;
use serde_json::Value;

/// Isolated home and data dir, so nothing reads the owner's real state.
fn bin(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("stylometry").unwrap();
    cmd.env("HOME", home)
        .env("STYLOMETRY_DATA_DIR", home.join("data"));
    cmd
}

fn run_json(args: &[&str]) -> Value {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(args).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty(), "{args:?} wrote stderr");
    serde_json::from_slice(&out.stdout).expect("one JSON document")
}

fn agent_info() -> Value {
    run_json(&["agent-info"])
}

fn keys(value: &Value) -> BTreeSet<String> {
    value["commands"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

const COMMANDS: &[&str] = &[
    "agent-info",
    "analyze",
    "background fetch",
    "background status",
    "calibrate",
    "check",
    "compare",
    "config path",
    "config show",
    "diff",
    "doctor",
    "examples",
    "export",
    "guide",
    "profile blend",
    "profile build",
    "profile list",
    "profile rebuild",
    "profile remove",
    "profile show",
    "skill install",
    "skill status",
    "update",
];

// ── Shape ──────────────────────────────────────────────────────────────────

#[test]
fn manifest_is_raw_with_required_fields() {
    let info = agent_info();
    assert!(info.get("status").is_none(), "agent-info is raw JSON");
    assert_eq!(info["name"], "stylometry");
    for field in ["version", "description"] {
        assert!(info[field].is_string(), "{field}");
    }
    for field in [
        "commands",
        "global_flags",
        "exit_codes",
        "envelope",
        "config",
    ] {
        assert!(info[field].is_object(), "{field}");
    }
    assert_eq!(info["auto_json_when_piped"], true);
    assert!(info["global_flags"]["--json"]["description"].is_string());
    assert!(info["global_flags"]["--quiet"]["description"].is_string());
    assert_eq!(info["config"]["env_prefix"], "STYLOMETRY_");
    assert!(info["config"]["path"].is_string());
    assert_eq!(info["config"]["data_dir_env"], "STYLOMETRY_DATA_DIR");
}

#[test]
fn manifest_stays_compact() {
    let tmp = tempfile::tempdir().unwrap();
    let full = bin(tmp.path()).arg("agent-info").output().unwrap().stdout;
    let check = bin(tmp.path())
        .args(["agent-info", "--command", "check"])
        .output()
        .unwrap()
        .stdout;
    // One line per command keeps discovery cheap; v0.2 was 6.4 KB for 12 commands.
    assert!(full.len() < 5_000, "manifest grew to {} bytes", full.len());
    assert!(
        check.len() < 2_500,
        "check entry grew to {} bytes",
        check.len()
    );
    assert_eq!(
        full.iter().filter(|b| **b == b'\n').count(),
        1,
        "compact JSON"
    );
}

#[test]
fn workflow_names_the_loop_in_order() {
    let info = agent_info();
    let steps: Vec<&str> = info["workflow"]
        .as_array()
        .expect("workflow is a list")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(steps.len(), 7);
    let order = [
        "profile build",
        "analyze",
        "guide",
        "examples",
        "write",
        "check",
        "calibrate",
    ];
    for (step, expected) in steps.iter().zip(order) {
        assert!(
            step.contains(expected),
            "{step:?} should mention {expected}"
        );
    }
}

// ── Exit codes ─────────────────────────────────────────────────────────────

#[test]
fn exit_codes_cover_the_contract_and_domain_code_5() {
    let info = agent_info();
    let codes = info["exit_codes"].as_object().unwrap();
    for code in ["0", "1", "2", "3", "4", "5"] {
        assert!(codes[code].is_string(), "exit code {code} undocumented");
    }
    let five = codes["5"].as_str().unwrap();
    assert!(five.contains("check --strict") && five.contains("error.details"));
    assert_eq!(
        codes.len(),
        6,
        "every documented code is triggerable via contract"
    );
}

// ── Discovery matches the parser, both directions ──────────────────────────

#[test]
fn manifest_lists_exactly_the_routable_commands() {
    let info = agent_info();
    let expected: BTreeSet<String> = COMMANDS.iter().map(|s| s.to_string()).collect();
    assert_eq!(keys(&info), expected);
    let tmp = tempfile::tempdir().unwrap();
    for path in COMMANDS {
        let mut argv: Vec<&str> = path.split(' ').collect();
        argv.push("--help");
        bin(tmp.path()).args(&argv).assert().code(0);
    }
}

#[test]
fn top_level_help_and_manifest_agree() {
    // The other direction: every command clap shows in --help is in the manifest.
    let tmp = tempfile::tempdir().unwrap();
    let help = bin(tmp.path()).arg("--help").output().unwrap().stdout;
    let help = String::from_utf8(help).unwrap();
    let listed: BTreeSet<String> = help
        .split("Commands:")
        .nth(1)
        .unwrap()
        .split("\n\n")
        .next()
        .unwrap()
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| *w != "help")
        .map(String::from)
        .collect();
    let tops: BTreeSet<String> = COMMANDS
        .iter()
        .map(|p| p.split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(listed, tops);
}

#[test]
fn every_line_matches_its_full_object() {
    let info = agent_info();
    for (path, line) in info["commands"].as_object().unwrap() {
        let line = line.as_str().unwrap_or_else(|| panic!("{path}: one line"));
        let scoped = run_json(&["agent-info", "--command", path]);
        let command = &scoped["commands"][path.as_str()];
        let description = command["description"].as_str().unwrap();
        assert!(line.contains(description), "{path}: {line}");
        assert!(command["effect"].is_string(), "{path}: effect");
        assert!(command["examples"].is_array(), "{path}: examples");
        for item in command["args"].as_array().unwrap() {
            assert!(
                line.contains(item["name"].as_str().unwrap()),
                "{path}: {line}"
            );
        }
        for item in command["options"].as_array().unwrap() {
            assert!(
                line.contains(item["name"].as_str().unwrap()),
                "{path}: {line}"
            );
        }
    }
}

// ── Scoped discovery ───────────────────────────────────────────────────────

#[test]
fn command_check_returns_the_full_object() {
    let scoped = run_json(&["agent-info", "--command", "check"]);
    let top: BTreeSet<&str> = scoped
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(top, BTreeSet::from(["commands", "name", "version"]));
    let check = &scoped["commands"]["check"];
    assert!(check["description"].as_str().unwrap().contains("voice"));
    let option = |name: &str| {
        check["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap_or_else(|| panic!("check must document {name}"))
            .clone()
    };
    let profile = option("--profile");
    assert_eq!(profile["required"], true);
    assert_eq!(profile["short"], "-p");
    assert_eq!(option("--limit")["default"], "50");
    assert_eq!(option("--limit")["short"], "-n");
    assert_eq!(option("--strict")["type"], "bool");
    assert_eq!(option("--avoid")["repeatable"], true);
    assert_eq!(check["args"][0]["name"], "file");
    assert_eq!(check["args"][0]["required"], false);
    assert!(check["exit_behavior"].as_str().unwrap().contains("exits 5"));
}

#[test]
fn enums_and_aliases_are_exposed() {
    let export = run_json(&["agent-info", "--command", "export"]);
    let format = export["commands"]["export"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "--format")
        .unwrap()
        .clone();
    assert_eq!(format["values"], serde_json::json!(["chat", "completion"]));
    assert_eq!(format["default"], "chat");
    let info = run_json(&["agent-info", "--command", "agent-info"]);
    assert_eq!(
        info["commands"]["agent-info"]["aliases"],
        serde_json::json!(["info"])
    );
}

#[test]
fn group_paths_select_their_descendants() {
    let profile = run_json(&["agent-info", "--command", "profile"]);
    assert_eq!(
        keys(&profile),
        ["blend", "build", "list", "rebuild", "remove", "show"]
            .iter()
            .map(|s| format!("profile {s}"))
            .collect()
    );
    let build = run_json(&["agent-info", "--command", "profile  build"]);
    assert_eq!(keys(&build), BTreeSet::from(["profile build".to_string()]));
}

#[test]
fn unknown_command_path_exits_3_with_a_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path())
        .args(["agent-info", "--command", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], "invalid_input");
    assert!(
        err["error"]["suggestion"]
            .as_str()
            .unwrap()
            .contains("agent-info")
    );
}

#[test]
fn info_alias_returns_the_same_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let info = bin(tmp.path()).arg("info").output().unwrap().stdout;
    let full = bin(tmp.path()).arg("agent-info").output().unwrap().stdout;
    assert_eq!(info, full);
}

// ── Framework commands route without state ─────────────────────────────────

#[test]
fn framework_commands_are_routable() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        &["profile", "list"][..],
        &["skill", "install"],
        &["skill", "status"],
        &["config", "show"],
        &["config", "path"],
        &["doctor"],
    ] {
        bin(tmp.path()).args(args).assert().code(0);
    }
}
