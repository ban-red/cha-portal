#!/bin/sh
# Assemble target/Cha Player.app from the release binary and Info.plist.
# Signed ad hoc, or with CHA_SIGN_IDENTITY (a codesigning identity's name)
# when set; not notarised. Run from anywhere: ./crates/cha-player/macos/bundle.sh
#
# macOS keeps privacy permissions (Input Monitoring) for an ad-hoc signed app
# only while the binary is unchanged; under an identity they survive rebuilds.
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
codesign --force --sign "${CHA_SIGN_IDENTITY:--}" --identifier sh.cha.player "$app"
echo "built $app"
