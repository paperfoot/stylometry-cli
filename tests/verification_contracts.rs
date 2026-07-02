//! Integration tests for the v0.2 hardening behaviors: stdin/`-` input to
//! `compare`, the new `compare`/`calibrate` JSON fields, short-text rejection,
//! imposter-count warnings, and frozen-reference calibration stability.
//!
//! Every test gets its OWN tempdir for STYLOMETRY_DATA_DIR so tests never
//! touch real profiles and stay parallel-safe.

use std::path::Path;

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

// ── Synthetic corpora ───────────────────────────────────────────────────────
//
// Each "author" has a distinct vocabulary; text is built by alternating
// vocabulary words with shared glue words, giving each author a clearly
// different word-frequency fingerprint while remaining deterministic (no
// randomness, so runs are reproducible).

const GLUE: [&str; 8] = ["the", "a", "and", "with", "over", "near", "under", "through"];

const VOCAB_A: [&str; 12] = [
    "whale", "harpoon", "ocean", "captain", "voyage", "storm", "sail", "tide", "current",
    "anchor", "reef", "compass",
];
const VOCAB_B: [&str; 12] = [
    "garden", "rose", "sunlight", "cottage", "meadow", "brook", "orchard", "breeze", "petal",
    "hive", "blossom", "trellis",
];
const VOCAB_C: [&str; 12] = [
    "engine", "circuit", "voltage", "signal", "sensor", "battery", "motor", "wire", "reactor",
    "turbine", "cable", "diode",
];
const VOCAB_D: [&str; 12] = [
    "mountain", "glacier", "summit", "ridge", "avalanche", "crevasse", "peak", "alpine",
    "boulder", "trail", "cairn", "moraine",
];

/// Deterministic synthetic text of exactly `target_words` unicode words,
/// alternating a fixed vocabulary with shared glue words.
fn corpus_text(vocab: &[&str], target_words: usize) -> String {
    let mut words = Vec::with_capacity(target_words);
    let mut vi = 0usize;
    let mut gi = 0usize;
    for i in 0..target_words {
        if i % 2 == 0 {
            words.push(vocab[vi % vocab.len()]);
            vi += 1;
        } else {
            words.push(GLUE[gi % GLUE.len()]);
            gi += 1;
        }
    }
    words.join(" ")
}

/// Build a profile named `name` from `words` synthetic words of `vocab`, at
/// --chunk-size 200 (1000 words -> 5 clean 200-word chunks).
fn build_profile(data_dir: &Path, corpus_dir: &Path, name: &str, vocab: &[&str], words: usize) {
    let file = corpus_dir.join(format!("{name}.txt"));
    std::fs::write(&file, corpus_text(vocab, words)).unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data_dir)
        .args(["profile", "build", name, "--corpus"])
        .arg(&file)
        .args(["--chunk-size", "200"])
        .assert()
        .success();
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout should be valid JSON: {e}\nstdout={:?}", out.stdout))
}

// ── stdin / `-` input ───────────────────────────────────────────────────────

#[test]
fn compare_via_piped_stdin_gives_verdict() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(corpus_text(&VOCAB_A, 200))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    assert_eq!(json["status"], "success");
    assert!(json["data"]["verdict"].is_string());
}

#[test]
fn compare_via_dash_arg_with_stdin_works() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a", "-"])
        .write_stdin(corpus_text(&VOCAB_A, 200))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    assert_eq!(json["status"], "success");
    assert!(json["data"]["verdict"].is_string());
}

// ── Short-text rejection ────────────────────────────────────────────────────

#[test]
fn compare_with_short_text_exits_3() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    // 20 words: well under the 50-word floor.
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(corpus_text(&VOCAB_A, 20))
        .assert()
        .code(3);
}

// ── New compare JSON fields ─────────────────────────────────────────────────

#[test]
fn compare_json_has_new_fields() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(corpus_text(&VOCAB_A, 200))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    let data = &json["data"];
    assert_eq!(data["query_words"], 200);
    assert!(data["length_mismatch"].is_boolean());
    assert!(data["ranking"].is_array());
    assert!(!data["ranking"].as_array().unwrap().is_empty());
    for r in data["ranking"].as_array().unwrap() {
        assert!(r["profile"].is_string());
        assert!(r["cosine_delta"].is_number());
    }
}

