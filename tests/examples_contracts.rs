//! Contracts for JEV re-ranking in `examples` (and through retrieval,
//! `guide`): with a task described in words, essays, blogs and posts are
//! ordered by JEV's fit to the task (`rank_basis: "jev"`, per-example
//! `fit`); anything else, or any JEV failure, keeps the word ranking
//! (`rank_basis: "lexical"`). JEV is a mock HTTP server in the test, so
//! these stay offline and deterministic; corpora are synthetic.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::Value;

/// How the mock answers.
#[derive(Clone, Copy)]
enum Mode {
    /// Fit by the topic word the passage holds (see `fit_of`).
    Fit,
    /// Every passage fits poorly.
    Low,
    /// The sailing essay fits at 1.9 (similar), the rest poorly.
    Near,
    /// 200 with a body that is not JSON.
    BadJson,
    /// Answers after the client's timeout.
    Slow,
    /// Takes the request and answers after 30 s (a hanging API).
    Hang,
}

/// Fit the mock gives a passage: by which essay's topic word it holds.
fn fit_of(passage: &str) -> f64 {
    for (word, fit) in [("harbor", 3.0), ("bishop", 2.0), ("saucepan", 1.0)] {
        if passage.contains(word) {
            return fit;
        }
    }
    0.2
}

struct Mock {
    url: String,
    requests: Arc<AtomicUsize>,
    /// Longest passage sent, in words.
    longest: Arc<AtomicUsize>,
}

/// A JEV stand-in on 127.0.0.1: one thread per connection, answers built
/// from the request's `author_passage`.
fn mock(mode: Mode) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let longest = Arc::new(AtomicUsize::new(0));
    let (count, most) = (requests.clone(), longest.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (count, most) = (count.clone(), most.clone());
            std::thread::spawn(move || answer(stream, mode, &count, &most));
        }
    });
    Mock { url, requests, longest }
}

