//! Compile + link the os_signpost shim (`src/signpost.c`) as a static lib.
//!
//! Why a C shim and not raw Rust FFI: the `os_signpost_*` macros place the
//! name/format strings in the special `__TEXT` sections the signpost runtime
//! expects and pass the correct dso handle. Hand-rolled Rust calls into
//! `_os_signpost_emit_with_name_impl` can silently emit nothing if either
//! detail is off, so the intervals would be invisible to
//! `xctrace record --template Logging`. Ported from the phase-0 headline
//! harness's build.rs (`cc` + `ar` via Command, no `cc` crate needed).
//!
//! macOS-only: on any other target the shim is skipped and `signpost.rs`
//! compiles its no-op fallback.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/signpost.c");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src = manifest.join("src/signpost.c");
    let obj = out_dir.join("nice_rs_signpost.o");
    let lib = out_dir.join("libnice_rs_signpost.a");

    // Compile the shim for the TARGET arch, not `cc`'s host default. Without
    // this, a cross-compile (e.g. the x86_64 slice of a universal build on an
    // Apple Silicon host) silently emits an arm64 object and the final link
    // fails with "nice_rs_signpost.o … found architecture 'arm64', required
    // 'x86_64'". `clang` spells aarch64 as `arm64`; x86_64 maps to itself.
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let clang_arch = match target_arch.as_str() {
        "aarch64" => "arm64",
        other => other,
    };

    let status = Command::new("cc")
        .arg("-arch")
        .arg(clang_arch)
        .arg("-O2")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("failed to spawn `cc` for the nice signpost shim");
    assert!(status.success(), "cc failed on src/signpost.c");

    let status = Command::new("ar")
        .arg("rcs")
        .arg(&lib)
        .arg(&obj)
        .status()
        .expect("failed to spawn `ar` for the nice signpost shim");
    assert!(status.success(), "ar failed for libnice_rs_signpost.a");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=nice_rs_signpost");
}
