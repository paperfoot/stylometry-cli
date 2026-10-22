#!/usr/bin/env bash
# Leave-One-Work-Out — the honest cross-work test.
#
# For each of the six Adams books, hold the WHOLE book out, build `adams` from
# the other five, calibrate against the background and the three
# near-neighbour profiles, then score the held-out book: 1,500-word excerpts
# (share verified same_author) and the whole book (passage-median verdict).
# A different book is the real use case; chunks of one book resembling each
# other proves little.
#
# Prereq: eval/fetch_corpora.sh; `cargo build --release`. Uses its own data
# dir (the full adams profile must not be an imposter here); background text
# is rebuilt from the validate run's download cache when present.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/stylometry}"
DATA="${DATA:-$ROOT/eval/data}"
export STYLOMETRY_DATA_DIR="$DATA-lowo"
[ -d "$DATA/background-src" ] && export STYLOMETRY_BACKGROUND_SOURCE="${STYLOMETRY_BACKGROUND_SOURCE:-$DATA/background-src}"
C="$ROOT/eval/corpora"
W="python3 -I $ROOT/eval/windows.py"
TMP="$STYLOMETRY_DATA_DIR/held-in"

"$BIN" background fetch --json | jq -c '.data | {installed}'
for a in jerome wodehouse chesterton; do "$BIN" profile build "$a" --corpus "$C/$a" --force --json >/dev/null; done

ADAMS=( "$C"/adams/*.md "$ROOT"/eval/holdout/adams_mostly_harmless.md )
pass=0; total=0
for held in "${ADAMS[@]}"; do
  rm -rf "$TMP"; mkdir -p "$TMP"
  for f in "${ADAMS[@]}"; do [ "$f" = "$held" ] || cp "$f" "$TMP/"; done
  "$BIN" profile build adams --corpus "$TMP" --force --json >/dev/null
  "$BIN" calibrate adams --json >/dev/null
  res=$("$BIN" compare "$held" --profile adams --json)
  v=$(echo "$res" | jq -r '.data.verdict'); p=$(echo "$res" | jq -r '.data.p_same_author')
  total=$((total+1)); [ "$v" = "same_author" ] && pass=$((pass+1))
  printf "  whole book: %-14s P=%-5s  " "$v" "$p"
  $W "$BIN" adams "$held" 1500 10 same_author
done
rm -rf "$TMP"
echo "LOWO: $pass/$total held-out Adams works verified same_author (whole book)"
