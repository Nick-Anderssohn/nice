//! OSC 7 cwd tee — a self-contained, byte-transparent scanner over the pty read
//! stream (R6 "Binding technical decisions" → OSC 7).
//!
//! vte 0.15 has **no OSC 7 arm** (it is silently discarded in vte's private
//! `ansi::Performer` — it does not desync), so cwd tracking cannot ride the VT
//! parser's event stream. Instead the feeder ([`crate::session::spawn_feeder`])
//! runs each raw read chunk through this scanner *alongside* — never in place of
//! — feeding the parser. The scanner recognises a complete
//! `ESC ] 7 ; file://<host>/<path> ST|BEL` sequence, percent-decodes the path,
//! validates the host is local (empty / `localhost` / this machine), and emits
//! [`CwdChanged`](crate::SessionEvent::CwdChanged) with the decoded [`PathBuf`].
//!
//! Two properties are load-bearing (and are exactly what R15's later status
//! parsing may extend):
//!
//! 1. **It never alters the bytes handed to the parser.** [`Osc7Scanner::feed`]
//!    takes the chunk by shared reference and only *observes* it; the feeder
//!    hands the very same slice to `parser.advance`. The transparency test
//!    proves this by diffing the grid with the tee on vs. off.
//! 2. **It streams, tolerant of split reads, without copying the whole stream.**
//!    Normal output bytes are only *compared* against the fixed introducer; only
//!    the bytes of a candidate OSC 7 payload are buffered, and that buffer is
//!    capped ([`MAX_PAYLOAD`]) so a malformed/unterminated sequence can never
//!    grow it without bound. A sequence split across read boundaries is carried
//!    across `feed` calls by the scanner's small state.
//!
//! Malformed, oversized, or foreign-host sequences are dropped silently; the
//! scanner returns to the ground state and keeps scanning, so a bad sequence
//! never wedges the tee (nor, since the bytes still reach the parser untouched,
//! the grid).

use std::path::PathBuf;

/// The literal bytes that introduce an OSC 7 sequence: `ESC ] 7 ;`.
const PREFIX: [u8; 4] = [0x1b, b']', b'7', b';'];
/// BEL — the 7-bit string terminator alternative to ST.
const BEL: u8 = 0x07;
/// ESC — begins the ST terminator (`ESC \`) and every escape sequence.
const ESC: u8 = 0x1b;
/// The final byte of a 7-bit ST terminator (`ESC \`).
const ST_FINAL: u8 = b'\\';
/// Cap on a captured OSC 7 payload, in bytes. A cwd path is bounded by PATH_MAX
/// (~1 KiB); percent-encoding at most triples it, plus the short `file://host`
/// prefix. 4 KiB is generous — a longer capture is treated as malformed and
/// dropped so a hostile or corrupt stream can never grow the partial buffer
/// unbounded.
const MAX_PAYLOAD: usize = 4096;

/// Where the scanner is in the `ESC ] 7 ; <payload> ST|BEL` grammar.
enum State {
    /// Outside a sequence. `matched` bytes of [`PREFIX`] have been seen so far.
    Ground,
    /// The full introducer matched; capturing the payload into `buf`.
    Payload,
    /// Inside the payload, an `ESC` was seen — waiting to see whether it is the
    /// `\` of an ST terminator or the start of something else (which aborts).
    PayloadEsc,
}

/// A streaming OSC 7 recogniser. One per feeder thread; `feed` is called with
/// each pty read chunk in order.
pub(crate) struct Osc7Scanner {
    state: State,
    /// Progress into [`PREFIX`] while in [`State::Ground`].
    matched: usize,
    /// The payload bytes captured between the introducer and the terminator.
    buf: Vec<u8>,
}

impl Osc7Scanner {
    pub(crate) fn new() -> Osc7Scanner {
        Osc7Scanner {
            state: State::Ground,
            matched: 0,
            buf: Vec::new(),
        }
    }

