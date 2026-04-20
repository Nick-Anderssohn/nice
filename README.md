# Nice

> **Never lose track of a Claude session again.**

A native macOS terminal that organizes your Claude Code sessions for you. Run `claude` anywhere — Nice spawns it in a fresh pty and files it under the right project in the sidebar. No config, no setup, no "new tab" dance.

```sh
brew install --cask Nick-Anderssohn/nice/nice
```

<p align="center">
  <img src="docs/images/nice-mocha.png" alt="Nice running on the Catppuccin Mocha terminal theme">
</p>

## Auto-organized sessions

You don't file your Claude sessions. Nice does.

Type `claude` at any shell, from any project directory — a new tab opens in Nice, grouped under that project, running in its own long-lived pty with a plain `zsh` pane alongside. Walk away, come back hours later, and the session is exactly where you left it. The projects in the sidebar are the working directories you actually use, populated as you go.

It's the way `claude` was meant to live: always open, already where it should be, never guessing which window it's in.

## Themes that look like you want them to

Twelve built-in terminal themes — Catppuccin (all four), Dracula, Nord, Gruvbox, Tokyo Night, Solarized, Atom One, and more — plus five native-chrome accents (Terracotta, Ocean, Fern, Iris, Graphite). Switch live from Settings; the whole window repaints instantly.

Already have a [Ghostty](https://ghostty.org) theme you love? Nice reads Ghostty's theme file format directly. Drop it in and it's a one-click swap.

<table>
  <tr>
    <td width="50%"><img src="docs/images/nice-latte.png" alt="Catppuccin Latte"></td>
    <td width="50%"><img src="docs/images/nice-mocha.png" alt="Catppuccin Mocha"></td>
  </tr>
  <tr>
    <td align="center"><sub><b>Catppuccin Latte</b></sub></td>
    <td align="center"><sub><b>Catppuccin Mocha</b></sub></td>
  </tr>
</table>

## Keyboard-first

| Shortcut | Action |
|---|---|
| `⌘⌥↓` / `⌘⌥↑` | Next / previous sidebar tab |
| `⌘⌥→` / `⌘⌥←` | Next / previous pane within a tab |
| `⌘T` | New terminal pane |
| `⌘B` | Toggle sidebar |
| `⌘=` / `⌘-` / `⌘0` | Zoom in, out, reset |

All rebindable in Settings (`⌘,`).

## Requirements

- macOS 14 (Sonoma) or later
- [Claude Code](https://github.com/anthropics/claude-code) on your `$PATH` — optional; tabs fall back to a plain `zsh` if it's missing

## Install

```sh
brew install --cask Nick-Anderssohn/nice/nice
```

Signed, notarized, universal (Apple Silicon + Intel). `brew upgrade --cask nice` picks up new releases; `brew uninstall --cask --zap nice` removes the app and wipes its settings.

## Credits

Designed at [claude.ai/design](https://claude.ai/design). Terminal rendering by [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm).
