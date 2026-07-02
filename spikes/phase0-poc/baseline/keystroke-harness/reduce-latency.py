#!/usr/bin/env python3
"""reduce-latency.py — join keyinject timestamps with an xctrace os-signpost
export and produce keyDown→present latency percentiles + a histogram.

Companion to keyinject.swift (spikes 4b/5). Parses the SAME export the other
baseline reducers consume:

    xcrun xctrace export --input run.trace \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]'

TIMEBASE / CLOCK-DOMAIN HANDLING (the whole point of this script):
  * Export `start` values are ns since trace start (verified on the spike-4
    export: <start-time fmt="00:12.176.242">12176242416</start-time>).
  * PRIMARY mode ("in-trace"): the injector emits its own KeyPost signpost
    interval around every post (subsystem nice.keyharness, identifier == seq).
    When the recording was made with --all-processes, KeyPost rows and the
    target's present rows sit in the SAME table and timebase, so latency is a
    plain subtraction — zero clock-domain conversion. This is the mode to use.
  * FALLBACK mode ("wallclock"): an --attach recording only contains the
    TARGET's signposts (the spike-4 TOC shows target-pid="SINGLE"), so KeyPost
    is absent. Then keyDown times come from the CSV's wall_epoch_ns column,
    anchored to the trace via the TOC's <start-date> (ISO8601). start-date has
    only MILLISECOND precision, so this mode carries a ±1–2 ms systematic
    uncertainty — flagged in the output; prefer --all-processes.
  * CROSS-CHECK: when both the CSV and in-trace KeyPost rows are available the
    script prints, per mach clock (absolute & continuous), the spread of
    (trace_ns − mach_ns) across samples. The near-constant one is the clock the
    trace timebase follows; a large spread on both means the join is broken.
    (mach ns = ticks*numer/denom, precomputed by keyinject into the CSV.)

MATCHING (single-in-flight, NOTES.md §3 / Harness §C): keys are posted with a
gap far above worst-case latency, so each keyDown is answered before the next.
For each keyDown t: take the first present signpost with start > t; latency is
measured to the interval END (start+duration ≈ CPU-side present submitted —
for SwiftTerm, present(drawable) is called inside Metal.Draw; pass
--edge start to use the interval begin instead). A sample is DROPPED when the
matched present lands after the next keyDown (coalesced/unanswered) or when no
present follows (tail). Optional --gate-name: require an intermediate signpost
(e.g. a damage/echo marker) between keyDown and the matched present — needed
for continuously-presenting targets (a RAF-driven gpui-term presents every
frame whether or not the echo landed, so "next present" alone would just
sample the frame phase; see RUN.md).

Percentile method mirrors harness.rs / the sibling reducers:
sorted asc; p50 = s[m//2]; pXX = s[min(m*XX//100, m-1)].
"""

import argparse
import sys
import xml.etree.ElementTree as ET
from bisect import bisect_right
from collections import defaultdict
from datetime import datetime


# ---------------------------------------------------------------------------
# xctrace export parsing (id/ref value compression, schema-ordered columns)
# ---------------------------------------------------------------------------

def parse_export(path):
    """Return (schema_cols, rows) where rows are lists of DEREFERENCED cell
    elements in schema column order. xctrace dedups repeated values: the first
    occurrence carries id=N (and the payload), later ones are <tag ref="N"/>.
    ids are global and refs always point backward, so one pre-order
    registration pass suffices."""
    tree = ET.parse(path)
    root = tree.getroot()

    ids = {}
    for el in root.iter():
        eid = el.get("id")
        if eid is not None:
            ids[eid] = el

    def deref(el):
        ref = el.get("ref")
        return ids.get(ref) if ref is not None else el

    schemas = root.iter("schema")
    cols = None
    for schema in schemas:
        cols = [c.findtext("mnemonic") for c in schema.findall("col")]
        break  # one table per export in our usage
    if cols is None:
        sys.exit("reduce-latency: no <schema> in export — wrong --xpath or empty table?")

    rows = []
    for row in root.iter("row"):
        cells = [deref(c) for c in row]
        if len(cells) != len(cols):
            # xctrace emits every column (empty ones as <sentinel/>); a
            # mismatch means a format change — warn once, index defensively.
            print(f"reduce-latency: WARNING row has {len(cells)} cells, schema has "
                  f"{len(cols)} cols — format drift?", file=sys.stderr)
        rows.append(cells)
    return cols, rows, deref