    /// Observe one pty read `chunk`. The chunk is taken by shared reference and
    /// is never modified. `emit` is called once for each complete, well-formed,
    /// local OSC 7 sequence, with the decoded working directory.
    pub(crate) fn feed(&mut self, chunk: &[u8], mut emit: impl FnMut(PathBuf)) {
        let mut i = 0;
        while i < chunk.len() {
            let b = chunk[i];
            match self.state {
                State::Ground => {
                    // Match the fixed introducer byte-by-byte. On a mismatch,
                    // restart the match: only ESC (PREFIX[0]) can begin a fresh
                    // introducer, and no later introducer byte equals ESC, so a
                    // simple restart is correct (no KMP fallback needed).
                    if b == PREFIX[self.matched] {
                        self.matched += 1;
                        if self.matched == PREFIX.len() {
                            self.state = State::Payload;
                            self.matched = 0;
                            self.buf.clear();
                        }
                    } else {
                        self.matched = usize::from(b == PREFIX[0]);
                    }
                    i += 1;
                }
                State::Payload => match b {
                    // BEL and `ESC \` are the only OSC terminators vte 0.15
                    // recognises (see its `advance_osc_string`: 0x07 ends the
                    // string, 0x1B routes to the ST check; every other >=0x20
                    // byte is payload). We match that so the tee never terminates
                    // where the real parser wouldn't. In particular C1 ST (0x9C)
                    // is NOT a terminator — it is an ordinary payload byte, and a
                    // valid UTF-8 continuation byte, so a path emitted without
                    // percent-encoding (e.g. `Ĝ` = 0xC4 0x9C) is not truncated.
                    BEL => {
                        self.finish(&mut emit);
                        i += 1;
                    }
                    ESC => {
                        self.state = State::PayloadEsc;
                        i += 1;
                    }
                    // Any other C0 control aborts: a well-formed OSC 7 payload is
                    // printable `file://…`, so a stray control byte means the
                    // sequence was interrupted. Drop it and re-scan from ground.
                    // (Such a byte cannot itself begin the introducer — ESC is
                    // handled above — so consuming it here is safe.)
                    _ if b < 0x20 => {
                        self.reset();
                        i += 1;
                    }
                    _ => {
                        self.buf.push(b);
                        if self.buf.len() > MAX_PAYLOAD {
                            self.reset(); // oversized — drop.
                        }
                        i += 1;
                    }
                },
                State::PayloadEsc => {
                    if b == ST_FINAL {
                        self.finish(&mut emit);
                        i += 1;
                    } else {
                        // ESC not followed by `\`: the OSC 7 is abandoned. Return
                        // to ground and RE-PROCESS this byte without consuming it
                        // (it may begin a fresh introducer, e.g. another ESC).
                        self.reset();
                        // deliberately no `i += 1`
                    }
                }
            }
        }
    }

    /// A payload was fully captured. Parse/validate it; emit the cwd on success.
    /// Always returns to [`State::Ground`].
    fn finish(&mut self, emit: &mut impl FnMut(PathBuf)) {
        if let Some(path) = parse_osc7_payload(&self.buf) {
            emit(path);
        }
        self.reset();
    }

    /// Drop any in-progress capture and return to the ground state.
    fn reset(&mut self) {
        self.state = State::Ground;
        self.matched = 0;
        self.buf.clear();
    }
}

/// Parse a captured OSC 7 payload (`file://<host>/<path>`) into a cwd, or `None`
/// if it is not a well-formed, local `file://` URI with an absolute path.
fn parse_osc7_payload(payload: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Must be a file URI.
    let rest = payload.strip_prefix(b"file://")?;
    // The host runs up to the first '/', which begins the absolute path. No '/'
    // means no absolute path → malformed.
    let slash = rest.iter().position(|&b| b == b'/')?;
    let host = &rest[..slash];
    let path_bytes = &rest[slash..]; // includes the leading '/'.

    if !host_is_local(host) {
        return None; // foreign host — ignore.
    }

    let decoded = percent_decode(path_bytes)?;
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&decoded)))
}

