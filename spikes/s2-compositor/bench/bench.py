#!/usr/bin/env python3
"""Spike S2: gst-wayland-display (waylanddisplaysrc) on the node GPU, with
Google Chrome as its Wayland client, encoded with NVENC.

For each source path (zero-copy CUDA, DMA-BUF -> CUDA if available, system
memory upload) and each load (no client, Chrome with a full-screen WebGL
page), runs `waylanddisplaysrc ! <path> ! nvh26Xenc ! fakesink` and reports:
- latency from a frame leaving the compositor to its encoded frame leaving the
  encoder (pad probes matched by PTS, as in gst-wayland-display's bench-e2e.py);
- compositor and encoder frame rates;
- CPU use of the compositor+encoder process and of Chrome;
- NVENC's own session stats (nvidia-smi), and what Chrome says about its GPU.

With S2_MODE=resize it changes the output size mid-stream and times the
compositor, the encoder and Chrome following it.

With S2_MODE=input it instead measures click -> composited reaction: Chrome
shows live.html, which turns white on every click; synthetic clicks go into
the compositor as its input events, and a pad probe finds the first white
frame leaving the compositor. Several Chrome/compositor settings are compared.

Writes $S2_OUT/s2-<time>.json and prints a summary.
"""
import glob
import json
import os
import pathlib
import re
import resource
import shutil
import subprocess
import threading
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

Gst.init(None)

W = int(os.environ.get("S2_WIDTH", "2560"))
H = int(os.environ.get("S2_HEIGHT", "1440"))
FPS = int(os.environ.get("S2_FPS", "60"))
SECONDS = float(os.environ.get("S2_SECONDS", "15"))
WARMUP = float(os.environ.get("S2_WARMUP", "6"))
NODE = os.environ.get("S2_RENDER_NODE", "/dev/dri/renderD128")
CODECS = os.environ.get("S2_CODECS", "h265").split(",")
OUT = pathlib.Path(os.environ.get("S2_OUT", "/out"))
BITRATE_KBPS = 40_000
RUNTIME = pathlib.Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp/xdg"))

