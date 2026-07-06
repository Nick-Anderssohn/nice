//! Shared atomic-file writer — hoisted from R16's module-private
//! `claude_hook_installer::write_atomic` and OWNED here (R18). The Claude hook
//! installer, the R18 session store, and R19's sort-settings store all write
//! small config/state files that a concurrently-reading peer (a pty child, a
//! still-installed Swift Nice) must never observe half-written. One temp-file +
//! rename helper covers them all; naming it in one place is the plan's
//! "Exported contracts" atomic-write helper.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Atomically replace `path` with `contents`: write a pid-suffixed sibling in
/// the same directory, `chmod` it if `mode` is given, then rename over the
/// target (rename is atomic within a filesystem — a reader mid-read never sees
/// a half-written file). Setting the mode on the temp BEFORE the rename means
/// the final path is never briefly non-executable.
///
/// A failure short of the rename (e.g. the temp create fails on a read-only
/// parent, a full disk) leaves the prior file at `path` intact — the recovery
/// contract the session store's write-failure test pins.
pub(crate) fn write_atomic(path: &Path, contents: &[u8], mode: Option<u32>) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    fs::write(&tmp, contents)?;
    if let Some(m) = mode {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(m))?;
    }
    fs::rename(&tmp, path)
}