def cell_text(cell):
    if cell is None or cell.tag == "sentinel":
        return None
    return cell.text


def cell_int(cell):
    t = cell_text(cell)
    try:
        return int(t)
    except (TypeError, ValueError):
        return None


def cell_pid(cell, deref):
    """Extract the pid from a <process> cell (its <pid> child may itself be a ref)."""
    if cell is None or cell.tag == "sentinel":
        return None
    p = cell.find("pid")
    if p is None:
        return None
    return cell_int(deref(p))


def extract_signposts(cols, rows, deref):
    """Rows -> dicts {start, dur, name, category, subsystem, identifier, pid}."""
    idx = {name: i for i, name in enumerate(cols)}
    need = ["start", "duration", "name", "category", "subsystem", "identifier", "process"]
    for n in need:
        if n not in idx:
            sys.exit(f"reduce-latency: schema is missing expected column '{n}' "
                     f"(have: {cols}) — not an os-signpost-interval export?")
    out = []
    for cells in rows:
        def cget(mnemonic):
            j = idx[mnemonic]
            return cells[j] if j < len(cells) else None
        start = cell_int(cget("start"))
        if start is None:
            continue
        out.append({
            "start": start,
            "dur": cell_int(cget("duration")) or 0,
            "name": cell_text(cget("name")),
            "category": cell_text(cget("category")),
            "subsystem": cell_text(cget("subsystem")),
            "identifier": cell_int(cget("identifier")),
            "pid": cell_pid(cget("process"), deref),
        })
    return out


def match(sp, name=None, subsystem=None, category=None, pid=None):
    if name is not None and sp["name"] != name:
        return False
    if subsystem is not None and sp["subsystem"] != subsystem:
        return False
    if category is not None and sp["category"] != category:
        return False
    if pid is not None and sp["pid"] != pid:
        return False
    return True


# ---------------------------------------------------------------------------
# CSV / TOC parsing
# ---------------------------------------------------------------------------

def parse_csv(path):
    """keyinject CSV -> {seq: row-dict}, warmup-seq set."""
    rows, warm = {}, set()
    header = None
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if header is None:
                header = line.split(",")
                continue
            vals = dict(zip(header, line.split(",")))
            seq = int(vals["seq"])
            rows[seq] = {k: int(v) for k, v in vals.items()}
            if int(vals["warmup"]):
                warm.add(seq)
    if not rows:
        sys.exit(f"reduce-latency: no data rows in {path}")
    return rows, warm


def toc_start_epoch_ns(path):
    root = ET.parse(path).getroot()
    sd = root.find(".//start-date")
    if sd is None or not sd.text:
        sys.exit(f"reduce-latency: no <start-date> in {path} — is it an "
                 f"'xctrace export --toc' output?")
    return int(datetime.fromisoformat(sd.text).timestamp() * 1e9), sd.text


# ---------------------------------------------------------------------------
# Reduction
# ---------------------------------------------------------------------------

