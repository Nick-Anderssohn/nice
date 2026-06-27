use std::path::PathBuf;

fn main() {
    // The Swift-built dylib lives next to this build script.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-link-search=native={}", dir.display());
    // Link the Swift-produced terminal-view library by name.
    println!("cargo:rustc-link-lib=dylib=swifttermstub");
    // So the binary can find it (and, transitively, the OS Swift runtime) at run time.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    println!("cargo:rerun-if-changed=libswifttermstub.dylib");
}