/// Whether an OSC 7 `host` names the local machine (so its cwd applies here).
/// Accepts an empty host, `localhost`, this machine's hostname, and the
/// short/FQDN variants of it (`mymac` ↔ `mymac.local`), but not a genuinely
/// different host.
fn host_is_local(host: &[u8]) -> bool {
    if host.is_empty() {
        return true;
    }
    let host = match std::str::from_utf8(host) {
        Ok(h) => h,
        Err(_) => return false,
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match local_hostname() {
        Some(local) => {
            host.eq_ignore_ascii_case(&local)
                // OSC gave the short name, we hold the FQDN (or vice versa).
                || host.eq_ignore_ascii_case(first_label(&local))
                || first_label(host).eq_ignore_ascii_case(&local)
        }
        None => false,
    }
}

/// The first dot-label of a hostname (`mymac.local` → `mymac`).
fn first_label(host: &str) -> &str {
    host.split('.').next().unwrap_or(host)
}

/// This machine's hostname via `gethostname(2)`, or `None` if it could not be
/// read (in which case only empty/`localhost` hosts are treated as local).
fn local_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).ok().map(str::to_owned)
}

/// Percent-decode `input` (`%XX` → the byte `0xXX`). Returns `None` on a
/// truncated or non-hex `%` escape (malformed → the whole sequence is dropped).
fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'%' => {
                let hi = hex_val(*input.get(i + 1)?)?;
                let lo = hex_val(*input.get(i + 2)?)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(out)
}

