<div align="center">

# stylometry

**Authorship verification from the terminal — a calibrated probability, not a vibe.**

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

Build a per-author profile by fingerprinting their writing, then ask of any
text: *was this written by that author?* It implements the method forensic and
academic stylometry actually use (Burrows/Cosine Delta) — lean and local, not a
court-grade instrument (see [Validation](#validation)).

[Install](#install) | [Quick start](#quick-start) | [How it works](#how-it-works) | [Validation](#validation) | [Roadmap](#roadmap)

</div>

## Why

LLM-era writing tools need a *trustworthy ruler*. A hand-weighted similarity
score with no standardization and no calibration can't tell you whether text is
"in an author's voice" — optimizing against it just games the metric. This tool
reports a calibrated verdict you can defend: `P(same author)`, an honest
holdout accuracy, and an **inconclusive** verdict where the evidence is thin,
instead of a forced guess.

It is also the independent **judge** for a sibling rewriting tool: kept separate
on purpose, run only on held-out text, never optimized against.

Pure Rust, single static binary, no model and no network required. Built on the
[agent-cli-framework](https://github.com/paperfoot/agent-cli-framework): JSON
envelopes, semantic exit codes, and a machine-readable `agent-info` manifest, so
agents and humans use it the same way.

## Install

```bash
brew install paperfoot/tap/stylometry                             # Homebrew
cargo install stylometry-cli                                      # crates.io (binary: stylometry)
cargo install --git https://github.com/paperfoot/stylometry-cli   # from source
```

## Quick start

```bash
# Fingerprint each author from a folder of .md/.txt (a file also works)
stylometry profile build adams      --corpus ./adams-nonfiction/
stylometry profile build wodehouse  --corpus ./wodehouse/     # others double as background
stylometry profile build jerome     --corpus ./jerome/

# Fit the verifier: delta -> P(same author), with AUC against the other profiles
stylometry calibrate adams

# Verdict on a new text
stylometry compare suspect.txt --profile adams
cat draft.md | stylometry compare --profile adams             # stdin works too
stylometry profile list
```

`compare` returns Cosine Delta, Classic Burrows Delta, the nearest profile, a
background-rank score (a simple rank fraction, not full General Imposters), and
(once calibrated) `P(same author)` with a **same / different / inconclusive**
verdict — probabilities in the 0.35–0.65 band abstain rather than force a call.
It reads a file, `-`/piped stdin, or `--text`, and warns when the text's length
is far from the length the calibration was fit on. Every command takes
`--json`; run `stylometry agent-info` for the full manifest.

## How it works

1. **Fingerprint.** Tokenize a corpus into ~1,500-word chunks; count the most
   frequent words and character trigrams (the two strongest authorship signals
   in the PAN literature).
2. **Standardize (Burrows Delta).** z-score every feature against the combined
   reference of all profiles — so a frequency counts as "unusual relative to
   writers in general", which is the whole point of Delta over raw cosine.
3. **Distance.** Default **Cosine Delta** (Würzburg variant) to the author's
   z-space centroid; Classic Burrows Delta reported alongside.
4. **Calibrate + verify.** Fit a logistic `delta → P(same author)` using the
   author's own held-out chunks (leave-one-out) as positives and the other
   profiles as imposters. The logistic's L2 strength is selected by 3-fold
   cross-validated Brier score, not hard-coded; the decision threshold is
   selected on a train split and its accuracy reported on a held-out tail split
   it never saw (`holdout_accuracy`), alongside AUC, holdout Brier, and PAN c@1
   with abstention. A background-rank score (a simple rank fraction, not full
   Koppel-Winter General Imposters) says how much closer the text is to the
   target than to any other profile.
5. **Frozen reference.** `calibrate` freezes the exact reference model (vocab +
   mean/SD) into the calibration, so a calibrated profile's verdicts never
   silently shift when profiles are added or removed later. If the profile set
   drifts, `compare` keeps the (still valid) frozen verdict and warns that the
   imposter pool changed. Calibrating against fewer than 3 imposters also
   warns: the probabilities would be overconfident.

## Validation

Adams vs three near-neighbour British comic authors (Jerome, Wodehouse,
Chesterton), with two deliberately adversarial checks:

| Check | Result |
|---|---|
| Same-source control (3 Gutenberg authors, identical formatting) | author-separation AUC 1.0 |
| Leave-one-work-out: hold out each whole Adams book, verify it | 5/6 works → same_author |
| Cross-author negative (Jerome) vs Adams | different_author, attributed to jerome |

Two things keep these honest rather than flattering:

- **Same-source control.** Separating three authors whose texts share an
  identical plain-text source shows the signal is authorial style, not a
  formatting or provenance artifact.
- **Leave-one-work-out.** Holding out an entire *book* (different topic, never
  trained) and still verifying it is a real generalization test, unlike
  leave-one-chunk-out within a single book. 5 of 6 Adams works pass.

The one LOWO miss is instructive but **confounded**: *The Salmon of Doubt* is a
posthumous, editor-assembled collection — its own contents list three
introductions, an editor's note, and an unfinished *novel* — so it is not clean
single-author non-fiction. As of v0.2 the verifier **abstains** on it
(`inconclusive`, P=0.399) rather than confidently rejecting it — the abstention
band doing exactly its job on contaminated input. Topic-invariance otherwise
held (the four Hitchhiker novels have different plots yet all verified).
Practical takeaway: build a profile from clean text of the kind you intend to
verify.

Caveat: each author here is one long book, so author/book/topic are partly
confounded and chunk-level AUC is optimistic. A topic-controlled evaluation
(short texts, same-topic different-authors, open-set imposters) is still owed;
see [ROADMAP.md](ROADMAP.md). The method and math were independently reviewed by
GPT-5.5 (Codex) and Gemini; their findings drove the calibration-binding,
train/query feature-parity, and logistic-regularization fixes.

Reproduce: `eval/fetch_corpora.sh`, then `eval/validate.sh` (smoke test) and
`eval/lowo.sh` (the honest cross-work test). The build is ritalin-gated.

## Roadmap

See [ROADMAP.md](ROADMAP.md). v0.2 shipped the hardening pass: frozen reference
model, holdout-split threshold accuracy, CV-selected regularization, the
abstention band, length-mismatch and imposter-count warnings, and stdin input.
Next up: a content-independent neural style embedding (StyleDistance) as a
second, separately-calibrated axis, proper Koppel–Winter General Imposters,
length-banded calibration for short texts (chat, email), and a pure-text
"reads-as-LLM" axis.

## Contributing

Issues and PRs welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Good first
contributions: corpora for non-English validation, eval scripts for new
domains, and anything in the roadmap marked "still owed".

## License

[MIT](LICENSE).

---

<div align="center">

Built by [Boris Djordjevic](https://github.com/longevityboris) at [Paperfoot AI](https://paperfoot.com)

<br />

**If this is useful to you:**

[![Star this repo](https://img.shields.io/github/stars/paperfoot/stylometry-cli?style=for-the-badge&logo=github&label=%E2%AD%90%20Star%20this%20repo&color=yellow)](https://github.com/paperfoot/stylometry-cli/stargazers)
&nbsp;&nbsp;
[![Follow @longevityboris](https://img.shields.io/badge/Follow_%40longevityboris-000000?style=for-the-badge&logo=x&logoColor=white)](https://x.com/longevityboris)

</div>
