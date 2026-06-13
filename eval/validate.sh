#!/usr/bin/env bash
# End-to-end validation of the stylometry verifier on a real labelled corpus.
# Exits 0 only if: author separation AUC >= 0.95, a held-out Adams book is
# verified as same_author, and a near-neighbour author is rejected.
#
# Prereq: eval/corpora/{adams,jerome,wodehouse,chesterton} and
# eval/holdout/adams_mostly_harmless.md exist (see eval/fetch_corpora.sh),
# and `cargo build` has produced target/debug/stylometry.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/stylometry"
export STYLOMETRY_DATA_DIR="$ROOT/eval/data"
C="$ROOT/eval/corpora"
H="$ROOT/eval/holdout"

[ -x "$BIN" ] || { echo "FAIL: $BIN missing (run: cargo build)"; exit 1; }
for a in adams jerome wodehouse chesterton; do
  [ -d "$C/$a" ] || { echo "FAIL: missing corpus $C/$a"; exit 1; }
done
[ -f "$H/adams_mostly_harmless.md" ] || { echo "FAIL: missing held-out Adams"; exit 1; }

rm -rf "$STYLOMETRY_DATA_DIR"
for a in adams jerome wodehouse chesterton; do
  "$BIN" profile build "$a" --corpus "$C/$a" --json >/dev/null
done

AUC=$("$BIN" calibrate adams --json | jq -r '.data.auc')
echo "AUC=$AUC"
awk "BEGIN{exit !($AUC>=0.95)}" || { echo "FAIL: AUC < 0.95"; exit 1; }

POS=$("$BIN" compare "$H/adams_mostly_harmless.md" --profile adams --json | jq -r '.data.verdict')
echo "holdout_adams_verdict=$POS"
[ "$POS" = "same_author" ] || { echo "FAIL: held-out Adams not same_author"; exit 1; }

NEG=$("$BIN" compare "$C/jerome/three_men_in_a_boat.txt" --profile adams --json | jq -r '.data.verdict')
echo "jerome_verdict=$NEG"
[ "$NEG" = "different_author" ] || { echo "FAIL: Jerome not different_author"; exit 1; }

echo "VALIDATION PASS"
