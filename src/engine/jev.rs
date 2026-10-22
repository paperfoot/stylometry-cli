//! JEV (TypeSafe System One): typed judgments about short text, used for
//! what counting cannot see, such as how polished or rough a passage reads.
//!
//! Every call fails open: no key, a timeout or a bad answer gives `None`
//! and the caller keeps its local result. Answers are cached on disk by a
//! hash of (model, state, questions), so re-checking a draft or rebuilding
//! a profile costs nothing for passages already judged. The key is read
//! from the environment or a configured command and never written anywhere.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::JevConfig;
use crate::engine::store;

/// The question sets, exported from the evals that measured them.
/// `texture`: genuine vs imitation AUC 0.90-1.00 for rough edges across
/// fiction, essays and posts (`~/x/stylometry-jev-eval`, 2026-10-22).
const TEXTURE: &str = include_str!("data/jev_texture.json");
/// `relevance`: how well an author passage fits the piece being written.
const RELEVANCE: &str = include_str!("data/jev_relevance.json");

pub struct Jev {
    agent: ureq::Agent,
    key: String,
    pub model: String,
    endpoint: String,
    workers: usize,
    cache: PathBuf,
    usage: Usage,
}

/// What a run cost: requests sent, answers served from the cache, and
/// input tokens billed (output tokens are free).
#[derive(Default)]
struct Usage {
    requests: AtomicUsize,
    cached: AtomicUsize,
    failed: AtomicUsize,
    input_tokens: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct UsageReport {
    pub requests: usize,
    pub cached: usize,
    pub failed: usize,
    pub input_tokens: u64,
}

/// One answer set, by question id.
#[derive(Debug, Clone)]
pub struct Answers(Map<String, Value>);

impl Answers {
    /// P(yes) of a Noul.
    pub fn noul(&self, id: &str) -> Option<f64> {
        self.0.get(id)?.get("noul")?.as_f64().filter(|p| (0.0..=1.0).contains(p))
    }

    /// The probability-weighted level of a Score.
    pub fn score(&self, id: &str) -> Option<f64> {
        self.0.get(id)?.get("score")?.as_f64().filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// The probability of one option of a Choice.
    pub fn prob(&self, id: &str, option: &str) -> Option<f64> {
        self.0.get(id)?.get("probabilities")?.get(option)?.as_f64()
    }

    /// A Noul's probability or a Score's level, whichever the question is.
    pub fn value(&self, id: &str) -> Option<f64> {
        self.noul(id).or_else(|| self.score(id))
    }
}

/// A question set by name (`texture`, `relevance`); `Null` if unknown.
pub fn questions(set: &str) -> Value {
    let raw = match set {
        "texture" => TEXTURE,
        "relevance" => RELEVANCE,
        _ => return Value::Null,
    };
    serde_json::from_str(raw).expect("embedded question sets are valid JSON")
}

/// The version tag of a question set; stored with anything measured by it,
/// so a change of wording invalidates old measurements.
pub fn questions_version(set: &str) -> String {
    let q = questions(set);
    let h = Sha256::digest(q.to_string().as_bytes());
    format!("{set}-{}", hex(&h[..6]))
}

/// Why JEV is not in use, for reports and `doctor`.
pub fn unavailable_reason(cfg: &JevConfig) -> Option<String> {
    if !cfg.enabled {
        return Some("off (jev.enabled = false)".into());
    }
    if resolve_key(cfg).is_none() {
        return Some(format!(
            "no key: set {} or jev.key_command in `stylometry config path`",
            cfg.key_env
        ));
    }
    None
}

impl Jev {
    /// A client when JEV is enabled and a key is found; otherwise the reason.
    pub fn from_config(cfg: &JevConfig) -> Result<Jev, String> {
        if let Some(why) = unavailable_reason(cfg) {
            return Err(why);
        }
        let key = resolve_key(cfg).ok_or("no key")?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(cfg.timeout_ms.max(500))))
            .build()
            .into();
        Ok(Jev {
            agent,
            key,
            model: cfg.model.clone(),
            endpoint: cfg.endpoint.clone(),
            workers: cfg.workers.clamp(1, 12),
            cache: store::data_dir().join("jev-cache"),
            usage: Usage::default(),
        })
    }

