//! Draft checking: line-anchored findings against a target voice, most
//! important first, each with a concrete fix. Hard failures (copying the
//! author's text, banned words) always fail; the rest is drift measured
//! against the author's own range at the draft's length.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use serde::Serialize;

use crate::engine::metrics::{self, Status};
use crate::engine::profile::Profile;
use crate::engine::text;
use crate::engine::lexicon;
use crate::engine::voice::{self, Concept, Corpus};

/// Shared n-gram length that counts as copying (the novel pipeline's guard).
pub const COPY_NGRAM: usize = 8;

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// copy_overlap | avoid_list | metric_out_of_range | llm_tell |
    /// off_voice_word | synonym_variation | structure
    pub kind: &'static str,
    /// 1-based line in the draft; null for whole-draft findings (metrics).
    pub line: Option<usize>,
    pub excerpt: String,
    pub message: String,
    pub suggestion: String,
    /// Hard findings fail the check on their own.
    pub hard: bool,
    #[serde(skip)]
    pub severity: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricRow {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub low: f64,
    pub median: f64,
    pub high: f64,
    pub status: Status,
}

/// The draft as lines, tokens anchored to lines, and prose paragraphs
/// anchored to their first line.
pub struct Draft<'a> {
    pub lines: Vec<&'a str>,
    pub tokens: Vec<String>,
    pub cased: Vec<String>,
    pub token_line: Vec<usize>,
    /// (paragraph, its lines trimmed, first line index)
    pub paragraphs: Vec<(String, Vec<&'a str>, usize)>,
}