# Low-latency NVENC settings, applied when this GStreamer's element has them (as in S1e).
WANTED = [
    ("preset", ["p1"]), ("tune", ["ultra-low-latency"]), ("rc-mode", ["cbr"]),
    ("bitrate", [str(BITRATE_KBPS)]), ("max-bitrate", [str(BITRATE_KBPS)]),
    ("vbv-buffer-size", [str(BITRATE_KBPS // FPS)]), ("bframes", ["0"]), ("gop-size", ["-1"]),
    ("rc-lookahead", ["0"]), ("zerolatency", ["true"]),
]


def inspect(element: str) -> str | None:
    r = subprocess.run(["gst-inspect-1.0", element], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def encoder_settings(element: str) -> list[str]:
    info = inspect(element) or ""
    out = []
    for name, candidates in WANTED:
        m = re.search(rf"^  {re.escape(name)}\s+:.*?(?=^  [a-z][\w-]*\s+:|\Z)", info, re.M | re.S)
        if not m:
            continue
        block = m.group(0)
        for value in candidates:
            if "Enum " in block and not re.search(rf"\(\d+\):\s+{re.escape(value)}\s", block):
                continue
            out.append(f"{name}={value}")
            break
    return out


def source_paths(enc: str) -> dict[str, str]:
    """Post-source pipelines to compare, keyed by name."""
    size = f"width={W},height={H},framerate={FPS}/1"
    paths = {
        # The plugin imports its frames into CUDA itself (cuda feature).
        "cuda": f"video/x-raw(memory:CUDAMemory),{size} ! {enc}",
        # Copy path for comparison: CPU frames uploaded to CUDA; NVENC takes RGBx and
        # converts itself (Ubuntu's GStreamer has no cudaconvert without NVRTC).
        "sysmem": f"video/x-raw,format=RGBx,{size} ! cudaupload ! {enc}",
    }
    if inspect("dmabuftocuda"):
        paths["dmabuf"] = (f"video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=NV12,{size} ! "
                           f"dmabuftocuda render-node={NODE} ! {enc}")
    return paths


def chrome_cpu_seconds() -> float:
    """utime+stime of every Chrome process, in seconds."""
    total = 0
    tick = os.sysconf("SC_CLK_TCK")
    for stat in glob.glob("/proc/[0-9]*/stat"):
        try:
            with open(stat) as f:
                fields = f.read().rsplit(")", 1)
            comm = open(stat.replace("stat", "comm")).read().strip()
        except OSError:
            continue
        if "chrome" not in comm:
            continue
        parts = fields[1].split()
        total += int(parts[11]) + int(parts[12])
    return total / tick


def self_cpu_seconds() -> float:
    r = resource.getrusage(resource.RUSAGE_SELF)
    return r.ru_utime + r.ru_stime


def nvidia_stats() -> dict:
    q = "utilization.gpu,utilization.encoder,encoder.stats.sessionCount,encoder.stats.averageFps,encoder.stats.averageLatency"
    r = subprocess.run(["nvidia-smi", f"--query-gpu={q}", "--format=csv,noheader,nounits"],
                       capture_output=True, text=True)
    vals = [v.strip() for v in r.stdout.strip().split(",")]
    keys = ["gpuUtil", "encUtil", "encSessions", "encAvgFps", "encAvgLatencyUs"]
    return dict(zip(keys, vals)) if len(vals) == len(keys) else {"raw": r.stdout + r.stderr}


def start_chrome(socket: str, log: list, url: str = "file:///bench/page/index.html",
                 extra: tuple[str, ...] = ()) -> subprocess.Popen:
    profile = pathlib.Path("/tmp/chrome-profile")
    shutil.rmtree(profile, ignore_errors=True)
    cmd = [
        "google-chrome-stable", "--ozone-platform=wayland", "--enable-features=UseOzonePlatform",
        # The container runs as root; Chrome's sandbox refuses root.
        "--no-sandbox", "--no-first-run", "--no-default-browser-check", "--disable-sync",
        "--password-store=basic", f"--user-data-dir={profile}", "--kiosk",
        f"--window-size={W},{H}", "--enable-logging=stderr", "--v=0",
        *extra, url,
    ]
    env = {**os.environ, "WAYLAND_DISPLAY": socket}
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)

    def pump():
        for line in proc.stderr:
            m = re.search(r'"CHA (\{.*\})"', line)
            if m:
                try:
                    log.append({**json.loads(m.group(1)), "at": time.monotonic()})
                except json.JSONDecodeError:
                    pass
            elif re.search(r"ERROR|GPU process|Falling back|SwiftShader|GL_RENDERER", line):
                log.append({"stderr": line.strip()[:300]})

    threading.Thread(target=pump, daemon=True).start()
    return proc


def wait_for_socket(timeout=10.0) -> str | None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        socks = [p for p in RUNTIME.glob("wayland-*") if not p.name.endswith(".lock")]
        if socks:
            return socks[0].name
        time.sleep(0.1)
    return None


def run_case(path_name: str, tail: str, codec: str, with_chrome: bool) -> dict:
    shutil.rmtree(RUNTIME, ignore_errors=True)
    RUNTIME.mkdir(mode=0o700, parents=True)
    extra = "cuda-device-id=0 " if path_name == "cuda" else ""
    desc = (f"waylanddisplaysrc name=wl render-node={NODE} {extra}do-timestamp=true ! {tail} "
            f"! fakesink name=sink sync=false")
    result = {"path": path_name, "codec": codec, "client": "chrome" if with_chrome else "none", "pipeline": desc}
    # The source reads DRM_FORMAT when it starts: NV12 for the DMA-BUF -> CUDA path.
    if path_name == "dmabuf":
        os.environ["DRM_FORMAT"] = "NV12"
    try:
        pipe = Gst.parse_launch(desc)
    except GLib.Error as err:
        os.environ.pop("DRM_FORMAT", None)
        result["error"] = f"parse: {err.message}"
        return result

    t_in, lat, src_times, out_times = {}, [], [], []
    measuring = {"on": False}
    # NVENC shifts output timestamps by a constant (encoded PTS start at 3600 s), so
    # learn the offset from the first input/output pair and match by it.
    first = {"src": None, "offset": None}

    def on_src(_pad, info):
        buf = info.get_buffer()
        if buf is not None:
            now = time.monotonic()
            t_in[buf.pts] = now
            if first["src"] is None:
                first["src"] = buf.pts
            if measuring["on"]:
                src_times.append(now)
        return Gst.PadProbeReturn.OK

    def on_sink(_pad, info):
        buf = info.get_buffer()
        if buf is not None and first["offset"] is None and first["src"] is not None:
            first["offset"] = buf.pts - first["src"]
        if buf is not None and measuring["on"]:
            now = time.monotonic()
            out_times.append(now)
            t0 = t_in.pop(buf.pts - (first["offset"] or 0), None)
            if t0 is not None:
                lat.append((now - t0) * 1000)
        return Gst.PadProbeReturn.OK

    pipe.get_by_name("wl").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, on_src)
    pipe.get_by_name("sink").get_static_pad("sink").add_probe(Gst.PadProbeType.BUFFER, on_sink)
    errors = []
    bus = pipe.get_bus()

    def poll_bus(seconds: float):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            msg = bus.timed_pop_filtered(50 * Gst.MSECOND, Gst.MessageType.ERROR | Gst.MessageType.EOS)
            if msg and msg.type == Gst.MessageType.ERROR:
                err, dbg = msg.parse_error()
                errors.append(f"{err.message} | {(dbg or '')[:400]}")
                return False
            if msg and msg.type == Gst.MessageType.EOS:
                return False
        return True

    pipe.set_state(Gst.State.PLAYING)
    chrome, chrome_log = None, []
    ok = poll_bus(1.0)
    if ok and with_chrome:
        socket = wait_for_socket()
        if not socket:
            errors.append(f"no Wayland socket in {RUNTIME}")
            ok = False
        else:
            result["socket"] = socket
            chrome = start_chrome(socket, chrome_log)
    if ok:
        ok = poll_bus(WARMUP)
    if ok:
        cpu0, chrome0, t0 = self_cpu_seconds(), chrome_cpu_seconds(), time.monotonic()
        measuring["on"] = True
        ok = poll_bus(SECONDS / 2)
        result["nvidia"] = nvidia_stats()
        ok = ok and poll_bus(SECONDS / 2)
        measuring["on"] = False
        wall = time.monotonic() - t0
        result["cpuPercent"] = round(100 * (self_cpu_seconds() - cpu0) / wall, 1)
        if chrome:
            result["chromeCpuPercent"] = round(100 * (chrome_cpu_seconds() - chrome0) / wall, 1)
    if chrome:
        chrome.terminate()
        try:
            chrome.wait(5)
        except subprocess.TimeoutExpired:
            chrome.kill()
    pipe.set_state(Gst.State.NULL)
    os.environ.pop("DRM_FORMAT", None)

    if errors:
        result["error"] = errors[0]
    rate = lambda ts: round((len(ts) - 1) / (ts[-1] - ts[0]), 1) if len(ts) > 2 else None
    result["sourceFps"] = rate(src_times)
    result["encodedFps"] = rate(out_times)
    if lat:
        lat.sort()
        pick = lambda q: round(lat[min(len(lat) - 1, int(q * len(lat)))], 3)
        result["latencyMs"] = {"n": len(lat), "p50": pick(0.5), "p95": pick(0.95), "p99": pick(0.99), "max": round(lat[-1], 3)}
    if chrome_log:
        fps = [e["fps"] for e in chrome_log if "fps" in e]
        result["chrome"] = {
            "renderer": next((e["renderer"] for e in chrome_log if "renderer" in e), None),
            "size": next((f'{e["w"]}x{e["h"]}' for e in chrome_log if "w" in e), None),
            "fps": fps[-5:],
            "notes": [e["stderr"] for e in chrome_log if "stderr" in e][:12],
        }
    return result


INPUT_CLICKS = int(os.environ.get("S2_CLICKS", "30"))
# (label, extra Chrome flags, compositor fps)
# Chrome flags made no consistent difference (2026-10-03 runs); the compositor rate
# does. Each rate twice, with a fresh Chrome each time, to show session spread.
# (label, extra Chrome flags, compositor fps, forwarded fps or None)
# Chrome flags made no consistent difference (2026-10-03 runs); the compositor
# rate does (~4.5 of its frames from click to composited). The decimated cases
# run the compositor fast and forward every Nth frame, as an encoder would see.
INPUT_CASES = [
    ("60", (), 60, None),
    ("120", (), 120, None),
    ("240", (), 240, None),
    ("240->120", (), 240, 120),
    ("240->60", (), 240, 60),
]


def send_input(pad: Gst.Pad, spec: str) -> None:
    """One of the compositor's input events, e.g. 'MouseButton, button=(uint)272, pressed=(boolean)true'."""
    pad.send_event(Gst.Event.new_custom(Gst.EventType.CUSTOM_UPSTREAM, Gst.Structure.new_from_string(spec)))


def corner_luma(buf: Gst.Buffer) -> float:
    """Mean luma of a few RGBx pixels in the bottom-right corner, read without mapping the frame."""
    total = 0.0
    points = [(0.92, 0.92), (0.95, 0.92), (0.92, 0.95), (0.95, 0.95)]
    for fx, fy in points:
        offset = (int(fy * H) * W + int(fx * W)) * 4
        px = buf.extract_dup(offset, 3)
        total += 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]
    return total / len(points)


def input_case(label: str, extra: tuple[str, ...], fps: int, out_fps: int | None = None) -> dict:
    """Click -> first frame showing live.html's white flash: as composited, or, with
    `out_fps`, as forwarded by a decimator that keeps every Nth frame on arrival."""
    shutil.rmtree(RUNTIME, ignore_errors=True)
    RUNTIME.mkdir(mode=0o700, parents=True)
    desc = (f"waylanddisplaysrc name=wl render-node={NODE} do-timestamp=true "
            f"! video/x-raw,format=RGBx,width={W},height={H},framerate={fps}/1 ! fakesink name=out sync=false")
    result = {"case": label, "chromeFlags": list(extra), "fps": fps, "forwardedFps": out_fps, "clicks": INPUT_CLICKS}
    pipe = Gst.parse_launch(desc)
    pad = pipe.get_by_name("wl").get_static_pad("src")
    # Measure where frames leave: the compositor, or after the decimator.
    measure_pad = pipe.get_by_name("wl").get_static_pad("src") if out_fps is None else \
        pipe.get_by_name("out").get_static_pad("sink")
    state = {"pending": None, "lat": [], "missed": 0, "frames": [], "next": 0}

    def probe(_pad, info):
        buf = info.get_buffer()
        now = time.monotonic()
        state["frames"].append(now)
        if buf is not None and state["pending"] is not None and corner_luma(buf) > 200:
            state["lat"].append((now - state["pending"]) * 1000)
            state["pending"] = None
        return Gst.PadProbeReturn.OK

    if out_fps is not None:
        period = Gst.SECOND // out_fps
        slack = Gst.SECOND // fps // 2

        def decimate(_pad, info):
            # Keep a frame once its slot is due; decide on arrival, never wait for the next.
            buf = info.get_buffer()
            if buf is None or buf.pts < state["next"]:
                return Gst.PadProbeReturn.DROP
            state["next"] = buf.pts + period - slack
            return Gst.PadProbeReturn.OK

        pad.add_probe(Gst.PadProbeType.BUFFER, decimate)
    measure_pad.add_probe(Gst.PadProbeType.BUFFER, probe)
    bus = pipe.get_bus()
    errors = []

    def wait(seconds: float) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            msg = bus.timed_pop_filtered(20 * Gst.MSECOND, Gst.MessageType.ERROR)
            if msg:
                err, dbg = msg.parse_error()
                errors.append(f"{err.message} | {(dbg or '')[:300]}")
                return

    pipe.set_state(Gst.State.PLAYING)
    wait(1.0)
    log: list = []
    chrome = None
    socket = wait_for_socket()
    if socket and not errors:
        chrome = start_chrome(socket, log, url="file:///bench/page/live.html", extra=extra)
        wait(WARMUP)
        send_input(pad, f"MouseMoveAbsolute, pointer_x=(double){W / 2}, pointer_y=(double){H / 2}")
        wait(0.5)
        for i in range(INPUT_CLICKS):
            if i == INPUT_CLICKS // 2:
                result["nvidia"] = nvidia_stats()  # other GPU work would skew this case
            if state["pending"] is not None:
                state["missed"] += 1
            state["pending"] = time.monotonic()
            send_input(pad, "MouseButton, button=(uint)272, pressed=(boolean)true")
            send_input(pad, "MouseButton, button=(uint)272, pressed=(boolean)false")
            wait(0.5)
            if errors:
                break
    else:
        errors.append(f"no Wayland socket in {RUNTIME}")
    if chrome:
        chrome.terminate()
        try:
            chrome.wait(5)
        except subprocess.TimeoutExpired:
            chrome.kill()
    pipe.set_state(Gst.State.NULL)

    lat = sorted(state["lat"])
    frames = state["frames"]
    result["compositorFps"] = round((len(frames) - 1) / (frames[-1] - frames[0]), 1) if len(frames) > 2 else None
    # A compositor below its own rate means the GPU was busy with something else.
    expected = out_fps or fps
    result["starved"] = result["compositorFps"] is None or result["compositorFps"] < 0.95 * expected
    result["missed"] = state["missed"]
    if lat:
        pick = lambda q: round(lat[min(len(lat) - 1, int(q * len(lat)))], 2)
        result["clickToCompositedMs"] = {"n": len(lat), "min": round(lat[0], 2), "p50": pick(0.5),
                                         "p95": pick(0.95), "max": round(lat[-1], 2)}
        frame_ms = 1000 / fps
        result["frames"] = {"p50": round(pick(0.5) / frame_ms, 2), "min": round(lat[0] / frame_ms, 2)}
    if errors:
        result["error"] = errors[0]
    notes = [e["stderr"] for e in log if "stderr" in e and "dbus" not in e["stderr"]]
    if notes:
        result["chromeNotes"] = notes[:6]
    return result


RESIZE_STEPS = [(1920, 1080), (1280, 720), (2560, 1440)]
RESIZE_HOLD = float(os.environ.get("S2_RESIZE_HOLD", "4"))


def caps_size(pad: Gst.Pad) -> tuple[int, int] | None:
    caps = pad.get_current_caps()
    if not caps or caps.get_size() == 0:
        return None
    s = caps.get_structure(0)
    ok_w, w = s.get_int("width")
    ok_h, h = s.get_int("height")
    return (w, h) if ok_w and ok_h else None


def resize_case(codec: str) -> dict:
    """Change the output size mid-stream; time the compositor, encoder and Chrome."""
    shutil.rmtree(RUNTIME, ignore_errors=True)
    RUNTIME.mkdir(mode=0o700, parents=True)
    enc_el = {"h264": "nvh264enc", "h265": "nvh265enc"}[codec]
    parse = {"h264": "h264parse", "h265": "h265parse"}[codec]
    enc = " ".join([enc_el, *encoder_settings(enc_el)])
    caps = lambda w, h: Gst.Caps.from_string(
        f"video/x-raw(memory:CUDAMemory),width={w},height={h},framerate={FPS}/1")
    desc = (f"waylanddisplaysrc name=wl render-node={NODE} cuda-device-id=0 do-timestamp=true "
            f"! capsfilter name=f caps=\"video/x-raw(memory:CUDAMemory),width={W},height={H},framerate={FPS}/1\" "
            f"! {enc} ! {parse} ! fakesink name=sink sync=false")
    result = {"codec": codec, "start": f"{W}x{H}", "steps": []}
    pipe = Gst.parse_launch(desc)
    src_pad = pipe.get_by_name("wl").get_static_pad("src")
    sink_pad = pipe.get_by_name("sink").get_static_pad("sink")
    seen = {"src": [], "enc": []}  # (time, size) of each buffer

    def watch(tag, pad):
        def probe(_pad, info):
            seen[tag].append((time.monotonic(), caps_size(pad)))
            return Gst.PadProbeReturn.OK
        pad.add_probe(Gst.PadProbeType.BUFFER, probe)

    watch("src", src_pad)
    watch("enc", sink_pad)
    bus = pipe.get_bus()
    errors = []

    def wait(seconds: float) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            msg = bus.timed_pop_filtered(20 * Gst.MSECOND, Gst.MessageType.ERROR)
            if msg:
                err, dbg = msg.parse_error()
                errors.append(f"{err.message} | {(dbg or '')[:300]}")
                return

    pipe.set_state(Gst.State.PLAYING)
    wait(1.0)
    log: list = []
    chrome = None
    socket = wait_for_socket()
    if socket and not errors:
        chrome = start_chrome(socket, log)
        wait(WARMUP)
        f = pipe.get_by_name("f")
        for w, h in RESIZE_STEPS:
            at = time.monotonic()
            f.set_property("caps", caps(w, h))
            wait(RESIZE_HOLD)
            first = lambda tag: next((t for t, s in seen[tag] if t >= at and s == (w, h)), None)
            src_t, enc_t = first("src"), first("enc")
            chrome_t = next((e["at"] for e in log if e.get("resized") and e["at"] >= at
                             and (e["w"], e["h"]) == (w, h)), None)
            window = [t for t, _ in seen["enc"] if at - 0.5 <= t <= at + 1.5]
            gaps = [b - a for a, b in zip(window, window[1:])]
            ms = lambda t: None if t is None else round((t - at) * 1000, 1)
            result["steps"].append({
                "size": f"{w}x{h}", "toSourceMs": ms(src_t), "toEncodedMs": ms(enc_t),
                "toChromeResizeMs": ms(chrome_t),
                "maxEncodedGapMs": round(max(gaps) * 1000, 1) if gaps else None,
            })
            if errors:
                break
    else:
        errors.append(f"no Wayland socket in {RUNTIME}")
    if chrome:
        chrome.terminate()
        try:
            chrome.wait(5)
        except subprocess.TimeoutExpired:
            chrome.kill()
    pipe.set_state(Gst.State.NULL)
    if errors:
        result["error"] = errors[0]
    result["finalChromeSize"] = next((f'{e["w"]}x{e["h"]}' for e in reversed(log) if "w" in e), None)
    return result


def main_resize() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    report = {
        "capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "mode": "resize",
        "gpu": subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
                              capture_output=True, text=True).stdout.strip(),
        "gstreamer": Gst.version_string(), "fps": FPS, "results": [],
    }
    for codec in CODECS:
        print(f"resize {codec}…", flush=True)
        report["results"].append(resize_case(codec))
    path = OUT / f"s2-resize-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\n{report['gpu']} | {report['gstreamer']}\n")
    for r in report["results"]:
        print(f"{r['codec']} from {r['start']}" + (f"  ERROR {r['error'][:300]}" if "error" in r else ""))
        for s in r["steps"]:
            print(f"   -> {s['size']:>9}: source {s['toSourceMs']} ms, encoded {s['toEncodedMs']} ms, "
                  f"Chrome resized {s['toChromeResizeMs']} ms, max encoded gap {s['maxEncodedGapMs']} ms")
        print(f"   Chrome ended at {r.get('finalChromeSize')}")
    print(f"\nFull report: {path}")


