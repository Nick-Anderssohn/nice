# Nice

A native macOS GUI that sits in front of the [`claude`](https://github.com/anthropics/claude-code) CLI. Each sidebar tab is a long-lived Claude Code session running in a pseudo-terminal; the companion pane on the right is a `zsh` rooted in the same working directory. An in-process MCP server lets a running Claude spawn sibling tabs, switch tabs, list tabs, and run shell commands in its own companion terminal — so voice control "just works" via OS dictation into the active Claude.

## Status

Early but functional. All six scaffold phases are landed, the app runs end-to-end, native controls respect the user's accent, SwiftTerm panes theme to match `niceBg3`, "Launch at login" is wired to `SMAppService`, and an automated MCP smoke test (`scripts/mcp-e2e.sh`) covers the Claude-in-a-tab → `nice.tab.new` loop.

## Requirements

- macOS 14 (Sonoma) or later
- Xcode 16+ (Swift 6)
- [`claude`](https://github.com/anthropics/claude-code) on your `$PATH` (optional — tabs fall back to `zsh` in the chat pane if missing)
- [XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`) to regenerate the project from `project.yml`

## Build & run

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
| `nice.tab.new` | `title?`, `cwd?`, `project?` | Creates a tab, returns `tabId` |
| `nice.tab.switch` | `tabId?` or `titleQuery?` (fuzzy) | Focuses a tab |
| `nice.tab.list` | — | Returns all tabs across all projects |
| `nice.run` | `tabId?`, `command` | Writes `command + "\n"` into that tab's `zsh` |

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

## Non-goals

- **Voice button.** Out of scope — OS dictation into the active Claude reaches the same MCP tools (`nice.tab.switch`, `nice.tab.new`, etc.) without needing a microphone UI or hotkey-managed speech pipeline in Nice itself.
- **Mac App Store distribution.** Blocked by the sandbox requirement — the App Sandbox forbids spawning child processes via pty.

## Testing the MCP surface

`scripts/mcp-e2e.sh` is a smoke test that simulates what a Claude running inside a tab does: it performs the MCP `initialize` handshake, asserts the four `nice.*` tools are advertised, calls `nice.tab.new`, and confirms the new tab shows up in `nice.tab.list`.

```sh
open ~/Library/Developer/Xcode/DerivedData/Nice-*/Build/Products/Debug/Nice.app
./scripts/mcp-e2e.sh
```

Requires `curl` and `jq`. Non-zero exit means a check failed.

## Credits

- Design: mocked in [claude.ai/design](https://claude.ai/design)
- Terminal rendering: [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) by Miguel de Icaza
- MCP server: [modelcontextprotocol/swift-sdk](https://github.com/modelcontextprotocol/swift-sdk)
