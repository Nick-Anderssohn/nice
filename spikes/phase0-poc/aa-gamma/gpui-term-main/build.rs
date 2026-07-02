//! Compile the headline harness's os_signpost "Draw" shim
//! (src/bin/headline/nice_signpost.c) into a static lib and link it
//! (same recipe as the 0.2.2 vendored-gpui patch's build.rs, minus bindgen).
//! Crate-wide link: the other bins (gpui-term-main, ime-spike) never reference
//! the shim's symbols, so their object is simply not pulled in.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src = manifest.join("src/bin/headline/nice_signpost.c");
    let obj = out_dir.join("nice_signpost.o");
    let lib = out_dir.join("libnice_signpost_main.a");

    println!("cargo:rerun-if-changed={}", src.display());

    let status = Command::new("cc")
        .arg("-O2")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("failed to spawn `cc` for the nice_signpost shim");
    assert!(status.success(), "cc failed on nice_signpost.c");

    let status = Command::new("ar")
        .arg("rcs")
        .arg(&lib)
        .arg(&obj)
        .status()
        .expect("failed to spawn `ar` for the nice_signpost shim");
    assert!(status.success(), "ar failed for libnice_signpost_main.a");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=nice_signpost_main");
}
