//! nice-term-core — the headless heart of Nice's terminal.
//!
//! This first R3 slice is the **process layer**: shell quoting, the spawn spec,
//! and real pty spawn/write/resize/teardown/exit-reaping honoring the PROTECTED
//! spawn contract (login + interactive zsh, the `zsh -ilc "exec <cmd>"` wrapper,
//! tilde-expanded cwd, caller env injection, initial winsize + SIGWINCH
//! propagation, process-group teardown so no orphaned zsh survives).
//!
//! Everything here is UI-free and testable without a window. Per the layering
//! rule this crate has **no `gpui` dependency**.
//!
//! On top of the process layer sits the `alacritty_terminal` VT core:
//! [`TermSession`] joins a [`PtyProcess`] to a `Term` behind a `FairMutex`, with
//! a per-session feeder thread parsing pty bytes into the `Term` off the render
//! thread ([`crate::session`]), the [`DamageCallback`] wake the renderer drains
//! on, resize propagation, the per-session scrollback knob
//! ([`DEFAULT_SCROLLBACK_LINES`]), and the owned grid read API
//! ([`GridSnapshot`], [`TermSession::grid_contains`]) in [`crate::vt`].
//!
//! The top of the crate is [`Session`] ([`crate::deferred`]): the value-owning
//! pane session that wraps `TermSession` into the explicit deferred-spawn state
//! machine ([`Phase`]: `NotSpawned → Spawning → Live → Exited{status, held}`),
//! the typed outward event stream ([`SessionEvent`]), and held-pane
//! classification ([`should_hold_on_exit`]). That is the API the renderer (R4)
//! and the session manager (R13) consume — still with **no `gpui` dependency**.
//!
//! R6 plumbs the escape-sequence side-channels Nice's app layer runs on onto
//! that same event stream and query surface: OSC 0/2 window titles
//! ([`SessionEvent::TitleChanged`] / [`SessionEvent::TitleReset`], via the
//! `Term`'s [`EventProxy`]), OSC 7 cwd tracking ([`SessionEvent::CwdChanged`],
//! teed off the raw pty bytes by the self-contained [`crate::osc7`] scanner
//! without ever altering the bytes the parser sees), and a synchronous
//! bracketed-paste (DECSET 2004) query ([`Session::bracketed_paste_active`] /
//! [`TermSession::bracketed_paste_active`]) the paste/drop paths consult. The
//! consumers (status parsing, cwd persistence, paste framing) come in later
//! stages; R6 is transport only.

pub mod deferred;
/// OSC 7 cwd tee (internal): the byte-transparent scanner the feeder runs over
/// the raw pty stream. Not part of the public surface — its existence and its
/// "never alters the parser's bytes" property are the contract later stages
/// (R15 status parsing) build on.
mod osc7;
pub mod pty;
pub mod quoting;
pub mod session;
pub mod spawn;
pub mod vt;

pub use deferred::{should_hold_on_exit, DrainWake, Phase, Session, SessionEvent};
pub use pty::{ExitStatus, ExitWaiter, PtyProcess};
pub use quoting::{shell_backslash_escape, shell_single_quote};
pub use session::{DamageCallback, TermSession};
pub use spawn::{build_argv, build_exec_args, expand_tilde, SpawnSpec, ZSH_PATH};
pub use vt::{EventProxy, GridSnapshot, SharedTerm, DEFAULT_SCROLLBACK_LINES};
