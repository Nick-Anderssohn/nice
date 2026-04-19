# Nice

A native macOS GUI that sits in front of the [`claude`](https://github.com/anthropics/claude-code) CLI. Each sidebar tab is a long-lived Claude Code session running in a pseudo-terminal; the companion pane on the right is a `zsh` rooted in the same working directory. You open a new tab the same way you'd start Claude anywhere else — by typing `claude …` at the Main Terminal prompt — and an in-process MCP server lets a running Claude switch tabs, list tabs, and run shell commands in its own companion terminal, so voice control "just works" via OS dictation into the active Claude.

## Status

Early but functional. The app runs end-to-end, native controls respect the user's accent, SwiftTerm panes theme to match `niceBg3`, "Launch at login" is wired to `SMAppService`, typing `claude …` in the Main Terminal opens a new tab with the args passed through, and an automated MCP smoke test (`scripts/mcp-e2e.sh`) covers the three `nice.*` tools.

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
│                   projects, activeTabId, pty sessions, MCP handle
├─ Process layer
│   TabPtySession   two LocalProcessTerminalViews per tab:
│                   - claude (middle) spawned with --mcp-config
│                   - zsh    (right)
│   MainTerminalSession  singleton zsh for the "Main terminal" row
└─ MCP server
    NiceMCPServer    swift-sdk 0.12 Server actor
    NiceHTTPBridge   NWListener HTTP/1.1 + SSE, speaks to
                     StatefulHTTPServerTransport on 127.0.0.1:7420
```

### MCP tools

| Name | Arguments | Effect |
|---|---|---|
| `nice.tab.switch` | `tabId?` or `titleQuery?` (fuzzy) | Focuses a tab |
| `nice.tab.list` | — | Returns all tabs across all projects |
| `nice.run` | `tabId?`, `command` | Writes `command + "\n"` into that tab's `zsh` |

Tab creation is deliberately not an MCP tool — see **Opening tabs** below.

Every spawned `claude` gets a temp `.mcp.json` threaded in via `--mcp-config`:

```json
{ "mcpServers": { "nice": { "url": "http://127.0.0.1:7420" } } }
```

## Stack

- **UI:** SwiftUI
- **Terminals:** [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) (`LocalProcessTerminalView`)
- **MCP server:** [swift-sdk](https://github.com/modelcontextprotocol/swift-sdk) 0.12 with a custom `NWListener` HTTP/SSE bridge (the SDK's transport is framework-agnostic)

## Customization

Settings (`⌘,`):
- **Appearance** — theme (Match system / Light / Dark) and one of 5 accent presets: Terracotta (default), Ocean, Fern, Iris, Graphite. Changes apply live.
- **General** — main terminal working directory
- **MCP** — live server status + exposed tools

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

- **Voice button.** Out of scope — OS dictation into the active Claude reaches the same MCP tools (`nice.tab.switch`, etc.) without needing a microphone UI or hotkey-managed speech pipeline in Nice itself.
- **Mac App Store distribution.** Blocked by the sandbox requirement — the App Sandbox forbids spawning child processes via pty.

## Testing the MCP surface

`scripts/mcp-e2e.sh` is a smoke test that simulates what a Claude running inside a tab does: it performs the MCP `initialize` handshake, asserts the three `nice.*` tools are advertised, and confirms `nice.tab.list` returns a coherent JSON array. Tab creation itself is out of scope for this script — it happens off the MCP surface, via the shadowed `claude()` function in the Main Terminal.

```sh
open ~/Library/Developer/Xcode/DerivedData/Nice-*/Build/Products/Debug/Nice.app
./scripts/mcp-e2e.sh
```

Requires `curl` and `jq`. Non-zero exit means a check failed.

## Credits

- Design: mocked in [claude.ai/design](https://claude.ai/design)
- Terminal rendering: [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) by Miguel de Icaza
- MCP server: [modelcontextprotocol/swift-sdk](https://github.com/modelcontextprotocol/swift-sdk)
