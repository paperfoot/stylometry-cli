//! Contracts for writing help: `guide` (a brief per register), `examples`
//! (retrieval), register-aware `analyze` and `diff`. Pins the JSON fields
//! agents read, the brief's sections and length budget, and the failure
//! cases the v0.4 critiques found: topic nouns as "recurring words",
//! word-choice rows that order up tells, blog openings skipped, a
//! fiction template for X posts; and round 2's typography, mechanics and
//! "what gives imitations away". Every test gets its OWN tempdir for
//! STYLOMETRY_DATA_DIR; corpora are synthetic.

use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

fn run_json(data: &Path, args: &[&str]) -> Value {
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn build(data: &Path, name: &str, corpus: &Path, register: &str) {
    run_json(
        data,
        &["profile", "build", name, "--corpus", corpus.to_str().unwrap(), "--register", register],
    );
}

// ── Synthetic corpora ───────────────────────────────────────────────────────

const NOUNS: [&str; 8] = ["door", "window", "phone", "coat", "table", "key", "car", "bag"];

/// First-person present narration with dialogue; each work has its own
/// topic nouns, used heavily there and nowhere else.
fn fiction_work(topic: &[&str], paras: usize, seed: usize) -> String {
    let mut out = Vec::new();
    for i in 0..paras {
        let n = |k: usize| NOUNS[(i * 3 + k + seed) % NOUNS.len()];
        let t = |k: usize| topic[(i + k) % topic.len()];
        let para = match i % 4 {
            0 => format!(
                "I walk to the {} and look at the {}. My hands shake when I open the {}. The {} is quiet tonight, and I am tired, but I keep going. I gaze at the {} for a long time.",
                n(0), t(0), n(1), t(1), n(2)
            ),
            1 => format!(
                "\u{201C}Where is the {}?\u{201D} she asks. \u{201C}I don\u{2019}t know,\u{201D} I say. \u{201C}Maybe near the {}.\u{201D} She shakes her head and looks at me.",
                t(2), n(3)
            ),
            2 => format!(
                "He is standing by the {} when I come back. I don\u{2019}t say anything. I just watch him, and he watches the {}, and neither of us moves. I think about the {} and the {}.",
                n(4), t(0), n(5), t(1)
            ),
            _ => format!(
                "I gaze at him. \u{201C}You came back,\u{201D} he says. \u{201C}I always do,\u{201D} I tell him, and I mean it. The {} is still open and the {} is gone.",
                n(6), t(2)
            ),
        };
        out.push(para);
    }
    out.join("\n\n")
}

fn fiction_corpus(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("one.txt"), fiction_work(&["galaxy", "starship", "captain"], 120, 0)).unwrap();
    std::fs::write(dir.join("two.txt"), fiction_work(&["garden", "orchard", "beehive"], 120, 1)).unwrap();
    std::fs::write(dir.join("three.txt"), fiction_work(&["kitchen", "stove", "pantry"], 120, 2)).unwrap();
}

const TOPICS: [&str; 6] = ["hiring", "pricing", "fundraising", "writing", "cities", "schools"];

/// Essays whose openings are distinct sentences about their topic.
fn blog_corpus(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for (i, topic) in TOPICS.iter().enumerate() {
        let mut paras = vec![format!(
            "Most people get {topic} wrong in the same way. I learned this the slow way, and it took me years to notice."
        )];
        for j in 0..14 {
            paras.push(format!(
                "When you think about {topic}, you notice that the obvious answer is usually the one people skip. \
                 It seems strange, but it happens again and again. If you look closely at {topic} you see why: \
                 the easy part is visible and the hard part is not. So the work is mostly about seeing the hard part early, \
                 and that is a skill you can practise. Paragraph {j} of essay {i} makes the point once more."
            ));
        }
        paras.push(format!("So next time {topic} comes up, start with the hard part."));
        std::fs::write(dir.join(format!("{topic}.md")), paras.join("\n\n")).unwrap();
    }
}

