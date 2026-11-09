#!/usr/bin/env python3
"""What an encoder did to colour, from its stream of colour patches
(docs/plans/hdr-and-vrr.md, step 2.2).

Encode the patches on a node, then read the dump back here:

    CHA_ENCODE_TEST=120 CHA_ENCODE_TEST_PATTERN=patches \\
      CHA_ENCODE_TEST_DUMP=/tmp/patches.h264 \\
      cha-streamer --probe-device vaapi:/dev/dri/renderD128
    scripts/color-check.py /tmp/patches.h264 1280 720

It decodes the last picture with ffmpeg as the decoder's own YCbCr (no colour
conversion), takes the middle of each patch, and says:

- which matrix the encoder used to make YCbCr (BT.709 or BT.601): the one
  that turns the primaries and secondaries back into the colours drawn;
- whether it kept the curve: the grey ramp's luma should be 16 + 219 · v / 255
  for every grey v when the encoder only changed the matrix, as every Cha
  encoder means to. Which transfer the stream should then be tagged with is
  for a screen to settle (Chrome on a Mac, beside the same picture in SDR).

The patch layout is crates/cha-streamer/src/encoder/patches.rs's; the test
(scripts/test_color_check.py) checks that they agree. Standard library only.
"""

import json
import subprocess
import sys

# Row by row, [r, g, b] sRGB codes: as PATCHES in encoder/patches.rs.
PATCHES = [
    [[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 255], [255, 255, 0], [255, 255, 255], [0, 0, 0]],
    [[v, v, v] for v in (0, 36, 73, 109, 146, 182, 219, 255)],
    [[v, v, v] for v in (0, 4, 8, 12, 16, 20, 24, 28)],
]

MATRICES = {"BT.709": (0.2126, 0.0722), "BT.601": (0.299, 0.114)}


def cell_rect(row, col, width, height):
    """The middle half of a cell, as patches.rs lays the cells out."""
    cw, ch = max(width // 8, 1), max(height // 3, 1)
    x0, y0 = col * cw, row * ch
    x1 = width if col == 7 else x0 + cw
    y1 = height if row == 2 else y0 + ch
    qx, qy = (x1 - x0) // 4, (y1 - y0) // 4
    return x0 + qx, y0 + qy, x1 - qx, y1 - qy


def pattern_bgrx(width, height):
    """The pattern as patches.rs paints it (BGRx), for tests and to encode elsewhere."""
    out = bytearray(width * height * 4)
    cw, ch = max(width // 8, 1), max(height // 3, 1)
    for y in range(height):
        row = min(y // ch, 2)
        for x in range(width):
            r, g, b = PATCHES[row][min(x // cw, 7)]
            i = (y * width + x) * 4
            out[i : i + 4] = bytes((b, g, r, 255))
    return bytes(out)


def probe_pix_fmt(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "json", path],
        check=True,
        capture_output=True,
    ).stdout
    return json.loads(out)["streams"][0]["pix_fmt"]


def decode_last_frame(path, width, height):
    """The last picture's Y, Cb and Cr planes (4:2:0) as 8-bit code values (floats)."""
    ten = "10" in probe_pix_fmt(path)
    fmt = "yuv420p10le" if ten else "yuv420p"
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-f", "rawvideo", "-pix_fmt", fmt, "-"],
        check=True,
        capture_output=True,
    ).stdout
    cw, chh = (width + 1) // 2, (height + 1) // 2
    samples = width * height + 2 * cw * chh
    size = samples * (2 if ten else 1)
    if len(raw) < size:
        raise SystemExit(f"{path}: no {width}x{height} picture in it")
    frame = raw[len(raw) - size :]
    if ten:
        values = [int.from_bytes(frame[i : i + 2], "little") / 4 for i in range(0, size, 2)]
    else:
        values = list(frame)
    y = values[: width * height]
    cb = values[width * height : width * height + cw * chh]
    cr = values[width * height + cw * chh :]
    return (y, width), (cb, cw), (cr, cw)


def mean(plane, rect, scale):
    data, stride = plane
    x0, y0, x1, y1 = (v // scale for v in rect)
    vals = [data[yy * stride + xx] for yy in range(y0, max(y1, y0 + 1)) for xx in range(x0, max(x1, x0 + 1))]
    return sum(vals) / len(vals)


def to_rgb(y, cb, cr, matrix):
    """Limited-range YCbCr (8-bit codes) to RGB codes under `matrix`."""
    kr, kb = MATRICES[matrix]
    kg = 1 - kr - kb
    yy, pb, pr = (y - 16) / 219, (cb - 128) / 224, (cr - 128) / 224
    r = yy + 2 * (1 - kr) * pr
    b = yy + 2 * (1 - kb) * pb
    g = (yy - kr * r - kb * b) / kg
    return [255 * c for c in (r, g, b)]


def analyse(planes, width, height):
    """What the patches say: each matrix's error on the colours, the best, and the greys' luma."""
    yp, cbp, crp = planes
    sample = {}
    for row in range(3):
        for col in range(8):
            rect = cell_rect(row, col, width, height)
            sample[(row, col)] = (mean(yp, rect, 1), mean(cbp, rect, 2), mean(crp, rect, 2))
    errors = {}
    for m in MATRICES:
        diffs = [
            abs(got - want)
            for col in range(8)
            for got, want in zip(to_rgb(*sample[(0, col)], m), PATCHES[0][col])
        ]
        errors[m] = sum(diffs) / len(diffs)
    best = min(errors, key=errors.get)
    greys = []
    for row in (1, 2):
        for col in range(8):
            v = PATCHES[row][col][0]
            greys.append((v, sample[(row, col)][0], 16 + 219 * v / 255))
    return {"errors": errors, "matrix": best, "greys": greys, "samples": sample}


def main(argv):
    if len(argv) != 4:
        print(__doc__.split("\n\n")[0], file=sys.stderr)
        print("usage: color-check.py STREAM WIDTH HEIGHT", file=sys.stderr)
        return 2
    path, width, height = argv[1], int(argv[2]), int(argv[3])
    report = analyse(decode_last_frame(path, width, height), width, height)
    print(f"{path}: {width}x{height}")
    for m, e in report["errors"].items():
        print(f"  {m}: the colours come back {e:.1f} codes off on average")
    print(f"  matrix: {report['matrix']}")
    worst = max(abs(got - want) for _, got, want in report["greys"])
    print(f"  greys: luma within {worst:.1f} codes of an unchanged curve" + (" (curve kept)" if worst < 3 else " (the curve was changed)"))
    for v, got, want in report["greys"]:
        print(f"    grey {v:3}: Y {got:6.1f} (unchanged curve: {want:6.1f})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
