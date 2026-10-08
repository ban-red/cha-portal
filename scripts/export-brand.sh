#!/usr/bin/env sh
# Makes every logo and icon file from the one master,
# web/packages/ui-spec/brand/logo.png (1024x1024, transparent):
#   the portal's favicon (PNG and ICO), Apple touch icon and in-page logo,
#   and Cha Player's macOS app icon (AppIcon.icns).
# Needs ImageMagick (`brew install imagemagick`) and, for the .icns, macOS's
# iconutil. Run from anywhere: sh scripts/export-brand.sh
set -eu
cd "$(dirname "$0")/.."
master=web/packages/ui-spec/brand/logo.png
public=web/apps/portal/public
# The logo's own background, for icons that need a solid tile.
tile="#17161b"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

resize() { magick "$master" -filter Lanczos -resize "${1}x${1}" -strip "PNG32:$2"; }

# Portal: tab icon, the in-page logo (BrandMark), and the home-screen icon.
resize 64 "$public/favicon.png"
resize 256 "$public/logo.png"
for s in 16 32 48; do resize "$s" "$tmp/ico-$s.png"; done
magick "$tmp/ico-16.png" "$tmp/ico-32.png" "$tmp/ico-48.png" "$public/favicon.ico"
# iOS draws the touch icon on its own rounded tile and ignores transparency:
# give it the logo's background and some margin.
magick -size 180x180 "xc:$tile" \( "$master" -filter Lanczos -resize 144x144 \) \
  -gravity center -compose over -composite -strip "PNG24:$public/apple-touch-icon.png"

# Cha Player: a macOS icon on Apple's grid (an 824 pt rounded tile in 1024).
if command -v iconutil >/dev/null 2>&1; then
  magick -size 1024x1024 xc:none \
    \( -size 824x824 xc:none -fill "$tile" -draw "roundrectangle 0,0 823,823 185,185" \) \
    -gravity center -compose over -composite \
    \( "$master" -filter Lanczos -resize 640x640 \) -gravity center -compose over -composite \
    -strip "PNG32:$tmp/icon-1024.png"
  set_dir="$tmp/AppIcon.iconset"
  mkdir "$set_dir"
  for s in 16 32 128 256 512; do
    magick "$tmp/icon-1024.png" -filter Lanczos -resize "${s}x${s}" "PNG32:$set_dir/icon_${s}x${s}.png"
    d=$((s * 2))
    magick "$tmp/icon-1024.png" -filter Lanczos -resize "${d}x${d}" "PNG32:$set_dir/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$set_dir" -o crates/cha-player/macos/AppIcon.icns
else
  echo "iconutil not found (not macOS): skipped crates/cha-player/macos/AppIcon.icns" >&2
fi
echo "brand files written"
