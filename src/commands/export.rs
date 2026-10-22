//! `export`: a profile's passages as fine-tuning data.
//!
//! Fine-tuning on the author's own text is the one lever with strong
//! evidence for voice fidelity (and for how detectors judge the output), so
//! the export is shaped for it: continuation pairs, "the ~500 words before
//! -> the next passage", never crossing from one work into another. The last
//! work is held out as an eval split so generations can be checked against
//! text the model never saw.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::ExportFormat;
use crate::engine::profile::Profile;
use crate::engine::store;
use crate::error::AppError;
use crate::output::{self, Ctx};

/// Preceding context per example: whole passages are prepended until at
/// least this many words.
const PROMPT_WORDS: usize = 500;
/// A passage needs at least this much preceding text in its work to be a
/// continuation target (the first passage of a work has none).
const MIN_PROMPT_WORDS: usize = 200;
const SYSTEM_PROMPT: &str = "Continue the passage in the author's own voice.";

#[derive(Debug)]
struct Example {
    prompt: String,
    completion: String,
    work: String,
    eval: bool,
}

#[derive(Serialize)]
struct Counts {
    train: usize,
    eval: usize,
}

#[derive(Serialize)]
struct ExportData {
    path: String,
    format: ExportFormat,
    examples: Counts,
    eval_work: Option<String>,
    prompt_words: usize,
    warnings: Vec<String>,
}

pub fn run(ctx: Ctx, profile: String, out: PathBuf, format: ExportFormat) -> Result<(), AppError> {
    let bin = env!("CARGO_BIN_NAME");
    let p = store::load(&profile)?;
    let (examples, warnings) = build_examples(&p);
    if examples.is_empty() {
        return Err(AppError::input(
            format!("profile '{profile}' has no passage with {MIN_PROMPT_WORDS}+ words of preceding text"),
            format!("Add longer works to the corpus, then: {bin} profile build {profile} --corpus <path> --force"),
        ));
    }
    write_jsonl(&out, &examples, format)?;
    let path = out.canonicalize().unwrap_or(out).display().to_string();
    let eval: Vec<&Example> = examples.iter().filter(|e| e.eval).collect();
    let data = ExportData {
        path: path.clone(),
        format,
        examples: Counts {
            train: examples.len() - eval.len(),
            eval: eval.len(),
        },
        eval_work: eval.first().map(|e| e.work.clone()),
        prompt_words: PROMPT_WORDS,
        warnings,
    };
    let next = vec![
        "Fine-tune on the \"train\" lines; keep the \"eval\" lines for scoring".to_string(),
        format!("Check eval generations with: {bin} check <generation.md> --profile {profile}"),
    ];
    output::print_success_with(ctx, &data, &next, |d| {
        println!("Wrote {} ({} train, {} eval examples)", d.path, d.examples.train, d.examples.eval);
        if let Some(w) = &d.eval_work {
            println!("eval split: {w}");
        }
        for w in &d.warnings {
            println!("warning: {w}");
        }
    })
}

/// Continuation pairs per work, in corpus order. The last work that yields
/// examples is the eval split; a single-work profile holds out its final
/// 10% instead (with a warning, since that split shares the work's topic).
fn build_examples(p: &Profile) -> (Vec<Example>, Vec<String>) {
    let mut per_work: Vec<Vec<Example>> = Vec::new();
    let mut i = 0;
    while i < p.chunks.len() {
        let src = p.chunks[i].source;
        let mut j = i;
        while j < p.chunks.len() && p.chunks[j].source == src {
            j += 1;
        }
        let work = p
            .sources
            .get(src as usize)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let mut exs = Vec::new();
        for t in i + 1..j {
            let mut parts: Vec<String> = Vec::new();
            let mut words = 0usize;
            for c in p.chunks[i..t].iter().rev() {
                words += c.n_words as usize;
                parts.push(c.text());
                if words >= PROMPT_WORDS {
                    break;
                }
            }
            if words < MIN_PROMPT_WORDS {
                continue;
            }
            parts.reverse();
            exs.push(Example {
                prompt: parts.join("\n\n"),
                completion: p.chunks[t].text(),
                work: work.clone(),
                eval: false,
            });
        }
        if !exs.is_empty() {
            per_work.push(exs);
        }
        i = j;
    }

    let mut warnings = Vec::new();
    match per_work.len() {
        0 => {}
        1 => {
            let exs = &mut per_work[0];
            let held = (exs.len() / 10).max(1).min(exs.len().saturating_sub(1));
            let n = exs.len();
            for e in &mut exs[n - held..] {
                e.eval = true;
            }
            warnings.push(
                "only one work: the eval split is the final 10% of it, so it shares the training text's topic and characters"
                    .to_string(),
            );
        }
        _ => {
            for e in per_work.last_mut().unwrap() {
                e.eval = true;
            }
        }
    }
    (per_work.into_iter().flatten().collect(), warnings)
}

