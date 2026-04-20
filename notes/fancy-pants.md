# Add "Terminal translucency" toggle

## Context

The macOS / Catppuccin chrome palettes already give the sidebar wallpaper-blur through an `NSVisualEffectView` (`Sources/Nice/Views/SidebarBackground.swift`). The user wants the same treatment available in the terminal area as an opt-in toggle — so the terminal pane becomes a translucent pane over a `.behindWindow` vibrant material, same as the sidebar. Applies to both terminal kinds (main terminal + Claude pane, both `NiceTerminalView`). The user also reminded us we own a SwiftTerm fork (`~/Projects/SwiftTerm`) so if SwiftTerm's cell renderer draws backgrounds opaquely and fights transparency, we can fix upstream. 

## Approach

Two layered changes, both gated on a new `Tweaks.terminalTranslucency: Bool` (default off):

1. **Behind the terminal pane**: replace the solid `terminalBackgroundColor` fill in `AppShellView.mainContent` and `AppShellView.windowBackground` with a `ZStack { VisualEffectView(.sidebar, .behindWindow) ; terminalBackgroundColor.opacity(0.75) }` when the toggle is on. Same pattern as `SidebarBackground`.

2. **Inside the terminal**: when applying the terminal theme in `TabPtySession.applyTerminalTheme(_:to:)`, reduce `nativeBackgroundColor`'s alpha to ~0.75 so SwiftTerm's own layer composites over the VisualEffectView behind it. Off ⇒ alpha 1.0 (current behavior).

The toggle plumbs the standard Tweaks → AppState → TabPtySession → NiceTerminalView path already used by `gpuRendering` and `smoothScrolling` (same caching, same `viewDidMoveToWindow` re-apply, same fan-out on change). Fresh-install default is `false` — translucency is a taste toggle, not everyone wants wallpaper bleeding through their terminal.

## SwiftTerm compatibility check

Before shipping, verify SwiftTerm's cell renderer composites correctly when `nativeBackgroundColor` has alpha < 1. Exploration found `layer.backgroundColor = nativeBackgroundColor.cgColor` at `MacTerminalView.swift:431` and an internal `withAlphaComponent(0.9)` use at line 1300, which is encouraging. Risk: SwiftTerm draws cell backgrounds (for SGR-modified cells) as solid fills, and the default-bg path may also paint opaquely per-cell rather than relying on the layer. If the empirical test shows stripes / opaque default cells, the fix lives in our fork at `~/Projects/SwiftTerm`:

- Find the cell-draw path in `Sources/SwiftTerm/Mac/MacTerminalView.swift` / the Metal renderer.
- Short-circuit the "default background" fill when `nativeBackgroundColor` has alpha < 1, letting the layer's own background (which already shows through via the VisualEffectView) handle it.
- Keep SGR-background cells (explicit `[41m` etc) painted opaque — they're intentional colors.

Only touch the fork if the empirical test fails. Record what was changed in the fork and the SHA in the nice `Package.resolved` after rebasing.

## Files to modify

### 1. `Sources/Nice/State/Tweaks.swift`

- Add key constant next to `smoothScrollingKey`:
  ```swift
  static let terminalTranslucencyKey = "terminalTranslucency"
  ```
- Add `@Published` property alongside `smoothScrolling` (Tweaks.swift:263):
  ```swift
  @Published var terminalTranslucency: Bool {
      didSet { UserDefaults.standard.set(terminalTranslucency, forKey: Self.terminalTranslucencyKey) }
  }
  ```
- Seed in `init()` (near the `smooth` loader, Tweaks.swift:320):
  ```swift
  let translucency: Bool = defaults.bool(forKey: Self.terminalTranslucencyKey)
  // absent key → false (default off)
  ```
  Assign `self.terminalTranslucency = translucency` with the other assignments.

### 2. `Sources/Nice/State/AppState.swift`

- Add cache var next to `currentSmoothScrolling` (AppState.swift:129):
  ```swift
  private var currentTerminalTranslucency: Bool = false
  ```
- Seed from `tweaks.terminalTranslucency` in the init block that seeds other prefs (near AppState.swift:198).
- Add `updateTerminalTranslucency(_ enabled: Bool)` mirroring `updateGpuRendering`, fanning out to every live `TabPtySession`.
- Wire an `.onChange(of: tweaks.terminalTranslucency)` in AppShellHost/`AppShellView` alongside the existing GPU observer.

### 3. `Sources/Nice/Process/TabPtySession.swift`

- Add `currentTerminalTranslucency: Bool = false` cache alongside `currentGpuRendering`.
- Add `applyTerminalTranslucency(enabled:)` that stores the new value and calls `applyTerminalTheme(currentTerminalTheme)` to re-fan color with the new alpha.
- In `applyTerminalTheme(_:to:)` (TabPtySession.swift:328), change:
  ```swift
  view.nativeBackgroundColor = theme.background.nsColor
  ```
  to:
  ```swift
  let bg = theme.background.nsColor
  view.nativeBackgroundColor = currentTerminalTranslucency
      ? bg.withAlphaComponent(0.75)
      : bg
  ```
  Mirror for `nicePanelNS` if the Claude-pane variant ever gets a distinct bg (currently uses the same path).

