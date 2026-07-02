#!/usr/bin/env python3
"""Extract Metal.Draw signpost-interval DURATIONS (and a few sibling signposts)
from an xctrace os-signpost-interval export. Distinguishes 'draw is slow' from
'draw is cheap but invalidation-throttled'."""
import sys, xml.etree.ElementTree as ET

path = sys.argv[1]
tree = ET.parse(path); root = tree.getroot()
ids = {}

def resolve(el):
    ref = el.get("ref")
    if ref is not None:
        return ids.get(ref)
    val = el.text
    eid = el.get("id")
    if eid is not None:
        ids[eid] = val
    return val

# collect durations per signpost name
from collections import defaultdict
durs = defaultdict(list)
for row in root.iter("row"):
    children = list(row)
    vals = [resolve(c) for c in children]
    # col0 = start (start-time ns), col1 = duration (ns), name = first 'string' child
    dur = None
    for c, v in zip(children, vals):
        if c.get is not None and c.tag == "duration":
            dur = v
            break
    # fallback: second column is duration by schema order
    if dur is None and len(vals) > 1:
        dur = vals[1]
    name = None
    for c, v in zip(children, vals):
        if c.tag == "string":
            name = v; break
    if name is None or dur is None:
        continue
    try:
        durs[name].append(int(dur) / 1e6)  # ns -> ms
    except (ValueError, TypeError):
        continue

def pct(xs, p):
    s = sorted(xs); m = len(s)
    return s[min(m * p // 100, m - 1)]

for name in ("Metal.Draw", "Metal.Encode", "Metal.Commit", "Metal.CurrentDrawable",
             "Metal.BuildDrawData", "Parser.Parse"):
    xs = durs.get(name)
    if not xs:
        continue
    print(f"{name:24s} n={len(xs):5d}  dur_ms p50={pct(xs,50):7.3f} p95={pct(xs,95):7.3f} "
          f"p99={pct(xs,99):7.3f} max={max(xs):7.3f} sum={sum(xs):8.1f}")
