#!/usr/bin/env bash
# Contemporary-fiction check on a private corpus (copyrighted: local only,
# never committed, and this prints numbers only).
#
# Builds `hoover` from ONE novel, calibrates it, then scores 3,000-word
# windows of a different Hoover novel (expect same_author) and of two other
# authors' novels (expect different_author).
#
# Prereq: eval/validate.sh has run (background + Adams/Jerome/... imposters).
# Env: AMORANA_DIR (default ~/Projects/amorana-research/text).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/stylometry}"
export STYLOMETRY_DATA_DIR="${DATA:-$ROOT/eval/data}"
A="${AMORANA_DIR:-$HOME/Projects/amorana-research/text}"
W="python3 -I $ROOT/eval/windows.py"

"$BIN" profile build hoover --corpus "$A/november-9--.txt" --force --json >/dev/null
"$BIN" calibrate hoover --json | jq -c '.data | {imposters, cross_work, bands: [.bands[] | {words, auc, holdout_accuracy}]}'
$W "$BIN" hoover "$A/hoover-saint.txt" 3000 10 same_author
$W "$BIN" hoover "$A/the-wives--a.txt" 3000 10 different_author
$W "$BIN" hoover "$A/ours-rain.txt" 3000 10 different_author
