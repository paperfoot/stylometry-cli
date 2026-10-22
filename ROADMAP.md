# Roadmap

## v0.3 (this release): from verifier to voice tool

- Voice profiles keep their text in ~250-word paragraph-aligned passages
  (re-analysable after upgrades with `profile rebuild`); blends of weighted
  authors as target voices.
- `analyze`, `guide`, `check`, `diff`, `export`: voice report, agent brief
  with exemplars, line-level draft checks with ranges at the draft's own
  length, a copy guard and a gate mode, voice contrast, fine-tuning data.
- Verification: 42 public-domain background authors, calibration per text
  length, cross-work positives, class-balanced logistic, verdicts from P
  only, per-passage drift maps for long manuscripts.
- agent-cli-framework (October 2026): parser-generated discovery,
  `agent-info --command`, inline parse-error help, `doctor`, exit 5 for a
  failed gate.

## Next

- Rolling drift report as its own command (per chapter, against the target
  and against a "machine register" profile of unedited agent drafts).
- Biber-lite grammatical proxies (nominalisations, agentless passives).
- Optional style-embedding second opinion (StyleDistance / LUAR) behind a
  feature flag; never fused with Delta.
- Topic-controlled and open-set verification evals; non-English lexicons.
