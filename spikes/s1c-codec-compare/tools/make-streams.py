#!/usr/bin/env python3
"""Make the S1c streams: the same 1440p60 content as PyroWave and as low-latency
H.264 / HEVC / AV1, in one container the S1 server replays.

  python3 make-streams.py            # writes ../streams/*.chastream

Container ("CHASTRM1"): magic, u32 header length, JSON header (space-padded to 4
bytes), then per frame: u32 size, u32 flags (bit 0 = keyframe), payload padded to
4 bytes. H.264/HEVC payloads are Annex B access units; AV1 payloads are temporal
units; PyroWave payloads are whole-frame packets.

Needs ffmpeg with libx264, libx265 and libaom, plus the S1b PyroWave clips
(spikes/s1b-pyrowave-webgpu/tools/make-clips.sh).
"""
import json
import pathlib
import struct
import subprocess

FPS = 60
WIDTH, HEIGHT = 2560, 1440
FRAMES = 240  # 4 s; one IDR at the start, so loops add one keyframe every 4 s
SOURCE = f"mandelbrot=size={WIDTH}x{HEIGHT}:rate={FPS}"

SPIKE = pathlib.Path(__file__).resolve().parent.parent
OUT = SPIKE / "streams"
CLIPS = SPIKE.parent / "s1b-pyrowave-webgpu" / "clips"


def write_stream(name: str, header: dict, frames: list[tuple[bytes, bool]]) -> None:
    total = sum(len(d) for d, _ in frames)
    header = {**header, "frames": len(frames), "fps": FPS, "bitrateMbps": round(total * 8 * FPS / len(frames) / 1e6, 1)}
    head = json.dumps(header).encode()
    head += b" " * (-len(head) % 4)
    with open(OUT / f"{name}.chastream", "wb") as f:
        f.write(b"CHASTRM1" + struct.pack("<I", len(head)) + head)
        for data, key in frames:
            f.write(struct.pack("<II", len(data), 1 if key else 0) + data + b"\0" * (-len(data) % 4))
    sizes = sorted(len(d) for d, _ in frames)
    print(f"{name}: {header['bitrateMbps']} Mbit/s, frame bytes median {sizes[len(sizes) // 2]}, max {sizes[-1]}, codec {header.get('codecString')}")


def ffmpeg(args: list[str], fmt: str) -> bytes:
    cmd = ["ffmpeg", "-loglevel", "error", "-f", "lavfi", "-i", SOURCE, "-frames:v", str(FRAMES),
           "-pix_fmt", "yuv420p", *args, "-f", fmt, "-"]
    return subprocess.run(cmd, check=True, capture_output=True).stdout


# --- Annex B --------------------------------------------------------------------

def nal_units(data: bytes) -> list[tuple[int, int]]:
    """(start, end) of each NAL unit including its start code."""
    starts = []
    i = 0
    while (i := data.find(b"\x00\x00\x01", i)) != -1:
        starts.append(i - 1 if i > 0 and data[i - 1] == 0 else i)
        i += 3
    return [(s, starts[k + 1] if k + 1 < len(starts) else len(data)) for k, s in enumerate(starts)]


def payload_offset(data: bytes, start: int) -> int:
    return start + (4 if data[start:start + 4] == b"\x00\x00\x00\x01" else 3)


def split_access_units(data: bytes, hevc: bool) -> list[tuple[bytes, bool]]:
    aud, idr = (35, {19, 20, 21}) if hevc else (9, {5})
    units: list[tuple[bytes, bool]] = []
    current, key = bytearray(), False
    for start, end in nal_units(data):
        b = data[payload_offset(data, start)]
        nal_type = (b >> 1) & 0x3F if hevc else b & 0x1F
        if nal_type == aud and current:
            units.append((bytes(current), key))
            current, key = bytearray(), False
        current += data[start:end]
        key |= nal_type in idr
    if current:
        units.append((bytes(current), key))
    return units


def unescape(rbsp: bytes) -> bytes:
    """Drop emulation-prevention bytes (00 00 03 → 00 00)."""
    out, zeros = bytearray(), 0
    for b in rbsp:
        if zeros >= 2 and b == 3:
            zeros = 0
            continue
        out.append(b)
        zeros = zeros + 1 if b == 0 else 0
    return bytes(out)


def find_nal(data: bytes, hevc: bool, wanted: int) -> bytes:
    for start, end in nal_units(data):
        off = payload_offset(data, start)
        nal_type = (data[off] >> 1) & 0x3F if hevc else data[off] & 0x1F
        if nal_type == wanted:
            return unescape(data[off:end])
    raise SystemExit("parameter set not found")