    /// The client for this process's config, or why there is none.
    pub fn load() -> Result<Jev, String> {
        let cfg = crate::config::load().map(|c| c.jev).unwrap_or_default();
        Jev::from_config(&cfg)
    }

    /// Ask every question about one state. Cached; one retry on a rate
    /// limit or an overloaded server; `None` on any other failure.
    pub fn ask(&self, state: &Value, questions: &Value) -> Option<Answers> {
        let body = json!({ "model": self.model, "state": state, "questions": questions });
        let text = body.to_string();
        let path = self.cache.join(format!("{}.json", hex(&Sha256::digest(format!("{}\n{text}", self.endpoint).as_bytes())[..16])));
        if let Some(a) = std::fs::read_to_string(&path).ok().and_then(|s| answers_of(&s, questions)) {
            self.usage.cached.fetch_add(1, Ordering::Relaxed);
            return Some(a);
        }
        for attempt in 0..2 {
            self.usage.requests.fetch_add(1, Ordering::Relaxed);
            let res = self
                .agent
                .post(&self.endpoint)
                .header("Authorization", &format!("Bearer {}", self.key))
                .header("Content-Type", "application/json")
                .send(text.as_str())
                .and_then(|mut r| r.body_mut().read_to_string());
            match res {
                Ok(s) => {
                    let Some(a) = answers_of(&s, questions) else { break };
                    if let Some(t) = serde_json::from_str::<Value>(&s).ok().and_then(|v| v["usage"]["input_tokens"].as_u64()) {
                        self.usage.input_tokens.fetch_add(t, Ordering::Relaxed);
                    }
                    let _ = std::fs::create_dir_all(&self.cache);
                    let tmp = path.with_extension("tmp");
                    if std::fs::write(&tmp, &s).is_ok() {
                        let _ = std::fs::rename(&tmp, &path);
                    }
                    return Some(a);
                }
                Err(ureq::Error::StatusCode(429 | 500 | 502 | 503 | 529)) if attempt == 0 => {
                    std::thread::sleep(Duration::from_millis(800));
                }
                Err(_) => break,
            }
        }
        self.usage.failed.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// `ask` for many states in parallel, answers in input order.
    pub fn ask_all(&self, states: &[Value], questions: &Value) -> Vec<Option<Answers>> {
        let next = AtomicUsize::new(0);
        let mut out: Vec<Option<Answers>> = vec![None; states.len()];
        let slots: Vec<std::sync::Mutex<Option<Answers>>> = (0..states.len()).map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..self.workers.min(states.len().max(1)) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= states.len() {
                            break;
                        }
                        let a = self.ask(&states[i], questions);
                        if let Ok(mut slot) = slots[i].lock() {
                            *slot = a;
                        }
                    }
                });
            }
        });
        for (o, slot) in out.iter_mut().zip(slots) {
            *o = slot.into_inner().ok().flatten();
        }
        out
    }

    pub fn usage(&self) -> UsageReport {
        UsageReport {
            requests: self.usage.requests.load(Ordering::Relaxed),
            cached: self.usage.cached.load(Ordering::Relaxed),
            failed: self.usage.failed.load(Ordering::Relaxed),
            input_tokens: self.usage.input_tokens.load(Ordering::Relaxed),
        }
    }
}

/// The answers of a response body, if every question was answered with a
/// value in range.
fn answers_of(body: &str, questions: &Value) -> Option<Answers> {
    let v: Value = serde_json::from_str(body).ok()?;
    let map = v.get("answers")?.as_object()?.clone();
    let a = Answers(map);
    let ids = questions.as_object()?;
    ids.keys().all(|id| a.value(id).is_some() || a.0.get(id).and_then(|x| x.get("choice")).is_some()).then_some(a)
}