/// Posts separated by `---`: lowercase, short, a recurring formula, a few
/// about coffee.
fn social_corpus(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let mut posts = Vec::new();
    for i in 0..48 {
        let p = match i % 6 {
            0 => format!("new episode of the pod is out, we talk about sleep and training block {i}"),
            1 => format!("coffee first, then code. brewing a pour over with beans from batch {i}"),
            2 => format!("shipped a tiny fix today. tests green, users happy, going for a walk now ({i})"),
            3 => format!("hot take, most dashboards are noise. track three numbers and ignore the rest, run {i}"),
            4 => format!("reading about cities and why some streets feel alive while others never do, note {i}"),
            _ => format!("tbh the best tools are boring. cron, sqlite, a text file. that is it, list {i}"),
        };
        posts.push(p);
    }
    std::fs::write(dir.join("posts.txt"), posts.join("\n---\n")).unwrap();
}

fn markdown(v: &Value) -> String {
    v["data"]["markdown"].as_str().unwrap().to_string()
}

fn sections(v: &Value) -> Vec<String> {
    v["data"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

// ── guide ───────────────────────────────────────────────────────────────────

#[test]
fn fiction_brief_has_modes_dialogue_and_stays_short() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("fic"));
    fiction_corpus(&corpus);
    build(&data, "fic", &corpus, "fiction");
    let v = run_json(&data, &["guide", "fic"]);
    let d = &v["data"];
    assert_eq!(d["register"], "fiction");
    assert_eq!(d["task"], "none");
    let secs = sections(&v);
    for want in ["Narration mode", "Dialogue", "What gives imitations away", "Loop"] {
        assert!(secs.iter().any(|s| s.starts_with(want)), "missing {want}: {secs:?}");
    }
    assert!(secs.iter().any(|s| s.contains("Scene rhythm targets")), "{secs:?}");
    let words = d["brief_words"].as_u64().unwrap();
    assert!(words > 100 && words <= 1200, "brief_words {words}");
    assert!(d["exemplar_words"].as_u64().unwrap() > 0);
    assert!(!d["exemplars"].as_array().unwrap().is_empty());
    let md = markdown(&v);
    assert!(md.contains("first person, present tense"), "{md}");
    // Topic nouns of one work are subjects, not voice.
    let recurring = md.lines().find(|l| l.contains("Recurring words")).unwrap_or("");
    for topic in ["galaxy", "starship", "garden", "kitchen"] {
        assert!(!recurring.contains(topic), "topic noun listed as a habit: {recurring}");
    }
    // A word-choice row never orders up a machine tell ("gazed" is slop).
    assert!(!md.contains("**gaze**") && !md.contains("**gazed**"), "{md}");
}

#[test]
fn guide_for_previous_chapter_warms_up_from_its_end() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("fic"));
    fiction_corpus(&corpus);
    build(&data, "fic", &corpus, "fiction");
    let prev = tmp.path().join("chapter1.md");
    let mut chapter = fiction_work(&["lighthouse", "harbour", "ferry"], 30, 3);
    chapter.push_str("\n\nThe last line of my chapter is this one, about the ferry at dawn.");
    std::fs::write(&prev, &chapter).unwrap();
    let v = run_json(&data, &["guide", "fic", "--for", prev.to_str().unwrap()]);
    assert_eq!(v["data"]["task"], "for");
    let secs = sections(&v);
    assert!(secs.iter().any(|s| s.contains("re-read the end of your previous text")), "{secs:?}");
    assert!(markdown(&v).contains("> The last line of my chapter is this one, about the ferry at dawn."));
}

