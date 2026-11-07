#!/bin/sh
# Export the bundled icons from the approved X + rose artwork.
set -eu
cd "$(dirname "$0")/.."
art=docs/branding/xrose.svg
assets=crates/xrose-app/assets
render() { rsvg-convert -w "$1" -h "$1" -o "$assets/$2" "$art"; }
render 900 logo-mark.png
render 512 app-icon.png
for size in 256 128 96 64; do
    render "$size" "app-icon-$size.png"
done
cp "$assets/app-icon-256.png" "$assets/icons/xrose.png"
cp "$assets/app-icon-128.png" "$assets/icons/xrose-128.png"
cp "$art" "$assets/icons/xrose.svg"

# Optical weights for the 16–22 pt status glyph. The OS supplies the ink;
# retain only the mark's coverage, with no background, tile, border or badge.
rsvg-convert -w 96 -h 96 -s /dev/stdin -o "$assets/tray-template.png" "$art" <<'CSS'
#mark { stroke: black; transform: translate(259px, 256px) scale(1.05) translate(-259px, -256px); }
#arms { stroke-width: 30; }
#ribbons { stroke-width: 21; }
#rose { stroke-width: 17; }
CSS
