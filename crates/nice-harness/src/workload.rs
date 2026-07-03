//! Deterministic synthetic "Claude-streaming" workload generator — the renderer
//! stressor the `term-perf` self-test floods a pane with.
//!
//! Ported verbatim in *shape* from the phase-0 spike's `harness::Workload`
//! (`spikes/phase0-poc/src/harness.rs` §E): a seeded xorshift64\* RNG driving a
//! weighted content mix (SGR-heavy token runs, line-redraw/reflow idioms, long
//! wrapped lines, unicode/box glyphs, plain ASCII). The mix is what makes the
//! stream a genuine renderer stressor rather than a flat character dump — it
//! exercises SGR churn, cursor save/restore + cursor-up overwrites, wide/box
//! glyphs, and reflow, the same content classes a live `claude` session emits.
//!
//! Deterministic per seed, so a perf regression is reproducible: the same seed
//! yields byte-identical output across runs and machines. Pure data — no gpui,
//! no I/O — so it unit-tests without a window (see the tests below).

/// Tuning for the generated stream. The defaults mirror the spike's
/// `WorkloadProfile::default` (seed 42, ~500 KB/s target when paced, 16..512-byte
/// bursts) — the profile the perf gate's pin baseline (16.67/17.95 ms) was
/// measured against.
#[derive(Clone, Copy, Debug)]
pub struct WorkloadProfile {
    /// RNG seed — fixes the entire stream. `0` is remapped to a nonzero constant.
    pub seed: u64,
    /// Paced feed target, bytes/second. Advisory: the *shape* is fixed by the
    /// mix; the feeder paces writes to approximate this rate.
    pub bytes_per_sec: usize,
    /// Inclusive-lower / exclusive-upper byte size of one burst chunk.
    pub burst_chunk: (usize, usize),
}

impl Default for WorkloadProfile {
    fn default() -> Self {
        WorkloadProfile {
            seed: 42,
            bytes_per_sec: 500_000,
            burst_chunk: (16, 512),
        }
    }
}

/// Deterministic xorshift64\* — identical stream per seed across machines. Ported
/// from the spike so the perf workload is byte-reproducible.
pub struct Rng(u64);

impl Rng {
    /// Seed the RNG. `0` is remapped (xorshift is degenerate at zero state).
    pub fn new(seed: u64) -> Self {
        Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    /// Next 64-bit value (xorshift64\* — the spike's exact constants).
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `[lo, hi)` (returns `lo` if `hi <= lo`).
    #[inline]
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() as usize) % (hi - lo)
    }
}

/// The renderer-stressor content generator (spike Harness §E.2).
pub struct Workload {
    rng: Rng,
    prof: WorkloadProfile,
}

impl Workload {
    /// A generator for `prof`.
    pub fn new(prof: WorkloadProfile) -> Self {
        Workload {
            rng: Rng::new(prof.seed),
            prof,
        }
    }

    /// The active profile (the feeder reads `bytes_per_sec` to pace itself).
    pub fn profile(&self) -> WorkloadProfile {
        self.prof
    }

    /// Produce ONE burst chunk. Weighted exactly as the spike: 40% SGR-heavy,
    /// 30% line-redraw/reflow, 15% long wrapped lines, 10% unicode/box, 5% plain.
    pub fn next_chunk(&mut self) -> Vec<u8> {
        let pick = self.rng.range(0, 100);
        let target = self.rng.range(self.prof.burst_chunk.0, self.prof.burst_chunk.1);
        let mut out = Vec::with_capacity(target + 32);
        match pick {
            0..=39 => self.sgr_heavy(&mut out, target),
            40..=69 => self.line_redraw(&mut out, target),
            70..=84 => self.long_line(&mut out, target),
            85..=94 => self.unicode_box(&mut out, target),
            _ => self.plain_ascii(&mut out, target),
        }
        out
    }