### 4. `Sources/Nice/Views/AppShellView.swift` (lines 405-440 + 444-468)

Introduce a small view builder mirroring `SidebarBackground`:

```swift
@ViewBuilder
private func terminalAreaBackground<Content: View>(
    @ViewBuilder content: () -> Content
) -> some View {
    if tweaks.terminalTranslucency {
        content()
            .background(
                ZStack {
                    VisualEffectView(
                        material: .sidebar,
                        blendingMode: .behindWindow,
                        state: .active
                    )
                    terminalBackgroundColor.opacity(0.75)
                }
            )
    } else {
        content().background(terminalBackgroundColor)
    }
}
```

Then:
- `mainContent` line 460 — replace `.background(terminalBackgroundColor)` with the helper above (wrap the TerminalHost chain).
- `windowBackground` line 425 — replace the solid `terminalBackgroundColor` fill in the VStack with the helper too. Can refactor into a shared computed view or duplicate the ZStack literal.

Reuse the existing `VisualEffectView` at `Sources/Nice/Views/VisualEffectView.swift` — no need to add another.

### 5. `Sources/Nice/Process/NiceTerminalView.swift` (optional)

For `gpuRendering` and `smoothScrolling` we plumb a preference provider through so the view can re-check on `viewDidMoveToWindow`. For translucency we don't need this — the alpha is applied via `nativeBackgroundColor`, which is already set by `TabPtySession` on attach and on every theme fan-out. Skip this file.

### 6. `Sources/Nice/Views/SettingsView.swift` (after line 268)

Add a new `SettingRow` after "Smooth scrolling" mirroring its toggle shape:

```swift
SettingRow(
    label: "Terminal translucency",
    hint: "Blur the desktop behind the terminal area, like the sidebar."
) {
    Toggle("", isOn: $tweaks.terminalTranslucency)
        .labelsHidden()
        .toggleStyle(.switch)
        .controlSize(.small)
        .accessibilityIdentifier("settings.appearance.terminalTranslucency")
}
```

### 7. Tests

`Tests/NiceUnitTests/TweaksTerminalResolverTests.swift` — add a persistence test mirroring `test_chromePaletteChange_persistsToNewKeys`:

```swift
func test_terminalTranslucency_persists() {
    let tweaks = makeTweaks()
    tweaks.terminalTranslucency = true
    XCTAssertTrue(
        UserDefaults.standard.bool(forKey: Tweaks.terminalTranslucencyKey)
    )
}
```

No palette-color test needed — this is a boolean toggle, not a palette value.

## Risks

- **SwiftTerm cell rendering** — see "SwiftTerm compatibility check" above. If the empirical test shows opaque cell backgrounds, fix in `~/Projects/SwiftTerm`.
- **Scrolling perf** — a translucent terminal makes the GPU composite a blurred background every frame. Probably fine on Apple Silicon, may tax Intel Macs. Mitigation: the toggle is opt-in; users who feel lag turn it off.
- **Toolbar discontinuity** — this change leaves the 52pt toolbar above the terminal opaque (`niceChrome`) while the terminal area is translucent, creating a hard color boundary at y=52. User asked specifically about "terminal area", so this is in scope. Follow-up if they want the toolbar blurred too.
- **Scrollbar visibility** — scrollbars are already hidden when disabled (`TerminalHost.swift:55-62`), so no interaction.

## Verification

1. `scripts/worktree-lock.sh acquire catppuccin-translucency && scripts/install.sh && scripts/worktree-lock.sh release`.
2. Launch Nice.app. Settings → Appearance: new "Terminal translucency" toggle visible, default off.
3. Enable it. Expected: terminal area behind active pane shows wallpaper blur; text stays crisp; prompt / Claude output remain legible.
4. Drag the window over a colored desktop area — tint should shift subtly (confirms `.behindWindow`).
5. Disable it — terminal returns to opaque theme background.
6. Relaunch the app with translucency on — state persists.
7. Test with each chrome palette (Nice, macOS, Catppuccin Latte, Catppuccin Mocha) to confirm tinting stacks sensibly in all four.
8. Run `xcodebuild test … -only-testing:NiceUnitTests` to confirm persistence test + existing 145 tests still pass.
9. If SwiftTerm renders opaque cell stripes: fix in `~/Projects/SwiftTerm`, bump the Package.resolved SHA, re-test.

## Critical files

- `/Users/nick/Projects/nice/Sources/Nice/State/Tweaks.swift`
- `/Users/nick/Projects/nice/Sources/Nice/State/AppState.swift`
- `/Users/nick/Projects/nice/Sources/Nice/Process/TabPtySession.swift`
- `/Users/nick/Projects/nice/Sources/Nice/Views/AppShellView.swift`
- `/Users/nick/Projects/nice/Sources/Nice/Views/SettingsView.swift`
- `/Users/nick/Projects/nice/Sources/Nice/Views/SidebarBackground.swift` (reference only — mirror pattern)
- `/Users/nick/Projects/nice/Tests/NiceUnitTests/TweaksTerminalResolverTests.swift`
- Possibly: `~/Projects/SwiftTerm/Sources/SwiftTerm/Mac/MacTerminalView.swift` if cell rendering is opaque.