def avc_codec_string(data: bytes) -> str:
    sps = find_nal(data, False, 7)
    return f"avc1.{sps[1]:02X}{sps[2]:02X}{sps[3]:02X}"


def hevc_codec_string(data: bytes) -> str:
    sps = find_nal(data, True, 33)
    ptl = sps[3:]  # after 2-byte NAL header and the vps_id/sub-layer byte
    profile_space, tier, profile_idc = ptl[0] >> 6, (ptl[0] >> 5) & 1, ptl[0] & 0x1F
    compat = int.from_bytes(ptl[1:5], "big")
    compat_reversed = int(f"{compat:032b}"[::-1], 2)
    constraints = list(ptl[5:11])
    while constraints and constraints[-1] == 0:
        constraints.pop()
    level = ptl[11]
    space = "ABC"[profile_space - 1] if profile_space else ""
    parts = [f"hev1.{space}{profile_idc}", f"{compat_reversed:X}", f"{'H' if tier else 'L'}{level}"]
    parts += [f"{c:02X}" for c in constraints]
    return ".".join(parts)


# --- AV1 (IVF) --------------------------------------------------------------------

def split_ivf(data: bytes, gop: int) -> list[tuple[bytes, bool]]:
    header_len = struct.unpack_from("<H", data, 6)[0]
    frames, off, i = [], header_len, 0
    while off + 12 <= len(data):
        size = struct.unpack_from("<I", data, off)[0]
        frames.append((data[off + 12: off + 12 + size], i % gop == 0))
        off += 12 + size
        i += 1
    return frames


# --- PyroWave --------------------------------------------------------------------

def pyrowave_frames(path: pathlib.Path) -> tuple[dict, list[tuple[bytes, bool]]]:
    data = path.read_bytes()
    params = struct.unpack_from("<8i", data, 8)
    frames, off = [], 40
    while off + 4 <= len(data):
        size = struct.unpack_from("<I", data, off)[0]
        frames.append((data[off + 4: off + 4 + size], True))  # intra-only
        off += 4 + size
    return {"width": params[0], "height": params[1], "chroma": "444" if params[3] == 1 else "420"}, frames


def low_latency(codec: str, mbps: int) -> list[str]:
    # CBR with a VBV of about one frame, no B-frames, no lookahead, one IDR.
    vbv = f"vbv-maxrate={mbps * 1000}:vbv-bufsize={mbps * 1000 // FPS}"
    common = f"bframes=0:keyint={FRAMES}:min-keyint={FRAMES}:scenecut=0:aud=1:repeat-headers=1:{vbv}"
    if codec == "h264":
        return ["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", f"{mbps}M",
                "-x264-params", common]
    if codec == "hevc":
        return ["-c:v", "libx265", "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", f"{mbps}M",
                "-x265-params", f"{common}:rc-lookahead=0"]
    return ["-c:v", "libaom-av1", "-usage", "realtime", "-cpu-used", "8", "-lag-in-frames", "0", "-row-mt", "1",
            "-tiles", "2x2", "-b:v", f"{mbps}M", "-minrate", f"{mbps}M", "-maxrate", f"{mbps}M",
            "-bufsize", f"{mbps * 1000 // FPS}k", "-g", str(FRAMES), "-keyint_min", str(FRAMES)]


def main() -> None:
    OUT.mkdir(exist_ok=True)
    base = {"width": WIDTH, "height": HEIGHT, "chroma": "420", "content": "mandelbrot"}

    for clip, label in (("mandelbrot-1440p-420-604k", "pyrowave-420-290m"),
                        ("mandelbrot-1440p-444-1229k", "pyrowave-444-590m")):
        src = CLIPS / f"{clip}.pyrowave"
        if not src.exists():
            raise SystemExit(f"missing {src}; run spikes/s1b-pyrowave-webgpu/tools/make-clips.sh first")
        info, frames = pyrowave_frames(src)
        write_stream(label, {**base, **info, "codec": "pyrowave", "codecString": None}, frames)

    for codec, mbps in (("hevc", 40), ("hevc", 80), ("h264", 40), ("av1", 40)):
        if codec == "av1":
            frames = split_ivf(ffmpeg(low_latency(codec, mbps), "ivf"), FRAMES)
            codec_string = "av01.0.13M.08"  # Main profile, level 5.1, 8-bit
        else:
            raw = ffmpeg(low_latency(codec, mbps), "hevc" if codec == "hevc" else "h264")
            frames = split_access_units(raw, codec == "hevc")
            codec_string = hevc_codec_string(raw) if codec == "hevc" else avc_codec_string(raw)
        write_stream(f"{codec}-420-{mbps}m", {**base, "codec": codec, "codecString": codec_string}, frames)


if __name__ == "__main__":
    main()
