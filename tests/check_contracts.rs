//! Contracts for `check` v2: the fields agents act on (pass, score,
//! distance, score_basis, plan, grouped findings), the long-form and
//! short-form paths, hard rules (never-list, X length, copying), and that
//! the author's own unseen text scores above text in another voice. Every
//! test gets its own STYLOMETRY_DATA_DIR tempdir; corpora are synthetic.

use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn bin(data: &Path) -> Command {
    let mut c = Command::cargo_bin("stylometry").unwrap();
    c.env("STYLOMETRY_DATA_DIR", data);
    c
}

fn run_json(data: &Path, args: &[&str]) -> Value {
    let out = bin(data).args(args).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope")
}

/// Deterministic pseudo-random stream (no rand dependency).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

const NOUNS: [&str; 16] = [
    "door", "kitchen", "window", "street", "car", "phone", "table", "garden", "river", "coat",
    "letter", "stairs", "lamp", "bridge", "morning", "rain",
];
const VERBS: [&str; 12] = [
    "open", "watch", "leave", "find", "carry", "hold", "close", "wait for", "follow", "keep",
    "move", "check",
];

/// Prose in one voice: short first-person sentences, contractions, "looked"
/// as the only verb of looking, a line of dialogue now and then.
fn author_prose(seed: u64, words: usize) -> String {
    let mut r = Lcg(seed);
    let mut out = String::new();
    let mut n = 0;
    while n < words {
        let sentences = 3 + r.next(4);
        let mut para = Vec::new();
        for _ in 0..sentences {
            let s = match r.next(6) {
                0 => format!("I looked at the {}.", NOUNS[r.next(16)]),
                1 => format!("I don't {} the {} anymore.", VERBS[r.next(12)], NOUNS[r.next(16)]),
                2 => format!("\"It's the {},\" he said.", NOUNS[r.next(16)]),
                3 => format!("She looked at me and I {} the {} by the {}.", VERBS[r.next(12)], NOUNS[r.next(16)], NOUNS[r.next(16)]),
                4 => "I wait.".to_string(),
                _ => format!("We {} the {} and it's quiet.", VERBS[r.next(12)], NOUNS[r.next(16)]),
            };
            n += s.split_whitespace().count();
            para.push(s);
        }
        out.push_str(&para.join(" "));
        out.push_str("\n\n");
    }
    out
}

/// Another voice: long, comma-heavy, abstract, no contractions, no dialogue.
fn other_prose(words: usize) -> String {
    let s = "In the contemporary landscape of organisational transformation, leaders, managers, and \
             stakeholders must navigate a multifaceted tapestry of considerations, opportunities, and \
             constraints, cultivating resilience while fostering innovation across every dimension.";
    let mut out = String::new();
    let mut n = 0;
    while n < words {
        out.push_str(s);
        out.push_str(if n % 90 == 0 { "\n\n" } else { " " });
        n += s.split_whitespace().count();
    }
    out
}

fn build_fiction(data: &Path, dir: &Path) {
    for (i, seed) in [11u64, 22, 33].iter().enumerate() {
        std::fs::write(dir.join(format!("book{i}.txt")), author_prose(*seed, 9000)).unwrap();
    }
    let out = bin(data)
        .args(["profile", "build", "ann", "--register", "fiction", "--corpus"])
        .arg(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn check_text(data: &Path, profile: &str, text: &str, extra: &[&str]) -> Value {
    let mut args = vec!["check", "--profile", profile, "--text", text];
    args.extend_from_slice(extra);
    run_json(data, &args)
}

// ── Long-form ────────────────────────────────────────────────────────────────

#[test]
fn long_form_report_has_score_plan_and_grouped_findings() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let own = author_prose(99, 900);
    let j = check_text(data.path(), "ann", &own, &[]);
    let d = &j["data"];
    for k in ["pass", "score", "distance", "score_basis", "score_reliability", "plan", "findings", "metrics", "counts", "hard_failures"] {
        assert!(!d[k].is_null(), "missing {k}: {d}");
    }
    let score = d["score"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&score));
    assert_eq!(d["register"], "fiction");
    assert_eq!(d["score_basis"]["unit"], "windows");
    assert_eq!(d["score_basis"]["holdout"], "source", "three works: each window judged without its own work");
    assert!(d["score_basis"]["compared"].as_u64().unwrap() >= 8);
    assert!(d.get("voice_match").is_none(), "voice_match only with --verify");
    assert!(d["plan"].as_array().unwrap().len() <= 8);
    for f in d["findings"].as_array().unwrap() {
        for k in ["kind", "key", "count", "lines", "excerpts", "message", "suggestion", "hard"] {
            assert!(!f[k].is_null(), "finding lacks {k}: {f}");
        }
        assert!(f["excerpts"].as_array().unwrap().len() <= 3);
    }
    let next = j["metadata"]["next"].as_array().unwrap();
    assert!(!next.is_empty() && next[0].as_str().unwrap().contains("check"));
}

