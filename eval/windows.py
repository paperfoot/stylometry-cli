#!/usr/bin/env python3
"""Score evenly spaced excerpts of one text against one profile.

usage: windows.py BIN PROFILE FILE WORDS COUNT [EXPECT]

Cuts COUNT paragraph-aligned excerpts of about WORDS words, spread evenly
through FILE (Project Gutenberg header/footer stripped), runs
`BIN compare - --profile PROFILE --json` on each, and prints one summary
line: verdict tally, median P(same author), and the share matching EXPECT
(a verdict, e.g. same_author). Prints only numbers, never text.
"""
import json
import statistics
import subprocess
import sys


def body(raw):
    s = raw.find("*** START OF")
    if s >= 0:
        s = raw.find("\n", s) + 1
    else:
        s = 0
    e = raw.find("*** END OF", s)
    return raw[s:e if e >= 0 else len(raw)]


def paragraphs(text):
    out, cur = [], []
    for line in text.splitlines():
        if line.strip():
            cur.append(line.strip())
        elif cur:
            out.append(" ".join(cur))
            cur = []
    if cur:
        out.append(" ".join(cur))
    return [p for p in out if len(p.split()) >= 5]


def windows(paras, words, count):
    total = sum(len(p.split()) for p in paras)
    if total < words:
        return []
    starts = [int(i * (total - words) / max(count - 1, 1)) for i in range(count)]
    offsets, acc = [], 0
    for p in paras:
        offsets.append(acc)
        acc += len(p.split())
    out = []
    for s in starts:
        i = next(j for j, o in enumerate(offsets) if o + len(paras[j].split()) > s)
        buf, n = [], 0
        while i < len(paras) and n < words:
            buf.append(paras[i])
            n += len(paras[i].split())
            i += 1
        out.append("\n\n".join(buf))
    return out


def main():
    bin_, profile, path, words, count = sys.argv[1:6]
    expect = sys.argv[6] if len(sys.argv) > 6 else None
    paras = paragraphs(body(open(path, encoding="utf-8", errors="replace").read()))
    tally, ps = {}, []
    for w in windows(paras, int(words), int(count)):
        r = subprocess.run([bin_, "compare", "-", "--profile", profile, "--json"],
                           input=w, capture_output=True, text=True)
        if r.returncode != 0:
            tally["error"] = tally.get("error", 0) + 1
            continue
        d = json.loads(r.stdout)["data"]
        tally[d["verdict"]] = tally.get(d["verdict"], 0) + 1
        if d.get("p_same_author") is not None:
            ps.append(d["p_same_author"])
    n = sum(tally.values())
    hit = tally.get(expect, 0) if expect else 0
    med = f"{statistics.median(ps):.2f}" if ps else "-"
    share = f" {expect}={hit}/{n}" if expect else ""
    name = path.rsplit("/", 1)[-1]
    print(f"{name:<34} {words:>5}w x{n:<3} vs {profile:<8} medP={med}{share}  {json.dumps(tally, sort_keys=True)}")


if __name__ == "__main__":
    main()
