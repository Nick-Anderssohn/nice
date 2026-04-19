# Why `TERM_PROGRAM=ghostty` for spawned Claude panes

`Sources/Nice/Process/TabPtySession.swift:130` sets `TERM_PROGRAM=ghostty`
when spawning Claude Code. Claude Code gates OSC title emission on
`TERM_PROGRAM ∈ {iTerm.app, ghostty, WezTerm, Apple_Terminal}`, so we
need one of those four. The question came up: since SwiftTerm is
closer to iTerm2 than to ghostty, should we advertise as `iTerm.app`
instead?

Answer: no. Switching to `iTerm.app` would cause Claude Code to emit a
larger surface of iTerm-specific OSC sequences that SwiftTerm only
partially handles, with no user-visible gain.

## SwiftTerm's iTerm OSC coverage

Dispatch table:
`build/SourcePackages/checkouts/SwiftTerm/Sources/SwiftTerm/EscapeSequenceParser.swift:510-544`
Handlers: `.../SwiftTerm/Sources/SwiftTerm/Terminal.swift`

### Fully implemented

| OSC | Purpose |
|---|---|
| 0 / 1 / 2 | Window / icon title |
| 4 | ANSI color palette set/query |
| 6 | Current document URI |
| 7 | CWD (`file://`) |
| 8 | Hyperlinks |
| 10 / 11 / 12 | FG / BG / cursor color |
| 52 | Clipboard write (iTerm/xterm variant) |
| 104 | Reset color palette |
| 112 | Reset cursor color |
| **1337 `File=`** | Inline image protocol |

### Partially implemented

- **OSC 1337** — only `File=` (images) is acted on. `SetMark`,
  `CurrentDir=`, `RemoteHost=`, `ShellIntegrationVersion=`,
  `CopyToClipboard=` / `EndCopy`, `SetUserVar=`, `ReportVariable=`,
  `RequestAttention`, `StealFocus`, `ClearScrollback`, `CursorShape=`,
  `HighlightCursorLine=`, `SetBadgeFormat=`, `SetProfile=`,
  `ReportCellSize`, `CursorGuideColor`, etc. are routed to an
  `iTermContent` callback that the library itself drops.
- **OSC 9** — only `9;4;*` (ConEmu progress) is handled; iTerm growl
  notifications fall through.
- **OSC 777** — only the `777;notify;title;body` subformat is wired
  through to the `notify` delegate.

### Missing

- **OSC 133** — shell prompt marks (A/B/C/D). iTerm popularized these
  and Claude Code may emit them when it thinks it's talking to iTerm,
  but SwiftTerm has no handler; they fall through to
  `oscHandlerFallback`.

## Conclusion

Keep `TERM_PROGRAM=ghostty`. The OSC sequences Claude Code relies on
for title/CWD reporting work fine under `ghostty`, and we avoid the
partially-supported iTerm extension surface. The comment at
`TabPtySession.swift:124-129` could be tightened to name this reason
explicitly (SwiftTerm's partial OSC 1337 coverage and missing OSC 133).