def pct(sorted_xs, p):
    m = len(sorted_xs)
    return sorted_xs[min(m * p // 100, m - 1)]


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--xml", required=True, help="xctrace os-signpost-interval export")
    ap.add_argument("--csv", help="keyinject CSV (post timestamps)")
    ap.add_argument("--present-name", help="target's present signpost name (e.g. Metal.Draw)")
    ap.add_argument("--present-subsystem", help="e.g. org.tirania.SwiftTerm")
    ap.add_argument("--present-category", help="e.g. MetalProfile")
    ap.add_argument("--present-pid", type=int, help="restrict presents to this pid "
                    "(recommended with --all-processes recordings)")
    ap.add_argument("--inject-name", default="KeyPost")
    ap.add_argument("--inject-subsystem", default="nice.keyharness")
    ap.add_argument("--inject-category", default="inject")
    ap.add_argument("--gate-name", help="optional signpost that must occur between "
                    "keyDown and the matched present (damage/echo marker for "
                    "continuously-presenting targets)")
    ap.add_argument("--gate-subsystem")
    ap.add_argument("--gate-category")
    ap.add_argument("--edge", choices=["end", "start"], default="end",
                    help="present-interval edge to measure to (default: end = "
                    "start+duration ≈ CPU present submitted)")
    ap.add_argument("--toc", help="'xctrace export --toc' XML — enables the "
                    "wall-clock FALLBACK when KeyPost rows are absent")
    ap.add_argument("--force-fallback", action="store_true",
                    help="use the wall-clock path even if KeyPost rows exist")
    ap.add_argument("--hist-bin-ms", type=float, default=2.0)
    ap.add_argument("--out-csv", help="write per-sample latencies (seq,latency_ms)")
    ap.add_argument("--list", action="store_true",
                    help="inventory the export's signposts (subsystem/category/"
                    "name/pid + counts) and exit — use to discover the "
                    "gpui-term signpost names")
    args = ap.parse_args()

    cols, raw_rows, deref = parse_export(args.xml)
    sps = extract_signposts(cols, raw_rows, deref)

    if args.list:
        agg = defaultdict(lambda: [0, []])
        for sp in sps:
            k = (sp["subsystem"], sp["category"], sp["name"], sp["pid"])
            agg[k][0] += 1
            agg[k][1].append(sp["dur"])
        print(f"{'subsystem':32s} {'category':16s} {'name':24s} {'pid':>7s} "
              f"{'count':>6s} {'dur_p50_ms':>10s}")
        for (sub, cat, name, pid), (n, durs) in sorted(agg.items(), key=lambda kv: -kv[1][0]):
            durs.sort()
            print(f"{str(sub):32s} {str(cat):16s} {str(name):24s} {str(pid):>7s} "
                  f"{n:6d} {pct(durs, 50) / 1e6:10.3f}")
        return

    if not args.present_name:
        ap.error("--present-name is required (or use --list to discover names)")

    presents = sorted((sp for sp in sps if match(
        sp, args.present_name, args.present_subsystem,
        args.present_category, args.present_pid)), key=lambda s: s["start"])
    if not presents:
        sys.exit(f"reduce-latency: 0 present signposts matched "
                 f"name={args.present_name} subsystem={args.present_subsystem} "
                 f"category={args.present_category} pid={args.present_pid}. "
                 f"Run with --list to see what the trace contains.")

    gates = None
    if args.gate_name:
        gates = sorted((sp for sp in sps if match(
            sp, args.gate_name, args.gate_subsystem, args.gate_category)),
            key=lambda s: s["start"])
        if not gates:
            sys.exit(f"reduce-latency: --gate-name {args.gate_name} matched 0 signposts")

    csv_rows, warm = ({}, set())
    if args.csv:
        csv_rows, warm = parse_csv(args.csv)

    # ---- keyDown times in trace-ns ----------------------------------------
    injects = sorted((sp for sp in sps if match(
        sp, args.inject_name, args.inject_subsystem, args.inject_category)),
        key=lambda s: s["start"])

    method = None
    keys = []  # (seq, t_trace_ns)
    if injects and not args.force_fallback:
        method = "in-trace"
        for sp in injects:
            seq = sp["identifier"]
            if seq in warm:
                continue
            if csv_rows and seq not in csv_rows:
                continue  # stray signpost from another run
            keys.append((seq, sp["start"]))
        if not csv_rows:
            print("reduce-latency: WARNING no --csv given — cannot exclude warmup "
                  "keystrokes or cross-check clocks", file=sys.stderr)
        # cross-check: trace time vs each mach clock (offset must be ~constant)
        if csv_rows:
            for clk in ("mach_abs_ns", "mach_cont_ns"):
                offs = [t - csv_rows[seq][clk] for seq, t in keys if seq in csv_rows]
                if offs:
                    spread = (max(offs) - min(offs)) / 1e3
                    print(f"clock-check: trace_ns - {clk}: spread {spread:.1f} µs "
                          f"over {len(offs)} samples (near-zero spread = trace "
                          f"timebase follows this clock)", file=sys.stderr)
    else:
        method = "wallclock-fallback"
        if not injects:
            print("reduce-latency: no KeyPost signposts in the trace (attach-mode "
                  "recording only captures the target's signposts — record with "
                  "--all-processes for the primary path). Falling back to "
                  "wall-clock correlation.", file=sys.stderr)
        if not args.csv:
            sys.exit("reduce-latency: fallback needs --csv")
        if not args.toc:
            sys.exit("reduce-latency: fallback needs --toc (xcrun xctrace export "
                     "--input run.trace --toc > toc.xml) for the trace start-date")
        t0_ns, iso = toc_start_epoch_ns(args.toc)
        print(f"reduce-latency: WARNING wall-clock fallback via TOC start-date "
              f"{iso} — millisecond precision only; treat results as ±1–2 ms",
              file=sys.stderr)
        for seq, row in sorted(csv_rows.items()):
            if seq in warm:
                continue
            t = row["wall_epoch_ns"] - t0_ns
            if t >= 0:
                keys.append((seq, t))

    keys.sort(key=lambda kt: kt[1])
    if not keys:
        sys.exit("reduce-latency: 0 usable keyDown timestamps")

    # ---- match each keyDown to its answering present -----------------------
    p_starts = [p["start"] for p in presents]
    g_starts = [g["start"] for g in gates] if gates else None

    samples = []           # (seq, latency_ms)
    drop_overrun = 0       # matched present lands after the next keyDown
    drop_tail = 0          # nothing after this keyDown at all
    drop_nogate = 0
    for k, (seq, t) in enumerate(keys):
        t_next = keys[k + 1][1] if k + 1 < len(keys) else None
        lo = t
        if gates:
            gj = bisect_right(g_starts, t)
            if gj >= len(gates) or (t_next is not None and g_starts[gj] >= t_next):
                drop_nogate += 1
                continue
            lo = g_starts[gj]
        pj = bisect_right(p_starts, lo)
        if pj >= len(presents):
            drop_tail += 1
            continue
        p = presents[pj]
        if t_next is not None and p["start"] >= t_next:
            drop_overrun += 1
            continue
        edge_ns = p["start"] + (p["dur"] if args.edge == "end" else 0)
        samples.append((seq, (edge_ns - t) / 1e6))

    if not samples:
        sys.exit(f"reduce-latency: matched 0 samples (keys={len(keys)}, "
                 f"presents={len(presents)}, overrun={drop_overrun}, "
                 f"tail={drop_tail}, nogate={drop_nogate})")

    lats = sorted(l for _, l in samples)
    m = len(lats)
    p50, p95, p99 = lats[m // 2], pct(lats, 95), pct(lats, 99)
    print(f"LATENCY method={method} edge={args.edge} present={args.present_name} "
          f"keys={len(keys)} matched={m} dropped_overrun={drop_overrun} "
          f"dropped_tail={drop_tail} dropped_nogate={drop_nogate} "
          f"p50_ms={p50:.2f} p95_ms={p95:.2f} p99_ms={p99:.2f} "
          f"min_ms={lats[0]:.2f} max_ms={lats[-1]:.2f}")

    # ---- histogram ----------------------------------------------------------
    bin_ms = args.hist_bin_ms
    hist = defaultdict(int)
    for l in lats:
        hist[int(l // bin_ms)] += 1
    peak = max(hist.values())
    print(f"histogram (bin={bin_ms:g} ms):")
    for b in range(min(hist), max(hist) + 1):
        n = hist.get(b, 0)
        bar = "#" * max(1, round(n * 50 / peak)) if n else ""
        print(f"  [{b * bin_ms:6.1f}-{(b + 1) * bin_ms:6.1f}) {n:5d} {bar}")

    if args.out_csv:
        with open(args.out_csv, "w") as f:
            f.write("seq,latency_ms\n")
            for seq, l in samples:
                f.write(f"{seq},{l:.4f}\n")
        print(f"per-sample latencies -> {args.out_csv}")


if __name__ == "__main__":
    main()
