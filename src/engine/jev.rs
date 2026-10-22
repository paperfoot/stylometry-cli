//! JEV (TypeSafe System One): typed judgments about short text, used for
//! what counting cannot see, such as how polished or rough a passage reads.
//!
//! Every call fails open: no key, a timeout or a bad answer gives `None`
//! and the caller keeps its local result. Answers are cached on disk by a
//! hash of (model, state, questions), so re-checking a draft or rebuilding
//! a profile costs nothing for passages already judged. The key is read
//! from the environment or a configured command and never written anywhere.
//!
//! A slow or hanging server cannot stall a command: after `BREAKER`
//! failures in a row the client stops sending, and once a command's time
//! budget (`start_budget`) is spent nothing more goes out; each request's
//! timeout is cut to the time left. Cached answers are still served.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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

/// Failures in a row (timeouts, errors, bad answers) after which nothing
/// more is sent: a hanging server costs one timeout, not one per passage.
pub const BREAKER: usize = 4;

/// Set once JEV stopped answering (breaker tripped, budget spent) in this
/// process: later commands' features (a brief ranks examples and reads
/// texture) go local at once instead of each paying its own timeout.
static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether JEV gave up earlier in this run.
pub fn stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

/// Give up on JEV for the rest of this run. Unit tests share one process
/// and build many clients, so there the flag stays per client (the
/// contract tests run the binary and cover it).
pub fn stop() {
    if cfg!(not(test)) {
        STOPPED.store(true, Ordering::SeqCst);
    }
}

pub struct Jev {
    agent: ureq::Agent,
    key: String,
    pub model: String,
    endpoint: String,
    workers: usize,
    cache: PathBuf,
    usage: Usage,
    timeout: Duration,
    /// The configured budget for one command (`start_budget` applies it).
    pub budget: Duration,
    /// The deadline and the budget it came from.
    deadline: Mutex<Option<(Instant, Duration)>>,
    /// Failures since the last answer.
    streak: AtomicUsize,
    /// Requests on the wire now.
    inflight: AtomicUsize,
}

/// What a run cost: requests sent, answers served from the cache, and
/// input tokens billed (output tokens are free).
#[derive(Default)]
struct Usage {
    requests: AtomicUsize,
    cached: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
    input_tokens: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct UsageReport {
    pub requests: usize,
    pub cached: usize,
    pub failed: usize,
    /// Never sent: the breaker tripped or the time budget ran out.
    pub skipped: usize,
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
        if stopped() {
            return Err("JEV stopped answering earlier in this run".into());
        }
        let key = resolve_key(cfg).ok_or("no key")?;
        let timeout = Duration::from_millis(cfg.timeout_ms.max(500));
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
        Ok(Jev {
            agent,
            key,
            model: cfg.model.clone(),
            endpoint: cfg.endpoint.clone(),
            workers: cfg.workers.clamp(1, 12),
            cache: store::data_dir().join("jev-cache"),
            usage: Usage::default(),
            timeout,
            budget: Duration::from_millis(cfg.budget_ms.max(1000)),
            deadline: Mutex::new(None),
            streak: AtomicUsize::new(0),
            inflight: AtomicUsize::new(0),
        })
    }

    /// From now on, send nothing after `budget` has passed.
    pub fn start_budget(&self, budget: Duration) {
        if let Ok(mut d) = self.deadline.lock() {
            *d = Some((Instant::now() + budget, budget));
        }
    }

    /// Time left in the budget (`None`: no budget set).
    pub fn time_left(&self) -> Option<Duration> {
        let (d, _) = (*self.deadline.lock().ok()?)?;
        Some(d.saturating_duration_since(Instant::now()))
    }

    /// Why the client has stopped sending, if it has.
    pub fn halted(&self) -> Option<String> {
        // The budget first: requests cut off at the deadline also fail.
        if let Some((d, budget)) = self.deadline.lock().ok().and_then(|d| *d) {
            if Instant::now() >= d {
                return Some(format!("JEV is too slow: the {:.0} s budget ran out (jev.budget_ms)", budget.as_secs_f64()));
            }
        }
        let streak = self.streak.load(Ordering::SeqCst);
        if streak >= BREAKER {
            return Some(format!("JEV stopped answering: {streak} requests failed in a row (network, timeout or bad responses)"));
        }
        None
    }

    /// Parallel requests.
    pub fn workers(&self) -> usize {
        self.workers
    }

    fn cache_path(&self, body: &str) -> PathBuf {
        self.cache.join(format!("{}.json", hex(&Sha256::digest(format!("{}\n{body}", self.endpoint).as_bytes())[..16])))
    }

    fn body(&self, state: &Value, questions: &Value) -> String {
        json!({ "model": self.model, "state": state, "questions": questions }).to_string()
    }

    /// Whether this question set about this state is answered from the cache.
    pub fn is_cached(&self, state: &Value, questions: &Value) -> bool {
        self.cache_path(&self.body(state, questions)).exists()
    }

    /// The client for this process's config, or why there is none.
    pub fn load() -> Result<Jev, String> {
        let cfg = crate::config::load().map(|c| c.jev).unwrap_or_default();
        Jev::from_config(&cfg)
    }

