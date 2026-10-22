//! Contracts for verification: `compare`, `calibrate`, `background`.
//! Pins the JSON fields agents and the owner's gate script read, verdicts
//! following P(same author), length bands, frozen-reference stability, and
//! exit codes. Every test gets its OWN tempdir for STYLOMETRY_DATA_DIR.

use std::path::Path;

use assert_cmd::Command;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

// ── Synthetic corpora ───────────────────────────────────────────────────────
//
// Each "author" has a distinct vocabulary mixed with shared glue words, in
// ten-word sentences and five-sentence paragraphs (so passages chunk like
// prose). Deterministic: no randomness.

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

/// Deterministic prose of `n` words: vocabulary and glue words alternate
/// (the vocabulary walks with a stride so passages vary a little).
fn corpus_text(vocab: &[&str], n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let w = if i % 2 == 0 {
            vocab[(i / 2 * 5 + i / 7) % vocab.len()]
        } else {
            GLUE[(i / 2 + i / 11) % GLUE.len()]
        };
        out.push_str(w);
        if i % 10 == 9 {
            out.push('.');
            out.push_str(if i % 50 == 49 { "\n\n" } else { " " });
        } else {
            out.push(' ');
        }
    }
    out
}

fn build_profile(data: &Path, corpus: &Path, name: &str, vocab: &[&str], words: usize) {
    let file = corpus.join(format!("{name}.txt"));
    std::fs::write(&file, corpus_text(vocab, words)).unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(["profile", "build", name, "--corpus"])
        .arg(&file)
        .assert()
        .success();
}

/// Four authors; `a` is the target.
fn four_authors(data: &Path, corpus: &Path) {
    build_profile(data, corpus, "a", &VOCAB_A, 4000);
    build_profile(data, corpus, "b", &VOCAB_B, 4000);
    build_profile(data, corpus, "c", &VOCAB_C, 4000);
    build_profile(data, corpus, "d", &VOCAB_D, 4000);
}

fn run_json(data: &Path, args: &[&str], stdin: Option<String>) -> serde_json::Value {
    let mut cmd = bin();
    cmd.env("STYLOMETRY_DATA_DIR", data).args(args);
    if let Some(s) = stdin {
        cmd.write_stdin(s);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope")
}

// ── compare: input and fields ───────────────────────────────────────────────

#[test]
fn compare_reads_stdin_only_on_dash_and_returns_gate_fields() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());

    let j = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(corpus_text(&VOCAB_A, 600)));
    let d = &j["data"];
    // Fields the owner's gate script reads.
    assert!(d["cosine_delta"].is_number());
    assert!(d["p_same_author"].is_null(), "uncalibrated: no P");
    assert!(d["nearest_profile"].is_string());
    assert_eq!(d["verdict"], "close_uncalibrated");
    assert_eq!(d["words"], 600);
    assert!(d["band_words"].as_u64().unwrap() <= 600);
    assert!(d["typicality"].is_number());
    assert_eq!(d["calibration_stale"], false);
    let ranking = d["ranking"].as_array().unwrap();
    assert!(!ranking.is_empty() && ranking.len() <= 5);
    assert_eq!(ranking[0]["profile"], "a");
    // Next step: calibrate.
    let next = j["metadata"]["next"].as_array().unwrap();
    assert!(next.iter().any(|n| n.as_str().unwrap().contains("calibrate a")));

    // Without '-', piped stdin is NOT read: an agent's open pipe must never hang it.
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "--profile", "a"])
        .write_stdin(corpus_text(&VOCAB_A, 600))
        .assert()
        .code(3);
}

#[test]
fn compare_rejects_texts_under_150_words() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_profile(data.path(), corpus.path(), "a", &VOCAB_A, 3000);
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "-", "--profile", "a", "--json"])
        .write_stdin(corpus_text(&VOCAB_A, 120))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(err["error"]["message"].as_str().unwrap().contains("150"));
}

// ── calibrate ───────────────────────────────────────────────────────────────

