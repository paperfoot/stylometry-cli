#!/usr/bin/env bash
# Unseen-author probe: texts whose authors the verifier has never seen.
#   - Saki (Beasts and Super-Beasts, #269) and Austen (Pride and Prejudice,
#     #1342) vs adams and vs jerome: false same_author should be <= 10% at
#     >= 800 words.
#   - Jerome's *Idle Thoughts of an Idle Fellow* (#849), a different book
#     from the one the jerome profile was built on: same_author expected.
# Excerpts of 300/800/1500/6000 words, evenly spread through each book.
#
# Prereq: eval/validate.sh has run (background, adams and jerome profiles).
# Downloads go to their own folder under the data dir.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/stylometry}"
export STYLOMETRY_DATA_DIR="${DATA:-$ROOT/eval/data}"
SRC="$STYLOMETRY_DATA_DIR/probe-src"
W="python3 -I $ROOT/eval/windows.py"
mkdir -p "$SRC"

for id in 269 1342 849; do
  [ -s "$SRC/$id.txt" ] || { curl -sS -m 120 -A "stylometry-cli eval" \
    "https://www.gutenberg.org/cache/epub/$id/pg$id.txt" -o "$SRC/$id.txt"; sleep 1; }
done
"$BIN" calibrate jerome --json | jq -c '.data | {profile, cross_work, bands: [.bands[] | {words, auc, holdout_accuracy}]}'

for n in 300 800 1500 6000; do
  c=20; [ "$n" = 6000 ] && c=8
  for p in adams jerome; do
    $W "$BIN" "$p" "$SRC/269.txt" "$n" "$c" same_author
    $W "$BIN" "$p" "$SRC/1342.txt" "$n" "$c" same_author
  done
  $W "$BIN" jerome "$SRC/849.txt" "$n" "$c" same_author
  $W "$BIN" adams "$SRC/849.txt" "$n" "$c" same_author
done
