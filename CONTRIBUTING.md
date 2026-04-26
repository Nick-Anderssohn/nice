# Contributing to Nice

Thanks for your interest in contributing! Nice is a young project maintained by one person — contributions are welcome, and the bar for small improvements is low.

## Before you start

For bug fixes and small, self-contained improvements, feel free to open a PR directly.

For anything non-trivial — new features, significant refactors — please open an issue or a draft PR first to align on direction before investing time in implementation. Not every PR will be accepted, and that's okay; it's better to talk before writing a lot of code.

## Dev setup

**Requirements:**

- macOS 14 (Sonoma) or later
- Xcode 26.4 (the version pinned in `.github/workflows/release.yml`)
- [XcodeGen](https://github.com/yonaskolb/XcodeGen): `brew install xcodegen`

The Xcode project (`Nice.xcodeproj`) is generated from `project.yml` via XcodeGen and is not committed to the repo. Run `xcodegen generate` after cloning and after pulling changes that touch `project.yml`.

## Build and test

Nice ships two parallel app bundles — `Nice.app` (production) and `Nice Dev.app` (development target). The scripts below default to the dev variant, so they won't touch your working install.

```sh
# Install Nice Dev to /Applications
scripts/install.sh

# Run the test suite against the dev bundle ID
scripts/test.sh
```

See [`CLAUDE.md`](CLAUDE.md) for the full rationale behind the two-build split and the worktree locking protocol.

## Code style

Nice is written in Swift 6 with strict concurrency enabled. CI enforces this — violations will fail the build. There is no SwiftLint config; please match the surrounding style.

## PR checklist

- Tests pass locally (`scripts/test.sh`)
- New behaviour has tests where reasonable
- PRs are small and focused — one concern per PR
- No force-pushes once review has started

## What to expect

Nice is maintained by [@Nick-Anderssohn](https://github.com/Nick-Anderssohn) as a solo project. Response times are best-effort and the final merge decision rests with the maintainer. If a PR isn't accepted it's not personal — direction choices for a young project can be hard to explain in full.

## Bugs and security

Use the [bug report template](https://github.com/Nick-Anderssohn/nice/issues/new?template=bug_report.yml) to file bugs.

For security issues, **do not** open a public issue. See [`SECURITY.md`](SECURITY.md) for how to report privately.