fn answer(mut s: TcpStream, mode: Mode, count: &AtomicUsize, longest: &AtomicUsize) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let body_start = loop {
        let Ok(n) = s.read(&mut chunk) else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..body_start]).to_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < body_start + len {
        let Ok(n) = s.read(&mut chunk) else { return };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    count.fetch_add(1, Ordering::SeqCst);
    let req: Value = serde_json::from_slice(&buf[body_start..]).unwrap_or(Value::Null);
    let passage = req["state"]["author_passage"].as_str().unwrap_or("");
    longest.fetch_max(passage.split_whitespace().count(), Ordering::SeqCst);
    let body = match mode {
        Mode::Fit => format!(r#"{{"answers":{{"fit":{{"type":"score","score":{}}}}},"usage":{{"input_tokens":50}}}}"#, fit_of(passage)),
        Mode::Low => r#"{"answers":{"fit":{"type":"score","score":0.4}},"usage":{"input_tokens":50}}"#.to_string(),
        Mode::Near => {
            let fit = if passage.contains("harbor") { 1.9 } else { 0.4 };
            format!(r#"{{"answers":{{"fit":{{"type":"score","score":{fit}}}}}}}"#)
        }
        Mode::BadJson => "not json".to_string(),
        Mode::Hang => {
            std::thread::sleep(Duration::from_secs(30));
            String::new()
        }
        Mode::Slow => {
            std::thread::sleep(Duration::from_millis(2500));
            r#"{"answers":{"fit":{"type":"score","score":3}}}"#.to_string()
        }
    };
    let _ = write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// `stylometry` with JEV off, as every other contract runs it.
fn bin_offline() -> Command {
    let mut c = Command::cargo_bin("stylometry").unwrap();
    c.env("STYLOMETRY_JEV__ENABLED", "false");
    c
}

/// `stylometry` with JEV on and pointed at `url` (never the real API).
fn bin_jev(url: &str) -> Command {
    let mut c = Command::cargo_bin("stylometry").unwrap();
    c.env("STYLOMETRY_JEV__ENABLED", "true")
        .env("STYLOMETRY_JEV__ENDPOINT", url)
        .env("STYLOMETRY_JEV__KEY_ENV", "STYLO_TEST_JEV_KEY")
        .env("STYLOMETRY_JEV__KEY_COMMAND", "")
        .env("STYLO_TEST_JEV_KEY", "dummy-key")
        .env("STYLOMETRY_JEV__TIMEOUT_MS", "800");
    c
}

fn json_of(mut c: Command, data: &Path, args: &[&str]) -> Value {
    let out = c.env("STYLOMETRY_DATA_DIR", data).args(args).arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Eight essays of `paras` paragraphs (7: one window each), each on its own
/// subject: a few topic nouns used in every paragraph and nowhere else,
/// around shared plain sentences.
fn essays(dir: &Path, paras: usize) {
    let topics: [(&str, [&str; 3]); 8] = [
        ("gardening", ["compost", "seedlings", "trowel"]),
        ("sailing", ["harbor", "mainsail", "tiller"]),
        ("chess", ["bishop", "gambit", "endgame"]),
        ("cooking", ["saucepan", "simmer", "shallots"]),
        ("cycling", ["derailleur", "saddle", "spokes"]),
        ("pottery", ["kiln", "glaze", "clay"]),
        ("birding", ["warbler", "binoculars", "plumage"]),
        ("baking", ["sourdough", "starter", "crust"]),
    ];
    for (name, [a, b, c]) in topics {
        let paras: Vec<String> = (0..paras)
            .map(|i| {
                format!(
                    "I keep coming back to the {a}. Most people think the {b} is the hard part, but it is the {c} that \
                     teaches you. You start out sure of yourself, and then the {a} shows you what you missed. \
                     I have done this for years, and paragraph {i} is where I admit the {b} still surprises me. \
                     If you try it, go slowly, watch the {c}, and expect to be wrong about the {a} more than once."
                )
            })
            .collect();
        std::fs::write(dir.join(format!("{name}.md")), paras.join("\n\n")).unwrap();
    }
}

fn blog_profile(data: &Path) {
    blog_profile_of(data, 7);
}

fn blog_profile_of(data: &Path, paras: usize) {
    let corpus = data.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    essays(&corpus, paras);
    json_of(
        bin_offline(),
        data,
        &["profile", "build", "ess", "--corpus", corpus.to_str().unwrap(), "--register", "blog"],
    );
}

/// A task that shares words only with the gardening essay.
const TASK: &str = "An essay about gardening: compost, seedlings and the trowel, and what they teach a beginner.";

fn sources(v: &Value) -> Vec<String> {
    v["data"]["examples"].as_array().unwrap().iter().map(|e| e["source"].as_str().unwrap().to_string()).collect()
}

fn notes(v: &Value) -> String {
    v["data"]["notes"].as_array().map(|n| n.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | ")).unwrap_or_default()
}

fn assert_lexical(v: &Value) {
    assert_eq!(v["data"]["rank_basis"], "lexical", "{v}");
    assert!(v["data"]["examples"].as_array().unwrap().iter().all(|e| e.get("fit").is_none()), "{v}");
    // Today's ranking: the essay that shares the task's words comes first.
    assert_eq!(sources(v)[0], "gardening.md", "{v}");
}

#[test]
fn without_jev_the_ranking_is_lexical() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let v = json_of(bin_offline(), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_lexical(&v);
    assert!(!notes(&v).contains("JEV"), "JEV off is the normal path, not a failure: {}", notes(&v));
}

#[test]
fn jev_ranks_by_fit_and_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Fit);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "jev", "{v}");
    // Order follows fit (3, 2, 1), not the shared words.
    assert_eq!(sources(&v), ["sailing.md", "chess.md", "cooking.md"], "{v}");
    let fits: Vec<f64> = v["data"]["examples"].as_array().unwrap().iter().map(|e| e["fit"].as_f64().unwrap()).collect();
    assert_eq!(fits, [3.0, 2.0, 1.0]);
    // The best fit (3) clears the strong line.
    assert_eq!(v["data"]["topic_match"], "strong");
    // One request per candidate (8 essays, one window each), each a short
    // excerpt of its ~500-word window, never the whole of it.
    assert_eq!(m.requests.load(Ordering::SeqCst), 8);
    let longest = m.longest.load(Ordering::SeqCst);
    assert!((60..=180).contains(&longest), "excerpt of {longest} words");
    // A second run is served from the cache.
    let again = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_eq!(sources(&again), sources(&v));
    assert_eq!(m.requests.load(Ordering::SeqCst), 8, "cached answers are reused");
}

#[test]
fn low_best_fit_says_nothing_fits() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Low);
    // No word of the task in the corpus, and nothing fits: none.
    let off = "A recipe for a lemon tart: oven temperatures, and how to tell when the custard has set.";
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", off, "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "jev");
    assert_eq!(v["data"]["topic_match"], "none", "{v}");
    assert!(notes(&v).contains("nothing in the corpus is like the task"), "{}", notes(&v));
}

#[test]
fn low_fit_lowers_a_word_match_to_weak_not_none() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let lexical = json_of(bin_offline(), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_eq!(lexical["data"]["topic_match"], "strong", "the gardening essay shares the task's words: {lexical}");
    let m = mock(Mode::Low);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "jev");
    // The corpus does take up the subject, so never "nothing is like it".
    assert_eq!(v["data"]["topic_match"], "weak", "{v}");
    assert!(notes(&v).contains("no passage is a close model"), "{}", notes(&v));
    assert!(!notes(&v).contains("nothing in the corpus"), "{}", notes(&v));
    // With equal fits the word ranking breaks the tie.
    assert_eq!(sources(&v)[0], "gardening.md");
}

