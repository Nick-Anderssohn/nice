// swift-tools-version:5.9
// AA/gamma spike (rank-1) — SwiftTerm reference fixture.
//
// Hosts the SwiftTerm fork's Metal terminal renderer with Nice's EXACT
// shipping config, feeds it the deterministic scene bytes (aa-gamma/scene/
// scene.bin), reads back the presented CAMetalLayer drawable, writes PNG +
// meta.json, exits. See Sources/swiftterm-fixture/main.swift and
// ../aa-gamma/RUNBOOK.md.
//
// NOTE: depends on the LOCAL SwiftTerm fork checkout by absolute path
// (machine-specific, spike-only). That checkout must stay READ-ONLY on
// branch phase0-txn-present @ 583551f = Nice's pinned rev 5f07dc6 (project.yml)
// + 2 commits that do not change default rendering (a docs commit and the
// OFF-by-default transactional-present opt-in).
import PackageDescription

let package = Package(
    name: "swiftterm-fixture",
    platforms: [
        .macOS(.v14)
    ],
    dependencies: [
        .package(path: "/Users/nick/Projects/SwiftTerm")
    ],
    targets: [
        .executableTarget(
            name: "swiftterm-fixture",
            dependencies: [
                .product(name: "SwiftTerm", package: "SwiftTerm")
            ]
        )
    ]
)
