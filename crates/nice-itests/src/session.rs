//! Fixture-session builders + capture-file readers.
//!
//! These reuse the landed live-suite patterns rather than inventing a pty-free
//! stand-in (real ptys under plain `cargo test` are already proven by
//! `nice-term-core/tests/`):
//!
//! * [`cat_fixture_spec`] pipes a deterministic byte stream into the grid via
//!   `cat <file>` with `ZDOTDIR` pointed at an empty dir — the `term-render`
//!   scenario's pattern (no user zsh rc pollutes the grid).
//! * [`capture_tee_spec`] captures what the view **writes to the pty** verbatim
//!   into a file via `sh -c 'stty raw -echo; exec tee <cap>'` — the `input-live`
//!   scenario's pattern (raw mode: no line discipline / echo, so encoder bytes
//!   land byte-exact).
//! * [`silent_command_spec`] runs a command that produces no output (e.g.
//!   `sleep`) — for timing tests that must keep a pane silent past a deadline.
//!
//! Pure `std` + `nice_term_core::SpawnSpec` — no gpui. A caller passes the spec
//! to `nice_term_view::TerminalSessionHandle::spawn`. The capture readers poll a
//! real file on the real clock (the pty child + `tee` run on OS threads, outside
//! the simulated dispatcher), so they **poll for readiness with a timeout and
//! fail loudly** rather than sleep-and-hope.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use nice_term_core::SpawnSpec;

/// Interval between polls of a capture file (real wall-clock; the pty child runs
/// on an OS thread the simulated dispatcher does not drive).
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Create a fresh, uniquely-named temp dir for a fixture session, reused as an
/// empty `ZDOTDIR` so no user zsh rc pollutes a grid. Unique per call (pid +
/// process-global counter) so parallel behavior tests never collide.
pub fn temp_dir(tag: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("nice-itests-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// Write `bytes` to `dir/name` and return the path.
pub fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// A deterministic fixture byte stream: clear + home, then a single grid row of
/// solid **truecolor background** cells (one space per colour). Sampling a cell's
/// centre reads back its background quad — the `term-render` swatch-row pattern,
/// minimised for a harness proof.
pub fn bg_swatch_row(row: usize, cells: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut f = String::new();
    // Clear + home so absolute CUP lands on a clean screen (any stray output
    // that leaks past ZDOTDIR is wiped).
    f.push_str("\x1b[2J\x1b[H");
    f.push_str(&format!("\x1b[{};1H", row + 1));
    for &(r, g, b) in cells {
        f.push_str(&format!("\x1b[48;2;{r};{g};{b}m "));
    }
    f.push_str("\x1b[0m");
    f.into_bytes()
}

/// A session that `cat`s `fixture` verbatim into the grid, with `ZDOTDIR` blanked
/// (`dir`) so no user rc emits. The `cat` exits at EOF; the parsed cells stay in
/// the `Term`, so the grid remains sampleable afterward.
pub fn cat_fixture_spec(dir: &Path, fixture: &Path, rows: u16, cols: u16) -> SpawnSpec {
    let d = dir.to_string_lossy().to_string();
    SpawnSpec::command(format!("cat {}", fixture.display()), d.clone())
        .with_env(vec![("ZDOTDIR".to_string(), d)])
        .with_size(rows, cols)
}

/// A session running `command` with `ZDOTDIR` blanked (`dir`). Intended for a
/// command that produces **no output** (e.g. `"sleep 30"`) so a pane stays silent
/// past a timing deadline. `command` is exec-wrapped by the spawn contract
/// (`zsh -ilc "exec <command>"`), so no extra `exec` is needed.
pub fn silent_command_spec(dir: &Path, command: &str, rows: u16, cols: u16) -> SpawnSpec {
    let d = dir.to_string_lossy().to_string();
    SpawnSpec::command(command.to_string(), d.clone())
        .with_env(vec![("ZDOTDIR".to_string(), d)])
        .with_size(rows, cols)
}

/// A capture-`tee` session: `sh -c 'stty raw -echo; exec tee <cap>'`. Raw mode
/// (no line discipline / echo / signals), then `tee` copies everything the view
/// writes to the pty verbatim into `cap` **and** echoes it back to the pty so the
/// core still tracks output. Read the captured bytes with [`cap_len`] /
/// [`cap_since`] / [`poll_capture_contains`].
pub fn capture_tee_spec(dir: &Path, cap: &Path, rows: u16, cols: u16) -> SpawnSpec {
    let d = dir.to_string_lossy().to_string();
    let inner = format!("stty raw -echo; exec tee {}", cap.display());
    SpawnSpec::command(format!("sh -c '{inner}'"), d.clone())
        .with_env(vec![("ZDOTDIR".to_string(), d)])
        .with_size(rows, cols)
}

/// Current length of the capture file, or 0 if it does not exist yet.
pub fn cap_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Bytes appended to the capture file since offset `start`.
pub fn cap_since(path: &Path, start: u64) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(all) if (all.len() as u64) >= start => all[start as usize..].to_vec(),
        Ok(all) => all,
        Err(_) => Vec::new(),
    }
}

/// Poll the capture file until it contains `needle`, or `timeout` elapses. Use
/// this to confirm the `tee` pipeline is live + in raw mode (write a probe to the
/// pty, then wait for it to reappear in the file) before driving the real input
/// whose bytes you want to assert.
pub fn poll_capture_contains(path: &Path, needle: &[u8], timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let data = std::fs::read(path).unwrap_or_default();
        if contains(&data, needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "capture file {} never contained {} within {:?} (has {} bytes)",
                path.display(),
                render_bytes(needle),
                timeout,
                data.len()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Poll until at least `min_len` bytes have been appended to the capture file
/// since offset `start`, then return them. `timeout` bounds the wait; a timeout
/// returns an error carrying whatever bytes did arrive (for a readable diff).
pub fn poll_capture_after(
    path: &Path,
    start: u64,
    min_len: usize,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        let got = cap_since(path, start);
        if got.len() >= min_len {
            return Ok(got);
        }
        if Instant::now() >= deadline {
            bail!(
                "capture file {} did not grow by {min_len} byte(s) within {:?}; got {} byte(s): {}",
                path.display(),
                timeout,
                got.len(),
                render_bytes(&got)
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Whether `haystack` contains the contiguous `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Render bytes with non-printables escaped, for readable diffs in errors.
fn render_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            0x0d => out.push_str("\\r"),
            0x0a => out.push_str("\\n"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bg_swatch_row_frames_cells() {
        let f = bg_swatch_row(0, &[(1, 2, 3)]);
        let s = String::from_utf8(f).unwrap();
        assert!(s.starts_with("\x1b[2J\x1b[H"));
        assert!(s.contains("\x1b[48;2;1;2;3m "));
        assert!(s.ends_with("\x1b[0m"));
    }

    #[test]
    fn contains_matches_subslice() {
        assert!(contains(b"abc__ready__def", b"__ready__"));
        assert!(!contains(b"abcdef", b"__ready__"));
        assert!(contains(b"anything", b""));
    }

    #[test]
    fn temp_dirs_are_unique() {
        let a = temp_dir("unit").unwrap();
        let b = temp_dir("unit").unwrap();
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
