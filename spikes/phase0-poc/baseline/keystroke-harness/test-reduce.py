#!/usr/bin/env python3
"""test-reduce.py — self-test for reduce-latency.py (run via `make test`).

Generates a synthetic xctrace os-signpost-interval export that reproduces the
REAL export format (verified against the spike-4 trace): the 18-column schema,
id/ref value compression, <sentinel/> for empty cells, <process><pid> nesting.
Plants keystrokes with KNOWN latencies plus hazards (cursor-blink decoy draws,
a dropped/coalesced keystroke, foreign-subsystem noise), a matching keyinject
CSV and a TOC, then asserts both reduction modes recover the ground truth:

  * in-trace mode      (KeyPost signposts joined by identifier)
  * wallclock fallback (CSV wall stamps + TOC start-date)
  * --edge start       (latency shifted by exactly the present duration)
  * --list             (inventory runs and mentions both subsystems)
"""

import re
import subprocess
import sys
import tempfile
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
REDUCE = HERE / "reduce-latency.py"

# ---- ground truth ----------------------------------------------------------
WARMUP = 3
MEASURED = 20
GAP_NS = 100_000_000          # 100 ms
FIRST_KEY_NS = 1_000_000_000  # 1 s into the trace
DRAW_DUR_NS = 1_200_000       # 1.2 ms Metal.Draw-ish duration
DROPPED_SEQ = 15              # this keystroke gets no present (coalesced)
MACH_ABS_OFF = 5_000_000_000  # trace_ns = mach_abs_ns + 5 s (constant)
MACH_CONT_OFF = 9_000_000_000
TOC_ISO = "2026-07-01T12:00:00.000-07:00"
TRACE_START_EPOCH_NS = int(datetime.fromisoformat(TOC_ISO).timestamp() * 1e9)

SUB_T = "org.example.target"   # target's present subsystem
SUB_I = "nice.keyharness"      # injector

def lat_ns(seq):
    return (10 + (seq % 7)) * 1_000_000  # 10..16 ms, deterministic


SCHEMA = (
    '<schema name="os-signpost-interval"><col><mnemonic>start</mnemonic></col>'
    '<col><mnemonic>duration</mnemonic></col><col><mnemonic>layout-qualifier</mnemonic></col>'
    '<col><mnemonic>name</mnemonic></col><col><mnemonic>category</mnemonic></col>'
    '<col><mnemonic>subsystem</mnemonic></col><col><mnemonic>identifier</mnemonic></col>'
    '<col><mnemonic>process</mnemonic></col><col><mnemonic>end-process</mnemonic></col>'
    '<col><mnemonic>start-thread</mnemonic></col><col><mnemonic>end-thread</mnemonic></col>'
    '<col><mnemonic>start-message</mnemonic></col><col><mnemonic>end-message</mnemonic></col>'
    '<col><mnemonic>start-backtrace</mnemonic></col><col><mnemonic>end-backtrace</mnemonic></col>'
    '<col><mnemonic>start-emit-location</mnemonic></col><col><mnemonic>end-emit-location</mnemonic></col>'
    '<col><mnemonic>signature</mnemonic></col></schema>'
)


class Emitter:
    """Mimics xctrace's id/ref compression: first occurrence of a repeated
    value gets id=N and the payload; later occurrences are <tag ref="N"/>."""

    def __init__(self):
        self.next_id = 1
        self.seen = {}  # (tag, payload-key) -> id

    def el(self, tag, text=None, inner=None, dedup=True):
        key = (tag, text, inner)
        if dedup and key in self.seen:
            return f'<{tag} ref="{self.seen[key]}"/>'
        eid = self.next_id
        self.next_id += 1
        if dedup:
            self.seen[key] = eid
        body = inner if inner is not None else (text if text is not None else "")
        return f'<{tag} id="{eid}">{body}</{tag}>'

    def process(self, pid):
        pid_el = self.el("pid", str(pid))
        return self.el("process", inner=f"{pid_el}<device-session>x</device-session>",
                       dedup=True)

    def row(self, start, dur, name, cat, sub, ident, pid):
        cells = [
            self.el("start-time", str(start), dedup=False),
            self.el("duration", str(dur), dedup=False),
            self.el("layout-id", "0"),
            self.el("string", name),
            self.el("category", cat),
            self.el("subsystem", sub),
            self.el("os-signpost-identifier", str(ident), dedup=False),
            self.process(pid),
            self.process(pid),
            self.el("thread", "t"),
            self.el("thread", "t"),
            "<sentinel/>", "<sentinel/>", "<sentinel/>", "<sentinel/>",
            "<sentinel/>", "<sentinel/>",
            self.el("os-log-metadata", name),
        ]
        return "<row>" + "".join(cells) + "</row>"


def build_fixture(tmp, with_keyposts=True):
    em = Emitter()
    rows = []  # (start, xml)
    total = WARMUP + MEASURED
    for seq in range(1, total + 1):
        t = FIRST_KEY_NS + (seq - 1) * GAP_NS
        if with_keyposts:
            rows.append((t, em.row(t, 100_000, "KeyPost", "inject", SUB_I, seq, 4242)))
        if seq != DROPPED_SEQ:
            # answering present: END lands exactly at t + lat
            ps = t + lat_ns(seq) - DRAW_DUR_NS
            rows.append((ps, em.row(ps, DRAW_DUR_NS, "Present.Frame", "render",
                                    SUB_T, 7000 + seq, 5151)))
        if seq % 4 == 0:
            # cursor-blink decoy draw AFTER the true present, before next key
            bs = t + 60_000_000
            rows.append((bs, em.row(bs, DRAW_DUR_NS, "Present.Frame", "render",
                                    SUB_T, 8000 + seq, 5151)))
        if seq % 5 == 0:
            # foreign-subsystem noise straddling everything
            ns = t + 5_000_000
            rows.append((ns, em.row(ns, 50_000, "Noise.Work", "noise",
                                    "com.example.noise", 9000 + seq, 6161)))
    rows.sort(key=lambda r: r[0])
    xml = ('<?xml version="1.0"?>\n<trace-query-result>\n<node xpath="x">'
           + SCHEMA + "".join(x for _, x in rows) + "</node>\n</trace-query-result>\n")
    name = "export.xml" if with_keyposts else "export-nokeypost.xml"
    p = tmp / name
    p.write_text(xml)
    return p


