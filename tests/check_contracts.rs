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
fn the_copy_guard_fails_lifted_lines_not_idioms_or_thank_you_names() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    // Each of these occurs once in the corpus; the names recur so the
    // profile knows them as names.
    let once = "And I'd be lying if I said I wasn't scared of the dark.\n\n\
                The gargoyles leered down at the rain-slick cobblestones below us.\n\n\
                We sat in the car for a long time and nobody said a word about it.\n\n\
                Thanks to Trevor Blackwell, Jessica Livingston and Robert Morris for reading drafts of this.\n\n"
        .to_string()
        + &"I had lunch with Jessica Livingston and we talked about the work for a while.\n\n\
            Later that week Robert Morris said the plan was fine and the rest could wait.\n\n"
            .repeat(5);
    for (i, seed) in [11u64, 22, 33].iter().enumerate() {
        let mut t = author_prose(*seed, 9000);
        if i == 0 {
            t.push_str(&once);
        }
        std::fs::write(corpus.path().join(format!("book{i}.txt")), t).unwrap();
    }
    let out = bin(data.path())
        .args(["profile", "build", "ann", "--register", "fiction", "--corpus"])
        .arg(corpus.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let draft = "And I'd be lying if I said I wasn't sure.\n\n\
                 The gargoyles leered down at the rain-slick cobblestones again.\n\n\
                 We sat in the car for a long time and nobody said a word about it.\n\n\
                 Thanks to Sam Altman, Jessica Livingston and Robert Morris for reading drafts of this.\n";
    let j = check_text(data.path(), "ann", draft, &[]);
    let copies: Vec<&Value> = j["data"]["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "copy_overlap").collect();
    let find = |key: &str| copies.iter().find(|f| f["key"].as_str().unwrap().starts_with(key)).copied();
    // A common idiom met once in the corpus is a soft note, not a failure.
    let idiom = find("and i'd be lying").expect("the shared idiom is still noted");
    assert_eq!(idiom["hard"], false, "an idiom is not a lifted line: {idiom}");
    assert!(idiom["message"].as_str().unwrap().contains("common words"));
    assert_eq!(find("the gargoyles leered").expect("rare-word run flagged")["hard"], true);
    assert_eq!(find("we sat in the").expect("12+ word run flagged")["hard"], true);
    assert!(
        !copies.iter().any(|f| f.to_string().contains("Livingston")),
        "a thank-you line of names is not copying: {copies:?}"
    );
    let plan = j["data"]["plan"][0]["action"].as_str().unwrap();
    assert!(plan.starts_with("Rewrite 2 passage(s)"), "only the lifted lines are rewrite steps: {plan}");
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
fn social_post(r: &mut Lcg) -> String {
    match r.next(4) {
        0 => format!("Shipped the {} fix today. It took {} tries.", NOUNS[r.next(16)], 2 + r.next(9)),
        1 => format!("Most people {} the {} too early. Wait a week first.", VERBS[r.next(12)], NOUNS[r.next(16)]),
        2 => format!("New episode of the pod is out. We talk about the {} and the {}.", NOUNS[r.next(16)], NOUNS[r.next(16)]),
        _ => format!("The {} is the product. Everything else is a {}.", NOUNS[r.next(16)], NOUNS[r.next(16)]),
    }
}

fn build_social(data: &Path, dir: &Path) {
    let mut r = Lcg(5);
    let mut lines = String::new();
    for i in 0..120 {
        let text = social_post(&mut r);
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
fn a_post_with_one_off_llm_tells_does_not_pass_cleanly() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let plain = check_text(data.path(), "bo", "Shipped the lamp fix today. It took 4 tries to get into it.", &[]);
    let draft = "Shipped the lamp fix today. It took 4 tries to delve into this tapestry.";
    let j = check_text(data.path(), "bo", draft, &[]);
    let d = &j["data"];
    let tells: Vec<&Value> = d["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "llm_tell").collect();
    for key in ["delve", "tapestry"] {
        let f = tells.iter().find(|f| f["key"] == key).unwrap_or_else(|| panic!("one '{key}' is flagged: {}", d["findings"]));
        assert_eq!(f["count"], 1);
        assert_eq!(f["lines"], serde_json::json!([1]));
        assert!(f["message"].as_str().unwrap().contains("never writes it"), "{f}");
    }
    assert!(d["score"].as_f64().unwrap() < plain["data"]["score"].as_f64().unwrap(), "tells lower the post's score");
    let tell_habit = d["habits"].as_array().unwrap().iter().find(|h| h["key"] == "llm_tells_per_item").unwrap();
    assert_eq!(tell_habit["draft_value"], 2.0);
    assert!(d["plan"].to_string().contains("Cut or rephrase the LLM tells"), "{}", d["plan"]);
    let next = j["metadata"]["next"][0].as_str().unwrap();
    assert!(!next.starts_with("Final pass"), "steps left: re-check before the final pass: {next}");

    // Three stock phrases in one post: a habit none of the author's 120 posts show.
    let j = check_text(data.path(), "bo", "A testament to the lamp fix: we delve into the tapestry today.", &[]);
    assert_eq!(j["data"]["pass"], false);
    let never = j["data"]["findings"].as_array().unwrap().iter().find(|f| f["kind"] == "never" && f["key"] == "stock_phrases");
    assert!(never.is_some_and(|f| f["hard"] == true && f["message"].as_str().unwrap().contains("of 120")), "{}", j["data"]["findings"]);
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
fn a_batch_of_150_plus_words_is_also_judged_on_window_rates() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());

    let mut r = Lcg(77);
    let own: Vec<String> = (0..18).map(|_| social_post(&mut r)).collect();
    let own = check_text(data.path(), "bo", &own.join("\n---\n"), &[]);
    let d = &own["data"];
    assert!(d["words"].as_u64().unwrap() >= 150);
    let families: Vec<&str> = d["score_basis"]["families"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    for f in ["items", "rhythm", "punctuation", "lexis", "function_words"] {
        assert!(families.contains(&f), "{f} judged at 150+ words: {families:?}");
    }
    assert!(d["metrics"].as_array().unwrap().iter().any(|r| r["family"] == "punctuation"));
    assert!(d["score_basis"]["window_words"].as_u64().unwrap() >= 150);
    let parts = &d["score_basis"]["parts"];
    assert!(parts["items"].is_number() && parts["windows"].is_number(), "{parts}");
    assert!(d["score_basis"]["note"].as_str().unwrap().contains("Fisher"));
    assert!(!d["warnings"].to_string().contains("post-level habits only"));
    // (Templated posts share whole runs with the templated corpus, so copy
    // findings are expected here; the score is what this test is about.)
    assert!(d["score"].as_f64().unwrap() >= 10.0, "the author's own posts score as theirs: {}", d["score"]);

    // Post-shaped (one line, a full stop, no emoji or tags) but in another
    // voice: the item habits fit, the rates don't.
    let other: Vec<String> = (0..12)
        .map(|i| format!("Organisational transformation requires leaders, managers, and stakeholders to coordinate considerations, opportunities, and constraints {i}."))
        .collect();
    let other = check_text(data.path(), "bo", &other.join("\n---\n"), &[]);
    let w = |j: &Value| j["data"]["score_basis"]["parts"]["windows"].as_f64().unwrap();
    assert!(w(&other) < 10.0 && w(&other) < w(&own), "rates expose the other voice: {} vs {}", w(&other), w(&own));
    assert!(other["data"]["score"].as_f64().unwrap() < own["data"]["score"].as_f64().unwrap());
    assert!(other["data"]["findings"].as_array().unwrap().iter().any(|f| f["kind"] == "metric_out_of_range" && f["key"] != "item_words"));
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

// ── Typography, mechanics and polish ────────────────────────────────────────

/// Book typesetting: curly quotes and apostrophes.
fn curly(t: &str) -> String {
    let cs: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut open = true;
    for (i, &c) in cs.iter().enumerate() {
        match c {
            '"' => {
                out.push(if open { '\u{201C}' } else { '\u{201D}' });
                open = !open;
            }
            '\'' if i > 0 && cs[i - 1].is_alphabetic() => out.push('\u{2019}'),
            _ => out.push(c),
        }
    }
    out
}

/// A curly-set author whose books break scenes with "• • •".
fn build_curly_fiction(data: &Path, dir: &Path) {
    for (i, seed) in [11u64, 22, 33].iter().enumerate() {
        let book: Vec<String> = (0..6).map(|k| curly(&author_prose(*seed * 10 + k, 1500))).collect();
        std::fs::write(dir.join(format!("book{i}.txt")), book.join("\n\n\u{2022} \u{2022} \u{2022}\n\n")).unwrap();
    }
    let out = bin(data).args(["profile", "build", "cora", "--register", "fiction", "--corpus"]).arg(dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// An essayist of long abstract sentences: no quotes, no short beats.
fn build_plain(data: &Path, dir: &Path) {
    for i in 0..3 {
        std::fs::write(dir.join(format!("essay{i}.txt")), other_prose(4000)).unwrap();
    }
    let out = bin(data).args(["profile", "build", "plain", "--register", "nonfiction", "--corpus"]).arg(dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn findings_of<'a>(j: &'a Value, kind: &str) -> Vec<&'a Value> {
    j["data"]["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == kind).collect()
}

fn row<'a>(j: &'a Value, key: &str) -> &'a Value {
    j["data"]["metrics"].as_array().unwrap().iter().find(|r| r["key"] == key).unwrap_or_else(|| panic!("no {key} row"))
}

#[test]
fn typography_findings_give_the_exact_conversion_and_lead_the_plan() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_curly_fiction(data.path(), corpus.path());

    // The author's voice, typed by an agent: straight quotes, a '---' break.
    let draft = format!("{}\n---\n\n{}", author_prose(501, 450), author_prose(502, 450));
    let straight_quotes = draft.matches('"').count();
    let j = check_text(data.path(), "cora", &draft, &[]);
    let typo = findings_of(&j, "typography");
    let by = |key: &str| typo.iter().find(|f| f["key"] == key).copied().unwrap_or_else(|| panic!("no {key}: {typo:?}"));
    let q = by("double_quotes:straight");
    assert_eq!(q["count"].as_u64().unwrap() as usize, straight_quotes, "every straight quote, one finding");
    assert!(!q["lines"].as_array().unwrap().is_empty());
    let s = q["suggestion"].as_str().unwrap();
    assert!(s.starts_with("Use curly quotes") && s.contains("\u{201C} to open, \u{201D} to close"), "{s}");
    assert!(s.contains(&format!("{straight_quotes} straight quotes")), "{s}");
    assert!(q["message"].as_str().unwrap().contains("100%"), "{q}");
    assert!(by("apostrophes:straight")["suggestion"].as_str().unwrap().contains('\u{2019}'));
    let brk = by("scene_breaks:---");
    assert!(brk["suggestion"].as_str().unwrap().contains("\u{2022} \u{2022} \u{2022}"), "{brk}");
    assert_eq!(j["data"]["counts"]["typography"], 3);

    // Mechanical and certain: the first step after any hard ones.
    let plan = j["data"]["plan"].as_array().unwrap();
    let first_soft = plan.iter().find(|s| s["hard"] == false).unwrap();
    assert!(first_soft["action"].as_str().unwrap().starts_with("Set type the author's way"), "{plan:?}");

    // The shares score in their own family; the finding, not a metric row, says what to do.
    for (key, family) in [
        ("curly_quote_share", "typography"),
        ("curly_apostrophe_share", "typography"),
        ("double_punct_per_1k", "mechanics"),
        ("space_before_punct_per_1k", "mechanics"),
        ("nonword_per_1k", "mechanics"),
        ("short_triplets_per_1k", "constructions"),
        ("anaphora_runs_per_1k", "constructions"),
        ("mirror_pairs_per_1k", "constructions"),
    ] {
        assert_eq!(row(&j, key)["family"], family, "{key}");
    }
    assert_eq!(row(&j, "curly_quote_share")["status"], "out_of_range");
    assert!(!findings_of(&j, "metric_out_of_range").iter().any(|f| f["key"] == "curly_quote_share"));

    // Set the author's way, the same words have no typography finding and
    // score higher; a closed-up "•••" is the author's mark, set tighter.
    let fixed = curly(&draft).replace("\n---\n", "\n\u{2022}\u{2022}\u{2022}\n");
    let k = check_text(data.path(), "cora", &fixed, &[]);
    assert_eq!(k["data"]["counts"]["typography"], 0, "{}", k["data"]["findings"]);
    assert!(k["data"]["score"].as_f64().unwrap() > j["data"]["score"].as_f64().unwrap());
}

#[test]
fn typography_is_author_relative() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    // The author types straight quotes: straight is their voice, curly is not.
    let j = check_text(data.path(), "ann", &author_prose(601, 600), &[]);
    assert_eq!(j["data"]["counts"]["typography"], 0, "{}", j["data"]["findings"]);
    let j = check_text(data.path(), "ann", &curly(&author_prose(601, 600)), &[]);
    let q = findings_of(&j, "typography").into_iter().find(|f| f["key"] == "double_quotes:curly").expect("curly quotes flagged");
    assert!(q["suggestion"].as_str().unwrap().starts_with("Use straight quotes"), "{q}");

    // A corpus without a single quote has no quote style to hold a draft to.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_plain(data.path(), corpus.path());
    let j = check_text(data.path(), "plain", &author_prose(602, 500), &[]);
    assert_eq!(row(&j, "curly_quote_share")["status"], "unmeasured");
    assert_eq!(row(&j, "curly_apostrophe_share")["status"], "unmeasured");
    assert_eq!(j["data"]["counts"]["typography"], 0);
}

#[test]
fn typos_count_in_the_score_but_never_become_advice() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    // An author who slips: misspelt words every few sentences.
    for (i, seed) in [11u64, 22, 33].iter().enumerate() {
        let t = author_prose(*seed, 9000).replace("anymore.", "anymroe.").replace("quiet.", "quiettt.");
        std::fs::write(corpus.path().join(format!("book{i}.txt")), t).unwrap();
    }
    let out = bin(data.path())
        .args(["profile", "build", "slip", "--register", "fiction", "--corpus"])
        .arg(corpus.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let j = check_text(data.path(), "slip", &author_prose(701, 900), &[]);
    let r = row(&j, "nonword_per_1k");
    assert_eq!(r["family"], "mechanics");
    assert!(r["value"].as_f64().unwrap() < r["low"].as_f64().unwrap(), "the clean draft sits below the author's slips: {r}");
    assert_ne!(r["status"], "in_range");
    let all = format!("{}{}", j["data"]["plan"], j["data"]["findings"]);
    assert!(!all.contains("nonword") && !all.to_lowercase().contains("typo") && !all.contains("misspel"), "never told to misspell: {all}");
}

#[test]
fn polish_runs_above_the_authors_band_are_cited_and_fewer_is_never_a_fault() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let mut draft = author_prose(801, 500);
    for k in 0..4 {
        draft.push_str("I want him to stay with me tonight. I don't want him to stay with me tonight.\n\n");
        draft.push_str(&author_prose(900 + k, 20));
    }
    let pair_lines: Vec<usize> = (1..).zip(draft.lines()).filter(|(_, l)| l.starts_with("I want him")).map(|(i, _)| i).collect();
    let j = check_text(data.path(), "ann", &draft, &[]);
    let m = findings_of(&j, "structure")
        .into_iter()
        .find(|f| f["key"] == "mirror_pairs_per_1k")
        .unwrap_or_else(|| panic!("mirrored pairs cited: {}", j["data"]["findings"]));
    assert_eq!(m["count"], 4, "one per pair: {m}");
    let lines: Vec<usize> = m["lines"].as_array().unwrap().iter().map(|l| l.as_u64().unwrap() as usize).collect();
    assert_eq!(lines, pair_lines);
    assert!(m["excerpts"][0].as_str().unwrap().contains("I don't want him to stay"));
    assert!(
        !findings_of(&j, "metric_out_of_range").iter().any(|f| f["key"] == "mirror_pairs_per_1k"),
        "the cited runs replace the bare metric"
    );

    // The author's own prose has no mirrors to cut, and a draft with no
    // short beats at all is not told to add some.
    let own = check_text(data.path(), "ann", &author_prose(802, 900), &[]);
    assert!(!own["data"]["findings"].to_string().contains("mirror_pairs"));
    let flat = check_text(data.path(), "ann", &other_prose(900), &[]);
    let t = row(&flat, "short_triplets_per_1k");
    assert_eq!(t["value"], 0.0);
    assert_eq!(t["status"], "in_range", "one-sided: fewer tidy beats is never off-voice: {t}");
    assert!(!flat["data"]["plan"].to_string().contains("short sentences (6 words"));
}

#[test]
fn polish_runs_stop_at_paragraphs_so_speakers_and_posts_are_never_one_run() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_plain(data.path(), corpus.path());

    // A quick exchange, one speaker per paragraph: no beats anyone could join.
    let lines = ["\"Nothing.\"", "\"That's not a number.\"", "\"Jeremy.\""];
    let mut turns = other_prose(500);
    let mut tidy = turns.clone();
    for _ in 0..4 {
        turns.push_str(&format!("\n\n{}\n\n{}", lines.join("\n\n"), other_prose(30)));
        tidy.push_str(&format!("\n\nI try to laugh. It comes out wrong. He steps in.\n\n{}", other_prose(30)));
    }
    let j = check_text(data.path(), "plain", &turns, &[]);
    assert_eq!(row(&j, "short_triplets_per_1k")["value"], 0.0, "{}", row(&j, "short_triplets_per_1k"));
    assert!(!j["data"]["findings"].to_string().contains("short_triplets"), "{}", j["data"]["findings"]);

    // The same beats inside one paragraph are a run, cited on its own line.
    let k = check_text(data.path(), "plain", &tidy, &[]);
    let t = findings_of(&k, "structure").into_iter().find(|f| f["key"] == "short_triplets_per_1k").expect("tidy beats cited");
    let want: Vec<usize> = (1..).zip(tidy.lines()).filter(|(_, l)| l.starts_with("I try to laugh")).map(|(i, _)| i).collect();
    let got: Vec<usize> = t["lines"].as_array().unwrap().iter().map(|l| l.as_u64().unwrap() as usize).collect();
    assert_eq!(got, want);
    assert_eq!(t["excerpts"][0], "I try to laugh. It comes out wrong. He steps in.");

    // Posts: the end of one and the start of the next are not one run.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_social(data.path(), corpus.path());
    let mut r = Lcg(78);
    let own: Vec<String> = (0..18).map(|_| social_post(&mut r)).collect();
    let j = check_text(data.path(), "bo", &own.join("\n---\n"), &[]);
    assert_eq!(row(&j, "short_triplets_per_1k")["value"], 0.0, "two beats a post at most: {}", row(&j, "short_triplets_per_1k"));
}

#[test]
fn the_ellipsis_glyph_is_typography_not_rhythm() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let speech = "\"I don't know... Maybe it's fine... Maybe it isn't... We'll see what happens when he gets home.\"\n\n";
    let mut dots = String::new();
    for k in 0..4 {
        dots.push_str(speech);
        dots.push_str(&author_prose(1100 + k, 120));
    }
    let glyph = dots.replace("...", "\u{2026}");
    let (a, b) = (check_text(data.path(), "ann", &dots, &[]), check_text(data.path(), "ann", &glyph, &[]));
    for key in ["sentence_words_mean", "short_sentence_share", "short_triplets_per_1k", "anaphora_runs_per_1k"] {
        assert_eq!(row(&a, key)["value"], row(&b, key)["value"], "{key} moved with the glyph");
    }
    for j in [&a, &b] {
        assert!(!findings_of(j, "structure").iter().any(|f| f["excerpts"].to_string().contains("Maybe it")), "{}", j["data"]["findings"]);
    }
}

#[test]
fn one_polish_run_is_within_any_authors_band() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    // The author never mirrors a sentence; one mirrored pair in a scene is
    // still theirs to write. Three are a habit they don't have.
    let mut draft = author_prose(1201, 600);
    draft.push_str("I want him to stay with me tonight. I don't want him to stay with me tonight.\n\n");
    let j = check_text(data.path(), "ann", &draft, &[]);
    let m = row(&j, "mirror_pairs_per_1k");
    assert_eq!(m["median"], 0.0, "{m}");
    assert_eq!(m["status"], "in_range", "{m}");
    assert!(!j["data"]["findings"].to_string().contains("mirror_pairs"), "{}", j["data"]["findings"]);
    for k in 0..2 {
        draft.push_str(&author_prose(1210 + k, 20));
        draft.push_str("I want him to stay with me tonight. I don't want him to stay with me tonight.\n\n");
    }
    let j = check_text(data.path(), "ann", &draft, &[]);
    assert_eq!(row(&j, "mirror_pairs_per_1k")["status"], "out_of_range");
}

