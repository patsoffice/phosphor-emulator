#!/usr/bin/env sh
#
# Render every netlist in this directory to the SVG committed beside it.
# Run from the nix dev shell, which provides netlistsvg:
#
#     ./docs/schematics/render.sh
#
# netlistsvg auto-places and auto-routes through ELK, so nothing here places a
# symbol or routes a wire. It does emit an SVG that needs two fixes before it
# can go in a document:
#
#   - No viewBox. Input port labels are drawn to the left of x=0 and the
#     leftmost one is otherwise clipped. PAD has to exceed the longest label in
#     the first column, which is 6px per character at the 10px monospace the
#     default skin uses. Raise it per diagram if a label loses its first
#     letters; there is no measurement here that would do it automatically.
#
#   - No background. The geometry lives entirely in the file's <style> block,
#     which strokes black on a transparent ground, so the drawing disappears
#     against a dark theme. An opaque rect is cheaper than teaching the skin
#     about themes.
#
set -eu

cd "$(dirname "$0")"
PAD="${PAD:-40}"

for json in *.json; do
  svg="${json%.json}.svg"
  netlistsvg "$json" -o "$svg.tmp"
  awk -v pad="$PAD" '
    NR == 1 {
      match($0, /width="[0-9.]+"/);  w = substr($0, RSTART + 7, RLENGTH - 8)
      match($0, /height="[0-9.]+"/); h = substr($0, RSTART + 8, RLENGTH - 9)
      vw = w + pad + 8
      vh = h + 16
      sub(/width="[0-9.]+" height="[0-9.]+"/,
          "width=\"" vw "\" height=\"" vh "\" viewBox=\"-" pad " -8 " vw " " vh "\"")
      print
      next
    }
    /<\/style>/ {
      print
      printf "  <rect x=\"-%s\" y=\"-8\" width=\"%s\" height=\"%s\" fill=\"#fff\" stroke=\"none\"/>\n",
             pad, vw, vh
      next
    }
    { print }
  ' "$svg.tmp" > "$svg"
  rm "$svg.tmp"
  echo "$svg"
done
