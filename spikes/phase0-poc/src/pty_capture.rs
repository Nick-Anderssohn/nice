//! §13 spike 7 — `pty-capture`: record a REAL interactive pty session's
//! OUTPUT bytes with timestamps into the "nicetrace v1" format
//! (`harness::trace`), replayable timing-faithfully into both measurement
//! bins via `NICE_POC_TRACE=<file>`.
//!
//! Capture (run this INSIDE a real terminal; the child gets a real pty and
//! your keystrokes pass through — a live `claude` session works as-is):
//!
//! ```sh
//! cargo run --release --bin pty-capture -- -o /tmp/claude-session.nicetrace -- claude
//! # ... use the session normally; exit the child (Ctrl-D / /exit) to finish.
//! ```
//!
//! - Only pty OUTPUT (child -> master) is recorded — exactly the byte stream
//!   a terminal renderer must handle. Input bytes are forwarded but NOT
//!   recorded (keystroke privacy; replay doesn't need them).
//! - The child runs behind a real pty (alacritty_terminal::tty: openpty +
//!   setsid + TIOCSCTTY), inherits the environment (TERM etc.), and gets the
//!   hosting terminal's window size (mirrored on change, polled ~4 Hz).
//! - The hosting terminal is put in raw mode for transparent passthrough;
//!   it is restored on exit. If the process is killed hard and your terminal
//!   is left raw, run `reset`.
//!
//! Convert an existing macOS `script -r` recording instead (best-effort — the
//! BSD "stamp" record format; output records only):
//!
//! ```sh
//! script -r /tmp/session.rawtrace claude        # record with script(1)
//! cargo run --release --bin pty-capture -- --convert-script /tmp/session.rawtrace -o /tmp/claude-session.nicetrace
//! ```

#![allow(dead_code)]

#[path = "harness.rs"]
mod harness;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harness::trace::TraceWriter;

// ---------------------------------------------------------------------------
// Terminal raw-mode guard (restores termios on drop).
// ---------------------------------------------------------------------------

struct RawModeGuard {
    fd: i32,
    saved: libc::termios,
    active: bool,
}

impl RawModeGuard {
    fn enable(fd: i32) -> Option<RawModeGuard> {
        unsafe {
            if libc::isatty(fd) == 0 {
                return None;
            }
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut saved) != 0 {
                return None;
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawModeGuard {
                fd,
                saved,
                active: true,
            })
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
            }
        }
    }
}

fn host_winsize(fd: i32) -> (u16, u16) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (120, 40)
        }
    }
}

