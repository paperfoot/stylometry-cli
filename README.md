# stylometry

**Forensic-grade authorship verification from the terminal.** Build a per-author
profile by fingerprinting their writing, then ask of any text: *was this written
by that author?* — and get a calibrated probability, not a vibe.

Pure Rust, single static binary, no model and no network required. Built on the
[agent-cli-framework](https://github.com/paperfoot/agent-cli-framework): JSON
envelopes, semantic exit codes, and a machine-readable `agent-info` manifest, so
agents and humans use it the same way.

## Why

LLM-era writing tools need a *trustworthy ruler*. A hand-weighted similarity
score with no standardization and no calibration can't tell you whether text is
"in an author's voice" — optimizing against it just games the metric. This tool
implements the method forensic and academic stylometry actually use, and reports
a calibrated verdict you can defend.

It is also the independent **judge** for a sibling rewriting tool: kept separate
on purpose, run only on held-out text, never optimized against.

## Install

```bash
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
stylometry profile list
```

`compare` returns Cosine Delta, Classic Burrows Delta, the nearest profile, a
General-Imposters score, and (once calibrated) `P(same author)` with a
same/different verdict. Every command takes `--json`; run `stylometry agent-info`
for the full manifest.

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
   profiles as imposters; report AUC and the decision threshold. A
   General-Imposters score says how much closer the text is to the target than
   to any other profile.

## Validation

On a labelled set of Adams vs three near-neighbour British comic authors
(Jerome, Wodehouse, Chesterton):

| Metric | Result |
|---|---|
| Author-separation AUC | **0.9999** |
| Accuracy at threshold | **0.996** |
| Held-out Adams book (unseen) | **same_author**, P ≈ 1.0 |
| Jerome vs Adams | **different_author**, correctly attributed to jerome |

Reproduce: `eval/fetch_corpora.sh` then `eval/validate.sh` (exits 0 only if the
verifier holds). The build is ritalin-gated.

## Roadmap

See [ROADMAP.md](ROADMAP.md). v0.2 adds a content-independent neural style
embedding (StyleDistance) as a second, separately-calibrated axis, a frozen
reference + chunk manifest so a fine-tuning tool can exclude the judge's text,
the full PAN metric suite, and a pure-text "reads-as-LLM" axis.

## License

MIT.
