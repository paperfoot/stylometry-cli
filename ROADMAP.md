# Roadmap

v0.1 is the lean, pure-Rust 80/20: z-scored MFW + char-trigram Cosine/Classic
Delta, logistic calibration, General-Imposters scoring, profiles, and the
agent-cli-framework contract. Validated (AUC 0.9999 on Adams vs near-neighbours).
Items below were scoped out deliberately to keep v0.1 small; they come from the
design fleet and the LLM-detection research pass.

## v0.2 — rigor and the second axis

- **Neural style axis (separate, never fused).** Add a content-independent style
  embedding via one pinned Python sidecar (uv venv, CPU torch + sentence-
  transformers): primary `StyleDistance/styledistance` (MIT), optional
  `AnnaWegmann/Style-Embedding`. Report its cosine as a *third* calibrated number
  alongside Delta and GI; an "indistinguishable" verdict requires all three to
  agree. Keep the Rust core dependency-free; the sidecar is opt-in via `doctor`.
- **Frozen reference.** `profile build` should freeze `mean/sd` + the MFW list +
  provenance hashes into the artifact, so scoring never silently re-fits as
  profiles are added. v0.1 recomputes the reference from all loaded profiles
  (simpler, but reference-dependent).
- **Leakage firewall for the rewriter.** Emit a SHA-256 chunk manifest so a
  fine-tuning tool can exclude the judge's exact held-out text from training.
- **Full PAN metric suite.** Add c@1 (with a grey-zone abstention band), F0.5u,
  and Brier/ECE reliability alongside AUC and accuracy.
- **General Imposters, proper.** Koppel–Winter bootstrap (N≈100, feature/imposter
  subsampling, Ruzicka/minmax distance) with a p1<p2 abstain zone, parallelized
  with rayon — replacing v0.1's single-shot GI fraction.

## v0.2 — "reads as human vs LLM" axis (pure-text, no model)

From the detection literature (DetectGPT/Binoculars family, SpecDetect,
StyloMetrix; classic stylometry survives LLM impersonation — arXiv 2603.29454):

- Cheap, model-free features: sentence-length coefficient-of-variation
  (burstiness), function-word z-profile, "AI vocabulary" excess (delve,
  underscore, pivotal, moreover…), punctuation entropy + em-dash rate, MATTR,
  n-gram repetition. Report as an `ai_likeness` axis.
- Optional model-based signal later: DFT energy of a local LM's per-token
  log-perplexity sequence (SpecDetect, arXiv 2508.11343) via `gemma-cli`/MLX.

## Validation & distribution

- **Dev-only oracle cross-check.** An `xtask` that compares our Delta against the
  `stylo` R package (Würzburg + General Imposters) and `faststylometry` (Classic
  Delta + calibration) on a shared toy corpus, asserting max-abs deviation. These
  stay dev/CI dependencies, never runtime.
- **Honest generalization eval** (from the Codex + Gemini reviews). Leave-one-
  WORK-out (positives only across *different* books by the same author); texts
  normalized to 500–1000 words (the forensic range); a same-topic control (e.g.
  Federalist Papers or same-event news) to prove we aren't classifying topic;
  an open-set imposter pool (100+ authors) instead of the current closed set of
  three; and a train/test split so the threshold is never chosen on the test
  set. Register must match the target: the v0.1 LOWO showed Adams's non-fiction
  is rejected by a fiction-built profile, so the writer-judge profile must be
  built from Adams non-fiction.
- Publish to crates.io + a Homebrew formula in `paperfoot/homebrew-tap`.

## Open question

- **Name.** Shipping as `stylometry`. The design fleet preferred `voiceprint`
  (evocative, avoids the `stylo`/CRAN collision). Decide before first publish.