#[test]
fn own_unseen_text_scores_above_another_voice() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let own = check_text(data.path(), "ann", &author_prose(4242, 1200), &[]);
    let other = check_text(data.path(), "ann", &other_prose(1200), &[]);
    let (so, sx) = (own["data"]["score"].as_f64().unwrap(), other["data"]["score"].as_f64().unwrap());
    assert!(so >= 10.0, "own unseen text should pass the score bar: {so}");
    assert!(sx < 10.0, "another voice should fail: {sx}");
    assert!(own["data"]["distance"].as_f64().unwrap() < other["data"]["distance"].as_f64().unwrap());
    assert_eq!(other["data"]["pass"], false);
    // The plan names quantities, not just directions.
    let plan = other["data"]["plan"].as_array().unwrap();
    assert!(!plan.is_empty());
    assert!(plan.iter().all(|s| !s["quantity"].as_str().unwrap().is_empty() && !s["why"].as_str().unwrap().is_empty()));
}

#[test]
fn an_off_voice_word_is_one_finding_with_every_line_and_a_replacement_step() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let mut draft = author_prose(7, 400);
    draft.push_str("I gazed at the door.\n\nShe gazed at me.\n\nWe gazed at the rain.\n");
    let j = check_text(data.path(), "ann", &draft, &[]);
    let gazed: Vec<&Value> = j["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["key"] == "gazed")
        .collect();
    assert_eq!(gazed.len(), 1, "grouped, never one per sentence: {gazed:?}");
    assert_eq!(gazed[0]["count"], 3);
    assert_eq!(gazed[0]["lines"].as_array().unwrap().len(), 3);
    assert!(gazed[0]["suggestion"].as_str().unwrap().contains("looked"));
    let plan = j["data"]["plan"].to_string();
    assert!(plan.contains("Replace 'gazed' with 'looked'"), "{plan}");
}

#[test]
fn markup_the_corpus_never_uses_is_unmeasured() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let draft = format!("{}\n\nShe was *so* sure. **Never** again.\n", author_prose(5, 600));
    let j = check_text(data.path(), "ann", &draft, &[]);
    let rows = j["data"]["metrics"].as_array().unwrap();
    for key in ["italic_per_1k", "bold_per_1k", "headings_per_1k"] {
        let r = rows.iter().find(|r| r["key"] == key).unwrap();
        assert_eq!(r["status"], "unmeasured", "{key}: a .txt corpus has no markup to compare with");
    }
    assert!(!j["data"]["findings"].to_string().contains("italic"), "never 'remove italics' against plain text");
}

#[test]
fn adding_links_does_not_move_punctuation_rates_or_point_at_url_lines() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let base = author_prose(17, 700);
    let mut plain = String::new();
    let mut linked = String::new();
    for (i, para) in base.split("\n\n").filter(|p| !p.trim().is_empty()).enumerate() {
        plain.push_str(para);
        linked.push_str(para);
        if i % 3 == 0 {
            plain.push_str(" See this essay and for more.");
            linked.push_str(
                " See [this essay](https://example.com/2021/taste:aesthetics-03.html) and \
                 https://en.wikipedia.org/wiki/Taste_(sociology) for more.",
            );
        }
        plain.push_str("\n\n");
        linked.push_str("\n\n");
    }
    let rows = |t: &str| {
        let j = check_text(data.path(), "ann", t, &[]);
        let m = j["data"]["metrics"].as_array().unwrap().clone();
        (m, j["data"]["plan"].to_string())
    };
    let ((a, _), (b, plan)) = (rows(&plain), rows(&linked));
    for key in ["colons_per_1k", "parentheses_per_1k", "digits_per_1k", "echo_phrases_per_1k"] {
        let v = |m: &[Value]| m.iter().find(|r| r["key"] == key).unwrap()["value"].as_f64().unwrap();
        assert_eq!(v(&a), v(&b), "{key}: links must not count as prose");
    }
    assert!(!plan.contains("colons"), "no colon step from 'https:': {plan}");
}

// ── Short-form ───────────────────────────────────────────────────────────────

