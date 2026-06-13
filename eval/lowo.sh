#!/usr/bin/env bash
# Leave-One-Work-Out validation — the honest cross-work test.
#
# For each Adams book, hold the WHOLE book out, build the Adams profile from the
# remaining books (+ the three near-neighbour imposters), calibrate, then verify
# the held-out book. A held-out *work* (different book, different topic) is a far
# stronger test than leave-one-chunk-out within a single book, which only proves
# chunks of one book resemble each other.
#
# Prereq: eval/fetch_corpora.sh has run and `stylometry` is on PATH (or set BIN).
set -uo pipefail

BIN="${BIN:-stylometry}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export STYLOMETRY_DATA_DIR="$ROOT/eval/data_lowo"
C="$ROOT/eval/corpora"

ADAMS=( "$C"/adams/*.md "$ROOT"/eval/holdout/adams_mostly_harmless.md )
pass=0; total=0
for held in "${ADAMS[@]}"; do
  rm -rf "$STYLOMETRY_DATA_DIR" "$ROOT/eval/tmp_adams"; mkdir -p "$ROOT/eval/tmp_adams"
  for f in "${ADAMS[@]}"; do [ "$f" = "$held" ] || cp "$f" "$ROOT/eval/tmp_adams/"; done
  "$BIN" profile build adams --corpus "$ROOT/eval/tmp_adams" --json >/dev/null
  for a in jerome wodehouse chesterton; do "$BIN" profile build "$a" --corpus "$C/$a" --json >/dev/null; done
  "$BIN" calibrate adams --json >/dev/null
  res=$("$BIN" compare "$held" --profile adams --json)
  v=$(echo "$res" | jq -r '.data.verdict')
  n=$(echo "$res" | jq -r '.data.nearest_profile')
  p=$(echo "$res" | jq -r '.data.p_same_author')
  total=$((total+1)); [ "$v" = "same_author" ] && pass=$((pass+1))
  printf "  %-46s %-14s nearest=%-10s P=%s\n" "$(basename "$held")" "$v" "$n" "$p"
done
echo "LOWO: $pass/$total held-out Adams works verified same_author"
rm -rf "$ROOT/eval/tmp_adams"
# Salmon of Doubt (non-fiction) is the known register miss against a fiction-built
# profile, so the honest bar is >=5 of 6, not 6/6.
[ "$pass" -ge 5 ] || { echo "FAIL: expected >=5/6 held-out Adams works verified"; exit 1; }
