#!/bin/sh
# Assemble target/Cha Player.app from the release binary and Info.plist.
# Signed ad hoc, or with CHA_SIGN_IDENTITY (a codesigning identity's name)
# when set; not notarised. Run from anywhere: ./crates/cha-player/macos/bundle.sh
#
# macOS ties privacy permissions (Input Monitoring) to the app's designated
# requirement. An ad-hoc signature's default one is the binary's hash, so
# every rebuild silently lost the grant while System Settings still showed it
# on; ad hoc we pin the requirement to the bundle identifier instead. Under an
# identity the default requirement already survives rebuilds.
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
if [ -n "${CHA_SIGN_IDENTITY:-}" ]; then
    codesign --force --sign "$CHA_SIGN_IDENTITY" --identifier sh.cha.player "$app"
else
    codesign --force --sign - --identifier sh.cha.player \
        -r='designated => identifier "sh.cha.player"' "$app"
fi
echo "built $app"
