#!/usr/bin/env bash
#
# Regenerate every app icon raster from design/icon.svg and design/tray.svg.
#
#   1. Rasterise the mark at 1024 px with the Tauri CLI (resvg). ImageMagick's
#      built-in SVG coder drops stroked arcs and rotated polygons, so it is
#      used only for raster operations here.
#   2. iOS: opaque RGB PNG (App Store Connect rejects alpha), full bleed; the
#      OS applies the mask.
#   3. macOS: an 824 px tile with a 185 px corner radius centred on a
#      transparent 1024 canvas (Apple HIG grid), then `tauri icon` for the
#      bundle set; only the macOS files are copied into src-tauri/icons.
#   4. Menu bar: a 36 px black-on-transparent template PNG from tray.svg.
#
# Outputs are committed; rerun after editing design/*.svg.
# Requires ImageMagick 7 (`magick`) and `npm ci` in desktop/ (Tauri CLI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESIGN_DIR="$REPO_ROOT/design"
DESKTOP_DIR="$REPO_ROOT/desktop"
ICONS_DIR="$DESKTOP_DIR/src-tauri/icons"
IOS_ICON="$REPO_ROOT/ios/ObSink/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
INK="#15141B"

command -v magick >/dev/null || { echo "ImageMagick 7 (magick) is required" >&2; exit 1; }
[ -d "$DESKTOP_DIR/node_modules/@tauri-apps/cli" ] || { echo "run 'npm ci' in desktop/ first" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

tauri_icon() {
    ( cd "$DESKTOP_DIR" && npx --no-install tauri icon "$@" >/dev/null 2>&1 )
}

echo "==> Rasterising design/icon.svg at 1024 px"
tauri_icon -o "$WORK/full" -p 1024 "$DESIGN_DIR/icon.svg"
FULL="$WORK/full/1024x1024.png"

echo "==> iOS AppIcon.png (opaque RGB, full bleed)"
magick "$FULL" -background "$INK" -alpha remove -alpha off -strip "PNG24:$IOS_ICON"

echo "==> macOS bundle icons (rounded tile with margin)"
magick "$FULL" -resize 824x824 \
    \( -size 824x824 xc:none -fill white -draw 'roundrectangle 0,0,823,823 185,185' \) \
    -compose DstIn -composite -compose Over \
    -background none -gravity center -extent 1024x1024 -strip "PNG32:$WORK/icon-macos.png"
tauri_icon -o "$WORK/macos" "$WORK/icon-macos.png"
for name in 32x32.png 128x128.png 128x128@2x.png icon.icns icon.png; do
    cp "$WORK/macos/$name" "$ICONS_DIR/$name"
done

echo "==> Menu-bar template icon"
tauri_icon -o "$WORK/tray" -p 36 "$DESIGN_DIR/tray.svg"
cp "$WORK/tray/36x36.png" "$ICONS_DIR/tray.png"

echo "Done: $IOS_ICON"
echo "      $ICONS_DIR/{32x32,128x128,128x128@2x,icon,tray}.png, icon.icns"
