#!/bin/sh
# Assemble target/Cha Player.app from the release binary and Info.plist.
# Not signed or notarised. Run from anywhere: ./crates/cha-player/macos/bundle.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
bin="$root/target/release/cha-player"
app="$root/target/Cha Player.app"

[ -x "$bin" ] || { echo "build first: cargo build -p cha-player --release" >&2; exit 1; }
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../Cargo.toml" | head -1)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/cha-player"
sed "s/@VERSION@/$version/" "$here/Info.plist" > "$app/Contents/Info.plist"
echo "built $app"
