# Nice

A native macOS GUI that sits in front of the [`claude`](https://github.com/anthropics/claude-code) CLI. Each sidebar tab is a long-lived Claude Code session running in a pseudo-terminal; the companion pane on the right is a `zsh` rooted in the same working directory. You open a new tab the same way you'd start Claude anywhere else — by typing `claude …` at the Main Terminal prompt.

## Status

Early but functional. The app runs end-to-end, native controls respect the user's accent, SwiftTerm panes theme to match `niceBg3`, and typing `claude …` in the Main Terminal opens a new tab with the args passed through.

## Requirements

- macOS 14 (Sonoma) or later
- Xcode 16+ (Swift 6)
- [`claude`](https://github.com/anthropics/claude-code) on your `$PATH` (optional — tabs fall back to `zsh` in the chat pane if missing)
- [XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`) to regenerate the project from `project.yml`

## Install

To use Nice as a regular Mac app (Spotlight, Launchpad, Dock), install it into `/Applications`:

```sh
git clone https://github.com/Nick-Anderssohn/nice.git
cd nice
scripts/install.sh
```

The script builds Release, quits any running instance, and replaces `/Applications/Nice.app` in place — re-run it to upgrade. Settings (UserDefaults under `dev.nickanderssohn.nice`) survive an upgrade.

If you're running Claude Code inside the repo, the `/nice-install` slash command does the same thing and walks you through any missing prerequisites first.

To remove: `scripts/uninstall.sh` (add `--purge` to also wipe settings).

## Build & run (development)

```sh
git clone https://github.com/Nick-Anderssohn/nice.git
cd nice
xcodegen generate
open Nice.xcodeproj
# ⌘R in Xcode, or:
xcodebuild -project Nice.xcodeproj -scheme Nice -configuration Debug build -destination 'platform=macOS'
open ~/Library/Developer/Xcode/DerivedData/Nice-*/Build/Products/Debug/Nice.app
```

The app ships with **App Sandbox disabled** (`Resources/Nice.entitlements`) — required for spawning child processes via pty. Not distributable via the Mac App Store.

## Architecture

```
Nice.app                                    (single process)
├─ SwiftUI          3-column shell: Sidebar / Chat / Terminal
├─ AppState         @MainActor ObservableObject
│                   projects, activeTabId, pty sessions
└─ Process layer
    TabPtySession   two LocalProcessTerminalViews per tab:
                    - claude (middle)
                    - zsh    (right)
    MainTerminalSession  singleton zsh for the "Main terminal" row
```

Tab creation flows through a Unix-domain control socket — see **Opening tabs** below.

## Stack

- **UI:** SwiftUI
- **Terminals:** [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) (`LocalProcessTerminalView`)

## Customization

Settings (`⌘,`):
- **Appearance** — theme (Match system / Light / Dark) and one of 5 accent presets: Terracotta (default), Ocean, Fern, Iris, Graphite. Changes apply live.
- **General** — main terminal working directory

## Design

The look was mocked up in HTML/React at [claude.ai/design](https://claude.ai/design) and ported to SwiftUI 1:1. The terracotta-accent, 3-column Xcode-flavored aesthetic is a direct translation of that design; all dimensions, paddings, and animation curves were lifted from the CSS source.

## Opening tabs

The Main Terminal ships with a `claude` zsh function that intercepts interactive invocations and asks the app to open a new tab in its place:

```sh
claude                        # → new tab, claude starts fresh
claude "fix foo.swift"        # → new tab, claude gets that prompt as argv
cd ~/Projects/nice && claude  # → new tab rooted at the nice repo
```

Non-interactive runs stay on the Main Terminal — the function passes through to the real binary when you use `-p` / `--print`, info flags (`--version`, `--help`), subcommands (`claude mcp …`, `claude config …`, `claude update`), or piped stdin. The channel is a Unix-domain socket in `$TMPDIR`, set into the shell as `$NICE_SOCKET`; if the socket isn't reachable the function still falls back to running claude directly.

Regular tabs' right-side `zsh` is untouched — it's a plain interactive shell with no shadow.

## Non-goals

- **Mac App Store distribution.** Blocked by the sandbox requirement — the App Sandbox forbids spawning child processes via pty.

## Credits

- Design: mocked in [claude.ai/design](https://claude.ai/design)
- Terminal rendering: [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) by Miguel de Icaza