def main_input() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    report = {
        "capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "mode": "input",
        "gpu": subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
                              capture_output=True, text=True).stdout.strip(),
        "gstreamer": Gst.version_string(),
        "chromeVersion": subprocess.run(["google-chrome-stable", "--version"], capture_output=True, text=True).stdout.strip(),
        "size": f"{W}x{H}", "results": [],
    }
    for label, extra, fps, out_fps in INPUT_CASES:
        print(f"input {label}…", flush=True)
        report["results"].append(input_case(label, extra, fps, out_fps))
    path = OUT / f"s2-input-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\n{report['gpu']} | {report['gstreamer']} | {report['chromeVersion']}\n")
    for r in report["results"]:
        c = r.get("clickToCompositedMs", {})
        line = (f"{r['case']:>10}: click -> {'forwarded' if r.get('forwardedFps') else 'composited'} p50 {c.get('p50')} ms "
                f"(min {c.get('min')}, p95 {c.get('p95')}; {r.get('frames', {}).get('p50')} frames), "
                f"n {c.get('n')}, missed {r.get('missed')}, out {r.get('compositorFps')} fps, "
                f"gpu {r.get('nvidia', {}).get('gpuUtil')}%" + ("  STARVED" if r.get("starved") else ""))
        if "error" in r:
            line += f"  ERROR {r['error'][:200]}"
        print(line)
    print(f"\nFull report: {path}")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    report = {
        "capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "gpu": subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
                              capture_output=True, text=True).stdout.strip(),
        "gstreamer": Gst.version_string(),
        "chromeVersion": subprocess.run(["google-chrome-stable", "--version"], capture_output=True, text=True).stdout.strip(),
        "size": f"{W}x{H}@{FPS}", "seconds": SECONDS, "results": [],
    }
    for codec in CODECS:
        enc_el = {"h264": "nvh264enc", "h265": "nvh265enc"}[codec]
        enc = " ".join([enc_el, *encoder_settings(enc_el)])
        for name, tail in source_paths(enc).items():
            for with_chrome in (False, True):
                print(f"{codec} {name} {'chrome' if with_chrome else 'no client'}…", flush=True)
                report["results"].append(run_case(name, tail, codec, with_chrome))

    path = OUT / f"s2-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\n{report['gpu']} | {report['gstreamer']} | {report['chromeVersion']}\n")
    for r in report["results"]:
        head = f"{r['codec']} {r['path']:>6} {r['client']:>6}"
        if "error" in r and not r.get("latencyMs"):
            print(f"{head}: ERROR {r['error'][:300]}")
            continue
        l = r.get("latencyMs", {})
        line = (f"{head}: src→encoded p50 {l.get('p50')} / p99 {l.get('p99')} ms, "
                f"src {r.get('sourceFps')} fps, enc {r.get('encodedFps')} fps, cpu {r.get('cpuPercent')}%")
        if r["client"] == "chrome":
            c = r.get("chrome", {})
            line += f", chrome cpu {r.get('chromeCpuPercent')}% fps {c.get('fps')} on {c.get('renderer')}"
        print(line)
        if r.get("chrome", {}).get("notes"):
            for n in r["chrome"]["notes"][:4]:
                print(f"      chrome: {n[:200]}")
    print(f"\nFull report: {path}")


if __name__ == "__main__":
    mode = os.environ.get("S2_MODE", "encode")
    {"input": main_input, "resize": main_resize}.get(mode, main)()