/// One hex digit's value, or `None` if `b` is not `[0-9a-fA-F]`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a set of chunks (each an independent `feed` call, to model split
    /// read boundaries) and collect every emitted cwd.
    fn scan(chunks: &[&[u8]]) -> Vec<PathBuf> {
        let mut scanner = Osc7Scanner::new();
        let mut out = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, |p| out.push(p));
        }
        out
    }

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn bel_terminated_localhost_path() {
        let out = scan(&[b"\x1b]7;file://localhost/home/nick\x07"]);
        assert_eq!(out, vec![pb("/home/nick")]);
    }

    #[test]
    fn st_terminated_empty_host() {
        // `file:///abs` → empty host, path `/abs`. Terminated by ST (ESC \).
        let out = scan(&[b"\x1b]7;file:///var/tmp\x1b\\"]);
        assert_eq!(out, vec![pb("/var/tmp")]);
    }

    #[test]
    fn c1_st_byte_is_payload_not_terminator() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // vte 0.15 does NOT treat C1 ST (0x9C) as an OSC terminator — its
        // `advance_osc_string` ends only on BEL/CAN/SUB/ESC and treats every
        // other byte as payload. 0x9C is also a valid UTF-8 continuation byte
        // (`Ĝ` = U+011C = 0xC4 0x9C), so a non-percent-encoded path carrying it
        // must survive intact rather than truncate at 0x9C. Terminated by BEL.
        let out = scan(&[b"\x1b]7;file:///\xc4\x9c\x07"]);
        assert_eq!(
            out,
            vec![PathBuf::from(OsStr::from_bytes(b"/\xc4\x9c"))],
            "0x9C must be payload (matching vte), not an early terminator"
        );
        // And a bare 0x9C does not end the sequence: with no real terminator the
        // OSC 7 stays open and nothing is emitted.
        assert!(
            scan(&[b"\x1b]7;file:///srv\x9c"]).is_empty(),
            "0x9C alone must not terminate the OSC 7 sequence"
        );
    }

    #[test]
    fn percent_encoded_bytes_decode() {
        // `%20` → space, `%C3%A9` → 'é' (UTF-8), across an empty host.
        let out = scan(&[b"\x1b]7;file:///tmp/a%20b/caf%C3%A9\x07"]);
        assert_eq!(out, vec![pb("/tmp/a b/café")]);
    }

    #[test]
    fn split_across_read_boundaries() {
        // The same sequence delivered in three pieces, cutting through the
        // introducer, the payload, and just before the terminator.
        let out = scan(&[b"\x1b]7;file://localho", b"st/split/pa", b"th\x07"]);
        assert_eq!(out, vec![pb("/split/path")]);
    }

    #[test]
    fn introducer_split_at_every_byte() {
        // Deliver one byte per chunk — the introducer itself is split.
        let seq = b"\x1b]7;file:///x\x07";
        let chunks: Vec<&[u8]> = seq.iter().map(std::slice::from_ref).collect();
        let mut scanner = Osc7Scanner::new();
        let mut out = Vec::new();
        for c in &chunks {
            scanner.feed(c, |p| out.push(p));
        }
        assert_eq!(out, vec![pb("/x")]);
    }

    #[test]
    fn foreign_host_is_dropped() {
        let out = scan(&[b"\x1b]7;file://some-other-box.example.com/etc\x07"]);
        assert!(out.is_empty(), "foreign-host OSC 7 must be ignored");
    }

    #[test]
    fn local_hostname_is_accepted() {
        // Whatever gethostname() reports must be treated as local.
        let host = local_hostname().expect("gethostname");
        let seq = format!("\x1b]7;file://{host}/home/here\x07");
        let out = scan(&[seq.as_bytes()]);
        assert_eq!(out, vec![pb("/home/here")]);
    }

    #[test]
    fn non_file_scheme_is_dropped() {
        let out = scan(&[b"\x1b]7;http://localhost/x\x07"]);
        assert!(out.is_empty(), "non-file:// OSC 7 must be ignored");
    }

    #[test]
    fn no_absolute_path_is_dropped() {
        // `file://host` with no '/path' → malformed.
        let out = scan(&[b"\x1b]7;file://localhost\x07"]);
        assert!(out.is_empty());
    }

    #[test]
    fn truncated_percent_escape_is_dropped() {
        let out = scan(&[b"\x1b]7;file:///a%2\x07"]); // '%2' then terminator.
        assert!(out.is_empty(), "a truncated %-escape must drop the sequence");
    }

    #[test]
    fn non_hex_percent_escape_is_dropped() {
        let out = scan(&[b"\x1b]7;file:///a%zzb\x07"]);
        assert!(out.is_empty());
    }

    #[test]
    fn oversized_payload_is_dropped_then_recovers() {
        // A payload far past the cap, unterminated, then terminated; then a
        // valid sequence must still be recognised (the scanner recovered).
        let mut giant = Vec::new();
        giant.extend_from_slice(b"\x1b]7;file:///");
        giant.extend(std::iter::repeat(b'A').take(MAX_PAYLOAD + 100));
        giant.extend_from_slice(b"\x07");
        giant.extend_from_slice(b"\x1b]7;file:///good\x07");
        let out = scan(&[&giant]);
        assert_eq!(out, vec![pb("/good")], "oversized dropped, next one recovered");
    }

    #[test]
    fn c0_control_inside_payload_aborts() {
        // A newline mid-payload aborts; a following valid sequence still parses.
        let out = scan(&[b"\x1b]7;file:///a\nbad\x07\x1b]7;file:///ok\x07"]);
        assert_eq!(out, vec![pb("/ok")]);
    }

    #[test]
    fn esc_not_backslash_in_payload_aborts_but_next_parses() {
        // `ESC X` (not `ESC \`) inside the payload abandons it; the trailing
        // valid OSC 7 (whose own ESC re-enters the introducer) still parses.
        let out = scan(&[b"\x1b]7;file:///a\x1bX\x1b]7;file:///b\x07"]);
        assert_eq!(out, vec![pb("/b")]);
    }

    #[test]
    fn plain_output_emits_nothing() {
        let out = scan(&[b"hello world\r\n\x1b[31mred\x1b[0m no osc here\r\n"]);
        assert!(out.is_empty());
    }

    #[test]
    fn multiple_sequences_in_one_chunk_in_order() {
        let out = scan(&[b"\x1b]7;file:///one\x07 middle \x1b]7;file:///two\x07"]);
        assert_eq!(out, vec![pb("/one"), pb("/two")]);
    }

    #[test]
    fn empty_osc7_params_are_dropped() {
        let out = scan(&[b"\x1b]7;\x07"]); // no payload at all.
        assert!(out.is_empty());
    }
}
