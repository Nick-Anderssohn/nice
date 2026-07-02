# Spike 10 results — atlas pressure, 2026-07-02

Ran §13 spike 10 live on `gpui-term`: release build, 60 Hz panel, 60 s
runs, streaming workload underneath. The §12 audit expectation under test:
gpui 0.2.2's atlas frees storage **only when an entire texture drops to
zero references** — could animated kitty graphics grow the polychrome
atlas unboundedly, or uploads drop frames? Atlas counters come from the
vendored-gpui `nice_poc_metrics` hunks (passive, always-on).

## Verdict: PASS — `remove()` DOES reclaim (whole-texture granularity, as the audit predicted); stale-frame drops are mandatory hygiene; one new finding: the mono glyph atlas has NO eviction in 0.2.2

## Drop mode — the §13 scenario (30 fps 512×512 animated + 12 statics, `drop_image` on stale frames)

- Polychrome: tex **+423/−419, live plateau 4 textures = 16.0 MiB**
  (tiles +1695/−1679; 1681 animation frames emitted, 1679 dropped).
- Upload traffic **1682 MiB over 60 s** (~28 MiB/s) with **no
  upload-driven frame drops**: frame p50/p95/p99 16.67/18.34/18.65,
  auto-cliffs (1.5×p50) = 1 — the lone window-open frame.
- Process 157.3 MiB steady.
- **The audit expectation is confirmed live:** four 512×512 tiles pack a
  1024×1024 texture; once all four are removed the texture frees. Reclaim
  works — at whole-texture granularity, so a renderer must drop stale
  frames promptly to let textures empty.

## Retain mode — the failure demo (never drop)

Live polychrome grows linearly to **424 textures = 1696 MiB** in 60 s
(~1.7 GiB/min); process footprint 3539 MiB (~3.5 GiB, atlas + retained
CPU-side frames). Pacing still held (p95 18.35) — the failure is memory,
not frames. **Conclusion: dropping stale animation frames is mandatory
production hygiene, and it is sufficient.**

## Glyph-atlas sweep — adversarial distinct-glyph pressure

431,855 distinct mono tiles in 60 s (unbounded codepoint sweep ×
bold/italic variants; shape-cache hit rate 4.0%, 137,540 fresh CoreText
shapes): the mono atlas grew **+368 textures = 368 MiB and never evicted
one**. **Finding: gpui 0.2.2 has no eviction path for the monochrome
glyph atlas — unbounded under adversarial glyph diversity.** But pacing
held even here (p95 17.63, auto-cliffs 4, max 82.6 ms; CPU 39.4% of one
core — the cost is CoreText shaping, not the atlas), and every realistic
run in this program used ~1 MiB of mono atlas (one texture). **Classified
as a production-hygiene item (add an eviction/LRU story for the glyph
atlas, or cap tile insertion), not a Path B blocker.**

## Evidence

CSVs (committed, 386863b): `gpui-term-sweep.csv`; `gpui-term-atlas.csv` —
drop mode and retain mode share this filename, so the committed copy
holds the **retain** run (mem series climbing to 3539 MiB); the drop-mode
numbers are from its run summary (scratchpad `s10-atlas.log`).
