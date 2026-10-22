//! Contracts for prose texture (JEV): the `texture` block of `check`, its
//! findings and plan step, the author's bands sidecar, the guide's Texture
//! section, and failing open. Offline: a mock JEV server inside the test
//! answers from the request (a marker word makes a passage read as smooth,
//! polished AI prose), and every failure mode (no key, server down, bad
//! JSON, timeout) must leave the rest of `check` byte-identical.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};

/// A passage containing this word reads as polished, smooth imitation.
const SMOOTH: &str = "luminous";

#[derive(Clone, Copy)]
enum Mode {
    Answer,
    BadJson,
    Slow,
}

/// A mock JEV endpoint on 127.0.0.1; lives as long as the test process.
fn mock(mode: Mode) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = serve(stream, mode);
            });
        }
    });
    format!("http://127.0.0.1:{port}/v1/systemone")
}

fn serve(mut stream: std::net::TcpStream, mode: Mode) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut len = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" {
            break;
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let out = match mode {
        Mode::Answer => answer(&serde_json::from_slice(&body).unwrap_or(Value::Null)).to_string(),
        Mode::BadJson => "{\"answers\": not json".to_string(),
        Mode::Slow => {
            std::thread::sleep(Duration::from_secs(3));
            answer(&serde_json::from_slice(&body).unwrap_or(Value::Null)).to_string()
        }
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
        out.len()
    )?;
    stream.flush()
}

/// Canned answers derived from the request: author-like values with a
/// little spread per passage, or smooth AI prose when the marker is there.
fn answer(req: &Value) -> Value {
    // Passage questions get `text`; the sentence drill-down gets `sentence`.
    let text = req["state"]["text"].as_str().or(req["state"]["sentence"].as_str()).unwrap_or("");
    let j = (text.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619)) % 1000) as f64 / 1000.0;
    let smooth = text.contains(SMOOTH);
    let value = |k: &str| match (k, smooth) {
        ("figure" | "crafted", true) => 0.9,
        ("figure" | "crafted", false) => 0.1,
        ("hedge" | "oneliner", _) => panic!("counted keys must never be asked of JEV: {k}"),
        ("rough", true) => 0.05,
        ("polish", true) => 2.8,
        ("button" | "figurative", true) => 0.9,
        ("oneliner", true) => 0.1,
        (_, true) => 0.05,
        ("rough", false) => 0.55 + 0.2 * j,
        ("polish", false) => 1.2 + 0.4 * j,
        ("button", false) => 0.2 + 0.2 * j,
        ("figurative", false) => 0.15 + 0.1 * j,
        ("overexplain", false) => 0.6 + 0.1 * j,
        ("repeat", false) => 0.35 + 0.1 * j,
        ("splice", false) => 0.15 + 0.05 * j,
        _ => 0.4 + 0.3 * j,
    };
    let mut answers = serde_json::Map::new();
    for (id, q) in req["questions"].as_object().cloned().unwrap_or_default() {
        let a = if q["type"] == "score" {
            json!({ "type": "score", "score": value(&id) })
        } else {
            json!({ "type": "noul", "noul": value(&id) })
        };
        answers.insert(id, a);
    }
    json!({ "answers": answers, "usage": { "input_tokens": 1000 } })
}

/// The binary with JEV off (the default for every other test file).
fn bin(data: &Path) -> Command {
    let mut c = Command::cargo_bin("stylometry").unwrap();
    c.env("STYLOMETRY_DATA_DIR", data).env("STYLOMETRY_JEV__ENABLED", "false").env_remove("TYPESAFE_API_KEY");
    c
}

/// The binary with JEV on against `endpoint`, with a dummy key.
fn jev(data: &Path, endpoint: &str) -> Command {
    let mut c = bin(data);
    c.env("STYLOMETRY_JEV__ENABLED", "true")
        .env("STYLOMETRY_JEV__ENDPOINT", endpoint)
        .env("STYLOMETRY_JEV__TIMEOUT_MS", "500")
        // A key command in the machine's config must not stand in for a missing key.
        .env("STYLOMETRY_JEV__KEY_COMMAND", "")
        .env("TYPESAFE_API_KEY", "test-key");
    c
}