#[test]
fn a_similar_piece_below_the_old_line_is_strong() {
    // Excerpts score lower than whole windows: a best fit of 1.9 (an essay
    // labelled close in the eval read 1.92) is a strong match.
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Near);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "jev");
    assert_eq!(v["data"]["topic_match"], "strong", "{v}");
    assert!(!notes(&v).contains("voice, not their subject"), "{}", notes(&v));
    // The brief passes retrieval's notes on as warnings: none here either.
    let g = json_of(bin_jev(&m.url), tmp.path(), &["guide", "ess", "--about", TASK]);
    assert_eq!(g["data"]["topic_match"], "strong", "{g}");
    assert!(!g["data"]["warnings"].to_string().contains("voice, not their subject"), "{}", g["data"]["warnings"]);
}

#[test]
fn fails_open_without_a_key() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Fit);
    let mut c = bin_jev(&m.url);
    c.env_remove("STYLO_TEST_JEV_KEY");
    let v = json_of(c, tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_lexical(&v);
    assert_eq!(m.requests.load(Ordering::SeqCst), 0);
    assert!(!notes(&v).contains("JEV"), "no key is the local path, not a failure: {}", notes(&v));
}

#[test]
fn fails_open_when_the_server_is_down() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let v = json_of(bin_jev(&format!("http://127.0.0.1:{port}/v1/systemone")), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_lexical(&v);
    assert!(notes(&v).contains("JEV did not judge"), "{}", notes(&v));
}

#[test]
fn fails_open_on_bad_json() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::BadJson);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_lexical(&v);
    assert!(notes(&v).contains("JEV did not judge every passage"), "{}", notes(&v));
}

#[test]
fn fails_open_on_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Slow);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    assert_lexical(&v);
    assert!(notes(&v).contains("JEV did not judge"), "{}", notes(&v));
}

#[test]
fn a_hanging_server_costs_one_budget_per_run() {
    // A full pool (24 candidates) and the default 8 s per-request timeout:
    // without an overall deadline that is three waves of 8 s, and a brief
    // paid it once per retrieval.
    let tmp = tempfile::tempdir().unwrap();
    blog_profile_of(tmp.path(), 28);
    let m = mock(Mode::Hang);
    let jev_default_timeout = || {
        let mut c = bin_jev(&m.url);
        c.env_remove("STYLOMETRY_JEV__TIMEOUT_MS");
        c
    };
    let start = Instant::now();
    let v = json_of(jev_default_timeout(), tmp.path(), &["examples", "ess", "--about", TASK, "-n", "3"]);
    let took = start.elapsed();
    assert!(v["data"]["candidates"].as_u64().unwrap() >= 24, "{v}");
    assert_lexical(&v);
    assert!(notes(&v).contains("within 3 s"), "{}", notes(&v));
    assert!(took < Duration::from_secs(7), "examples took {took:?}");
    assert!(m.requests.load(Ordering::SeqCst) <= 8, "one wave of workers, then give up");
    // The brief retrieves several times; after the first gives up, the rest
    // of the run keeps the word ranking without asking again.
    let before = m.requests.load(Ordering::SeqCst);
    let start = Instant::now();
    json_of(jev_default_timeout(), tmp.path(), &["guide", "ess", "--about", TASK]);
    let took = start.elapsed();
    assert!(took < Duration::from_secs(7), "guide took {took:?}");
    assert!(m.requests.load(Ordering::SeqCst) - before <= 8, "guide asked again after JEV gave up");
}

#[test]
fn no_task_or_fiction_never_asks_jev() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Fit);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "ess", "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "lexical");
    // Fiction: the same essays profiled as fiction keep scene-shape ranking.
    let corpus = tmp.path().join("corpus");
    json_of(
        bin_offline(),
        tmp.path(),
        &["profile", "build", "fic", "--corpus", corpus.to_str().unwrap(), "--register", "fiction"],
    );
    let v = json_of(bin_jev(&m.url), tmp.path(), &["examples", "fic", "--about", TASK, "-n", "3"]);
    assert_eq!(v["data"]["rank_basis"], "lexical");
    assert_eq!(m.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn guide_gets_the_jev_ranking() {
    let tmp = tempfile::tempdir().unwrap();
    blog_profile(tmp.path());
    let m = mock(Mode::Fit);
    let v = json_of(bin_jev(&m.url), tmp.path(), &["guide", "ess", "--about", TASK]);
    let brief = v["data"]["brief"].as_str().or_else(|| v["data"]["markdown"].as_str()).unwrap_or_default().to_string();
    let text = if brief.is_empty() { v.to_string() } else { brief };
    let (sail, garden) = (text.find("harbor"), text.find("compost"));
    assert!(sail.is_some(), "the best-fitting essay is an exemplar: {v}");
    assert!(garden.is_none_or(|g| sail.unwrap() < g), "fit outranks shared words in the brief");
    assert!(m.requests.load(Ordering::SeqCst) > 0);
}
