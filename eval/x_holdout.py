#!/usr/bin/env python3
"""Held-out-quarter voice_match on X posts. Prints numbers only, never text.

usage: x_holdout.py BIN DATA_DIR WORK_DIR OWNER_JSON OTHER_JSON [IMPOSTER_JSON...]

For each held-out quarter Q of the owner's posts (newest first): build
`owner-wo-Q` from the other quarters, calibrate it, then score
- the owner's held-out posts of Q in batches of 10 (expect same_author)
- the other (unseen) author's posts in batches of 10 (expect not same_author)
Imposter JSONs (other X accounts) are built as --background profiles first.

Inputs are xmaster JSON (`xmaster timeline --user <handle> -c 200 --json`),
each fetched into its own empty folder. Posts are private or copyrighted:
keep them, and DATA_DIR, outside the repository. DATA_DIR should already
hold the background (`background fetch`); copy its bg-*.json profiles into
a fresh dir per run so imposters from one run never leak into the next.
"""
import json
import os
import re
import statistics
import subprocess
import sys

BATCH = 10


def load(path):
    d = json.load(open(path))
    return d["data"]["tweets"] if isinstance(d.get("data"), dict) else d


def quarter(t):
    y, m = int(t["date"][:4]), int(t["date"][5:7])
    return f"{y}Q{(m - 1) // 3 + 1}"


def clean(text):
    text = re.sub(r"^(@\w+\s+)+", "", text.strip())
    return text


def usable(t):
    return len(re.findall(r"\w+", re.sub(r"https?://\S+", "", clean(t["text"])))) >= 3


def batches(posts):
    posts = [p for p in posts if usable(p)]
    return ["\n---\n".join(clean(p["text"]) for p in posts[i:i + BATCH])
            for i in range(0, len(posts) - BATCH + 1, BATCH)]


def run(bin_, env, *args, stdin=None):
    r = subprocess.run([bin_, *args, "--json"], input=stdin, capture_output=True, text=True, env=env)
    return r.returncode, (json.loads(r.stdout) if r.returncode == 0 else json.loads(r.stderr or "{}"))


def score(bin_, env, profile, texts):
    tally, ps, words = {}, [], []
    for t in texts:
        code, j = run(bin_, env, "compare", "-", "--profile", profile, stdin=t)
        if code != 0:
            tally["error"] = tally.get("error", 0) + 1
            continue
        d = j["data"]
        tally[d["verdict"]] = tally.get(d["verdict"], 0) + 1
        words.append(d["words"])
        if d.get("p_same_author") is not None:
            ps.append(d["p_same_author"])
    med = f"{statistics.median(ps):.2f}" if ps else "-"
    w = f"{statistics.median(words):.0f}" if words else "-"
    return f"n={sum(tally.values())} medWords={w} medP={med} {json.dumps(tally, sort_keys=True)}"


def main():
    bin_, data, work, owner, other, *imps = sys.argv[1:]
    env = dict(os.environ, STYLOMETRY_DATA_DIR=data)
    os.makedirs(work, exist_ok=True)
    for i, path in enumerate(imps):
        name = f"x-imposter-{i + 1}"
        code, j = run(bin_, env, "profile", "build", name, "--corpus", path, "--background", "--force")
        print(f"imposter {name}: {'ok' if code == 0 else j.get('error', {}).get('message')}")
    posts = load(owner)
    other_batches = batches(load(other))
    quarters = sorted({quarter(t) for t in posts}, reverse=True)
    for q in quarters:
        held = [t for t in posts if quarter(t) == q]
        rest = [t for t in posts if quarter(t) != q]
        held_b = batches(held)
        if not held_b:
            print(f"{q}: {len(held)} posts, under one batch of {BATCH}; skipped")
            continue
        corpus = os.path.join(work, f"owner-wo-{q}.json")
        json.dump({"data": {"tweets": rest}}, open(corpus, "w"))
        name = f"owner-wo-{q.lower()}"
        code, j = run(bin_, env, "profile", "build", name, "--corpus", corpus, "--force")
        if code != 0:
            print(f"{q}: build failed: {j}")
            continue
        code, j = run(bin_, env, "calibrate", name)
        if code != 0:
            print(f"{q}: calibrate failed: {j.get('error', {}).get('message')}")
            continue
        c = j["data"]
        bands = " ".join(f"{b['words']}w:auc={b['auc']},acc={b['holdout_accuracy']}" for b in c["bands"])
        print(f"{q} held out ({len(held)} posts; profile from {len(rest)}): cross_work={c['cross_work']} {bands}")
        print(f"  owner held-out batches : {score(bin_, env, name, held_b)}")
        print(f"  other author batches   : {score(bin_, env, name, other_batches)}")
        os.remove(corpus)
        run(bin_, env, "profile", "remove", name, "--confirm")  # never an imposter for the next quarter


if __name__ == "__main__":
    main()
