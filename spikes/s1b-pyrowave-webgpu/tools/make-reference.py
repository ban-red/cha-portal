#!/usr/bin/env python3
"""Make S1b reference data for PyroWave clips.

For each <clip>.pyrowave in CLIPS_DIR: cut frame 0 into its own file, decode it with
the native `pyrowave-webgpu-decode` tool, and keep the raw planes as
<clip>.ref.yuv. Writes CLIPS_DIR/index.json for the bench page.

  python3 make-reference.py <clips_dir> <path/to/pyrowave-webgpu-decode>
"""
import json
import pathlib
import struct
import subprocess
import sys


def first_frame(src: pathlib.Path, dst: pathlib.Path) -> dict:
    data = src.read_bytes()
    if data[:8] != b"PYROWAVE":
        raise SystemExit(f"{src}: not a PyroWave file")
    params = struct.unpack_from("<8i", data, 8)
    size = struct.unpack_from("<I", data, 40)[0]
    dst.write_bytes(data[: 44 + size])
    frames, offset = 0, 40
    while offset + 4 <= len(data):
        n = struct.unpack_from("<I", data, offset)[0]
        offset += 4 + n
        frames += 1
    return {"width": params[0], "height": params[1], "chroma": "444" if params[3] == 1 else "420", "frames": frames}


def y4m_to_raw(y4m: pathlib.Path, raw: pathlib.Path) -> None:
    data = y4m.read_bytes()
    header_end = data.index(b"\n") + 1
    frame_end = data.index(b"\n", header_end) + 1  # "FRAME...\n"
    raw.write_bytes(data[frame_end:])


def main() -> None:
    clips, tool = pathlib.Path(sys.argv[1]), sys.argv[2]
    index = []
    for clip in sorted(clips.glob("*.pyrowave")):
        if clip.name.endswith(".f0.pyrowave"):
            continue
        cut = clip.with_name(clip.stem + ".f0.pyrowave")
        info = first_frame(clip, cut)
        y4m = clip.with_name(clip.stem + ".f0.y4m")
        subprocess.run([tool, str(cut), str(y4m)], check=True, capture_output=True)
        y4m_to_raw(y4m, clip.with_name(clip.stem + ".ref.yuv"))
        y4m.unlink()
        cut.unlink()
        index.append({"name": clip.stem, "file": clip.name, "ref": clip.stem + ".ref.yuv", **info})
        print(f"{clip.name}: {info}")
    (clips / "index.json").write_text(json.dumps(index, indent=2) + "\n")


if __name__ == "__main__":
    main()
