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

# Fit the horizontal brand gradient to the rose path rather than the entire X.
tray_art=$(mktemp)
trap 'rm -f "$tray_art"' EXIT
sed 's/<linearGradient id="brand"[^>]*>/<linearGradient id="brand">/' "$art" > "$tray_art"
mkdir -p "$assets/tray"
tray() {
    rsvg-convert -w 96 -h 96 -s /dev/stdin -o "$assets/tray/$ink-$1.png" "$tray_art" <<CSS
#mark { stroke: $ink; transform: translate(259px, 256px) scale(1.05) translate(-259px, -256px); }
#arms { stroke-width: 30; }
#ribbons { stroke-width: 21; }
#rose { stroke-width: 17; stroke: $2; }
CSS
}
for ink in black white; do
    tray plain "$ink"
    tray idle '#909399'
    tray proxy '#00bdf5'
    tray mesh '#963bf2'
    tray mesh-proxy 'url(#brand)'
    tray unavailable '#ef4444'
done