#[test]
fn calibrate_reports_bands_and_warns_about_few_imposters() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());

    let j = run_json(data.path(), &["calibrate", "a"], None);
    let d = &j["data"];
    assert_eq!(d["imposters"], 3);
    assert_eq!(d["cross_work"], false, "one work: neighbour exclusion");
    let bands = d["bands"].as_array().unwrap();
    assert!(!bands.is_empty());
    assert_eq!(bands[0]["words"], 250);
    for b in bands {
        for f in ["auc", "positives", "negatives", "delta_at_p50"] {
            assert!(b[f].is_number(), "band field {f}: {b}");
        }
        for f in ["holdout_accuracy", "holdout_brier", "c_at_1"] {
            assert!(b[f].is_number() || b[f].is_null(), "band field {f}: {b}");
        }
        assert!(b["positives"].as_u64().unwrap() >= 6);
        assert!(b["negatives"].as_u64().unwrap() >= 20);
    }
    let warnings = d["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| {
            let w = w.as_str().unwrap();
            w.contains("3 imposter") && w.contains("background fetch")
        }),
        "{warnings:?}"
    );
}

#[test]
fn calibrated_verdict_follows_probability() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());
    run_json(data.path(), &["calibrate", "a"], None);

    for (vocab, want) in [(&VOCAB_A, "same_author"), (&VOCAB_C, "different_author")] {
        let j = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(corpus_text(vocab, 700)));
        let d = &j["data"];
        let p = d["p_same_author"].as_f64().expect("calibrated: P present");
        let v = d["verdict"].as_str().unwrap();
        let from_p = if p > 0.65 {
            "same_author"
        } else if p < 0.35 {
            "different_author"
        } else {
            "inconclusive"
        };
        assert_eq!(v, from_p, "verdict must follow P={p}");
        assert_eq!(v, want);
        assert_eq!(d["band_words"], 500, "largest band not exceeding 700 words");
    }
}

#[test]
fn calibrate_refuses_a_blend_and_compare_scores_it_uncalibrated() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["profile", "blend", "ab", "--from", "a=0.6", "--from", "b=0.4"])
        .assert()
        .success();
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["calibrate", "ab"])
        .assert()
        .code(3);
    let j = run_json(data.path(), &["compare", "-", "--profile", "ab"], Some(corpus_text(&VOCAB_A, 600)));
    let d = &j["data"];
    assert!(d["p_same_author"].is_null(), "a blend never has a P");
    assert!(d["verdict"].as_str().unwrap().ends_with("_uncalibrated"));
}

#[test]
fn long_text_is_scored_passage_by_passage() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());
    run_json(data.path(), &["calibrate", "a"], None);

    // Calibrated bands reach 1,000 words on a 4,000-word corpus; 6,000 words
    // is well over twice that.
    let text = format!("{}\n\n{}", corpus_text(&VOCAB_A, 3000), corpus_text(&VOCAB_C, 3000));
    let j = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(text));
    let d = &j["data"];
    let band = d["band_words"].as_u64().unwrap();
    assert!(6000 > 2 * band);
    let passages = d["passages"].as_array().expect("passages for a long text");
    assert!(passages.len() >= 4);
    assert_eq!(passages[0]["start_line"], 1);
    for p in passages {
        for f in ["start_line", "words", "cosine_delta", "p_same_author", "nearest_profile"] {
            assert!(!p[f].is_null(), "passage field {f}: {p}");
        }
    }
    // The drift map shows where the voice changes.
    assert_eq!(passages.first().unwrap()["nearest_profile"], "a");
    assert_eq!(passages.last().unwrap()["nearest_profile"], "c");
}

// ── Short-form: pooled posts ────────────────────────────────────────────────