fn json_of(mut c: Command, args: &[&str]) -> Value {
    let out = c.args(args).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope")
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

const NOUNS: [&str; 12] = ["door", "kitchen", "window", "street", "car", "phone", "table", "garden", "river", "coat", "lamp", "rain"];

/// Plain first-person prose; `marker` is woven into every paragraph.
fn prose(seed: u64, words: usize, marker: Option<&str>) -> String {
    let mut r = Lcg(seed);
    let (mut out, mut n) = (String::new(), 0);
    while n < words {
        let mut para = Vec::new();
        for _ in 0..3 + r.next(4) {
            let s = match r.next(5) {
                0 => format!("I looked at the {}.", NOUNS[r.next(12)]),
                1 => format!("I don't open the {} anymore.", NOUNS[r.next(12)]),
                2 => format!("\"It's the {},\" he said.", NOUNS[r.next(12)]),
                3 => format!("She looked at me and I closed the {} by the {}.", NOUNS[r.next(12)], NOUNS[r.next(12)]),
                _ => format!("We left the {} and it's quiet.", NOUNS[r.next(12)]),
            };
            n += s.split_whitespace().count();
            para.push(s);
        }
        if let Some(m) = marker {
            para.push(format!("The {m} light held the room."));
        }
        out.push_str(&para.join(" "));
        out.push_str("\n\n");
    }
    out
}

fn build(c: Command, corpus: &Path) -> Value {
    for (i, seed) in [11u64, 22, 33].iter().enumerate() {
        std::fs::write(corpus.join(format!("book{i}.txt")), prose(*seed, 5000, None)).unwrap();
    }
    let mut c = c;
    c.args(["profile", "build", "ann", "--register", "fiction", "--corpus"]).arg(corpus);
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn check(c: Command, draft: &Path) -> Value {
    json_of(c, &["check", draft.to_str().unwrap(), "--profile", "ann"])
}

/// Everything `check` reports except the texture block and what texture adds.
fn without_texture(v: &Value) -> Value {
    let mut d = v["data"].clone();
    d.as_object_mut().unwrap().remove("texture");
    d
}

#[test]
fn build_measures_bands_and_check_reports_the_texture_block() {
    let endpoint = mock(Mode::Answer);
    let (data, corpus) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let built = build(jev(data.path(), &endpoint), corpus.path());
    let t = &built["data"]["texture"];
    assert_eq!(t["status"], "measured", "{t}");
    assert!(t["usage"]["passages"].as_u64().unwrap() >= 20, "{t}");
    let sidecar = data.path().join("profiles/ann.texture.json");
    assert!(sidecar.exists());
    let list = json_of(bin(data.path()), &["profile", "list"]);
    assert_eq!(list["data"]["profiles"].as_array().unwrap().len(), 1, "the sidecar is not a profile: {list}");
    assert!(list["data"].get("unusable").is_none(), "{list}");

    let draft = corpus.path().join("draft.md");
    std::fs::write(&draft, prose(99, 700, None)).unwrap();
    let v = check(jev(data.path(), &endpoint), &draft);
    let t = &v["data"]["texture"];
    assert_eq!(t["status"], "measured", "{t}");
    assert_eq!(t["model"], "jev-1.13.0");
    assert!(t["questions_version"].as_str().unwrap().starts_with("texture-"));
    assert!(t["pass"].is_boolean());
    assert!(t.get("author_measured").is_none(), "bands came from the sidecar: {t}");
    let keys = t["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 9);
    for k in keys {
        for f in ["key", "label", "side", "gate", "draft", "low", "median", "high", "status"] {
            assert!(k.get(f).is_some(), "key row lacks {f}: {k}");
        }
        assert!(k["low"].as_f64().unwrap() <= k["high"].as_f64().unwrap());
    }
    assert_eq!(keys.iter().filter(|k| k["gate"] == true).count(), 5);
    let passages = t["passages"].as_array().unwrap();
    assert!(passages.len() >= 3, "{t}");
    assert_eq!(passages[0]["index"], 1);
    assert_eq!(passages[0]["lines"][0], 1);
    assert_eq!(passages[0]["values"].as_object().unwrap().len(), 7, "JEV keys only: hedges and one-liners are counted");
    assert!(passages[0]["flags"].is_array());
    assert_eq!(t["band_passages"].as_u64().unwrap() as usize, passages.len());
    assert!(t["author_passages"].as_u64().unwrap() >= 20);
    assert_eq!(t["usage"]["passages"].as_u64().unwrap() as usize, passages.len());
    assert_eq!(t["pass"], true, "an author-like draft reads like the author: {t}");

    // Removing the profile removes its bands.
    let _ = json_of(bin(data.path()), &["profile", "remove", "ann", "--confirm"]);
    assert!(!sidecar.exists());
}

#[test]
fn a_smooth_draft_gets_texture_findings_and_a_plan_step_but_never_a_different_pass() {
    let endpoint = mock(Mode::Answer);
    let (data, corpus) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // Built offline: the first check measures the author lazily and says so.
    build(bin(data.path()), corpus.path());
    let draft = corpus.path().join("draft.md");
    std::fs::write(&draft, prose(99, 700, Some(SMOOTH))).unwrap();

    let off = check(bin(data.path()), &draft);
    let on = check(jev(data.path(), &endpoint), &draft);
    let t = &on["data"]["texture"];
    assert_eq!(t["status"], "measured", "{t}");
    assert!(t["author_measured"]["passages"].as_u64().unwrap() >= 20, "lazy author measurement is reported: {t}");
    assert_eq!(t["pass"], false, "{t}");
    let off_keys: Vec<&str> = t["keys"].as_array().unwrap().iter().filter(|k| k["status"] == "off").map(|k| k["key"].as_str().unwrap()).collect();
    for k in ["rough", "polish", "button", "figurative"] {
        assert!(off_keys.contains(&k), "{k} should be off: {off_keys:?}");
    }
    assert!(!off_keys.contains(&"hedge"), "few hedges is not a fault");

    let d = &on["data"];
    let tex: Vec<&Value> = d["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "texture").collect();
    assert_eq!(tex.len(), off_keys.len(), "one finding per key off");
    for f in &tex {
        assert_eq!(f["hard"], false);
        assert!(!f["lines"].as_array().unwrap().is_empty(), "{f}");
        assert!(!f["excerpts"].as_array().unwrap().is_empty(), "{f}");
    }
    let rough = tex.iter().find(|f| f["key"] == "rough").unwrap();
    assert!(rough["suggestion"].as_str().unwrap().contains("not with filler"), "{rough}");
    // Flagged passages are drilled into sentence by sentence: the figure
    // and crafted-line findings cite the marker sentences' own lines.
    let drilled: usize = t["passages"].as_array().unwrap().iter().map(|p| p["sentences"].as_array().map_or(0, Vec::len)).sum();
    assert!(drilled > 0 && drilled <= 40, "sentence requests are capped: {drilled}");
    let text = std::fs::read_to_string(&draft).unwrap();
    for key in ["figurative", "button", "polish"] {
        let f = tex.iter().find(|f| f["key"] == key).unwrap();
        for e in f["excerpts"].as_array().unwrap() {
            assert!(e.as_str().unwrap().contains(SMOOTH), "{key} cites the crafted sentences: {e}");
        }
        for l in f["lines"].as_array().unwrap() {
            let line = text.lines().nth(l.as_u64().unwrap() as usize - 1).unwrap();
            assert!(line.contains(SMOOTH), "{key} line {l} is a cited sentence's line");
        }
    }
    assert_eq!(d["counts"]["texture"].as_u64().unwrap() as usize, tex.len());
    let step = d["plan"].as_array().unwrap().iter().find(|s| s["action"].as_str().unwrap().contains("texture")).expect("a texture plan step");
    let action = step["action"].as_str().unwrap();
    assert!(action.starts_with("Rework the texture"), "{action}");
    assert!(action.contains("Start with lines"), "names the worst passages: {action}");
    assert!(step["why"].as_str().unwrap().contains("Don't fake roughness"), "{step}");
    assert_eq!(step["hard"], false);

    // Texture is advice: pass, score, distance and hard failures don't move.
    for f in ["pass", "score", "distance", "hard_failures", "metrics", "score_basis"] {
        assert_eq!(on["data"][f], off["data"][f], "{f} changed with texture on");
    }
    // --strict gates on the stylometric check alone.
    let strict = jev(data.path(), &endpoint).args(["check", draft.to_str().unwrap(), "--profile", "ann", "--strict"]).output().unwrap();
    assert_eq!(strict.status.code() == Some(5), off["data"]["pass"] == false, "--strict follows pass, not texture");
}

#[test]
fn hedges_are_counted_in_code_and_piling_them_on_fails_the_texture_gate() {
    let endpoint = mock(Mode::Answer);
    let (data, corpus) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    build(jev(data.path(), &endpoint), corpus.path());
    // Author-like prose with filler hedges piled into three paragraphs, and
    // "kind of" as a noun ("what kind of car"), which is not a hedge.
    let mut paras: Vec<String> = prose(99, 700, None).split("\n\n").map(str::to_string).filter(|p| !p.is_empty()).collect();
    for i in [1, 3, 5] {
        paras[i].push_str(" I guess it was kind of fine, sort of, or whatever.");
    }
    paras[0].push_str(" I asked what kind of car it was.");
    let draft = corpus.path().join("hedgy.md");
    std::fs::write(&draft, paras.join("\n\n")).unwrap();
    let v = check(jev(data.path(), &endpoint), &draft);
    let t = &v["data"]["texture"];
    let hedge = t["keys"].as_array().unwrap().iter().find(|k| k["key"] == "hedge").expect("a hedge row");
    assert_eq!(hedge["source"], "count", "{hedge}");
    assert_eq!(hedge["status"], "off", "{hedge}");
    assert_eq!(hedge["off_side"], "high", "{hedge}");
    assert_eq!(t["pass"], false, "piled-on hedges fail the texture gate: {t}");
    let f = v["data"]["findings"].as_array().unwrap().iter().find(|f| f["kind"] == "texture" && f["key"] == "hedge").expect("a hedge finding");
    assert_eq!(f["count"], 12, "four hedges in each of three paragraphs: {f}");
    let expected: Vec<u64> = [1u64, 3, 5].iter().map(|i| 2 * i + 1).collect();
    assert_eq!(f["lines"].as_array().unwrap().iter().map(|l| l.as_u64().unwrap()).collect::<Vec<_>>(), expected, "{f}");
    assert!(f["excerpts"][0].as_str().unwrap().contains("'i guess'"), "{f}");
    // No passage was asked about hedges or one-liners (the mock panics if so),
    // and the one-sentence paragraph row is the check's own metric.
    let one = t["keys"].as_array().unwrap().iter().find(|k| k["key"] == "oneliner").expect("a one-liner row");
    assert_eq!(one["source"], "count");
    let metric = v["data"]["metrics"].as_array().unwrap().iter().find(|m| m["key"] == "one_sentence_paragraph_share").unwrap();
    assert_eq!(one["draft"].as_f64().unwrap(), (metric["value"].as_f64().unwrap() * 1000.0).round() / 1000.0);
}

#[test]
fn every_failure_fails_open_with_the_rest_of_check_unchanged() {
    let (data, corpus) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    build(bin(data.path()), corpus.path());
    let draft = corpus.path().join("draft.md");
    std::fs::write(&draft, prose(99, 700, Some(SMOOTH))).unwrap();
    let off = check(bin(data.path()), &draft);
    assert_eq!(off["data"]["texture"]["status"], "unavailable");
    assert!(off["data"]["texture"]["reason"].as_str().unwrap().contains("jev.enabled"));
    assert!(off["data"]["texture"].get("keys").is_none());
    assert_eq!(off["data"]["counts"]["texture"], 0);

    let down = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}/v1/systemone", l.local_addr().unwrap().port())
    };
    let keyless = {
        let mut c = jev(data.path(), &mock(Mode::Answer));
        c.env_remove("TYPESAFE_API_KEY");
        c
    };
    let cases = [
        ("no key", keyless, "no key"),
        ("server down", jev(data.path(), &down), "did not answer"),
        ("bad JSON", jev(data.path(), &mock(Mode::BadJson)), "did not answer"),
        ("timeout", jev(data.path(), &mock(Mode::Slow)), "did not answer"),
    ];
    for (name, c, why) in cases {
        let v = check(c, &draft);
        let t = &v["data"]["texture"];
        assert_eq!(t["status"], "unavailable", "{name}: {t}");
        assert!(t["reason"].as_str().unwrap().contains(why), "{name}: {t}");
        assert_eq!(without_texture(&v), without_texture(&off), "{name}: check changed beyond the texture block");
    }
    assert!(!data.path().join("profiles/ann.texture.json").exists(), "nothing measured, nothing saved");
}

#[test]
fn guide_has_a_texture_section_from_the_authors_bands() {
    let endpoint = mock(Mode::Answer);
    let (data, corpus) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    build(bin(data.path()), corpus.path());
    let off = json_of(bin(data.path()), &["guide", "ann"]);
    assert!(!off["data"]["sections"].as_array().unwrap().iter().any(|s| s == "Texture"));
    assert_eq!(off["data"]["texture"]["status"], "unavailable");

    let v = json_of(jev(data.path(), &endpoint), &["guide", "ann"]);
    let d = &v["data"];
    assert_eq!(d["texture"]["status"], "measured", "{}", d["texture"]);
    assert!(d["texture"]["author_measured"]["passages"].as_u64().unwrap() >= 20, "measured on first use");
    let sections: Vec<&str> = d["sections"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    let at = sections.iter().position(|s| *s == "Texture").expect("a Texture section");
    assert!(sections[..at].contains(&"What gives imitations away") || at > 2, "{sections:?}");
    let md = d["markdown"].as_str().unwrap();
    let body = &md[md.find("## Texture").unwrap()..];
    assert!(body.contains("Polish") && body.contains("Rough edges"), "{body}");
    assert!(body.contains("Don't fake roughness"), "{body}");
    assert!(d["brief_words"].as_u64().unwrap() <= 1200);

    // Second run: bands from the sidecar, nothing measured.
    let again = json_of(jev(data.path(), &endpoint), &["guide", "ann"]);
    assert!(again["data"]["texture"].get("author_measured").is_none());
}
