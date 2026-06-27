// swift-tools-version:5.9
//
// SwiftTermBridge — the REAL bridge for the Phase-0 PoC.
//
// This SwiftPM package path-depends on the SwiftTerm fork and exposes the
// `MacTerminalView` (the genuine Metal-backed terminal NSView) over a C ABI
// (`@_cdecl`) so the Rust PoC can `addSubview:` it under GPUI chrome and drive
// it through a live responder chain.
//
// It builds as a DYNAMIC library so cargo/cc can link the `st_*` / `nice_*`
// symbols into the Rust binary by name. SwiftTerm is statically linked into the
// resulting `libSwiftTermBridge.dylib`; the Metal-shaders resource bundle
// `SwiftTerm_SwiftTerm.bundle` lands next to the dylib in `.build/<config>/`.
//
// IMPORTANT (runtime): `Bundle(for: MetalTerminalRenderer.self)` resolves the
// Metal shader library relative to the dylib's location. The PoC therefore
// rpaths `.build/<config>/` (see ../build.rs) so both the dylib and its
// adjacent resource bundle are found at load time. If `st_set_use_metal`
// returns 0, the bundle is missing next to the dylib — see ../README.md §Caveats.

import PackageDescription

let package = Package(
    name: "SwiftTermBridge",
    // .v14 for NSView.displayLink(target:selector:) — the decoupled present loop
    // (st_start_present_link). SwiftTerm depends at .v13, so this is compatible.
    platforms: [ .macOS(.v14) ],
    products: [
        .library(name: "SwiftTermBridge", type: .dynamic, targets: ["SwiftTermBridge"]),
    ],
    dependencies: [
        // EXACT path dependency on the read-only fork pinned in the bridge spec.
        .package(name: "SwiftTerm", path: "/Users/nick/Projects/SwiftTerm"),
    ],
    targets: [
        .target(
            name: "SwiftTermBridge",
            dependencies: [
                .product(name: "SwiftTerm", package: "SwiftTerm"),
            ]
        ),
    ]
)
