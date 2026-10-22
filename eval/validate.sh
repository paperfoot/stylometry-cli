#!/usr/bin/env bash
# End-to-end validation of the v0.3 verifier on a real labelled corpus.
#
# Builds the background (~40 Gutenberg authors), an Adams profile from five
# books and three near-neighbour profiles (Jerome, Wodehouse, Chesterton),
# calibrates Adams per length band, then checks at 800/1500/3000 words:
#   - held-out Adams (Mostly Harmless, never seen): same_author >= 90%
#   - Jerome (Three Men in a Boat, a near neighbour):  same_author <= 10%
# Unseen authors (Saki, Austen) and a new Jerome book are in eval/probe.sh.
#
# Prereq: eval/fetch_corpora.sh has run; `cargo build --release`.
# Env: BIN (binary), DATA (data dir, reused so the background downloads once),
#      STYLOMETRY_BACKGROUND_SOURCE (offline folder of <id>.txt files).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/stylometry}"
export STYLOMETRY_DATA_DIR="${DATA:-$ROOT/eval/data}"
C="$ROOT/eval/corpora"
H="$ROOT/eval/holdout/adams_mostly_harmless.md"
W="python3 -I $ROOT/eval/windows.py"

[ -x "$BIN" ] || { echo "FAIL: $BIN missing (run: cargo build --release)"; exit 1; }
for a in adams jerome wodehouse chesterton; do
  [ -d "$C/$a" ] || { echo "FAIL: missing corpus $C/$a (run eval/fetch_corpora.sh)"; exit 1; }
done
[ -f "$H" ] || { echo "FAIL: missing $H"; exit 1; }

"$BIN" background fetch --json | jq -c '.data | {built: (.built|length), skipped: (.skipped|length), failed: (.failed|length), installed}'
for a in adams jerome wodehouse chesterton; do
  "$BIN" profile build "$a" --corpus "$C/$a" --force --json >/dev/null
done
"$BIN" calibrate adams --json | jq -r '.data | "imposters=\(.imposters) cross_work=\(.cross_work)", (.bands[] | "  \(.words)w AUC=\(.auc) holdout_acc=\(.holdout_accuracy) brier=\(.holdout_brier) c@1=\(.c_at_1) pos/neg=\(.positives)/\(.negatives)")'

fail=0
check() { # line expect min_share(0-1) — "x/n" share must be >= min (or <= for max_)
  echo "$1"
  local frac; frac=$(echo "$1" | sed -E "s/.*$2=([0-9]+)\/([0-9]+).*/\1 \2/")
  awk -v f="$frac" -v op="$3" -v t="$4" 'BEGIN{split(f,a," "); s=a[1]/a[2]; exit !((op=="ge" && s>=t) || (op=="le" && s<=t))}' \
    || { echo "  FAIL: share of $2 not $3 $4"; fail=1; }
}
for n in 800 1500 3000; do
  check "$($W "$BIN" adams "$H" "$n" 20 same_author)" same_author ge 0.9
  check "$($W "$BIN" adams "$C/jerome/three_men_in_a_boat.txt" "$n" 20 same_author)" same_author le 0.1
done
[ "$fail" = 0 ] && echo "VALIDATION PASS" || { echo "VALIDATION FAIL"; exit 1; }
