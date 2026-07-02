#!/usr/bin/env python3
"""Reduce an xctrace os-signpost-interval export to present-interval percentiles.

Mirrors spikes/phase0-poc/src/harness.rs reduction:
  intervals_ms[i] = ms between consecutive Metal.Draw BEGIN (start) timestamps
  cliffs = count(interval > 16.6 ms)   (120Hz-calibrated; on 60Hz read p50/p95)
  percentiles: sorted asc; p50 = s[m//2]; p95 = s[min(m*95//100, m-1)];
               p99 = s[min(m*99//100, m-1)]
Handles xctrace export's id/ref value compression.
"""
import sys
import xml.etree.ElementTree as ET

path = sys.argv[1]
want_name = sys.argv[2] if len(sys.argv) > 2 else "Metal.Draw"

tree = ET.parse(path)
root = tree.getroot()

# xctrace export dedups values: first occurrence has id=N and text, later ones ref=N.
ids = {}

def resolve(el):
    if el is None:
        return None
    ref = el.get("ref")
    if ref is not None:
        return ids.get(ref)
    val = el.text
    eid = el.get("id")
    if eid is not None:
        ids[eid] = val
    return val

starts = []
for row in root.iter("row"):
    children = list(row)
    # register ids / resolve every cell in document order (refs are global, in-order)
    vals = [resolve(c) for c in children]
    tags = [c.tag for c in children]
    # schema col order: start, duration, layout-qualifier, name, category, subsystem, ...
    # find the name cell: first 'string' tag; category: 'category' tag
    name = None
    for c, v in zip(children, vals):
        if c.tag == "string":
            name = v
            break
    if name != want_name:
        continue
    # start time: first child, engineering-type start-time; text is ns since trace start
    start_ns = vals[0]
    if start_ns is None:
        continue
    starts.append(int(start_ns))

starts.sort()
n = len(starts)
if n < 2:
    print(f"PRESENT samples={n} p50_ms=0 p95_ms=0 p99_ms=0 fps_p50=0 cliffs=0")
    sys.exit(0)

iv = []
cliffs = 0
for a, b in zip(starts, starts[1:]):
    d = (b - a) / 1e6
    if d < 0:
        continue
    iv.append(d)
    if d > 16.6:
        cliffs += 1

iv.sort()
m = len(iv)
p50 = iv[m // 2]
p95 = iv[min(m * 95 // 100, m - 1)]
p99 = iv[min(m * 99 // 100, m - 1)]
fps = 1000.0 / p50 if p50 > 0 else 0
span_s = (starts[-1] - starts[0]) / 1e9
print(f"PRESENT samples={n} span_s={span_s:.1f} p50_ms={p50:.2f} p95_ms={p95:.2f} "
      f"p99_ms={p99:.2f} fps_p50={fps:.1f} cliffs={cliffs}")