    /// Ask every question about one state. Cached; one retry on a rate
    /// limit or an overloaded server; `None` on any other failure.
    pub fn ask(&self, state: &Value, questions: &Value) -> Option<Answers> {
        let text = self.body(state, questions);
        let path = self.cache_path(&text);
        if let Some(a) = std::fs::read_to_string(&path).ok().and_then(|s| answers_of(&s, questions)) {
            self.usage.cached.fetch_add(1, Ordering::Relaxed);
            return Some(a);
        }
        for attempt in 0..2 {
            // After a failure, let the requests still on the wire land
            // first: against a hanging server they fail too and trip the
            // breaker before anything more is sent; one answer clears it.
            while (1..BREAKER).contains(&self.streak.load(Ordering::SeqCst))
                && self.inflight.load(Ordering::SeqCst) > 0
                && !self.time_left().is_some_and(|t| t.is_zero())
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            // The breaker or the budget: nothing more goes out.
            let left = self.time_left();
            if self.streak.load(Ordering::SeqCst) >= BREAKER || left.is_some_and(|t| t.is_zero()) {
                stop();
                if attempt > 0 {
                    break;
                }
                self.usage.skipped.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            let timeout = left.map_or(self.timeout, |t| t.min(self.timeout));
            self.usage.requests.fetch_add(1, Ordering::Relaxed);
            self.inflight.fetch_add(1, Ordering::SeqCst);
            let res = self
                .agent
                .post(&self.endpoint)
                .config()
                .timeout_global(Some(timeout))
                .build()
                .header("Authorization", &format!("Bearer {}", self.key))
                .header("Content-Type", "application/json")
                .send(text.as_str())
                .and_then(|mut r| r.body_mut().read_to_string());
            // The streak is settled before the request leaves `inflight`,
            // so a waiting worker never sees neither.
            let answer = res.as_ref().ok().and_then(|s| answers_of(s, questions));
            if let (Some(a), Ok(s)) = (answer, &res) {
                if let Some(t) = serde_json::from_str::<Value>(s).ok().and_then(|v| v["usage"]["input_tokens"].as_u64()) {
                    self.usage.input_tokens.fetch_add(t, Ordering::Relaxed);
                }
                let _ = std::fs::create_dir_all(&self.cache);
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, s).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                self.streak.store(0, Ordering::SeqCst);
                self.inflight.fetch_sub(1, Ordering::SeqCst);
                return Some(a);
            }
            if attempt == 0 && matches!(res, Err(ureq::Error::StatusCode(429 | 500 | 502 | 503 | 529))) {
                self.inflight.fetch_sub(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(800));
                continue;
            }
            if self.streak.fetch_add(1, Ordering::SeqCst) + 1 >= BREAKER {
                stop();
            }
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            self.usage.failed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // The retry was not sent: the breaker or the budget stopped it.
        self.streak.fetch_add(1, Ordering::SeqCst);
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
            skipped: self.usage.skipped.load(Ordering::Relaxed),
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

    /// A local endpoint that never answers, or answers every POST after
    /// `delay` with a fixed answer to question `a`.
    fn endpoint(delay: Option<Duration>) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/systemone", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for s in l.incoming().flatten() {
                let Some(delay) = delay else {
                    held.push(s);
                    continue;
                };
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut len = 0;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let _ = r.read_exact(&mut vec![0; len]);
                    std::thread::sleep(delay);
                    let body = r#"{"answers":{"a":{"type":"noul","noul":0.5}}}"#;
                    let mut s = s;
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                });
            }
        });
        url
    }

    fn client(endpoint: String, timeout_ms: u64, cache: &std::path::Path) -> Jev {
        let cfg = JevConfig { endpoint, key_env: "STYLO_TEST_NO_SUCH_VAR".into(), key_command: "echo k".into(), timeout_ms, ..JevConfig::default() };
        Jev { cache: cache.to_path_buf(), ..Jev::from_config(&cfg).unwrap() }
    }

    #[test]
    fn a_hanging_server_trips_the_breaker_after_one_round_of_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let jev = client(endpoint(None), 600, dir.path());
        let states: Vec<Value> = (0..60).map(|i| json!({ "text": i })).collect();
        let t = Instant::now();
        let a = jev.ask_all(&states, &json!({"a": {"type": "noul"}}));
        let took = t.elapsed();
        assert!(a.iter().all(Option::is_none));
        let u = jev.usage();
        assert_eq!(u.requests, jev.workers(), "the first round only: nothing is sent after its timeouts: {u:?}");
        assert_eq!(u.requests + u.skipped, 60, "{u:?}");
        assert!(took < Duration::from_millis(1100), "one round of 600 ms timeouts, not 60/8 rounds: {took:?}");
        assert!(jev.halted().unwrap().contains("in a row"));
    }

    #[test]
    fn the_budget_stops_a_slow_server_and_cuts_the_last_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let jev = client(endpoint(Some(Duration::from_millis(300))), 8000, dir.path());
        jev.start_budget(Duration::from_millis(1000));
        let states: Vec<Value> = (0..200).map(|i| json!({ "text": i })).collect();
        let t = Instant::now();
        let a = jev.ask_all(&states, &json!({"a": {"type": "noul"}}));
        let took = t.elapsed();
        let u = jev.usage();
        assert!(took < Duration::from_millis(1800), "{took:?} {u:?}");
        assert!(a.iter().filter(|x| x.is_some()).count() >= 8, "answers inside the budget are kept: {u:?}");
        assert!(u.skipped > 100, "{u:?}");
        assert!(jev.halted().unwrap().contains("too slow"));
        // Answered states come from the cache even after the budget is spent.
        let first = states.iter().zip(&a).find(|(_, x)| x.is_some()).unwrap().0;
        assert!(jev.is_cached(first, &json!({"a": {"type": "noul"}})));
        assert!(jev.ask(first, &json!({"a": {"type": "noul"}})).is_some());
    }

    #[test]
    fn question_sets_load_and_carry_a_version() {
        let t = questions("texture");
        for k in ["rough", "polish", "button", "figurative", "overexplain", "repeat", "splice"] {
            assert!(t.get(k).is_some(), "texture lacks {k}");
        }
        assert!(questions_version("texture").starts_with("texture-"));
        assert!(questions("relevance").is_object());
    }
}
