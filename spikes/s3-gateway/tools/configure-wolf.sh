#!/usr/bin/env sh
# Adjusts the S3 Wolf's config (idempotent), then restarts Wolf:
# - H.264 with P-frames, like Wolf's HEVC. Wolf's default NVENC H.264 is
#   all-intra (gop-size=0): every frame is an IDR, and Chrome's WebRTC path
#   decodes that ~4x slower (9.3 vs 2.5 ms per 1440p frame on an M4 Pro).
# - A "Test snow" app: Test ball's settings with a noise source (720p, scaled
#   up by Wolf), so frames are as large as the bitrate allows. 1440p noise is
#   too slow to generate on the CPU at 60 fps.
# Run on the node from the repository root, with the S3 stack up.
set -eu
compose="docker compose -f spikes/s3-gateway/compose.yaml"
$compose exec -T wolf python3 - <<'PY'
import pathlib
p = pathlib.Path("/etc/wolf/cfg/config.toml")
s = p.read_text()

intra = "nvh264enc preset=low-latency-hq zerolatency=true gop-size=0 rc-mode=cbr-ld-hq"
pframes = "nvh264enc gop-size=-1 rc-mode=cbr zerolatency=true preset=p1 tune=ultra-low-latency multi-pass=two-pass-quarter"
if intra in s:
    s = s.replace(intra, pframes)
    print("H.264: P-frames")
elif pframes in s:
    print("H.264: already P-frames")
else:
    raise SystemExit("Wolf's nvh264enc pipeline changed; update this script")

if "title = 'Test snow'" in s:
    print("Test snow: already there")
else:
    title = s.index("title = 'Test ball'")
    start = s.rindex("[[profiles.apps]]", 0, title)
    end = s.index("\n[[profiles]]", title)
    ball = s[start:end].rstrip("\n")
    snow = ball.replace("title = 'Test ball'", "title = 'Test snow'").replace(
        "videotestsrc pattern=ball flip=true is-live=true !\nvideo/x-raw, framerate={fps}/1",
        "videotestsrc pattern=snow is-live=true !\nvideo/x-raw, width=1280, height=720, framerate={fps}/1",
    )
    assert "pattern=snow" in snow, "Test ball's video source changed; update this script"
    s = s[:end] + "\n\n    " + snow.lstrip() + "\n" + s[end:]
    print("Test snow: added")
p.write_text(s)
PY
$compose restart wolf