/// `n` posts of ~30 words in `vocab`, offset by `seed` so batches differ,
/// separated by `---` lines.
fn posts(vocab: &[&str], n: usize, seed: usize) -> String {
    (0..n)
        .map(|i| {
            let words: Vec<&str> = (0..30)
                .map(|j| {
                    let k = seed * 31 + i * 7 + j;
                    if j % 2 == 0 { vocab[(k * 5 + k / 3) % vocab.len()] } else { GLUE[(k + k / 5) % GLUE.len()] }
                })
                .collect();
            format!("{}.", words.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

/// A social profile `x` of 450 undated vocab-A posts (its sources are
/// 200-item blocks), plus three long-form imposters.
fn social_and_imposters(data: &Path, corpus: &Path) {
    let dir = corpus.join("x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("posts.txt"), posts(&VOCAB_A, 450, 0)).unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(["profile", "build", "x", "--register", "social", "--corpus"])
        .arg(&dir)
        .assert()
        .success();
    build_profile(data, corpus, "b", &VOCAB_B, 6000);
    build_profile(data, corpus, "c", &VOCAB_C, 6000);
    build_profile(data, corpus, "d", &VOCAB_D, 6000);
}

#[test]
fn social_profile_calibrates_on_pooled_posts_and_compare_pools_a_batch() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());

    // Posts of ~30 words have no 250-word passage of their own: the bands
    // are pooled windows, held out across the two sources.
    let j = run_json(data.path(), &["calibrate", "x"], None);
    let d = &j["data"];
    assert_eq!(d["register"], "social");
    assert_eq!(d["unit"], "pooled posts");
    assert_eq!(d["cross_work"], true, "three 200-item blocks: held out across them");
    let bands: Vec<u64> = d["bands"].as_array().unwrap().iter().map(|b| b["words"].as_u64().unwrap()).collect();
    assert_eq!(bands[0], 250);
    assert!(bands.iter().all(|w| [250, 500, 1000].contains(w)), "{bands:?}");
    let warnings = d["warnings"].to_string();
    assert!(warnings.contains("other poster(s)") && warnings.contains("--background"), "{warnings}");
    let next = j["metadata"]["next"].to_string();
    assert!(next.contains("compare <posts-separated-by-dashes.md> --profile x"), "{next}");

    // A batch of ten unseen posts is pooled into one verdict. Against books
    // alone P(same) only says "posts, not books": never same_author.
    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, 7)));
    let d = &j["data"];
    assert_eq!(d["items"], 10);
    assert_eq!(d["words"], 300);
    assert_eq!(d["band_words"], 250);
    assert!(d["p_same_author"].as_f64().unwrap() > 0.65, "{d}");
    assert_eq!(d["verdict"], "inconclusive", "{d}");
    assert!(d["warnings"].to_string().contains("separates posts from books"), "{d}");
    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_C, 10, 7)));
    assert_eq!(j["data"]["verdict"], "different_author");

    // A long batch is scored window by window, each window whole posts.
    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 40, 9)));
    let passages = j["data"]["passages"].as_array().expect("windows of posts");
    assert!(passages.len() >= 2);
    assert_eq!(passages[0]["start_line"], 1);
    // Each post is one line plus a `---` line: posts start on odd lines.
    let second = passages[1]["start_line"].as_u64().unwrap();
    assert!(second > 1 && second % 2 == 1, "second window starts on a post's first line: {second}");
}

/// A short-form imposter: `n` posts in `vocab` (offset by `seed`).
fn build_poster(data: &Path, corpus: &Path, name: &str, vocab: &[&str], n: usize, seed: usize) {
    let f = corpus.join(format!("{name}.txt"));
    std::fs::write(&f, posts(vocab, n, seed)).unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(["profile", "build", name, "--register", "social", "--background", "--corpus"])
        .arg(&f)
        .assert()
        .success();
}

/// Three posters, in the books' vocabularies B, C, D.
fn three_posters(data: &Path, corpus: &Path) {
    for (name, vocab) in [("pb", &VOCAB_B), ("pc", &VOCAB_C), ("pd", &VOCAB_D)] {
        build_poster(data, corpus, name, vocab, 120, 3);
    }
}

#[test]
fn other_posters_as_imposters_allow_a_same_author_verdict() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    three_posters(data.path(), corpus.path());
    // Fit against the three posters only (books are not this voice's rivals).
    let j = run_json(data.path(), &["calibrate", "x"], None);
    assert_eq!(j["data"]["imposters"], 3);
    let warnings = j["data"]["warnings"].to_string();
    assert!(!warnings.contains("separates posts from books"), "{warnings}");

    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, 7)));
    assert_eq!(j["data"]["verdict"], "same_author", "{}", j["data"]);
    assert_eq!(j["data"]["nearest_profile"], "x");
    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_C, 10, 7)));
    assert_eq!(j["data"]["verdict"], "different_author");
    assert_eq!(j["data"]["nearest_profile"], "pc");
}

#[test]
fn a_books_only_calibration_stays_guarded_after_posters_are_added() {
    // Calibrated before any other poster existed, P(same author) separates
    // posts from books. Adding posters without re-calibrating must not turn
    // that into same_author (it did: 18 of 19 batches of an unseen X author).
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    run_json(data.path(), &["calibrate", "x"], None);
    three_posters(data.path(), corpus.path());

    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, 7)));
    let d = &j["data"];
    assert!(d["p_same_author"].as_f64().unwrap() > 0.65, "{d}");
    assert_eq!(d["calibration_stale"], true);
    assert_eq!(d["verdict"], "inconclusive", "{d}");
    assert!(d["warnings"].to_string().contains("calibrated against books only"), "{d}");
    let next = j["metadata"]["next"].to_string();
    assert!(next.contains("calibrate x"), "{next}");

    // Re-calibrated against the posters, the same batch verifies.
    run_json(data.path(), &["calibrate", "x"], None);
    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, 7)));
    assert_eq!(j["data"]["verdict"], "same_author", "{}", j["data"]);
}