fn set_pty_winsize(fd: i32, cols: u16, rows: u16) {
    unsafe {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let _ = libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

// ---------------------------------------------------------------------------
// Capture: child behind a real pty; forward stdin<->pty; tee output+timing.
// ---------------------------------------------------------------------------

fn run_capture(out_path: PathBuf, cmd: String, args: Vec<String>) -> i32 {
    use alacritty_terminal::event::WindowSize;
    use alacritty_terminal::tty::{self, EventedReadWrite};

    let stdin_fd = libc::STDIN_FILENO;
    let (cols, rows) = host_winsize(stdin_fd);

    let opts = tty::Options {
        shell: Some(tty::Shell::new(cmd.clone(), args.clone())),
        working_directory: None,
        drain_on_exit: true,
        env: std::collections::HashMap::new(),
    };
    let ws = WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: 8,
        cell_height: 16,
    };
    let mut pty = match tty::new(&opts, ws, 0) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pty-capture: failed to spawn `{cmd}` behind a pty: {e}");
            return 1;
        }
    };

    // alacritty's tty::new sets the master fd non-blocking (its own event
    // loop polls). We use blocking threads — clear O_NONBLOCK (flags live on
    // the shared open-file description).
    let writer = match pty.file().try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("pty-capture: dup master fd failed: {e}");
            return 1;
        }
    };
    unsafe {
        let fd = writer.as_raw_fd();
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
    }
    let master_fd = writer.as_raw_fd();

    let trace = match TraceWriter::create(&out_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pty-capture: cannot create {}: {e}", out_path.display());
            return 1;
        }
    };

    eprintln!(
        "pty-capture: recording OUTPUT of `{cmd} {}` ({cols}x{rows}) -> {} — exit the child \
         to finish.\r",
        args.join(" "),
        out_path.display()
    );

    // Raw mode AFTER the banner so it prints normally.
    let _raw = RawModeGuard::enable(stdin_fd);

    let stop = Arc::new(AtomicBool::new(false));

    // Reader thread: pty master -> (stdout passthrough + trace record).
    let reader_handle = {
        let stop = Arc::clone(&stop);
        let mut trace = trace;
        std::thread::Builder::new()
            .name("pty-capture-reader".into())
            .spawn(move || {
                let mut stdout = std::io::stdout();
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match pty.reader().read(&mut buf) {
                        Ok(0) => break, // child exited / EOF
                        Ok(n) => {
                            let _ = stdout.write_all(&buf[..n]);
                            let _ = stdout.flush();
                            if trace.record(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break, // EIO on child exit
                    }
                }
                stop.store(true, Ordering::SeqCst);
                trace.finish()
            })
            .expect("spawn reader thread")
    };

    // Main loop: stdin -> pty master (poll so we notice child exit), and
    // mirror host window-size changes onto the pty (~4 Hz).
    let mut writer = writer;
    let mut last_ws = (cols, rows);
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let pr = unsafe { libc::poll(&mut pfd, 1, 250) };
        if pr > 0 && (pfd.revents & libc::POLLIN) != 0 {
            let n = unsafe {
                libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n <= 0 {
                break; // stdin EOF/error — stop forwarding; child will EOF soon
            }
            if writer.write_all(&buf[..n as usize]).is_err() {
                break;
            }
        }
        let now_ws = host_winsize(stdin_fd);
        if now_ws != last_ws {
            last_ws = now_ws;
            set_pty_winsize(master_fd, now_ws.0, now_ws.1);
        }
    }

    let result = reader_handle.join();
    drop(_raw); // restore termios before printing the summary
    match result {
        Ok(Ok((records, bytes, secs))) => {
            eprintln!(
                "\npty-capture: DONE — {records} records / {bytes} bytes / {secs:.1}s -> {}",
                out_path.display()
            );
            eprintln!(
                "replay: NICE_POC_RUN=1 NICE_POC_TRACE={} cargo run --release --bin gpui-term",
                out_path.display()
            );
            0
        }
        Ok(Err(e)) => {
            eprintln!("\npty-capture: trace finalize failed: {e}");
            1
        }
        Err(_) => {
            eprintln!("\npty-capture: reader thread panicked");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Converter: macOS/FreeBSD `script -r` recording -> nicetrace. Best-effort:
// parses the BSD "stamp" record framing (script(1) writes a stamp header per
// event; direction 's' start, 'i' input, 'o' output, 'e' end). Only output
// records are converted. If parsing fails, prefer the first-party capture.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct ScriptStamp {
    scr_len: u64,       // amount of data following (for i/o records)
    scr_sec: u64,       // seconds
    scr_usec: u32,      // microseconds
    scr_direction: u32, // 's' | 'i' | 'o' | 'e'
}

fn run_convert_script(input: PathBuf, out_path: PathBuf) -> i32 {
    let data = match std::fs::read(&input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("pty-capture: cannot read {}: {e}", input.display());
            return 1;
        }
    };
    let stamp_size = std::mem::size_of::<ScriptStamp>();
    let mut off = 0usize;
    let mut base_us: Option<u64> = None;
    let mut records = 0u64;
    let mut skipped = 0u64;

    let mut writer = match TraceWriter::create(&out_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("pty-capture: cannot create {}: {e}", out_path.display());
            return 1;
        }
    };

    while off + stamp_size <= data.len() {
        let stamp: ScriptStamp = unsafe {
            std::ptr::read_unaligned(data[off..].as_ptr() as *const ScriptStamp)
        };
        off += stamp_size;
        let dir = char::from_u32(stamp.scr_direction).unwrap_or('?');
        let len = stamp.scr_len as usize;
        if !matches!(dir, 's' | 'i' | 'o' | 'e') || off + len > data.len() {
            eprintln!(
                "pty-capture: unrecognized record at byte {off} (direction {:#x}) — this \
                 doesn't look like a `script -r` recording on this macOS version. Use the \
                 first-party capture instead: pty-capture -o <out> -- <cmd>",
                stamp.scr_direction
            );
            return 1;
        }
        let t_us = stamp.scr_sec * 1_000_000 + stamp.scr_usec as u64;
        let base = *base_us.get_or_insert(t_us);
        if dir == 'o' && len > 0 {
            let rel_ns = t_us.saturating_sub(base) * 1_000;
            if writer.record_at(rel_ns, &data[off..off + len]).is_err() {
                eprintln!("pty-capture: write failed");
                return 1;
            }
            records += 1;
        } else {
            skipped += 1;
        }
        off += len;
    }

    match writer.finish() {
        Ok((recs, bytes, _)) => {
            eprintln!(
                "pty-capture: converted {} -> {} ({recs} output records / {bytes} bytes; \
                 {skipped} non-output records skipped)",
                input.display(),
                out_path.display()
            );
            let _ = records;
            0
        }
        Err(e) => {
            eprintln!("pty-capture: finalize failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------

fn usage() -> ! {
    eprintln!(
        "usage:\n  pty-capture -o <out.nicetrace> [-- <cmd> [args...]]   (default cmd: $SHELL)\n\
         \x20 pty-capture --convert-script <script-r-file> -o <out.nicetrace>\n\
         \nheadless self-test of the format + replay: \n\
         \x20 NICE_POC_TRACE=selftest cargo run --bin gpui-term"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<PathBuf> = None;
    let mut convert: Option<PathBuf> = None;
    let mut cmd_and_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            "--convert-script" => {
                i += 1;
                convert = args.get(i).map(PathBuf::from);
            }
            "--" => {
                cmd_and_args = args[i + 1..].to_vec();
                break;
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("pty-capture: unknown arg {other}");
                usage();
            }
        }
        i += 1;
    }

    let Some(out) = out else { usage() };

    let code = if let Some(input) = convert {
        run_convert_script(input, out)
    } else {
        let (cmd, rest) = if cmd_and_args.is_empty() {
            (
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()),
                Vec::new(),
            )
        } else {
            (cmd_and_args[0].clone(), cmd_and_args[1..].to_vec())
        };
        run_capture(out, cmd, rest)
    };
    std::process::exit(code);
}