// ── calibrate: imposter-count warning ───────────────────────────────────────

#[test]
fn calibrate_with_one_imposter_warns() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "a"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    let warning = json["data"]["warning"]
        .as_str()
        .expect("warning should be present with only 1 imposter");
    assert!(
        warning.to_lowercase().contains("imposter"),
        "warning should mention imposter count: {warning}"
    );
    assert!(warning.contains('1'), "warning should mention the count 1: {warning}");
}

#[test]
fn calibrate_with_three_imposters_has_no_imposter_warning() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);
    build_profile(data.path(), corpus.path(), "c", &VOCAB_C, 1000);
    build_profile(data.path(), corpus.path(), "d", &VOCAB_D, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "a"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    assert_eq!(json["data"]["imposters"], 3);
    match json["data"]["warning"].as_str() {
        None => {}
        Some(w) => assert!(
            !w.to_lowercase().contains("imposter"),
            "warning should not mention imposter count with 3 imposters: {w}"
        ),
    }
}

// ── calibrate: report fields ────────────────────────────────────────────────

#[test]
fn calibrate_report_has_quality_fields() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);
    build_profile(data.path(), corpus.path(), "c", &VOCAB_C, 1000);

    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "a"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    let data = &json["data"];
    assert!(data["auc"].is_number());
    assert!(data["accuracy"].is_number());
    assert!(data["c_at_1"].is_number());
    assert!(data["threshold"].is_number());
    assert!(data["holdout"].is_boolean());
    // holdout_brier may legitimately be null when there's no holdout split.
    assert!(data["holdout_brier"].is_number() || data["holdout_brier"].is_null());
}

// ── Frozen reference stability ──────────────────────────────────────────────

#[test]
fn frozen_reference_survives_profile_removal() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);
    build_profile(data.path(), corpus.path(), "c", &VOCAB_C, 1000);

    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "a"])
        .assert()
        .success();

    let query = corpus_text(&VOCAB_A, 200);

    let out1 = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(query.clone())
        .output()
        .unwrap();
    assert!(out1.status.success(), "stderr={:?}", out1.stderr);
    let json1 = json_of(&out1);
    let cosine1 = json1["data"]["cosine_delta"].clone();
    let p1 = json1["data"]["p_same_author"].clone();
    let verdict1 = json1["data"]["verdict"].as_str().unwrap().to_string();
    assert!(!verdict1.ends_with("_uncalibrated"), "verdict1={verdict1}");
    assert_eq!(json1["data"]["calibration_stale"], false);

    // Remove a non-target profile.
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["profile", "remove", "c"])
        .assert()
        .success();

    let out2 = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(query)
        .output()
        .unwrap();
    assert!(out2.status.success(), "stderr={:?}", out2.stderr);
    let json2 = json_of(&out2);

    assert_eq!(cosine1, json2["data"]["cosine_delta"], "cosine_delta must be bit-identical");
    assert_eq!(p1, json2["data"]["p_same_author"], "p_same_author must be bit-identical");
    assert_eq!(json2["data"]["calibration_stale"], true, "profile set changed -> stale");
    let verdict2 = json2["data"]["verdict"].as_str().unwrap();
    assert!(!verdict2.ends_with("_uncalibrated"), "verdict2={verdict2} should stay calibrated");
}

// ── Length mismatch ──────────────────────────────────────────────────────────

#[test]
fn length_mismatch_flagged_and_warned() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 1000);
    build_profile(data.path(), corpus.path(), "b", &VOCAB_B, 1000);

    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "a"])
        .assert()
        .success();

    // Calibration chunk_words = 200 (--chunk-size 200 at build time); a
    // ~1000-word query is 5x that, well outside the [0.5, 2.0] ratio band.
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(corpus_text(&VOCAB_A, 1000))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr={:?}", out.stderr);

    let json = json_of(&out);
    assert_eq!(json["data"]["length_mismatch"], true);
    let warning = json["data"]["warning"]
        .as_str()
        .expect("warning should be present when length_mismatch is true");
    assert!(warning.contains("word"), "warning should mention word length: {warning}");
}
