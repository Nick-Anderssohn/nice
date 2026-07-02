#!/usr/bin/env python3
"""Generate the deterministic AA/gamma test scene (scene.bin).

The SAME byte stream is fed to both terminals under test:
  * swiftterm-fixture (SwiftTerm fork's vt100 core + Metal renderer)
  * gpui-term-main / gpui-term-aa022 (alacritty_terminal core + GPUI renderer)

Design constraints:
  * 60 cols x 16 rows, every visible row <= 60 cells.
  * Cursor hidden up front (CSI ?25l) so it never pollutes the pixel diff.
  * Only ASCII + box-drawing (U+2500-257F) + block elements (U+2580-259F):
    width-1 in both wcwidth implementations, so cell layout cannot diverge.
    (No emoji / no East-Asian-ambiguous glyphs — those are a separate axis.)
  * Registration blocks (solid full-block runs) top-left and bottom-right so
    the diff tool's cross-correlation alignment has strong anchors.
  * Colors: default fg/bg (theme axis), the 16 ANSI colors (theme values),
    plus TRUECOLOR white-on-black / black-on-white rows that are
    theme-independent extremes for the luminance-curve comparison.
  * Row 13 (bold) and row 14 (underline) exercise font-selection and
    decoration paths — flagged in the per-row report as non-curve axes.
"""

import sys

ESC = "\x1b"
CSI = ESC + "["

ROWS = [
    # 0: registration anchor + title
    "████  NICE AA/GAMMA SCENE v1  ████",
    # 1-2: mixed ASCII
    "the quick brown fox jumps over the lazy dog 0123456789",
    "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG @#$%&*()[]{}",
    # 3: thin strokes
    "il1| Il1| i1l| |||| iiii llll 1111 .... ,,,, :::: ;;;; ''",
    # 4-5: box drawing + block elements
    "┌───┬───┐ ╔═══╗ ░░▒▒▓▓ ▀▄▌▐ ├┼┤",
    "│box│drw│ ╚═══╝ ┏━┓┗━┛ ╭─╮╰─╯ └┴─┘",
    # 6: white-on-black (ANSI 7/15 on ANSI 0)
    CSI + "37;40m white on black " + CSI + "97;40m BRIGHT white on black " + CSI + "0m",
    # 7: black-on-white (ANSI 0 on ANSI 7/15)
    CSI + "30;47m black on white " + CSI + "30;107m black on BRIGHT white " + CSI + "0m",
    # 8: truecolor extremes (theme-independent)
    CSI + "38;2;255;255;255m" + CSI + "48;2;0;0;0m #ffffff on #000000 " + CSI + "0m "
    + CSI + "38;2;0;0;0m" + CSI + "48;2;255;255;255m #000000 on #ffffff " + CSI + "0m",
    # 9-10: ANSI fg colors on default bg
    CSI + "31mred " + CSI + "32mgreen " + CSI + "33myellow " + CSI + "34mblue "
    + CSI + "35mmagenta " + CSI + "36mcyan" + CSI + "0m",
    CSI + "91mRED " + CSI + "92mGREEN " + CSI + "93mYELLOW " + CSI + "94mBLUE "
    + CSI + "95mMAGENTA " + CSI + "96mCYAN" + CSI + "0m",
    # 11: fg-on-bg pairs
    CSI + "33;44m yellow/blue " + CSI + "0m " + CSI + "31;42m red/green " + CSI + "0m "
    + CSI + "37;45m white/magenta " + CSI + "0m",
    # 12: inverse video
    CSI + "7m inverse video sample " + CSI + "0m normal tail il1|",
    # 13: bold (font-selection axis, not the curve — flagged in report)
    CSI + "1mbold: quick brown fox il1| 0123456789" + CSI + "0m",
    # 14: underline (decoration axis — flagged in report)
    CSI + "4munderline sample il1|" + CSI + "0m plain tail il1|",
    # 15: bottom registration anchor
    ".......... end of scene " + CSI + "0m████",
]


def visible_len(s: str) -> int:
    """Length in cells with CSI sequences stripped (all scene glyphs are width 1)."""
    out, i = 0, 0
    while i < len(s):
        if s[i] == ESC and i + 1 < len(s) and s[i + 1] == "[":
            i += 2
            while i < len(s) and not ("@" <= s[i] <= "~"):
                i += 1
            i += 1  # final byte
        else:
            out += 1
            i += 1
    return out


def main() -> None:
    for r, row in enumerate(ROWS):
        n = visible_len(row)
        assert n <= 60, f"row {r} too wide: {n}"
    stream = CSI + "?25l" + CSI + "H" + CSI + "2J"  # hide cursor, home, clear
    # Explicit cursor addressing per row — immune to convertEol differences.
    for r, row in enumerate(ROWS):
        stream += CSI + f"{r + 1};1H" + row
    # Park the (hidden) cursor at the bottom-right corner.
    stream += CSI + "16;60H"
    data = stream.encode("utf-8")
    out = sys.argv[1] if len(sys.argv) > 1 else "scene.bin"
    with open(out, "wb") as f:
        f.write(data)
    print(f"wrote {out}: {len(data)} bytes, {len(ROWS)} rows x <=60 cols")


if __name__ == "__main__":
    main()