/// An outline is the task, not text to continue: the warm-up is the
/// author's own passage, never the outline's bullets quoted back.
#[test]
fn guide_for_an_outline_warms_up_from_the_author() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("fic"));
    fiction_corpus(&corpus);
    build(&data, "fic", &corpus, "fiction");
    let outline = tmp.path().join("outline.md");
    std::fs::write(
        &outline,
        "# Chapter 12 outline\n\n\
         - Open in the kitchen at night; she is waiting up when he comes home late from the office.\n\
         - She has found the second phone in his coat pocket and put it on the table between them.\n\
         - He tries to laugh it off, then gets angry, then goes quiet; she keeps her voice low.\n\
         - Midpoint: he admits the phone is his but refuses to say who he calls on it.\n\
         - She threatens to call the number herself; he grabs the phone and the glass falls and breaks.\n\
         - End on her sweeping up the glass alone while he sits in the car with the engine running.\n",
    )
    .unwrap();
    let v = run_json(&data, &["guide", "fic", "--for", outline.to_str().unwrap()]);
    assert_eq!(v["data"]["task"], "for");
    let secs = sections(&v);
    assert!(!secs.iter().any(|s| s.contains("re-read the end of your previous text")), "{secs:?}");
    assert!(secs.iter().any(|s| s.starts_with("Warm-up: continue this passage")), "{secs:?}");
    assert!(!markdown(&v).contains("> - Open in the kitchen"), "outline quoted as a warm-up");
}

#[test]
fn blog_brief_quotes_real_openings_and_closings() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("blog"));
    blog_corpus(&corpus);
    build(&data, "essays", &corpus, "blog");
    let v = run_json(&data, &["guide", "essays", "--about", "pricing a product"]);
    assert_eq!(v["data"]["register"], "blog");
    let secs = sections(&v);
    for want in ["Openings", "Closings", "Stance and transitions"] {
        assert!(secs.iter().any(|s| s == want), "missing {want}: {secs:?}");
    }
    let md = markdown(&v);
    assert!(md.contains("Most people get pricing wrong in the same way. I learned this the slow way"), "{md}");
    assert!(md.contains("of 6 pieces open with a question"), "{md}");
    assert!(v["data"]["brief_words"].as_u64().unwrap() <= 1200);
}

#[test]
fn social_brief_is_about_posts_not_scenes() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("x"));
    social_corpus(&corpus);
    build(&data, "poster", &corpus, "social");
    let v = run_json(&data, &["guide", "poster"]);
    let d = &v["data"];
    assert_eq!(d["register"], "social");
    assert_eq!(d["kind"], "post");
    let secs = sections(&v);
    for want in ["Length and layout", "Hooks", "Vocabulary", "What gives imitations away", "Loop"] {
        assert!(secs.iter().any(|s| s == want), "missing {want}: {secs:?}");
    }
    assert!(secs.iter().any(|s| s.starts_with("Example posts")), "{secs:?}");
    assert!(!secs.iter().any(|s| s.contains("Scene") || s.contains("Narration")), "{secs:?}");
    // 6–10 example posts.
    let n = d["exemplars"].as_array().unwrap().len();
    assert!((6..=10).contains(&n), "{n} examples");
    let md = markdown(&v);
    assert!(md.contains("48 of 48 start in lowercase"), "{md}");
    assert!(md.contains("new episode of the pod is out"), "formula missing: {md}");
    assert!(md.contains("tbh"), "informal word missing: {md}");
    // Social tells are banned only when unused: "hot take" is the author's.
    let avoid = md.lines().find(|l| l.contains("Social-post tells")).unwrap();
    assert!(avoid.contains("let that sink in") && avoid.contains("🧵👇"), "{avoid}");
    assert!(!avoid.contains("hot take"), "{avoid}");
    assert!(!md.contains("dialogue"), "fiction wording in a social brief: {md}");
}

