#!/usr/bin/env bash
# Reproducibly assemble the validation corpus:
#  - Adams: cleaned non-quarantined markdown from the douglas-adamiser project,
#    holding out one book (Mostly Harmless) as an unseen positive.
#  - Near-neighbour British comic/essayist authors from Project Gutenberg
#    (public domain), Gutenberg boilerplate stripped.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EVAL="$ROOT/eval"
ADAMS_SRC="${ADAMS_SRC:-$HOME/Projects/douglas-adamiser/DA/markdown}"

mkdir -p "$EVAL"/corpora/{adams,jerome,wodehouse,chesterton} "$EVAL/holdout"

if [ -d "$ADAMS_SRC" ]; then
  for f in "$ADAMS_SRC"/*.md; do
    base="$(basename "$f")"
    if [ "$base" = "Mostly_Harmless_.md" ]; then
      cp "$f" "$EVAL/holdout/adams_mostly_harmless.md"
    else
      cp "$f" "$EVAL/corpora/adams/"
    fi
  done
else
  echo "WARN: Adams source $ADAMS_SRC not found; populate eval/corpora/adams manually."
fi

fetch() { # id outfile
  curl -sS -m 90 "https://www.gutenberg.org/cache/epub/$1/pg$1.txt" \
    | awk '/\*\*\* *START OF/{p=1;next} /\*\*\* *END OF/{p=0} p' > "$2"
  printf "%8s words  %s\n" "$(wc -w < "$2")" "$2"
}
fetch 308  "$EVAL/corpora/jerome/three_men_in_a_boat.txt"
fetch 8164 "$EVAL/corpora/wodehouse/my_man_jeeves.txt"
fetch 8092 "$EVAL/corpora/chesterton/tremendous_trifles.txt"
echo "corpora ready under $EVAL/corpora"