#[test]
fn a_posts_calibration_gone_stale_withholds_same_author() {
    // A poster added after calibrating was never an imposter: until
    // re-calibration, a batch that passes is reported inconclusive.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    three_posters(data.path(), corpus.path());
    run_json(data.path(), &["calibrate", "x"], None);
    build_poster(data.path(), corpus.path(), "pe", &VOCAB_B, 120, 11);

    let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, 7)));
    let d = &j["data"];
    assert_eq!(d["calibration_stale"], true);
    assert!(d["p_same_author"].as_f64().unwrap() > 0.65, "{d}");
    assert_eq!(d["verdict"], "inconclusive", "{d}");
    assert!(d["warnings"].to_string().contains("pe added since calibration, never an imposter"), "{d}");
}

#[test]
fn posts_are_ranked_only_against_posters_in_a_posts_model() {
    // A book whose text is the very posts of the batch: in the posters'
    // reference model it can sit nearer than any poster, so a posts model
    // ranks posters (and the target) only.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    three_posters(data.path(), corpus.path());
    let book = corpus.path().join("c-posts-book.txt");
    std::fs::write(&book, posts(&VOCAB_C, 200, 7).replace("\n---\n", "\n\n")).unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["profile", "build", "cbook", "--register", "nonfiction", "--background", "--corpus"])
        .arg(&book)
        .assert()
        .success();

    let batch = posts(&VOCAB_C, 10, 7);
    for calibrated in [false, true] {
        if calibrated {
            run_json(data.path(), &["calibrate", "x"], None);
        }
        let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(batch.clone()));
        let d = &j["data"];
        assert_eq!(d["nearest_profile"], "pc", "calibrated={calibrated}: {d}");
        let ranked: Vec<&str> = d["ranking"].as_array().unwrap().iter().map(|r| r["profile"].as_str().unwrap()).collect();
        assert!(ranked.iter().all(|n| ["x", "pb", "pc", "pd"].contains(n)), "books ranked: {ranked:?}");
    }
}

#[test]
fn a_posts_same_author_needs_the_target_nearest() {
    // Two posters share the target's vocabulary: whichever is nearest, a
    // same_author verdict always names the target as the nearest poster.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    three_posters(data.path(), corpus.path());
    build_poster(data.path(), corpus.path(), "pa", &VOCAB_A, 200, 5);
    run_json(data.path(), &["calibrate", "x"], None);

    let mut withheld = 0;
    for seed in 20..32 {
        let j = run_json(data.path(), &["compare", "-", "--profile", "x"], Some(posts(&VOCAB_A, 10, seed)));
        let d = &j["data"];
        let p = d["p_same_author"].as_f64().unwrap();
        let warnings = d["warnings"].to_string();
        if d["verdict"] == "same_author" {
            assert_eq!(d["nearest_profile"], "x", "{d}");
        } else if p > 0.65 {
            withheld += 1;
            assert!(warnings.contains("withheld"), "{d}");
        }
    }
    assert!(withheld > 0, "the gate was never exercised");
}

#[test]
fn calibrate_tells_a_posts_voice_to_add_posters_not_books() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    let add = "profile build <name> --corpus <their-posts.json> --background";

    // No posters yet: fit against books, the fix is posters.
    let j = run_json(data.path(), &["calibrate", "x"], None);
    let next: Vec<&str> = j["metadata"]["next"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(next[0].contains(add), "{next:?}");
    assert!(next.iter().all(|n| !n.contains("background fetch")), "{next:?}");
    let warnings = j["data"]["warnings"].to_string();
    assert!(warnings.contains("0 other poster(s)") && !warnings.contains("background fetch"), "{warnings}");

    // Three posters (under ten): still more posters, never `background fetch`.
    three_posters(data.path(), corpus.path());
    let j = run_json(data.path(), &["calibrate", "x"], None);
    assert_eq!(j["data"]["imposters"], 3);
    let next: Vec<&str> = j["metadata"]["next"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(next[0].contains(add) && next[1].contains("calibrate x"), "{next:?}");
    assert!(next.iter().all(|n| !n.contains("background fetch")), "{next:?}");
    let warnings = j["data"]["warnings"].to_string();
    assert!(warnings.contains("3 other poster(s)") && !warnings.contains("background fetch"), "{warnings}");
}

#[test]
fn a_short_batch_says_how_much_to_pool() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    social_and_imposters(data.path(), corpus.path());
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["compare", "-", "--profile", "x", "--json"])
        .write_stdin(posts(&VOCAB_A, 3, 2))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(err["error"]["message"].as_str().unwrap().contains("across 3 items"), "{err}");
    let fix = err["error"]["suggestion"].to_string() + &err.to_string();
    assert!(fix.contains("---"), "{err}");
}

