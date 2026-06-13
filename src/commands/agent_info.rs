/// Machine-readable capability manifest.
///
/// agent-info is always JSON -- the whole point is machine readability.
/// An agent calling agent-info is bootstrapping the tool's full capability set.
pub fn run() {
    let name = env!("CARGO_PKG_NAME");
    let config_path = crate::config::config_path();

    let info = serde_json::json!({
        "name": name,
        "version": env!("CARGO_PKG_VERSION"),
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "concepts": {
            "profile": "An author's stylometric fingerprint, built from a corpus of their writing.",
            "delta": "Cosine Delta (Wurzburg) over z-scored most-frequent words + char trigrams; lower = stylistically closer.",
            "verification": "Calibrate a profile against other profiles (imposters) to get P(same author), AUC, and a threshold."
        },
        "commands": {
            "profile build": {
                "description": "Build a profile by analysing a corpus",
                "args": [
                    { "name": "name", "kind": "positional", "type": "string", "required": true, "description": "Profile name (letters, digits, - _)" }
                ],
                "options": [
                    { "name": "--corpus", "type": "path", "required": true, "description": "File or directory of .md/.txt to fingerprint" },
                    { "name": "--chunk-size", "type": "int", "required": false, "default": 1500, "description": "Words per analysis chunk" },
                    { "name": "--force", "type": "bool", "required": false, "default": false, "description": "Overwrite an existing profile" }
                ]
            },
            "profile list": { "description": "List all profiles", "args": [], "options": [] },
            "profile show": {
                "description": "Show a profile's fingerprint summary",
                "args": [ { "name": "name", "kind": "positional", "type": "string", "required": true, "description": "Profile name" } ],
                "options": []
            },
            "profile remove": {
                "description": "Remove a profile",
                "args": [ { "name": "name", "kind": "positional", "type": "string", "required": true, "description": "Profile name" } ],
                "options": []
            },
            "compare": {
                "description": "Compare a text against a profile and return a verdict",
                "args": [
                    { "name": "file", "kind": "positional", "type": "path", "required": false, "description": "Text file to score (or use --text)" }
                ],
                "options": [
                    { "name": "--profile", "type": "string", "required": true, "description": "Profile to compare against" },
                    { "name": "--text", "type": "string", "required": false, "description": "Inline text instead of a file" }
                ],
                "data_fields": [
                    "profile", "cosine_delta", "classic_delta", "nearest_profile",
                    "nearest_cosine_delta", "gi_score", "p_same_author", "verdict", "ranking"
                ]
            },
            "calibrate": {
                "description": "Calibrate a profile's verifier against the other profiles (imposters)",
                "args": [ { "name": "name", "kind": "positional", "type": "string", "required": true, "description": "Profile to calibrate" } ],
                "options": [],
                "data_fields": ["profile", "auc", "accuracy", "threshold", "positives", "negatives", "imposters"]
            },
            "agent-info": { "description": "This manifest", "aliases": ["info"], "args": [], "options": [] },
            "skill install": { "description": "Install skill file to agent platforms", "args": [], "options": [] },
            "skill status": { "description": "Check skill installation status", "args": [], "options": [] },
            "config show": { "description": "Display effective merged configuration", "args": [], "options": [] },
            "config path": { "description": "Show configuration file path", "args": [], "options": [] },
            "update": {
                "description": "Distribution-aware update check/apply",
                "args": [],
                "options": [
                    { "name": "--check", "type": "bool", "required": false, "default": false, "description": "Check only, don't install" }
                ],
                "install_sources": [
                    "standalone", "homebrew", "cargo", "cargo_binstall", "npm", "bun",
                    "uv_tool", "pipx", "winget", "scoop", "apt", "managed", "unknown"
                ],
                "data_fields": [
                    "current_version", "latest_version", "status", "install_source",
                    "update_mode", "upgrade_command", "release_url", "requires_skill_reinstall"
                ]
            }
        },
        "global_flags": {
            "--json": { "description": "Force JSON output (auto-enabled when piped)", "type": "bool", "default": false },
            "--quiet": { "description": "Suppress informational output", "type": "bool", "default": false }
        },
        "exit_codes": {
            "0": "Success",
            "1": "Transient error (IO) -- retry",
            "2": "Config error -- fix setup",
            "3": "Bad input -- fix arguments",
            "4": "Rate limited -- wait and retry",
        },
        "envelope": {
            "version": "1",
            "success": "{ version, status, data }",
            "error": "{ version, status, error: { code, message, suggestion } }",
        },
        "config": {
            "path": config_path.display().to_string(),
            "env_prefix": format!("{}_", name.to_uppercase()),
            "data_dir_env": "STYLOMETRY_DATA_DIR",
        },
        "auto_json_when_piped": true,
    });
    println!("{}", serde_json::to_string_pretty(&info).unwrap());
}