impl<'a> Draft<'a> {
    pub fn of(text: &'a str) -> Draft<'a> {
        let lines: Vec<&str> = text.lines().collect();
        let (mut tokens, mut cased, mut token_line) = (Vec::new(), Vec::new(), Vec::new());
        for (i, l) in lines.iter().enumerate() {
            let c = text::word_tokens_cased(l);
            for t in &c {
                tokens.push(t.to_lowercase());
                token_line.push(i);
            }
            cased.extend(c);
        }
        let mut paragraphs = Vec::new();
        let mut block: Vec<&str> = Vec::new();
        let mut start = 0;
        for (i, l) in lines.iter().chain(std::iter::once(&"")).enumerate() {
            if l.trim().is_empty() {
                if !block.is_empty() {
                    if let Some(p) = text::paragraphs(&block.join("\n")).into_iter().next() {
                        paragraphs.push((p, std::mem::take(&mut block), start));
                    }
                    block.clear();
                }
            } else {
                if block.is_empty() {
                    start = i;
                }
                block.push(l.trim());
            }
        }
        Draft { lines, tokens, cased, token_line, paragraphs }
    }

    pub fn prose(&self) -> Vec<String> {
        self.paragraphs.iter().map(|p| p.0.clone()).collect()
    }

    /// 1-based line of a byte offset inside paragraph `pi`'s joined text.
    fn line_of(&self, pi: usize, offset: usize) -> usize {
        let (_, lines, start) = &self.paragraphs[pi];
        let mut acc = 0;
        for (j, l) in lines.iter().enumerate() {
            acc += l.len() + 1;
            if offset < acc {
                return start + j + 1;
            }
        }
        start + lines.len()
    }

    fn excerpt_at(&self, tok: usize, len: usize) -> String {
        let line = self.lines[self.token_line[tok]].trim();
        let words: Vec<&str> = self.cased[tok..(tok + len).min(self.cased.len())]
            .iter()
            .map(String::as_str)
            .collect();
        excerpt(line, &words.join(" "))
    }

    /// Words capitalized at every occurrence: the draft's own names.
    fn names(&self) -> HashSet<String> {
        let mut caps: HashMap<&str, (usize, usize)> = HashMap::new();
        for (t, c) in self.tokens.iter().zip(&self.cased) {
            let e = caps.entry(t.as_str()).or_insert((0, 0));
            e.0 += 1;
            if c.chars().next().is_some_and(char::is_uppercase) {
                e.1 += 1;
            }
        }
        caps.into_iter()
            .filter(|(t, (n, up))| *t != "i" && n == up)
            .map(|(t, _)| t.to_string())
            .collect()
    }
}

/// The line, cut to about 140 characters around `needle`.
fn excerpt(line: &str, needle: &str) -> String {
    const MAX: usize = 140;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let lower = line.to_lowercase();
    let at = lower.find(&needle.to_lowercase()).unwrap_or(0);
    let start = line[..at].char_indices().rev().nth(50).map(|(i, _)| i).unwrap_or(0);
    let s: String = line[start..].chars().take(MAX).collect();
    format!("{}{}…", if start > 0 { "…" } else { "" }, s.trim())
}

fn ngram_hash(toks: &[String]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    toks.hash(&mut h);
    h.finish()
}

/// Every 8-gram of the guarded profiles' stored text, with its owner.
fn guard_index(guards: &[&Profile]) -> HashMap<u64, usize> {
    let mut idx = HashMap::new();
    for (gi, g) in guards.iter().enumerate() {
        for c in &g.chunks {
            let toks: Vec<String> = c.paragraphs.iter().flat_map(|p| text::word_tokens(p)).collect();
            for w in toks.windows(COPY_NGRAM) {
                idx.entry(ngram_hash(w)).or_insert(gi);
            }
        }
    }
    idx
}

pub fn copy_overlaps(d: &Draft, guards: &[&Profile]) -> Vec<Finding> {
    if d.tokens.len() < COPY_NGRAM {
        return Vec::new();
    }
    let idx = guard_index(guards);
    let mut out = Vec::new();
    let mut i = 0;
    while i + COPY_NGRAM <= d.tokens.len() {
        let Some(&owner) = idx.get(&ngram_hash(&d.tokens[i..i + COPY_NGRAM])) else {
            i += 1;
            continue;
        };
        let mut end = i + COPY_NGRAM;
        while end < d.tokens.len() && idx.contains_key(&ngram_hash(&d.tokens[end + 1 - COPY_NGRAM..=end])) {
            end += 1;
        }
        let n = end - i;
        out.push(Finding {
            kind: "copy_overlap",
            line: Some(d.token_line[i] + 1),
            excerpt: d.cased[i..end.min(i + 25)].join(" "),
            message: format!("{n} consecutive words match {}'s own text", guards[owner].name),
            suggestion: "Rewrite this passage in new words: imitate the author's mechanisms, never their lines".into(),
            hard: true,
            severity: 1000.0 + n as f64,
        });
        i = end;
    }
    out
}

/// Banned words and phrases (one per line, '#' comments) from --avoid files.
pub fn avoid_hits(d: &Draft, avoid: &[String], concepts: &[Concept]) -> Vec<Finding> {
    let mut out = Vec::new();
    for entry in avoid {
        let pat = text::word_tokens(entry);
        if pat.is_empty() {
            continue;
        }
        for i in 0..d.tokens.len().saturating_sub(pat.len() - 1) {
            if d.tokens[i..i + pat.len()] == pat[..] {
                let alt = preferred_for(&pat.join(" "), concepts);
                out.push(Finding {
                    kind: "avoid_list",
                    line: Some(d.token_line[i] + 1),
                    excerpt: d.excerpt_at(i, pat.len()),
                    message: format!("'{entry}' is on the avoid list"),
                    suggestion: match alt {
                        Some((w, s)) => format!("Use '{w}' (the author's choice, {:.0}% of uses) or rephrase", s * 100.0),
                        None => "Cut it or rephrase in plain words".into(),
                    },
                    hard: true,
                    severity: 900.0,
                });
            }
        }
    }
    out
}

/// The author's preferred member of `w`'s synonym group and its share.
fn preferred_for(w: &str, concepts: &[Concept]) -> Option<(String, f64)> {
    let g = lexicon::group_of(w)?;
    let c = concepts.iter().find(|c| c.group == g)?;
    (c.preferred != w).then(|| (c.preferred.clone(), c.preferred_share))
}

fn share_in(c: &Concept, w: &str) -> f64 {
    c.members.iter().find(|m| m.word == w).map(|m| m.share).unwrap_or(0.0)
}

/// Draft rows against the author's bands at the draft's own length.
pub fn metric_rows(target: &Profile, d: &Draft, words: usize) -> (metrics::Ranges, Vec<MetricRow>) {
    let fw = metrics::top_function_words(target, metrics::FUNCTION_WORDS_TRACKED);
    let ranges = metrics::ranges(target, words, &fw);
    let values = metrics::measure(&d.prose(), &fw);
    let rows = ranges
        .bands
        .iter()
        .zip(values)
        .map(|(b, v)| MetricRow {
            key: b.key.clone(),
            label: b.label.clone(),
            value: metrics::round_unit(v, b.unit),
            low: b.low,
            median: b.median,
            high: b.high,
            status: metrics::status(v, b, words),
        })
        .collect();
    (ranges, rows)
}

pub fn metric_findings(rows: &[MetricRow], ranges: &metrics::Ranges) -> Vec<Finding> {
    rows.iter()
        .zip(&ranges.defs)
        .filter(|(r, _)| r.status == Status::OutOfRange)
        .map(|(r, def)| {
            let up = r.value < r.low;
            let width = (r.high - r.low).abs().max(1e-6);
            let dist = if up { r.low - r.value } else { r.value - r.high } / width;
            Finding {
                kind: "metric_out_of_range",
                line: None,
                excerpt: String::new(),
                message: format!(
                    "{}: {} (author {}–{}, median {}): {}",
                    r.label,
                    r.value,
                    r.low,
                    r.high,
                    r.median,
                    if up { "raise" } else { "lower" }
                ),
                suggestion: capitalize(if up { def.raise } else { def.lower }),
                hard: false,
                severity: 100.0 + dist.min(50.0),
            }
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// LLM tells the draft uses more than the author would at this length.
pub fn tell_findings(d: &Draft, rates: &HashMap<&'static str, f64>, concepts: &[Concept]) -> Vec<Finding> {
    let words = d.tokens.len().max(1) as f64;
    let hits = voice::find_tells(&d.tokens);
    let mut by_tell: HashMap<&str, usize> = HashMap::new();
    for h in &hits {
        *by_tell.entry(h.tell).or_insert(0) += 1;
    }
    let names = d.names();
    hits.iter()
        .filter(|h| !names.contains(&d.tokens[h.at]))
        .filter_map(|h| {
            let author = rates.get(h.tell).copied().unwrap_or(0.0);
            let expected = author * words / 1000.0;
            let n = by_tell[h.tell];
            if (n as f64) <= 2.0 * expected + 0.5 {
                return None;
            }
            let alt = preferred_for(h.tell, concepts);
            Some(Finding {
                kind: "llm_tell",
                line: Some(d.token_line[h.at] + 1),
                excerpt: d.excerpt_at(h.at, h.len),
                message: format!(
                    "'{}' reads as LLM prose: the author uses it {:.2} times per 1k words, this draft {:.2}",
                    h.tell,
                    author,
                    n as f64 * 1000.0 / words
                ),
                suggestion: match alt {
                    Some((w, s)) => format!("Use '{w}' (the author's choice, {:.0}%) or say it plainly", s * 100.0),
                    None => "Cut it, or say the thing plainly the way the author would".into(),
                },
                hard: false,
                severity: 60.0 + (n as f64 - expected).min(20.0),
            })
        })
        .collect()
}

/// Words the author avoids, and synonyms the draft mixes where the author
/// keeps to one.
pub fn word_findings(
    d: &Draft,
    target: &Profile,
    concepts: &[Concept],
    avoided: &[voice::AvoidedWord],
) -> Vec<Finding> {
    let by_group: HashMap<usize, &Concept> = concepts.iter().map(|c| (c.group, c)).collect();
    let avoided: HashMap<&str, &voice::AvoidedWord> = avoided.iter().map(|a| (a.word.as_str(), a)).collect();
    let used: HashSet<&str> = d.tokens.iter().map(String::as_str).collect();
    let names = d.names();
    let n_author = target.n_tokens.max(1) as f64;
    let mut out = Vec::new();
    for (i, w) in d.tokens.iter().enumerate() {
        if names.contains(w) || target.is_proper_noun(w) {
            continue;
        }
        let concept = lexicon::group_of(w).and_then(|g| by_group.get(&g)).filter(|c| c.total >= 10.0);
        if let Some(c) = concept {
            let s = share_in(c, w);
            if c.preferred == *w || s >= 0.05 {
                continue;
            }
            let mixes = used.contains(c.preferred.as_str());
            out.push(Finding {
                kind: if mixes { "synonym_variation" } else { "off_voice_word" },
                line: Some(d.token_line[i] + 1),
                excerpt: d.excerpt_at(i, 1),
                message: if mixes {
                    format!(
                        "the draft mixes '{w}' with '{}'; the author keeps to '{}' ({:.0}% of uses)",
                        c.preferred, c.preferred, c.preferred_share * 100.0
                    )
                } else {
                    format!(
                        "'{w}' is off-voice: {:.0}% of the author's uses in this group (preferred '{}', {:.0}%)",
                        s * 100.0, c.preferred, c.preferred_share * 100.0
                    )
                },
                suggestion: format!("Use '{}'", c.preferred),
                hard: false,
                severity: if mixes { 40.0 } else { 50.0 },
            });
            continue;
        }
        let ly_never = metrics::is_ly_adverb(w)
            && !target.word_counts.contains_key(w)
            && lexicon::per_million(w) * n_author / 1e6 >= 8.0;
        if let Some(a) = avoided.get(w.as_str()) {
            out.push(off_voice(d, i, w, a.observed, a.expected, a.instead.as_deref()));
        } else if ly_never {
            out.push(off_voice(d, i, w, 0, lexicon::per_million(w) * n_author / 1e6, None));
        }
    }
    out
}

fn off_voice(d: &Draft, i: usize, w: &str, observed: u64, expected: f64, instead: Option<&str>) -> Finding {
    Finding {
        kind: "off_voice_word",
        line: Some(d.token_line[i] + 1),
        excerpt: d.excerpt_at(i, 1),
        message: format!(
            "the author almost never uses '{w}' ({observed} times where general English would have ~{expected:.0})"
        ),
        suggestion: match instead {
            Some(alt) => format!("Use '{alt}', the author's word for this"),
            None => "Cut it or rephrase in the author's plainer words".into(),
        },
        hard: false,
        severity: 45.0,
    }
}

/// Sentence constructions beyond the author's range: one finding per
/// instance, only for patterns whose draft rate is above the author's band.
pub fn structure_findings(d: &Draft, rows: &[MetricRow]) -> Vec<Finding> {
    let above = |key: &str| {
        rows.iter()
            .find(|r| r.key == key)
            .is_some_and(|r| r.status != Status::InRange && r.value > r.high)
    };
    let (chains, not_but, ing, echoes) = (
        above("and_chains_per_1k_sentences"),
        above("not_but_per_1k"),
        above("trailing_ing_per_1k"),
        above("echo_phrases_per_1k"),
    );
    let mut out = Vec::new();
    let finding = |line: usize, excerpt: String, message: &str, suggestion: &str| Finding {
        kind: "structure",
        line: Some(line),
        excerpt,
        message: message.to_string(),
        suggestion: suggestion.to_string(),
        hard: false,
        severity: 30.0,
    };
    for (pi, (para, _, _)) in d.paragraphs.iter().enumerate() {
        let sents = text::sentences(para);
        let mut cursor = 0;
        for (si, s) in sents.iter().enumerate() {
            let off = para[cursor..].find(s.as_str()).map(|o| o + cursor).unwrap_or(cursor);
            cursor = off;
            let line = d.line_of(pi, off);
            let short = excerpt(s, "");
            if chains && metrics::is_and_chain(&text::word_tokens(s)) {
                out.push(finding(line, short.clone(), "sentence chains three or more 'and's",
                    "Break it up, or build it from but/because/when clauses instead"));
            }
            if ing && metrics::trailing_ing_count(s) > 0 {
                out.push(finding(line, short.clone(), "trailing ', -ing' clause",
                    "End on the main verb; give the -ing action its own sentence or cut it"));
            }
            let pair = &sents[si..(si + 2).min(sents.len())];
            if not_but && metrics::not_but_count(&pair[..1]) + cross_frame(pair) > 0 {
                out.push(finding(line, short, "'not X, but Y' contrast frame",
                    "Say what it is directly; drop the 'not this, but that' setup"));
            }
        }
    }
    if echoes {
        for (phrase, n) in metrics::echo_phrases(&d.tokens).into_iter().take(10) {
            let pat: Vec<&str> = phrase.split(' ').collect();
            let at: Vec<usize> = (0..d.tokens.len().saturating_sub(2))
                .filter(|&i| pat.iter().zip(&d.tokens[i..]).all(|(a, b)| a == b))
                .collect();
            if let Some(&second) = at.get(1) {
                out.push(Finding {
                    kind: "structure",
                    line: Some(d.token_line[second] + 1),
                    excerpt: d.excerpt_at(second, 3),
                    message: format!("'{phrase}' repeats {n} times (echo)"),
                    suggestion: "Vary or cut the repeat unless it is a deliberate refrain".into(),
                    hard: false,
                    severity: 25.0,
                });
            }
        }
    }
    out
}

/// The two-sentence "It's not X. It's Y." frame starting at pair[0].
fn cross_frame(pair: &[String]) -> usize {
    if pair.len() < 2 {
        return 0;
    }
    metrics::not_but_count(pair) - metrics::not_but_count(&pair[..1]) - metrics::not_but_count(&pair[1..])
}

/// Every non-metric finding plus metric rows, for one draft.
pub struct Checked {
    pub findings: Vec<Finding>,
    pub rows: Vec<MetricRow>,
    pub ranges: metrics::Ranges,
}

pub fn check(target: &Profile, guards: &[&Profile], avoid: &[String], draft_text: &str) -> Checked {
    let d = Draft::of(draft_text);
    let words = d.tokens.len();
    let corpus = Corpus::of(target);
    let concepts = voice::concepts(&corpus, 10.0);
    let avoided = voice::avoided_words(&corpus, 1000);
    let rates = voice::author_tell_rates(&corpus);
    let mut guarded: Vec<&Profile> = vec![target];
    guarded.extend(guards.iter().copied());
    let (ranges, rows) = metric_rows(target, &d, words.max(1));
    let mut findings = copy_overlaps(&d, &guarded);
    findings.extend(avoid_hits(&d, avoid, &concepts));
    findings.extend(metric_findings(&rows, &ranges));
    findings.extend(tell_findings(&d, &rates, &concepts));
    findings.extend(word_findings(&d, target, &concepts, &avoided));
    findings.extend(structure_findings(&d, &rows));
    findings.sort_by(|a, b| {
        b.severity
            .total_cmp(&a.severity)
            .then(a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
    });
    Checked { findings, rows, ranges }
}

/// Finding counts by kind, every kind present (zero when absent).
pub fn counts(findings: &[Finding]) -> BTreeMap<&'static str, usize> {
    let mut m: BTreeMap<&'static str, usize> = [
        "copy_overlap", "avoid_list", "metric_out_of_range", "llm_tell", "off_voice_word",
        "synonym_variation", "structure",
    ]
    .into_iter()
    .map(|k| (k, 0))
    .collect();
    for f in findings {
        *m.entry(f.kind).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_anchors_tokens_and_paragraphs_to_lines() {
        let d = Draft::of("# Title\n\nFirst line of prose\ncontinues here.\n\nSecond para.");
        assert_eq!(d.paragraphs.len(), 2);
        assert_eq!(d.paragraphs[0].2, 2);
        assert_eq!(d.line_of(0, 25), 4);
        let i = d.tokens.iter().position(|t| t == "continues").unwrap();
        assert_eq!(d.token_line[i], 3);
    }

    #[test]
    fn excerpt_keeps_short_lines_whole() {
        assert_eq!(excerpt("short line", "line"), "short line");
        let long = "word ".repeat(60) + "needle " + &"word ".repeat(60);
        let e = excerpt(&long, "needle");
        assert!(e.contains("needle") && e.chars().count() < 150);
    }
}