#[test]
fn long_form_scene_breaks_are_not_items() {
    // A *** scene break in a chapter is not a batch: no `items`, and the
    // separator itself adds nothing to the score.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());
    let plain = corpus_text(&VOCAB_A, 600);
    let broken = plain.replacen("\n\n", "\n\n***\n\n", 3);
    let a = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(plain));
    let b = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(broken));
    assert!(b["data"]["items"].is_null());
    assert_eq!(a["data"]["cosine_delta"], b["data"]["cosine_delta"]);
}

// ── Frozen reference stability ──────────────────────────────────────────────

#[test]
fn frozen_reference_survives_profile_removal() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    four_authors(data.path(), corpus.path());
    run_json(data.path(), &["calibrate", "a"], None);

    let q = corpus_text(&VOCAB_A, 600);
    let j1 = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(q.clone()));
    assert_eq!(j1["data"]["calibration_stale"], false);

    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .args(["profile", "remove", "d", "--confirm"])
        .assert()
        .success();

    let j2 = run_json(data.path(), &["compare", "-", "--profile", "a"], Some(q));
    assert_eq!(j1["data"]["cosine_delta"], j2["data"]["cosine_delta"], "delta bit-identical");
    assert_eq!(j1["data"]["p_same_author"], j2["data"]["p_same_author"], "P bit-identical");
    assert_eq!(j2["data"]["calibration_stale"], true, "profile set changed -> stale");
    assert!(!j2["data"]["verdict"].as_str().unwrap().ends_with("_uncalibrated"));
}

// ── background ──────────────────────────────────────────────────────────────

/// A fake Gutenberg file: header, a body of `vocab` prose, licence footer.
fn gutenberg(vocab: &[&str]) -> String {
    format!(
        "Title: Test\n\n*** START OF THE PROJECT GUTENBERG EBOOK TEST ***\n\n{}\n\n*** END OF THE PROJECT GUTENBERG EBOOK TEST ***\nLicence text.\n",
        corpus_text(vocab, 5000)
    )
}

#[test]
fn background_fetch_builds_from_a_local_source_and_lists_failures() {
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    // Two catalog ids (A Tale of Two Cities, Silas Marner) present locally.
    std::fs::write(src.path().join("98.txt"), gutenberg(&VOCAB_B)).unwrap();
    std::fs::write(src.path().join("550.txt"), gutenberg(&VOCAB_C)).unwrap();

    let mut cmd = bin();
    cmd.env("STYLOMETRY_DATA_DIR", data.path())
        .env("STYLOMETRY_BACKGROUND_SOURCE", src.path())
        .args(["background", "fetch"]);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let d = &j["data"];
    let built: Vec<&str> = d["built"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(built, ["bg-dickens-two-cities", "bg-eliot-silas-marner"]);
    assert!(d["failed"].as_array().unwrap().len() >= 30, "missing files are listed, not fatal");
    assert_eq!(d["installed"], 2);

    // Second run skips what is installed.
    let j = run_json(data.path(), &["background", "status"], None);
    assert_eq!(j["data"]["installed"], 2);
    assert!(j["data"]["missing"].as_u64().unwrap() >= 30);
    let listed = run_json(data.path(), &["profile", "list", "--all"], None);
    assert!(listed.to_string().contains("bg-dickens-two-cities"));
}

#[test]
fn background_fetch_with_every_download_failing_is_an_error() {
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    bin()
        .env("STYLOMETRY_DATA_DIR", data.path())
        .env("STYLOMETRY_BACKGROUND_SOURCE", src.path())
        .args(["background", "fetch"])
        .assert()
        .code(2);
}
