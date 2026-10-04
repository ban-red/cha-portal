#!/usr/bin/env bash
# Builds PyroWave's native WebGPU tools and encodes the S1b test clips.
# Needs git, ffmpeg, a C++ toolchain, ninja and uv (for cmake). Leaves ~200 MB of
# clips in ../clips; each raw source (330–660 MB) is deleted right after encoding.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="$HERE/.cache"
CLIPS="$HERE/clips"
FORK_URL=https://github.com/imbcmdth/pyrowave.git
FORK_REV=5e80f92 # branch webgpu, 2026-09-26; bitstream of upstream 89f7e47

mkdir -p "$CACHE" "$CLIPS"
SRC="$CACHE/pyrowave-webgpu"
if [ ! -d "$SRC/.git" ]; then
  git clone --branch webgpu "$FORK_URL" "$SRC"
fi
git -C "$SRC" checkout -q "$FORK_REV"
# The shader-embedding command passes a ';'-separated list through /bin/sh; under
# Ninja it needs VERBATIM.
grep -q VERBATIM "$SRC/webgpu/CMakeLists.txt" ||
  perl -0pi -e 's/(\t\tCOMMENT "Embedding WGSL shaders")\)/$1\n\t\tVERBATIM)/' "$SRC/webgpu/CMakeLists.txt"

CMAKE=(uvx --from cmake cmake)
"${CMAKE[@]}" -P "$SRC/webgpu/fetch_wgpu_native.cmake"
"${CMAKE[@]}" -S "$SRC/webgpu" -B "$CACHE/build" -G Ninja -DCMAKE_BUILD_TYPE=Release
ninja -C "$CACHE/build"
ENC="$CACHE/build/pyrowave-webgpu-encode"
DEC="$CACHE/build/pyrowave-webgpu-decode"

# 1440p60, 60 frames each. Per-frame byte caps: 290 Mbit/s for 4:2:0 and
# 590 Mbit/s for 4:4:4 at 60 fps (the S1 presets).
for src in testsrc2 mandelbrot; do
  for spec in "yuv420p 420 604166 604k" "yuv444p 444 1229166 1229k"; do
    read -r pix chroma bytes tag <<<"$spec"
    out="$CLIPS/$src-1440p-$chroma-$tag.pyrowave"
    [ -f "$out" ] && continue
    y4m="$CACHE/$src-$pix.y4m"
    ffmpeg -loglevel error -y -f lavfi -i "$src=size=2560x1440:rate=60" -frames:v 60 -pix_fmt "$pix" "$y4m"
    "$ENC" "$y4m" "$out" "$bytes"
    rm -f "$y4m"
  done
done

python3 "$HERE/tools/make-reference.py" "$CLIPS" "$DEC"
ln -sfn ../../clips "$HERE/web/public/clips"
echo "Clips ready in $CLIPS"