/// The typography, mechanics and giveaway sections follow the analyze
/// report: rendered when it measures them, absent (never empty) when it
/// doesn't, in every register, and the brief stays inside its budget.
#[test]
fn briefs_carry_typography_mechanics_and_giveaways() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (fic, blog, x) = (tmp.path().join("fic"), tmp.path().join("blog"), tmp.path().join("x"));
    fiction_corpus(&fic);
    blog_corpus(&blog);
    social_corpus(&x);
    build(&data, "fic", &fic, "fiction");
    build(&data, "essays", &blog, "blog");
    build(&data, "poster", &x, "social");
    for name in ["fic", "essays", "poster"] {
        let a = run_json(&data, &["analyze", name]);
        let typo = &a["data"]["typography"];
        for family in ["double_quotes", "apostrophes", "ellipses", "dashes", "scene_breaks"] {
            assert!(typo[family].is_array(), "{name}: typography.{family} missing: {typo}");
        }
        let has_band = |key: &str| a["data"]["ranges"]["bands"].as_array().unwrap().iter().any(|b| b["key"] == key);
        let v = run_json(&data, &["guide", name]);
        let (md, secs) = (markdown(&v), sections(&v));
        let families = if name == "poster" { 4 } else { 5 };
        let measured = ["double_quotes", "apostrophes", "ellipses", "dashes", "scene_breaks"][..families]
            .iter()
            .any(|f| !typo[*f].as_array().unwrap().is_empty());
        assert_eq!(secs.iter().any(|s| s.starts_with("Typography")), measured, "{name}: {secs:?}");
        assert_eq!(secs.iter().any(|s| s == "Mechanics"), has_band("double_punct_per_1k"), "{name}: {secs:?}");
        assert!(secs.iter().any(|s| s == "What gives imitations away"), "{name}: {secs:?}");
        assert!(md.contains("Their punctuation and quote marks are part of the voice"), "{name}: no exemplar note");
        assert!(!md.contains("## Avoid") && !md.contains("## Never"), "{name}: old sections left: {secs:?}");
        let words = v["data"]["brief_words"].as_u64().unwrap();
        assert!(words <= 1200, "{name}: brief_words {words}");
        // Slips are stated, never prescribed.
        assert!(!md.contains("typo"), "{name}: {md}");
        if name == "fic" && !typo["double_quotes"].as_array().unwrap().is_empty() {
            // The synthetic novel is set with curly quotes and apostrophes.
            assert!(md.contains("Quotes: curly \u{201C} \u{201D} (100%)"), "{md}");
            assert!(md.contains("Agents default to forms this author never uses: straight \" quotes"), "{md}");
        }
    }
}

/// Edited emails: every sentence ends on a full stop, every email on
/// "Best,\nBoris". The item ends on the name, so its missing full stop is
/// the sign-off's, not a loose hand: the brief must not call the writer
/// informal for it, nor list "ends with . ? ! or …" as a zero.
#[test]
fn email_sign_offs_are_not_a_missing_full_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("mail"));
    std::fs::create_dir_all(&corpus).unwrap();
    let names = ["Sam", "Priya", "Tom", "Lena", "Marco", "Ada"];
    let mails: Vec<String> = (0..60)
        .map(|i| {
            format!(
                "Hi {},\n\nThank you for sending the {} over. I have read it carefully and the terms look fine to me. \
                 Could you confirm the date before Friday? I would like to tell the team. Reference {i}.\n\nBest,\nBoris",
                names[i % names.len()],
                NOUNS[i % NOUNS.len()]
            )
        })
        .collect();
    std::fs::write(corpus.join("mail.txt"), mails.join("\n---\n")).unwrap();
    build(&data, "mailer", &corpus, "email");
    let md = markdown(&run_json(&data, &["guide", "mailer"]));
    assert!(md.contains("Sign-offs:"), "{md}");
    assert!(!md.contains("no final full stop"), "{md}");
    assert!(!md.contains("ends with . ? ! or"), "{md}");
    assert!(!md.contains("Polish: informal"), "{md}");
}

// ── examples ────────────────────────────────────────────────────────────────

#[test]
fn examples_match_the_task_or_say_none() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("x"));
    social_corpus(&corpus);
    build(&data, "poster", &corpus, "social");
    let v = run_json(&data, &["examples", "poster", "--about", "brewing coffee beans", "-n", "3"]);
    let d = &v["data"];
    assert_eq!(d["task"], "about");
    assert_eq!(d["topic_match"], "strong");
    let ex = d["examples"].as_array().unwrap();
    assert_eq!(ex.len(), 3);
    for key in ["rank", "source", "similarity", "topic", "words", "text", "kind"] {
        assert!(ex[0].get(key).is_some(), "missing {key}: {}", ex[0]);
    }
    assert!(ex[0]["text"].as_str().unwrap().contains("coffee"), "{}", ex[0]);
    // Few-shot examples carry the author's marks: say so once.
    assert!(d["typography_note"].as_str().unwrap().starts_with("Their punctuation and quote marks are part of the voice"), "{d}");
    assert!(v["metadata"]["next"][0].as_str().unwrap().contains("guide poster --about"));

    let v = run_json(&data, &["examples", "poster", "--about", "quantum chromodynamics lattice"]);
    assert_eq!(v["data"]["topic_match"], "none");
    assert!(v["data"]["notes"][0].as_str().unwrap().contains("typical pieces"));
    assert_eq!(v["data"]["examples"].as_array().unwrap().len(), 5);
}