fn write_jsonl(out: &Path, examples: &[Example], format: ExportFormat) -> Result<(), AppError> {
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut buf = String::new();
    for e in examples {
        let split = if e.eval { "eval" } else { "train" };
        let line = match format {
            ExportFormat::Chat => serde_json::json!({
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": e.prompt},
                    {"role": "assistant", "content": e.completion},
                ],
                "split": split,
                "work": e.work,
            }),
            ExportFormat::Completion => serde_json::json!({
                "prompt": e.prompt,
                "completion": e.completion,
                "split": split,
                "work": e.work,
            }),
        };
        buf.push_str(&serde_json::to_string(&line).map_err(|_| AppError::Serialization)?);
        buf.push('\n');
    }
    // Write-then-rename so an interrupted export never leaves half a file.
    let tmp = out.with_extension("jsonl.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::profile::ProfileKind;
    use crate::engine::text::{self, Doc};

    /// Synthetic prose: `paras` paragraphs of about 60 words each.
    fn doc(name: &str, paras: usize) -> Doc {
        let para = "She walked to the window and looked out at the street below. \
                    The light was going and the cars moved slowly in the rain. \
                    He said nothing, but she knew he was watching her, waiting \
                    for her to turn around and tell him what she had decided to do.";
        Doc {
            name: name.into(),
            text: vec![para; paras].join("\n\n"),
        }
    }

    #[test]
    fn last_work_is_eval_and_prompts_stay_in_their_work() {
        let docs = [doc("a.md", 30), doc("b.md", 30), doc("c.md", 20)];
        let p = Profile::build_min("t", &docs, ProfileKind::Author, 1).unwrap();
        let (exs, warnings) = build_examples(&p);
        assert!(warnings.is_empty());
        assert!(exs.iter().filter(|e| e.eval).all(|e| e.work == "c.md"));
        assert!(exs.iter().filter(|e| !e.eval).all(|e| e.work != "c.md"));
        assert!(exs.iter().any(|e| e.eval) && exs.iter().any(|e| !e.eval));
        for e in &exs {
            assert!(text::count_words(&e.prompt) >= MIN_PROMPT_WORDS, "{}", e.prompt.len());
        }
        // Each work loses its first passage(s) as context: no cross-work prompts.
        let per_work = |w: &str| exs.iter().filter(|e| e.work == w).count();
        let chunks_of = |s: u32| p.chunks.iter().filter(|c| c.source == s).count();
        assert!(per_work("a.md") < chunks_of(0));
        assert!(per_work("b.md") < chunks_of(1));
    }

    #[test]
    fn single_work_holds_out_its_tail_with_a_warning() {
        let p = Profile::build_min("t", &[doc("only.md", 80)], ProfileKind::Author, 1).unwrap();
        let (exs, warnings) = build_examples(&p);
        assert_eq!(warnings.len(), 1);
        let eval = exs.iter().filter(|e| e.eval).count();
        assert!(eval >= 1 && eval < exs.len());
        assert!(exs.last().unwrap().eval && !exs[0].eval);
    }

    #[test]
    fn writes_one_json_object_per_line() {
        let p = Profile::build_min("t", &[doc("a.md", 30), doc("b.md", 30)], ProfileKind::Author, 1)
            .unwrap();
        let (exs, _) = build_examples(&p);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("sub/out.jsonl");
        write_jsonl(&out, &exs, ExportFormat::Completion).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<serde_json::Value> =
            body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), exs.len());
        assert!(lines.iter().all(|l| l["prompt"].is_string() && l["split"].is_string()));
        write_jsonl(&out, &exs, ExportFormat::Chat).unwrap();
        let first: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&out).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(first["messages"][2]["role"], "assistant");
    }
}