#[test]
fn habits_the_draft_keeps_add_nothing_to_the_score() {
    // A straight-quote author: a draft set straight keeps the habit, so
    // typography and mechanics are reported but left out of the score.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());
    let j = check_text(data.path(), "ann", &author_prose(1301, 900), &[]);
    let families = j["data"]["score_basis"]["families"].to_string();
    assert!(!families.contains("typography") && !families.contains("mechanics"), "{families}");
    assert_eq!(row(&j, "curly_quote_share")["status"], "in_range");

    // A curly-set author: the straight draft departs, and typography counts.
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_curly_fiction(data.path(), corpus.path());
    let j = check_text(data.path(), "cora", &author_prose(1302, 900), &[]);
    assert!(j["data"]["score_basis"]["families"].to_string().contains("typography"), "{}", j["data"]["score_basis"]);
}

/// The blog writer in round 2 was told to "spell out 17 contractions" for a
/// draft whose contracted share was already the author's: spelling out
/// fixed the rate and broke the share. The rate step now says to rephrase.
#[test]
fn too_many_contractions_is_rephrased_not_spelled_out() {
    let data = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    build_fiction(data.path(), corpus.path());

    let lines = ["I don't know.", "It's late.", "I'm here and it's cold.", "We're not leaving, I don't think.", "It's the door.", "That's what I'm saying."];
    let draft: Vec<String> = (0..40).map(|i| format!("{} {}", lines[i % 6], lines[(i + 1) % 6])).collect();
    let j = check_text(data.path(), "ann", &draft.join("\n\n"), &[]);
    assert_eq!(row(&j, "contractions_per_1k")["status"], "out_of_range", "{}", row(&j, "contractions_per_1k"));
    assert_ne!(row(&j, "contraction_share")["status"], "out_of_range");
    let plan = j["data"]["plan"].to_string();
    assert!(plan.contains("rephrase so fewer forms contract"), "{plan}");
    assert!(!plan.contains("spell out more"), "{plan}");
}
