# Roadmap

v0.1 was the lean, pure-Rust 80/20: z-scored MFW + char-trigram Cosine/Classic
Delta, logistic calibration, General-Imposters scoring, profiles, and the
agent-cli-framework contract. Validated (AUC 0.9999 on Adams vs near-neighbours).

## Done in v0.2 (hardening)

- **Frozen reference.** `calibrate` freezes the exact reference model (vocab +
  mean/sd) into the calibration artifact; `compare` scores calibrated profiles
  in that frozen z-space, so verdicts never silently shift as profiles are
  added or removed. Uncalibrated profiles still use the live reference. A
  profile-set drift after calibration downgrades to a warning (imposter pool
  changed) instead of invalidating the verdict.
- **Honest threshold selection.** The decision threshold is selected on a train
  split and its accuracy reported on a contiguous held-out tail split it never
  saw (`holdout_accuracy`); the shipped threshold/logistic then refit on all
  data. Contiguous — not interleaved — so adjacent-chunk topic doesn't leak
  into the holdout.
- **Data-selected regularization.** The logistic's L2 strength is picked by
  3-fold cross-validated Brier score over a grid (weak lambdas only allowed
  with enough samples), replacing the hard-coded λ=0.5 that compressed all
  probabilities toward 0.5.
- **Abstention band (partial PAN suite).** P(same) in [0.35, 0.65] returns
  `inconclusive` instead of a forced call; `calibrate` reports c@1 (with
  abstention) and holdout Brier alongside AUC and accuracy. Still owed from
  the full suite: F0.5u and ECE.
- **Length-mismatch guard.** Calibrations record the chunk length they were fit
  on; `compare` warns when the query text is <0.5x or >2x that length, where
  P(same author) is not calibrated.
- **Minimum-imposter warning.** Calibrating against fewer than 3 imposter
  profiles warns that probabilities will be overconfident.
- **One definition of "word".** Chunk sizing, the short-text gate, and length
  warnings all use Unicode word segmentation — the same tokenizer the features
  use — instead of mixing in whitespace-token counts.
- **stdin input.** `compare -` or piped stdin work alongside a file path and
  `--text`.
- **Name.** Shipping as `stylometry` (decided; `voiceprint` rejected).

## v0.2+ — rigor and the second axis

- **Neural style axis (separate, never fused).** Add a content-independent style
  embedding via one pinned Python sidecar (uv venv, CPU torch + sentence-
  transformers): primary `StyleDistance/styledistance` (MIT), optional
  `AnnaWegmann/Style-Embedding`. Report its cosine as a *third* calibrated number
  alongside Delta and GI; an "indistinguishable" verdict requires all three to
  agree. Keep the Rust core dependency-free; the sidecar is opt-in via `doctor`.
- **Leakage firewall for the rewriter.** Emit a SHA-256 chunk manifest so a
  fine-tuning tool can exclude the judge's exact held-out text from training.
- **Finish the PAN metric suite.** F0.5u and ECE reliability (c@1 and Brier
  landed in v0.2).
- **General Imposters, proper.** Koppel–Winter bootstrap (N≈100, feature/imposter
  subsampling, Ruzicka/minmax distance) with a p1<p2 abstain zone, parallelized
  with rayon — replacing the single-shot GI fraction.
- **Length-banded calibration.** The v0.2 length-mismatch warning tells you when
  P(same) is unreliable; the real fix is calibrating per length band (e.g.
  150/500/1500 words) so short texts get an honest probability instead of a
  warning. Prerequisite for chat/short-message use cases.

## v0.2+ — "reads as human vs LLM" axis (pure-text, no model)

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
  three. (The chunk-level train/test split landed in v0.2; the work-level and
  open-set evals are still owed.) Cleanliness/register: the v0.1 LOWO miss
  (*The Salmon of Doubt*) turned out to be a contaminated multi-author
  collection, so cross-register sensitivity was NOT cleanly demonstrated — a
  proper cross-genre test (clean Adams non-fiction vs clean Adams fiction) is
  still owed. Regardless, build the writer-judge profile from clean Adams
  non-fiction.
- Publish to crates.io + a Homebrew formula in `paperfoot/homebrew-tap`.
