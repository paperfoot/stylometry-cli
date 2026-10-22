use serde::Serialize;
use std::path::PathBuf;

use crate::error::AppError;
use crate::output::{self, Ctx};

// ── Skill content ───────────────────────────────────────────────────────────
// Built from the binary name. A signpost (Agent Skills format): when to use
// the CLI, the loop with real commands, and how to inspect one command. All
// flag detail lives in `agent-info`, so the skill never drifts from the binary.

fn skill_content() -> String {
    let name = env!("CARGO_BIN_NAME");
    format!(
        r#"---
name: {name}
description: >
  Capture a writer's voice from their own text and write or edit in it, for
  novels, non-fiction, blogs, X posts, emails and messages: signature words,
  the synonyms they pick and avoid, rhythm, punctuation, post habits (length,
  layout, emoji, hashtags, openers), a style brief with matched examples, and
  a scored line-by-line check of drafts with a revision plan. Also authorship
  verification. Use for "write like X", "in my voice", "post like me", "match
  the style", "check the voice", "does this sound like X", "who wrote this",
  style briefs, voice profiles and blended voices.
---

## {name}

The loop: build a profile once, brief the writer, draft, check, revise. It
works for novels, non-fiction, blogs, X posts, emails and messages: the
profile's register decides the unit (passages or posts), the targets and the
brief.

```bash
{name} profile build ada --corpus ./ada-books/          # .md/.txt; each file is one work
{name} profile build ada-x --corpus tweets.js           # X archive / xmaster JSON / JSON / CSV
{name} guide ada --about "chapter 4: the argument"      # brief + the author's most similar passages
{name} examples ada-x --about "launch post" --as post   # more matched examples
{name} check draft.md --profile ada                     # score, plan, findings with line numbers
{name} check posts.md --profile ada-x --as post         # posts separated by --- lines
```

`check` returns `score` (0-100: how typical of the author at this length),
`pass`, a `plan` of the highest-value revisions with quantities, and grouped
`findings` (copied runs and banned words are hard failures). Revise in that
order and re-check; add `--verify` on the final pass for authorship, `--strict`
in a gate (exit 5 on failure). Blend authors with `{name} profile blend`.
Exemplars are the author's text: imitate the mechanisms, never copy lines.
Set type exactly as the brief's Typography section says (quotes, apostrophes,
dashes, ellipses, scene breaks), and skip the tidy moves it lists under "What
gives imitations away": those, not the topic, are what readers catch.

For authorship of books, run `{name} background fetch` once, then
`{name} calibrate ada` and `{name} compare text.txt --profile ada`. Piped
output is JSON; follow `metadata.next` and each error's `suggestion`. Inspect a
command with `{name} agent-info --command check`.
"#
    )
}

// ── Platform targets ────────────────────────────────────────────────────────

struct SkillTarget {
    name: &'static str,
    path: PathBuf,
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn skill_targets() -> Vec<SkillTarget> {
    let h = home();
    let app = env!("CARGO_BIN_NAME");
    vec![
        SkillTarget {
            name: "Claude Code",
            path: h.join(format!(".claude/skills/{app}")),
        },
        SkillTarget {
            name: "Codex CLI",
            path: h.join(format!(".codex/skills/{app}")),
        },
        SkillTarget {
            name: "Gemini CLI",
            path: h.join(format!(".gemini/skills/{app}")),
        },
    ]
}

// ── Install ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct InstallResult {
    platform: String,
    path: String,
    status: String,
}

pub fn install(ctx: Ctx) -> Result<(), AppError> {
    let content = skill_content();
    let mut results: Vec<InstallResult> = Vec::new();

    for target in &skill_targets() {
        let skill_path = target.path.join("SKILL.md");

        if skill_path.exists() && std::fs::read_to_string(&skill_path).is_ok_and(|c| c == content) {
            results.push(InstallResult {
                platform: target.name.into(),
                path: skill_path.display().to_string(),
                status: "already_current".into(),
            });
            continue;
        }

        std::fs::create_dir_all(&target.path)?;
        std::fs::write(&skill_path, &content)?;
        results.push(InstallResult {
            platform: target.name.into(),
            path: skill_path.display().to_string(),
            status: "installed".into(),
        });
    }

    output::print_success_or(ctx, &results, |r| {
        use owo_colors::OwoColorize;
        for item in r {
            let marker = if item.status == "installed" { "+" } else { "=" };
            println!(
                " {} {} -> {}",
                marker.green(),
                item.platform.bold(),
                item.path.dimmed()
            );
        }
    })?;

    Ok(())
}

// ── Status ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SkillStatus {
    platform: String,
    installed: bool,
    current: bool,
}

pub fn status(ctx: Ctx) -> Result<(), AppError> {
    let content = skill_content();
    let mut results: Vec<SkillStatus> = Vec::new();

    for target in &skill_targets() {
        let skill_path = target.path.join("SKILL.md");
        let (installed, current) = if skill_path.exists() {
            let current = std::fs::read_to_string(&skill_path).is_ok_and(|c| c == content);
            (true, current)
        } else {
            (false, false)
        };
        results.push(SkillStatus {
            platform: target.name.into(),
            installed,
            current,
        });
    }

    output::print_success_or(ctx, &results, |r| {
        use owo_colors::OwoColorize;
        let mut table = comfy_table::Table::new();
        table.set_header(vec!["Platform", "Installed", "Current"]);
        for item in r {
            table.add_row(vec![
                item.platform.clone(),
                if item.installed {
                    "Yes".green().to_string()
                } else {
                    "No".red().to_string()
                },
                if item.current {
                    "Yes".green().to_string()
                } else {
                    "No".dimmed().to_string()
                },
            ]);
        }
        println!("{table}");
    })?;

    Ok(())
}