/// One shared word is never a strong topic, however much one work dwells on
/// it, and an off-topic task's "best" pieces never read as near-perfect
/// matches; a task that shares several of a work's words is strong.
#[test]
fn examples_one_shared_word_is_not_a_strong_topic() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("fic"));
    fiction_corpus(&corpus);
    build(&data, "fic", &corpus, "fiction");
    for task in ["a recipe for sourdough bread in the kitchen", "people", "a recipe for sourdough bread"] {
        let v = run_json(&data, &["examples", "fic", "--about", task, "-n", "3"]);
        let d = &v["data"];
        assert_ne!(d["topic_match"], "strong", "{task}: {d}");
        assert!(d["notes"][0].as_str().is_some_and(|n| !n.is_empty()), "{task}: no note: {d}");
        for e in d["examples"].as_array().unwrap() {
            assert!(e["similarity"].as_f64().unwrap() < 0.9, "{task}: {e}");
        }
    }
    let v = run_json(&data, &["examples", "fic", "--about", "the starship captain in the galaxy", "-n", "3"]);
    assert_eq!(v["data"]["topic_match"], "strong", "{}", v["data"]);
    assert_eq!(v["data"]["examples"][0]["source"], "one.txt");

    // The guide says the same, with the topic overlap on every exemplar row.
    let v = run_json(&data, &["guide", "fic", "--about", "a recipe for sourdough bread"]);
    let d = &v["data"];
    assert_ne!(d["topic_match"], "strong");
    assert!(d["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("typical pieces")), "{d}");
    assert!(d["exemplars"].as_array().unwrap().iter().all(|e| e["topic"].as_f64().is_some()), "{d}");
}

/// One long work on gardens and two short ones on other subjects: every
/// window that matches a garden task belongs to the long work.
fn dominant_corpus(dir: &Path) {
    const COMMON: [&str; 12] = ["the", "man", "went", "out", "to", "see", "how", "the", "day", "would", "turn", "and"];
    let work = |topic: &[&str], paras: usize| -> String {
        (0..paras)
            .map(|i| {
                let mut words: Vec<String> = Vec::new();
                for j in 0..60 {
                    words.push(if j % 7 == 3 { topic[(i * 5 + j) % topic.len()].to_string() } else { COMMON[(i + j) % COMMON.len()].to_string() });
                    if j % 12 == 11 {
                        words.last_mut().unwrap().push('.');
                    }
                }
                let mut t = words.join(" ");
                t[..1].make_ascii_uppercase();
                t.replace(". t", ". T").replace(". m", ". M").replace(". a", ". A")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    std::fs::create_dir_all(dir).unwrap();
    let garden = ["garden", "roses", "pruning", "compost", "hedge", "bulbs", "greenhouse", "mulch"];
    std::fs::write(dir.join("a_garden.txt"), work(&garden, 800)).unwrap();
    std::fs::write(dir.join("b_sea.txt"), work(&["harbour", "ships", "tide", "anchor", "gulls"], 60)).unwrap();
    std::fs::write(dir.join("c_rail.txt"), work(&["railway", "engine", "platform", "signal", "tunnel"], 60)).unwrap();
}

/// A dominant work fills the shortlist; one-per-work must not stop at one
/// pick waiting for works that are not in it, and the guide keeps its
/// warm-up passage.
#[test]
fn examples_fill_n_when_one_work_dominates() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("garden"));
    dominant_corpus(&corpus);
    build(&data, "garden", &corpus, "nonfiction");
    let v = run_json(&data, &["examples", "garden", "--about", "pruning roses in the garden", "-n", "3"]);
    let ex = v["data"]["examples"].as_array().unwrap();
    assert_eq!(ex.len(), 3, "{}", v["data"]);
    assert!(ex.iter().all(|e| e["source"] == "a_garden.txt"), "{ex:?}");
    let v = run_json(&data, &["guide", "garden", "--about", "pruning roses in the garden"]);
    assert_eq!(v["data"]["exemplars"].as_array().unwrap().len(), 3);
    assert!(sections(&v).iter().any(|s| s.starts_with("Warm-up")), "{:?}", sections(&v));
    // Without a task, one per work still comes first.
    let v = run_json(&data, &["examples", "garden", "-n", "3"]);
    let mut srcs: Vec<&str> = v["data"]["examples"].as_array().unwrap().iter().map(|e| e["source"].as_str().unwrap()).collect();
    srcs.sort_unstable();
    assert_eq!(srcs, ["a_garden.txt", "b_sea.txt", "c_rail.txt"]);
}

#[test]
fn examples_long_form_returns_passages_with_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("blog"));
    blog_corpus(&corpus);
    build(&data, "essays", &corpus, "blog");
    let v = run_json(&data, &["examples", "essays", "--about", "hiring", "-n", "2"]);
    let ex = v["data"]["examples"].as_array().unwrap();
    assert_eq!(ex.len(), 2);
    assert_eq!(ex[0]["source"], "hiring.md");
    assert!(ex[0]["shape"].as_f64().is_some());
    assert!(ex[0]["words"].as_u64().unwrap() >= 250);
    // One per work first: the second pick comes from another essay.
    assert_ne!(ex[1]["source"], "hiring.md");
}

