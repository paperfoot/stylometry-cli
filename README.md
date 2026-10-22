<div align="center">

# stylometry

**Capture a writer's voice. Brief an AI agent to write in it. Check every draft against it.**

*Novels, non-fiction, blogs, X posts, emails, messages.*

<br />

[![Star this repo](https://img.shields.io/github/stars/paperfoot/stylometry-cli?style=for-the-badge&logo=github&label=%E2%AD%90%20Star%20this%20repo&color=yellow)](https://github.com/paperfoot/stylometry-cli/stargazers)
&nbsp;&nbsp;
[![Follow @longevityboris](https://img.shields.io/badge/Follow_%40longevityboris-000000?style=for-the-badge&logo=x&logoColor=white)](https://x.com/longevityboris)

<br />

[![crates.io](https://img.shields.io/crates/v/stylometry-cli?style=for-the-badge&logo=rust&color=orange)](https://crates.io/crates/stylometry-cli)
&nbsp;
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=for-the-badge)](LICENSE)
&nbsp;
[![Pure Rust](https://img.shields.io/badge/Pure_Rust-no_model,_no_network-9cf?style=for-the-badge&logo=rust&logoColor=white)](#how-it-works)

---

Feed it a writer's books, blog posts or X archive. It learns the habits that
make the voice: the words they reach for and the ones they never use, which
synonym they pick, how long their sentences (or posts) run, how they
punctuate, open and close, how much is dialogue. It turns those into a brief an
agent can write from, with the writer's own most similar pieces as examples,
then scores the agent's draft, says what to change and how much.

[Install](#install) | [Quick start](#quick-start) | [What it measures](#what-it-measures) | [For agents](#for-agents) | [Verification](#authorship-verification) | [Validation](#validation)

</div>

## Why

Language models write in one voice: their own. Ask for "the style of" a writer
and you get the topic, a few borrowed words, and the model's habits underneath:
choppy fragments or uniform sentences, *delve* and *tapestry*, *gazed* where the
writer would say *looked*, a different synonym every time the same thing comes
up. Prompting with adjectives ("wry, intimate, spare") doesn't fix it, because
the voice lives in counts no one can see by reading: how often *but* appears,
how many sentences run under five words, whether the narrator asks herself
questions.

`stylometry` measures those counts from the writer's own text and holds drafts
to them. Everything is local: a single Rust binary, no model, no network
(except a one-time download of public-domain reference books).

## Install

```bash
brew install paperfoot/tap/stylometry                             # Homebrew
cargo install stylometry-cli                                      # crates.io (binary: stylometry)
cargo install --git https://github.com/paperfoot/stylometry-cli   # from source
```

## Quick start

```bash
# 1. Profile a voice (each file is one work; the kind of writing is detected)
stylometry profile build me --corpus ./my-novels/
stylometry profile build me-x --corpus ./twitter-archive/data/tweets.js

# 2. See what makes it distinctive
stylometry analyze me

# 3. Brief a writing agent for the piece at hand (targets, word choices, ban
#    list, and the writer's own passages most like it)
stylometry guide me --about "chapter 7: the fight in the kitchen" -o brief.md
stylometry guide me-x --about "launching our new CLI" --as post

# 4. Check the draft: a 0-100 score, a revision plan with quantities, and
#    line-numbered findings with the writer's own alternatives
stylometry check chapter-07.md --profile me
stylometry check posts.md --profile me-x --as post   # posts separated by ---
```

| Writing | Corpus | Unit of comparison |
|---|---|---|
| Fiction, non-fiction | `.md`/`.txt` books (epub conversions are cleaned) | passages at the draft's length; chapters as sections (POV/tense modes) |
| Blog, essays | a folder of posts, JSON/CSV exports | windows across posts; headings, lists, openings and closings |
| X / social | X archive (`tweets.js`), xmaster JSON, JSONL, CSV | each post, by kind (post, reply, thread part); layout, emoji, hashtags, endings |
| Email, messages | `.eml`, DMs from an X archive, text separated by `---` | each message; greetings, sign-offs, length |

Create a voice by mixing writers:

```bash
stylometry profile build hoover --corpus ./hoover/
stylometry profile build fisher --corpus ./fisher/
stylometry profile blend target --from hoover=0.6 --from fisher=0.4
stylometry check chapter-07.md --profile target --strict   # exit 5 if it fails: a CI-style gate
```

## What it measures

| Area | Examples |
|---|---|
| **Vocabulary** | Signature words and phrases (dispersed across the work, not one scene); words the writer avoids that general English uses; character and place names filtered out |
| **Word choice** | Per-concept preferences: *said* vs *remarked*, *look* vs *gaze*, *but* vs *however*, *want* vs *desire*; members the writer never uses |
| **Rhythm** | Sentence length (mean, median, spread, share of fragments and long sentences), paragraph length, one-sentence paragraphs |
| **Texture** | Punctuation per 1,000 words, dialogue share and tags, contractions, questions, similes, *which* clauses, *and*-chains, sentence openers, lexical diversity |
| **Machine tells** | LLM-typical words and structures (*delve*, *a testament to*, "not X, but Y", trailing *-ing* clauses), flagged only when the writer doesn't use them |
| **Posts** | Length and layout (one line, line-per-sentence, blocks), openers, endings, emoji and hashtags (which, where, how often), casing, links and mentions, what the writer never does (citing counts) |
| **Structure** | Headings and lists (blogs), opening and closing paragraphs, narrative mode per chapter (fiction) |
| **Copying** | Any long run shared with the writer's own text is a hard failure; the writer's recurring formulas are exempt and listed as signature phrasing |

Targets are measured at the length you're checking. A 1,200-word scene is
compared to 1,200-word passages of the writer, not to whole-book averages,
because a short passage varies more than a book does.

## For agents

Every command answers in a JSON envelope when piped, with `metadata.next`
naming the obvious next command, and every error carries the exact fix.
`stylometry agent-info` prints a compact manifest; `stylometry agent-info
--command check` returns one command in full. `stylometry skill install` adds
the skill to Claude Code and other agent platforms.

The loop an agent runs:

1. `guide <voice> --about "<the piece>"` (and `examples` for more) → draft
2. `check draft.md -p <voice>` → follow the `plan` (hard failures first)
3. re-check until it passes; `--verify` on the final pass adds authorship

`check` reports `score` (how typical of the writer the draft is at its length:
50 is as typical as the writer's median passage), a `plan` of the revisions
worth most ("merge about 9 of your 31 fragments", "add 2-4 questions"), and
grouped `findings` with line numbers. `--strict` exits 5 on failure, for gates.

`export <voice> -o train.jsonl` writes the writer's passages as fine-tuning
pairs. Fine-tuning on a writer's own text is the strongest documented way to
get a model to hold a voice.

## Authorship verification

`compare` asks whether a text was written by a profile's author, with a
calibrated probability and an *inconclusive* band rather than a forced call.
It uses the method academic stylometry relies on (Burrows/Cosine Delta over
function words and character trigrams), calibrated per text length against a
background of ~40 public-domain authors (`background fetch`, once). On a long
manuscript it returns a per-passage drift map. It is a strong signal, not a
court-grade instrument.

## Validation

Measured on real books (scripts in `eval/`; copyrighted corpora stay local).

**Voice checks.** A test the owner's own novel pipeline had already run by
hand: a blended target voice (two published romantic-suspense authors,
60/40) against an AI-drafted manuscript. On a 3,000-word window the check
fails the draft with 27 of 86 measures out of range, in the same directions
the hand-built gate found: fragments (median sentence 6 words vs the
authors' 8–13), *and*-chains, similes and colour words too high; *but*,
*as*, narrated questions and *-ly* adverbs too low. A genuine 3,000-word
passage of one of the authors lands 79 of 86 in range. `diff` between the
draft and the target recovers the same picture unprompted: the draft
leans on *the, and, it, rain, wet, like*; the authors on *to, but, as, if,
would, why*.

**Verification.** An Adams profile from five books, calibrated against 45
other authors (42 public-domain background authors plus three
near-neighbour British humorists):

| Check | Result |
|---|---|
| Separation (AUC), 250 → 4,000-word passages | 0.953 → 0.994 |
| Held-out book (*Mostly Harmless*), 800 / 1,500 / 3,000-word excerpts | 19/20, 18/20, 20/20 `same_author`; the rest `inconclusive`, never `different_author` |
| Near-neighbour author (Jerome), any length | 0/20 false `same_author` |
| Holdout accuracy on a held-out *work* in a different register (non-fiction) | 0.70–0.74: an honest, harder number than within-book tests |

Earlier versions calibrated against three authors only and accepted
unseen writers as the target about half the time; the background pool is
what fixes that.

## Data and licences

Code: MIT. The embedded English frequency table comes from
[wordfreq](https://github.com/rspeer/wordfreq) (Robyn Speer et al.), data
licensed CC-BY-SA 4.0. It predates widespread LLM-generated text, which makes
it a fair baseline for human English. Fiction slop lists adapted from
[slop-score](https://github.com/sam-paech/slop-score) (MIT).

## Contributing

Issues and PRs welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).

---

<div align="center">

Built by [Boris Djordjevic](https://github.com/longevityboris) at [Paperfoot AI](https://paperfoot.com)

<br />

**If this is useful to you:**

[![Star this repo](https://img.shields.io/github/stars/paperfoot/stylometry-cli?style=for-the-badge&logo=github&label=%E2%AD%90%20Star%20this%20repo&color=yellow)](https://github.com/paperfoot/stylometry-cli/stargazers)
&nbsp;&nbsp;
[![Follow @longevityboris](https://img.shields.io/badge/Follow_%40longevityboris-000000?style=for-the-badge&logo=x&logoColor=white)](https://x.com/longevityboris)

</div>
