// SPIKE: alacritty_terminal (parser + grid model) driven by a real shell via
// portable-pty. Proves: (1) we can spawn a real PTY shell, (2) feed it a
// command, (3) run the bytes through alacritty's VT parser into a Term grid,
// (4) read back the rendered grid + per-cell styling (SGR colors/flags).
//
// This is the "headless terminal core" a GPU renderer would draw from.

use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Minimal terminal dimensions implementing alacritty's `Dimensions` trait.
/// (alacritty's own `TermSize` is `#[cfg(test)]`, so we provide our own.)
#[derive(Clone, Copy)]
struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// alacritty pushes side-effects (bells, title changes, clipboard, PTY writes)
/// through an `EventListener`. For a headless spike a no-op proxy is enough;
/// all default methods are no-ops, so an empty impl compiles.
#[derive(Clone)]
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {}
}

fn color_name(c: Color) -> String {
    match c {
        Color::Named(NamedColor::Foreground) => "default-fg".into(),
        Color::Named(NamedColor::Background) => "default-bg".into(),
        Color::Named(n) => format!("named({:?})", n),
        Color::Spec(rgb) => format!("rgb(#{:02x}{:02x}{:02x})", rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => format!("indexed({})", i),
    }
}

fn main() {
    println!("=== SPIKE: alacritty_terminal + portable-pty ===\n");

    // 1. Open a real PTY at 24x80.
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty failed");

    // 2. Spawn a real shell in the slave. `sh -c` runs a deterministic script
    //    that emits plain text AND an ANSI-colored token, then exits so the
    //    master read loop sees EOF.
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(
        "echo SPIKE_PLAIN_LINE; \
         printf '\\033[1;32mGREEN_BOLD\\033[0m and \\033[31mRED\\033[0m\\n'; \
         echo done=$((6*7))",
    );
    let mut child = pair.slave.spawn_command(cmd).expect("spawn failed");

    // 3. Pump the master end into a channel from a reader thread (the master
    //    read is blocking; this is the same shape a real app's PTY loop has).
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let _writer = pair.master.take_writer().expect("take writer"); // keep master write open
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: child exited
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 4. Build the headless Term + VT parser and feed it the PTY bytes.
    let size = Size {
        rows: ROWS as usize,
        cols: COLS as usize,
    };
    let mut term = Term::new(Config::default(), &size, EventProxy);
    let mut parser: Processor = Processor::new();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut total_bytes = 0usize;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                total_bytes += chunk.len();
                parser.advance(&mut term, &chunk);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() > deadline {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.wait();
    let _ = reader_thread.join();

    println!("PTY bytes parsed: {}\n", total_bytes);

    // 5a. Reconstruct visible text from the grid, row by row.
    println!("--- reconstructed grid (non-empty rows) ---");
    let cols = term.columns();
    let lines = term.screen_lines();
    let mut found_plain = false;
    let mut found_compute = false;
    for line in 0..lines {
        let mut row = String::new();
        for col in 0..cols {
            let cell = &term.grid()[Point::new(Line(line as i32), Column(col))];
            row.push(cell.c);
        }
        let trimmed = row.trim_end();
        if !trimmed.is_empty() {
            println!("row {:>2}: {}", line, trimmed);
            if trimmed.contains("SPIKE_PLAIN_LINE") {
                found_plain = true;
            }
            if trimmed.contains("done=42") {
                found_compute = true;
            }
        }
    }

    // 5b. Prove SGR styling parsed: walk the grid, report the styling of the
    //     first non-default-foreground glyphs we find (the colored tokens).
    println!("\n--- per-cell styling (SGR parse proof) ---");
    let mut styled_samples = 0;
    'outer: for line in 0..lines {
        for col in 0..cols {
            let cell = &term.grid()[Point::new(Line(line as i32), Column(col))];
            let is_default_fg = matches!(cell.fg, Color::Named(NamedColor::Foreground));
            if cell.c != ' ' && !is_default_fg {
                println!(
                    "  glyph '{}' fg={} flags={:?}",
                    cell.c,
                    color_name(cell.fg),
                    cell.flags
                );
                styled_samples += 1;
                if styled_samples >= 12 {
                    break 'outer;
                }
            }
        }
    }

    println!("\n--- assertions ---");
    println!("found 'SPIKE_PLAIN_LINE' in grid : {}", found_plain);
    println!("found 'done=42' (shell computed) : {}", found_compute);
    println!("found styled (non-default) glyphs: {}", styled_samples > 0);

    let ok = found_plain && found_compute && styled_samples > 0;
    println!("\nRESULT: {}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
