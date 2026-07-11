#!/bin/zsh
# render-test.sh — visual smoke test for Nice RS terminal rendering.
# Exercises the paint paths the run-batching + quad-merging changes touch:
# full-width rules, block/shade glyphs (merged band quads), box-drawing
# (per-cell), and contiguous same-style colored text runs. Run it in Nice RS
# and (for comparison) in prod Nice; the two should look the same.
#
#   zsh ~/render-test.sh
#
# Works in zsh or bash. Uses the current terminal width for full-width rows.

cols=${COLUMNS:-$(tput cols 2>/dev/null || echo 100)}
e() { printf '\033[%sm' "$1"; }          # emit SGR
reset() { printf '\033[0m'; }
line() { printf "$1%.0s" $(seq 1 "$cols"); printf '\n'; }   # repeat $1 to full width
sect() { printf '\n\033[1;7;38;5;15m %s \033[0m\n' "$1"; }

clear

sect "1. FULL-WIDTH RULES  (x-uniform → merged band quads; lever A)"
for spec in "light:─" "heavy:━" "double:═" "dash:┄" "lower:▁" "upper:▔"; do
  name=${spec%%:*}; ch=${spec##*:}
  printf '%-8s ' "$name"; line "$ch"
done
printf 'colored  '; e '38;5;196'; line '━'
printf 'colored  '; e '38;5;46';  line '═'; reset

sect "2. BLOCK ELEMENTS & SHADE BARS  (x-uniform → merged; lever A)"
printf 'blocks   '; for c in ▁ ▂ ▃ ▄ ▅ ▆ ▇ █; do printf '%s%s%s%s%s%s' "$c" "$c" "$c" "$c" "$c" "$c"; done; printf '\n'
printf 'shades   '; e '38;5;33'; line '░'
printf 'shades   '; e '38;5;33'; line '▒'
printf 'shades   '; e '38;5;33'; line '▓'
printf 'shades   '; e '38;5;33'; line '█'; reset
printf 'ramp     '; for g in 0 3 7 11 15 19 23; do e "48;5;$((232+g))"; printf '        '; done; reset; printf '\n'

sect "3. BAR CHART  (colored █ runs of varying length: merged quads + color runs)"
draw_bar() { e "38;5;$2"; printf "█%.0s" $(seq 1 "$1"); reset; printf ' %s\n' "$3"; }
draw_bar 42 196 "cpu"
draw_bar 30 46  "mem"
draw_bar 55 33  "net"
draw_bar 18 226 "disk"
draw_bar 64 129 "gpu"

sect "4. BOX DRAWING  (─ merges; corners/tees stay per-cell — alignment test)"
printf '┌────────────┬────────────┬────────────┐\n'
printf '│ %-10s │ %-10s │ %-10s │\n' "left" "center" "right"
printf '├────────────┼────────────┼────────────┤\n'
printf '│ %-10s │ %-10s │ %-10s │\n' "aaa" "bbbbb" "ccccccc"
printf '│ %-10s │ %-10s │ %-10s │\n' "1" "22" "333"
printf '└────────────┴────────────┴────────────┘\n'
printf '  rounded ╭──────────╮   thick ┏━━━━━━━━━━┓   double ╔══════════╗\n'
printf '          │  inside  │         ┃  inside  ┃          ║  inside  ║\n'
printf '          ╰──────────╯         ┗━━━━━━━━━━┛          ╚══════════╝\n'

sect "5. ANSI 16 COLORS  (fg then bg swatches)"
printf 'fg  '; for c in 0 1 2 3 4 5 6 7; do e "3$c"; printf ' %d ' "$c"; done
for c in 0 1 2 3 4 5 6 7; do e "9$c"; printf ' %d ' "$((c+8))"; done; reset; printf '\n'
printf 'bg  '; for c in 0 1 2 3 4 5 6 7; do e "4$c"; printf '    '; done
for c in 0 1 2 3 4 5 6 7; do e "10$c"; printf '    '; done; reset; printf '\n'

sect "6. 256-COLOR CUBE  (6×6×6 ramp; contiguous same-color cells → runs)"
for g in 0 1 2 3 4 5; do
  for r in 0 1 2 3 4 5; do
    for b in 0 1 2 3 4 5; do
      e "48;5;$((16 + 36*r + 6*g + b))"; printf ' '
    done
  done
  reset; printf '\n'
done

sect "7. TRUECOLOR GRADIENT  (per-cell color change = worst case for run batching)"
w=$(( cols < 76 ? cols : 76 ))
for row in 0 1; do
  for ((i=0; i<w; i++)); do
    t=$(( i * 255 / (w-1) ))
    if [ "$row" -eq 0 ]; then e "48;2;$t;$((255-t));128"; else e "48;2;128;$t;$((255-t))"; fi
    printf ' '
  done
  reset; printf '\n'
done

sect "8. TEXT STYLES  (each forces its own style run)"
e 1;  printf 'bold '; reset
e 2;  printf 'dim '; reset
e 3;  printf 'italic '; reset
e 4;  printf 'underline '; reset
e 9;  printf 'strike '; reset
e 7;  printf ' reverse '; reset
e '4;3'; printf ' underline+italic '; reset
e '1;4;38;5;208'; printf ' bold+underline+orange '; reset
printf '\n'

sect "9. RUN-BATCHING BOUNDARIES"
printf 'one long same-style run: '; e '38;5;34'; printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'; reset; printf '\n'
printf 'alternating style/cell:  '
i=0; for ch in {A..Z} {a..n}; do e "38;5;$((196 + i % 24))"; printf '%s' "$ch"; i=$((i+1)); done; reset; printf '\n'
printf 'per-cell bg change:      '
i=0; for ch in {A..Z} {a..n}; do e "48;5;$((17 + i))"; printf '%s' "$ch"; i=$((i+1)); done; reset; printf '\n'

sect "10. WIDE GLYPHS & NON-ASCII  (break runs; wide-spacer handling)"
printf 'CJK    ascii 你好世界 more ascii 日本語 end\n'
printf 'emoji  start 🎨 middle 🔥 tail 🚀 done ✅\n'
printf 'mixed  a1你b2好c3—d4│e5░\n'

printf '\n'; e '38;5;46'; printf '── END OF RENDER TEST '; e '38;5;239'; line '─'; reset
