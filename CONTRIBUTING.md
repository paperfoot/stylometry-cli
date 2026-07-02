# Contributing

Thanks for wanting to help. The process is short:

1. **Open an issue first** for anything beyond a small fix, so we can agree on
   the approach before you write code.
2. **Keep the honesty rules.** This tool's whole point is calibrated,
   defensible numbers: no metric may be reported on data it was fit on, and
   claims in the README must be reproducible via the scripts in `eval/`.
3. **Test it.** `cargo test` must pass; new CLI behavior needs an integration
   test in `tests/` (see `tests/verification_contracts.rs` for the pattern —
   isolated `STYLOMETRY_DATA_DIR` tempdir per test).

Especially welcome: non-English validation corpora, eval scripts for new
domains (chat, email, news), and anything in [ROADMAP.md](ROADMAP.md) marked
"still owed".