/// The key from the configured variable, else the configured command.
fn resolve_key(cfg: &JevConfig) -> Option<String> {
    if let Ok(k) = std::env::var(&cfg.key_env) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    if cfg.key_command.trim().is_empty() {
        return None;
    }
    let out = std::process::Command::new("sh").arg("-c").arg(&cfg.key_command).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let raw = raw.trim();
    let key = match serde_json::from_str::<Value>(raw) {
        Ok(v) => v.pointer("/data/value").or_else(|| v.get("value")).and_then(Value::as_str)?.trim().to_string(),
        Err(_) => raw.to_string(),
    };
    (!key.is_empty()).then_some(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, env: &str, cmd: &str) -> JevConfig {
        JevConfig { enabled, key_env: env.into(), key_command: cmd.into(), ..JevConfig::default() }
    }

    #[test]
    fn off_or_keyless_is_unavailable_with_the_fix() {
        assert!(unavailable_reason(&cfg(false, "STYLO_TEST_NO_SUCH_VAR", "")).unwrap().contains("jev.enabled"));
        let why = unavailable_reason(&cfg(true, "STYLO_TEST_NO_SUCH_VAR", "")).unwrap();
        assert!(why.contains("STYLO_TEST_NO_SUCH_VAR") && why.contains("key_command"), "{why}");
        assert!(Jev::from_config(&cfg(true, "STYLO_TEST_NO_SUCH_VAR", "")).is_err());
    }

    #[test]
    fn key_command_reads_plain_text_or_akm_json() {
        assert_eq!(resolve_key(&cfg(true, "STYLO_TEST_NO_SUCH_VAR", "echo plain-key")).as_deref(), Some("plain-key"));
        let akm = r#"printf '{"status":"success","data":{"name":"K","masked":false,"value":"json-key"}}'"#;
        assert_eq!(resolve_key(&cfg(true, "STYLO_TEST_NO_SUCH_VAR", akm)).as_deref(), Some("json-key"));
        assert_eq!(resolve_key(&cfg(true, "STYLO_TEST_NO_SUCH_VAR", "exit 3")), None);
    }

    #[test]
    fn answers_must_cover_every_question_in_range() {
        let q = json!({"a": {"type": "noul"}, "b": {"type": "score"}});
        let ok = r#"{"answers":{"a":{"type":"noul","noul":0.7},"b":{"type":"score","score":1.4}}}"#;
        let a = answers_of(ok, &q).unwrap();
        assert_eq!(a.noul("a"), Some(0.7));
        assert_eq!(a.value("b"), Some(1.4));
        assert!(answers_of(r#"{"answers":{"a":{"noul":0.7}}}"#, &q).is_none(), "missing b");
        assert!(answers_of(r#"{"answers":{"a":{"noul":1.7},"b":{"score":1}}}"#, &q).is_none(), "out of range");
        assert!(answers_of("not json", &q).is_none());
    }

    /// Live: `TYPESAFE_API_KEY=… cargo test -- --ignored live_texture`.
    #[test]
    #[ignore]
    fn live_texture_answers_every_question_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let jev = Jev { cache: dir.path().to_path_buf(), ..Jev::from_config(&JevConfig::default()).expect("key") };
        let q = questions("texture");
        let states = [json!({"text": "I go to the store. I buy milk, I guess, and then I kind of walk home."}),
                      json!({"text": "The light folded itself over the lake like a letter nobody would send."})];
        let a = jev.ask_all(&states, &q);
        assert!(a.iter().all(Option::is_some), "{:?}", jev.usage());
        assert!(a[0].as_ref().unwrap().noul("hedge").unwrap() > a[1].as_ref().unwrap().noul("hedge").unwrap());
        assert!(a[1].as_ref().unwrap().noul("figurative").unwrap() > 0.5);
        let again = jev.ask(&states[0], &q).unwrap();
        assert_eq!(again.noul("hedge"), a[0].as_ref().unwrap().noul("hedge"));
        let u = jev.usage();
        assert_eq!((u.requests, u.cached, u.failed), (2, 1, 0), "{u:?}");
    }

    #[test]
    fn question_sets_load_and_carry_a_version() {
        let t = questions("texture");
        for k in ["rough", "polish", "button", "figurative", "hedge", "overexplain", "repeat", "splice", "oneliner"] {
            assert!(t.get(k).is_some(), "texture lacks {k}");
        }
        assert!(questions_version("texture").starts_with("texture-"));
        assert!(questions("relevance").is_object());
    }
}
