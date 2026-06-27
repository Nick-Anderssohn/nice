//! build.rs — build + link the Swift bridge dylib for the Phase-0 PoC.
//!
//! Two modes (selected by env var `NICE_POC_REAL_BRIDGE`):
//!
//!   * DEFAULT (unset / "0") — HEADLESS STUB. Compile
//!     `swift-embed/StubBridge.swift` with a single `swiftc -emit-library` into
//!     `$OUT_DIR/libswifttermbridge.dylib`. No SwiftTerm, no Metal, no network,
//!     no display. This is what lets `cargo check`/`cargo build` succeed on a
//!     headless box. The stub exposes the identical C ABI as the real bridge.
//!
//!   * REAL  (NICE_POC_REAL_BRIDGE=1) — build the real SwiftPM package
//!     `swift-bridge/` (path-depends on the SwiftTerm fork) via `swift build`,
//!     producing `.build/<config>/libSwiftTermBridge.dylib` with SwiftTerm
//!     statically linked in + the `SwiftTerm_SwiftTerm.bundle` Metal-shader
//!     resource bundle alongside it. We link that dylib by name and rpath its
//!     directory so `Bundle(for: MetalTerminalRenderer.self)` resolves the
//!     shaders at runtime. Run this mode on a machine WITH a display when you
//!     actually take the measurements.
//!
//! Mirrors the working link pattern in
//! spikes/spike-reuse-swiftterm/swift-embed/build.rs (rustc-link-search +
//! rustc-link-lib=dylib + rustc-link-arg rpath), extended to also *produce* the
//! dylib rather than assume a pre-built one.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let real = matches!(env::var("NICE_POC_REAL_BRIDGE").as_deref(), Ok("1") | Ok("true"));

    println!("cargo:rerun-if-env-changed=NICE_POC_REAL_BRIDGE");
    println!("cargo:rerun-if-changed=swift-embed/StubBridge.swift");
    println!("cargo:rerun-if-changed=swift-bridge/Sources/SwiftTermBridge/Bridge.swift");
    println!("cargo:rerun-if-changed=swift-bridge/Package.swift");

    if real {
        build_real_bridge(&manifest);
    } else {
        build_stub_bridge(&manifest, &out_dir);
    }
}

/// DEFAULT: `swiftc -emit-library StubBridge.swift -o $OUT_DIR/libswifttermbridge.dylib`.
fn build_stub_bridge(manifest: &Path, out_dir: &Path) {
    let src = manifest.join("swift-embed/StubBridge.swift");
    let dylib = out_dir.join("libswifttermbridge.dylib");

    let status = Command::new("swiftc")
        .arg("-emit-library")
        .arg("-O")
        // Embed an rpath-relative install name so the linked binary can find it
        // via the rpath we add below.
        .arg("-Xlinker").arg("-install_name")
        .arg("-Xlinker").arg("@rpath/libswifttermbridge.dylib")
        .arg(&src)
        .arg("-o")
        .arg(&dylib)
        .status()
        .expect("failed to spawn swiftc — is the Swift toolchain installed?");
    assert!(status.success(), "swiftc failed to build the stub bridge dylib");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=swifttermbridge");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", out_dir.display());
    println!("cargo:warning=phase0-poc: linked HEADLESS STUB bridge (no SwiftTerm/Metal). \
              Set NICE_POC_REAL_BRIDGE=1 to build the real SwiftTerm Metal bridge.");
}

/// REAL: `swift build -c release` of swift-bridge/, then link the dynamic product.
fn build_real_bridge(manifest: &Path) {
    let pkg = manifest.join("swift-bridge");
    let config = "release";

    let status = Command::new("swift")
        .arg("build")
        .arg("-c").arg(config)
        .arg("--package-path").arg(&pkg)
        // Build only the bridge product; SwiftTerm is pulled as a dependency.
        .arg("--product").arg("SwiftTermBridge")
        .status()
        .expect("failed to spawn `swift build` for the real bridge");
    assert!(
        status.success(),
        "`swift build` of swift-bridge failed. If this is a dependency-resolution \
         error, the SwiftTerm fork's transitive SwiftPM deps must be reachable \
         (they are normally cached under ~/Library/Caches/org.swift.swiftpm)."
    );

    // SwiftPM puts the dynamic product + the SwiftTerm resource bundle here.
    let build_dir = pkg.join(".build").join(config);
    let dylib = build_dir.join("libSwiftTermBridge.dylib");
    assert!(
        dylib.exists(),
        "expected {} after `swift build`; not found",
        dylib.display()
    );

    println!("cargo:rustc-link-search=native={}", build_dir.display());
    println!("cargo:rustc-link-lib=dylib=SwiftTermBridge");
    // rpath the build dir so BOTH the dylib AND the adjacent
    // SwiftTerm_SwiftTerm.bundle (Metal shaders) are found at run time.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", build_dir.display());
    // Swift runtime lives on the default dyld path on macOS; add the toolchain
    // lib dir defensively in case of a non-default toolchain.
    if let Some(swift_lib) = swift_runtime_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", swift_lib);
    }
    println!(
        "cargo:warning=phase0-poc: linked REAL SwiftTerm Metal bridge from {}. \
         The SwiftTerm_SwiftTerm.bundle must remain next to the dylib for \
         st_set_use_metal to succeed.",
        build_dir.display()
    );
}

/// Best-effort: `<toolchain>/usr/lib/swift/macosx` for an rpath fallback.
fn swift_runtime_dir() -> Option<String> {
    let out = Command::new("xcrun")
        .arg("--find").arg("swift")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let swift_bin = String::from_utf8(out.stdout).ok()?;
    let swift_bin = swift_bin.trim();
    // .../usr/bin/swift -> .../usr/lib/swift/macosx
    let usr = Path::new(swift_bin).parent()?.parent()?;
    let lib = usr.join("lib").join("swift").join("macosx");
    lib.exists().then(|| lib.display().to_string())
}