/// 120 plain posts: no hashtags, no emoji, mostly one line, ending in '.'.
fn build_social(data: &Path, dir: &Path) {
    let mut r = Lcg(5);
    let mut lines = String::new();
    for i in 0..120 {
        let text = match r.next(4) {
            0 => format!("Shipped the {} fix today. It took {} tries.", NOUNS[r.next(16)], 2 + r.next(9)),
            1 => format!("Most people {} the {} too early. Wait a week first.", VERBS[r.next(12)], NOUNS[r.next(16)]),
            2 => format!("New episode of the pod is out. We talk about the {} and the {}.", NOUNS[r.next(16)], NOUNS[r.next(16)]),
            _ => format!("The {} is the product. Everything else is a {}.", NOUNS[r.next(16)], NOUNS[r.next(16)]),
        };
        let row = serde_json::json!({"text": text, "likes": i % 7, "date": format!("2026-0{}-1{}", 1 + i % 9, i % 10)});
        lines.push_str(&row.to_string());
        lines.push('\n');
    }
    std::fs::write(dir.join("posts.jsonl"), lines).unwrap();
    let out = bin(data).args(["profile", "build", "bo", "--corpus"]).arg(dir.join("posts.jsonl")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_short_post_is_scored_on_post_habits_without_window_rates() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let j = check_text(data.path(), "bo", "Shipped the lamp fix today. It took 4 tries.", &[]);
    let d = &j["data"];
    assert_eq!(d["register"], "social");
    assert_eq!(d["score_basis"]["unit"], "items");
    assert_eq!(d["score_basis"]["kind"], "post");
    assert!(d["score"].as_f64().unwrap() >= 10.0, "a typical post passes: {}", d["score"]);
    assert_eq!(d["pass"], true);
    let habits = d["habits"].as_array().unwrap();
    let ending = habits.iter().find(|h| h["key"] == "ending").unwrap();
    assert_eq!(ending["draft_value"], ".");
    assert!(ending["author_share"].as_f64().unwrap() > 0.5);
    assert!(d["metrics"].as_array().unwrap().iter().all(|r| r["family"] == "items"), "no window rates for a 9-word post");
    assert!(d["warnings"].to_string().contains("150"));
}

#[test]
fn never_habits_and_x_length_are_hard_failures_with_counts() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let j = check_text(data.path(), "bo", "🚀 Excited to share the new lamp! #buildinpublic #AI", &[]);
    let d = &j["data"];
    assert_eq!(d["pass"], false);
    let never: Vec<&Value> = d["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "never").collect();
    let keys: Vec<&str> = never.iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"hashtags") && keys.contains(&"emoji"), "{keys:?}");
    assert!(never.iter().all(|f| f["hard"] == true));
    assert!(never[0]["message"].as_str().unwrap().contains("of 120"), "cites the author's counts");
    assert!(d["plan"][0]["hard"].as_bool().unwrap(), "hard steps come first");

    let long = "The lamp is the product and everything else is a detail we can fix later. ".repeat(5);
    let j = check_text(data.path(), "bo", long.trim(), &[]);
    assert!(j["data"]["findings"].as_array().unwrap().iter().any(|f| f["kind"] == "length" && f["hard"] == true));

    // --strict turns the failure into exit 5 with the report.
    let out = bin(data.path())
        .args(["check", "--profile", "bo", "--strict", "--text", "Big news! #launch"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(err["error"]["details"]["plan"].is_array());
}

#[test]
fn the_authors_own_formulas_are_not_copying() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    // "New episode of the pod is out. We talk about the" recurs in ~30 posts.
    let j = check_text(data.path(), "bo", "New episode of the pod is out. We talk about the moon and the tide.", &[]);
    assert!(!j["data"]["findings"].to_string().contains("copy_overlap"), "{}", j["data"]["findings"]);
}

#[test]
fn a_batch_of_posts_is_scored_per_item_and_compared_as_a_batch() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let draft = "Shipped the door fix today. It took 3 tries.\n---\nThe car is the product. Everything else is a coat.\n---\nMost people open the window too early. Wait a week first.\n";
    let j = check_text(data.path(), "bo", draft, &[]);
    let d = &j["data"];
    let items = d["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[1]["lines"], serde_json::json!([3, 3]));
    assert_eq!(d["batch"]["items"], 3);
    assert!(d["batch"]["author_endings"].is_array());
}

#[test]
fn a_draft_starting_with_a_handle_is_compared_with_replies() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let j = check_text(data.path(), "bo", "@sam thanks, the lamp fix took 4 tries.", &[]);
    assert_eq!(j["data"]["score_basis"]["kind"], "reply");
    assert!(j["data"]["warnings"].to_string().contains("replies"));
}

#[test]
fn verify_on_a_short_draft_explains_why_voice_match_is_skipped() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let j = check_text(data.path(), "bo", "Shipped the lamp fix today. It took 4 tries.", &["--verify"]);
    assert!(j["data"].get("voice_match").is_none());
    assert!(j["data"]["warnings"].to_string().contains("voice_match skipped"));
}