#[test]
fn examples_rejects_zero_with_a_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, corpus) = (tmp.path().join("data"), tmp.path().join("x"));
    social_corpus(&corpus);
    build(&data, "poster", &corpus, "social");
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", &data)
        .args(["examples", "poster", "-n", "0", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(v["error"]["suggestion"].as_str().unwrap().contains("examples poster -n 5"), "{v}");
}

// ── analyze and diff ────────────────────────────────────────────────────────

#[test]
fn analyze_reports_register_modes_items_and_formulas() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (fic, x) = (tmp.path().join("fic"), tmp.path().join("x"));
    fiction_corpus(&fic);
    social_corpus(&x);
    build(&data, "fic", &fic, "fiction");
    build(&data, "poster", &x, "social");

    let v = run_json(&data, &["analyze", "fic"]);
    let d = &v["data"];
    assert_eq!(d["register"], "fiction");
    assert!(d["headline"].as_array().unwrap().len() >= 5);
    let modes = d["modes"]["modes"].as_array().unwrap();
    assert_eq!(modes[0]["pov"], "first");
    assert_eq!(modes[0]["tense"], "present");
    assert!(d.get("items").is_none());

    let v = run_json(&data, &["analyze", "poster"]);
    let d = &v["data"];
    assert_eq!(d["register"], "social");
    assert!(d.get("modes").is_none());
    assert_eq!(d["items"][0]["kind"], "post");
    assert_eq!(d["items"][0]["items"], 48);
    let formulas: Vec<&str> = d["formulas"].as_array().unwrap().iter().map(|f| f["phrase"].as_str().unwrap()).collect();
    assert!(formulas.iter().any(|f| f.starts_with("new episode of the pod is out")), "{formulas:?}");
    assert!(d["social_tells_unused"].as_array().unwrap().iter().any(|t| t == "let that sink in"));
}

#[test]
fn diff_compares_short_form_at_a_pooled_window() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (blog, x) = (tmp.path().join("blog"), tmp.path().join("x"));
    blog_corpus(&blog);
    social_corpus(&x);
    build(&data, "essays", &blog, "blog");
    build(&data, "poster", &x, "social");
    let v = run_json(&data, &["diff", "poster", "essays"]);
    assert_eq!(v["data"]["window_words"], 250);
    let a: Vec<&str> = v["data"]["a_words"].as_array().unwrap().iter().map(|w| w["word"].as_str().unwrap()).collect();
    assert!(!a.contains(&"url") && !a.contains(&"user"), "{a:?}");
}
