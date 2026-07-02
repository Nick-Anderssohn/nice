# Spike 3 results — Path A `txn` + GPUI transactional present, 2026-07-01

Ran §13 spike 3 live: does making GPUI *also* present transactionally collapse
Path A's residual p95≈31 ms dual-stack tail? The audit's counter-case was that
the "~1-line GPUI-side fix was never tested" — so the 31 ms tail might be a
present-mode artifact, not a structural cost. Same machine, same 60 Hz panel,
same §10 harness/workload, 18 s runs.

## Verdict: NEGATIVE — the tail is structural, not a present-mode artifact

Both-sides-transactional is **identical to the txn control within noise**. The
"~1-line GPUI-side fix might collapse Path A's tail" hypothesis is **REFUTED on
live timings**. The dual-stack p95≈31 ms tail is a structural property of two
Metal stacks in one window, and it is the only measured A-vs-B quality
differentiator — so this result **strengthens Path B**.

## Mechanism

- gpui 0.2.2 vendored at `vendor/gpui-0.2.2` (gitignored; reproduce =
  `vendor-gpui.sh` + `gpui-0.2.2-nice.patch`), wired via `[patch.crates-io]`.
- Env `NICE_POC_GPUI_TXN=1` forces `presents_with_transaction` at renderer init
  AND ORs it into every `set_presents_with_transaction` call, so window.rs's
  transient `false` resets become no-ops. Verified wired by patch inspection.
- **Deviation from the plan:** a separate env var, NOT `NICE_POC_PRESENT=txn`
  (which selects the *SwiftTerm-side* txn) — the two sides must be
  independently switchable.

## Numbers (both arms: `NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn`, replicating §10)

| Metric | Arm A — SwiftTerm txn only (control) | Arm B — + `NICE_POC_GPUI_TXN=1` |
|---|---|---|
| Term present p50/p95 (ms) | 18.34 / 31.07 (cliffs 971) | 18.32 / 31.10 (cliffs 978) |
| GPUI composite p50/p95 (ms) | 18.05 / 31.21 (cliffs 1089) | 17.99 / 31.33 (cliffs 1087) |
| Seam keystroke p50/p95/p99 (ms) | 3.86 / 16.59 / 17.23 | — |
| phys_footprint idle/steady/peak (MiB) | 71.5 / 152.4 / 157.0 | — / 162.1 / 166.8 |
| `present_now()` wall p50/max (ms) | 3.51 / 17.63 (n=1196) | 3.69 / 17.85 |

(Cliff counts remain the known 120 Hz-calibrated over-count; read percentiles.)

## What this retires

The §13 measurement-program correction "don't lean on Path A's 31 ms tail as
A's ceiling (the GPUI-side `presents_with_transaction` ~1-line fix was never
tested)" is now closed: the fix WAS tested, and the tail did not move. Path A's
measured ceiling on this panel stands at p50 ~18.3 / p95 ~31 ms under
continuous load, vs Path B single-stack p50 16.67 / p95 ~16.8 ms.