def build_csv(tmp):
    p = tmp / "keyinject.csv"
    lines = [
        "# keyinject v1",
        "# pid=4242 n=20 warmup=3 gap_ms=100 keycode=0 char=a",
        "# mach_timebase numer=125 denom=3  (ns = ticks*numer/denom)",
        "seq,warmup,keycode,mach_abs_ticks,mach_abs_ns,mach_cont_ticks,mach_cont_ns,wall_epoch_ns",
    ]
    for seq in range(1, WARMUP + MEASURED + 1):
        t = FIRST_KEY_NS + (seq - 1) * GAP_NS
        abs_ns = t - MACH_ABS_OFF
        cont_ns = t - MACH_CONT_OFF
        wall = TRACE_START_EPOCH_NS + t
        lines.append(f"{seq},{1 if seq <= WARMUP else 0},0,"
                     f"{abs_ns * 3 // 125},{abs_ns},{cont_ns * 3 // 125},{cont_ns},{wall}")
    p.write_text("\n".join(lines) + "\n")
    return p


def build_toc(tmp):
    p = tmp / "toc.xml"
    p.write_text(f'<?xml version="1.0"?>\n<trace-toc><run number="1"><info><summary>'
                 f"<start-date>{TOC_ISO}</start-date>"
                 f"</summary></info></run></trace-toc>\n")
    return p


def expected():
    lats = sorted(lat_ns(s) / 1e6 for s in range(WARMUP + 1, WARMUP + MEASURED + 1)
                  if s != DROPPED_SEQ)
    m = len(lats)
    return {
        "matched": m,
        "p50": lats[m // 2],
        "p95": lats[min(m * 95 // 100, m - 1)],
        "p99": lats[min(m * 99 // 100, m - 1)],
    }


def run_reduce(args):
    r = subprocess.run([sys.executable, str(REDUCE)] + args,
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"reduce-latency.py failed ({r.returncode}):\n"
                             f"STDOUT:\n{r.stdout}\nSTDERR:\n{r.stderr}")
    return r.stdout, r.stderr


def parse_line(stdout):
    m = re.search(r"LATENCY method=(\S+).*matched=(\d+) dropped_overrun=(\d+).*"
                  r"p50_ms=([\d.]+) p95_ms=([\d.]+) p99_ms=([\d.]+)", stdout)
    assert m, f"no LATENCY line in output:\n{stdout}"
    return (m.group(1), int(m.group(2)), int(m.group(3)),
            float(m.group(4)), float(m.group(5)), float(m.group(6)))


def approx(a, b, tol=0.011):
    assert abs(a - b) <= tol, f"{a} != {b} (±{tol})"


def main():
    exp = expected()
    with tempfile.TemporaryDirectory() as d:
        tmp = Path(d)
        xml = build_fixture(tmp, with_keyposts=True)
        xml_nk = build_fixture(tmp, with_keyposts=False)
        csv = build_csv(tmp)
        toc = build_toc(tmp)
        present = ["--present-name", "Present.Frame", "--present-subsystem", SUB_T,
                   "--present-pid", "5151"]

        # 1. in-trace mode
        out, err = run_reduce(["--xml", str(xml), "--csv", str(csv)] + present)
        method, matched, overrun, p50, p95, p99 = parse_line(out)
        assert method == "in-trace", method
        assert matched == exp["matched"], (matched, exp["matched"])
        assert overrun == 1, f"expected 1 coalesced drop, got {overrun}"
        approx(p50, exp["p50"]); approx(p95, exp["p95"]); approx(p99, exp["p99"])
        # both synthetic mach offsets are exactly constant -> spread 0.0 µs
        assert "spread 0.0 µs" in err, f"clock-check missing/nonzero:\n{err}"
        print(f"PASS in-trace         matched={matched} p50={p50} p95={p95} p99={p99}")

        # 2. --edge start shifts every sample by exactly the draw duration
        out, _ = run_reduce(["--xml", str(xml), "--csv", str(csv), "--edge", "start"]
                            + present)
        _, matched2, _, p50s, _, _ = parse_line(out)
        assert matched2 == exp["matched"]
        approx(p50s, exp["p50"] - DRAW_DUR_NS / 1e6)
        print(f"PASS --edge start     p50={p50s}")

        # 3. wallclock fallback (no KeyPost rows in the trace)
        out, err = run_reduce(["--xml", str(xml_nk), "--csv", str(csv),
                               "--toc", str(toc)] + present)
        method, matched3, overrun3, p50f, p95f, _ = parse_line(out)
        assert method == "wallclock-fallback", method
        assert "no KeyPost signposts" in err
        assert matched3 == exp["matched"]
        assert overrun3 == 1
        approx(p50f, exp["p50"]); approx(p95f, exp["p95"])
        print(f"PASS wallclock        matched={matched3} p50={p50f} p95={p95f}")

        # 4. --list inventory
        out, _ = run_reduce(["--xml", str(xml), "--list"])
        assert SUB_T in out and SUB_I in out and "com.example.noise" in out, out
        print("PASS --list")

    print("test-reduce: ALL PASS")


if __name__ == "__main__":
    main()