    /// A deterministic byte stream of at least `bytes` total (truncated to
    /// exactly `bytes`). The perf feeder pre-generates one large buffer with this
    /// and feeds sequential slices cyclically, so no generation cost lands on the
    /// hot feed path.
    pub fn stream(&mut self, bytes: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes + 1024);
        while out.len() < bytes {
            out.extend_from_slice(&self.next_chunk());
        }
        out.truncate(bytes);
        out
    }

    fn sgr_heavy(&mut self, out: &mut Vec<u8>, target: usize) {
        while out.len() < target {
            let (r, g, b) = (
                self.rng.range(0, 256),
                self.rng.range(0, 256),
                self.rng.range(0, 256),
            );
            out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
            if self.rng.range(0, 2) == 0 {
                out.extend_from_slice(b"\x1b[1m");
            }
            if self.rng.range(0, 3) == 0 {
                out.extend_from_slice(b"\x1b[4m");
            }
            let words = self.rng.range(2, 8);
            for _ in 0..words {
                out.extend_from_slice(b"token ");
            }
            out.extend_from_slice(b"\x1b[0m");
        }
        out.extend_from_slice(b"\r\n");
    }

    fn line_redraw(&mut self, out: &mut Vec<u8>, target: usize) {
        // The streaming-rewrite idiom: CR + clear-line, re-emit the same line.
        let reps = (target / 24).max(1);
        for i in 0..reps {
            out.extend_from_slice(b"\r\x1b[2K");
            out.extend_from_slice(format!("working... {}%", i % 100).as_bytes());
        }
        // Cursor-up overwrite of an N-line status block + save/restore.
        out.extend_from_slice(b"\x1b7\x1b[3A\x1b[2Kspinner\x1b8");
    }

    fn long_line(&mut self, out: &mut Vec<u8>, target: usize) {
        let cols = self.rng.range(200, 2000.min(200 + target * 4).max(201));
        for i in 0..cols {
            out.push(b'a' + (i % 26) as u8);
        }
        out.extend_from_slice(b"\r\n");
    }

    fn unicode_box(&mut self, out: &mut Vec<u8>, target: usize) {
        let glyphs = ["─", "│", "┌", "┐", "└", "┘", "█", "🚀", "é", "中", "🧩"];
        while out.len() < target {
            let g = glyphs[self.rng.range(0, glyphs.len())];
            out.extend_from_slice(g.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }

    fn plain_ascii(&mut self, out: &mut Vec<u8>, target: usize) {
        while out.len() < target {
            out.extend_from_slice(b"the quick brown fox jumps over the lazy dog ");
        }
        out.extend_from_slice(b"\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_per_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn rng_zero_seed_is_remapped_not_stuck() {
        // xorshift is degenerate at zero state; the remap must produce motion.
        let mut r = Rng::new(0);
        let first = r.next_u64();
        assert_ne!(first, 0);
        assert_ne!(first, r.next_u64());
    }

    #[test]
    fn rng_range_is_bounded_and_degenerate_safe() {
        let mut r = Rng::new(7);
        for _ in 0..1000 {
            let v = r.range(3, 9);
            assert!((3..9).contains(&v));
        }
        // hi <= lo returns lo without dividing by zero.
        assert_eq!(r.range(5, 5), 5);
        assert_eq!(r.range(9, 2), 9);
    }

    #[test]
    fn stream_is_exact_length_and_deterministic() {
        let a = Workload::new(WorkloadProfile::default()).stream(100_000);
        let b = Workload::new(WorkloadProfile::default()).stream(100_000);
        assert_eq!(a.len(), 100_000);
        assert_eq!(a, b, "same seed must yield byte-identical streams");
    }

    #[test]
    fn stream_exercises_the_full_content_mix() {
        // A long enough stream must hit every branch: SGR truecolor, the
        // line-redraw clear-line idiom, and a unicode/box glyph.
        let s = Workload::new(WorkloadProfile::default()).stream(200_000);
        assert!(s.windows(7).any(|w| w == b"\x1b[38;2;"), "SGR-heavy runs present");
        assert!(s.windows(5).any(|w| w == b"\r\x1b[2K"), "line-redraw idiom present");
        assert!(
            s.windows(3).any(|w| w == "█".as_bytes()),
            "unicode/box glyphs present"
        );
    }

    #[test]
    fn next_chunk_output_is_whole_utf8() {
        // Each burst writes only complete codepoints (whole glyphs) — the feeder
        // relies on this so the only possible split is the buffer's final
        // `truncate` seam, which the VT parser tolerates across a cycle wrap.
        let mut w = Workload::new(WorkloadProfile::default());
        for _ in 0..2000 {
            let chunk = w.next_chunk();
            assert!(
                std::str::from_utf8(&chunk).is_ok(),
                "a burst chunk split a codepoint"
            );
        }
    }
}
