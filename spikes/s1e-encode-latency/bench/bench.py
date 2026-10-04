#!/usr/bin/env python3
"""Spike S1e: per-frame encode latency on the node GPU at 1440p60.

- NVENC H.264 / HEVC / AV1 through GStreamer (nvcodec), low-latency settings, a
  live 60 fps source. Timed with GStreamer's latency tracer: per-element latency
  of the encoder (and of the CUDA upload) for every frame.
- PyroWave 4:2:0 and 4:4:4 through the WebGPU port's encoder on Vulkan, with the
  frame already on the GPU (`--gpu-input`), at the S1 byte budgets.

Writes $S1E_OUT/s1e-<time>.json and prints a summary.
"""
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys
import time

W, H, FPS = 2560, 1440, 60
FRAMES = int(os.environ.get("S1E_FRAMES", "600"))
OUT = pathlib.Path(os.environ.get("S1E_OUT", "/out"))
BITRATE_KBPS = 40_000

# The CUDA-mode encoders first (GStreamer ≥ 1.22), then the older names.
ENCODERS = {
    "h264": ["nvcudah264enc", "nvh264enc", "nvautogpuh264enc"],
    "hevc": ["nvcudah265enc", "nvh265enc", "nvautogpuh265enc"],
    "av1": ["nvcudaav1enc", "nvav1enc", "nvautogpuav1enc"],
}

# Low-latency settings; each is applied only if this GStreamer's element has it.
WANTED = [
    ("preset", ["p1", "low-latency-hp", "hp"]),
    ("tune", ["ultra-low-latency", "low-latency"]),
    ("rate-control", ["cbr"]),
    ("rc-mode", ["cbr", "cbr-ld-hq"]),
    ("bitrate", [str(BITRATE_KBPS)]),
    ("max-bitrate", [str(BITRATE_KBPS)]),
    ("vbv-buffer-size", [str(BITRATE_KBPS // FPS)]),
    ("bframes", ["0"]),
    ("b-frames", ["0"]),
    ("gop-size", ["-1"]),
    ("rc-lookahead", ["0"]),
    ("zerolatency", ["true"]),
    ("zero-reorder-delay", ["true"]),
]


def inspect(element: str) -> str | None:
    r = subprocess.run(["gst-inspect-1.0", element], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def property_block(info: str, name: str) -> str | None:
    """gst-inspect text for one property, up to the next property."""
    m = re.search(rf"^  {re.escape(name)}\s+:.*?(?=^  [a-z][\w-]*\s+:|\Z)", info, re.M | re.S)
    return m.group(0) if m else None


def settings_for(element: str, info: str) -> list[str]:
    out = []
    for name, candidates in WANTED:
        block = property_block(info, name)
        if not block:
            continue
        is_enum = "Enum " in block
        for value in candidates:
            if is_enum and not re.search(rf"\(\d+\):\s+{re.escape(value)}\s", block):
                continue
            out.append(f"{name}={value}")
            break
    return out


def tracer_latencies(log: pathlib.Path, element: str) -> list[float]:
    """Per-buffer latency in ms of `element` from latency-tracer output."""
    pat = re.compile(rf"element-latency,.*?element=\(string\){re.escape(element)},.*?time=\(guint64\)(\d+)")
    return [int(m.group(1)) / 1e6 for line in log.read_text(errors="replace").splitlines() if (m := pat.search(line))]


def pipeline_latencies(log: pathlib.Path) -> list[float]:
    pat = re.compile(r"\blatency, src-element-id.*?time=\(guint64\)(\d+)")
    return [int(m.group(1)) / 1e6 for line in log.read_text(errors="replace").splitlines() if (m := pat.search(line))]


def stats(values: list[float]) -> dict:
    if not values:
        return {"n": 0}
    v = sorted(values[min(30, len(values) // 10):])  # skip warm-up frames
    pick = lambda q: v[min(len(v) - 1, int(q * len(v)))]
    return {"n": len(v), "p50": round(pick(0.5), 3), "p95": round(pick(0.95), 3),
            "p99": round(pick(0.99), 3), "mean": round(statistics.fmean(v), 3)}


def run_nvenc(codec: str, pattern: str) -> dict:
    element = next((e for e in ENCODERS[codec] if inspect(e)), None)
    if not element:
        return {"codec": codec, "pattern": pattern,
                "error": f"no NVENC element ({', '.join(ENCODERS[codec])}); see diagnostics"}
    settings = settings_for(element, inspect(element) or "")
    upload = "cudaupload name=up ! video/x-raw(memory:CUDAMemory),format=NV12 ! " if inspect("cudaupload") else ""
    pipeline = (
        f"videotestsrc is-live=true num-buffers={FRAMES} pattern={pattern} "
        f"! video/x-raw,format=NV12,width={W},height={H},framerate={FPS}/1 "
        f"! {upload}{element} name=enc {' '.join(settings)} ! fakesink sync=false"
    )
    log = pathlib.Path(f"/tmp/trace-{codec}-{pattern}.log")
    env = {**os.environ, "GST_TRACERS": "latency(flags=element+pipeline)", "GST_DEBUG": "GST_TRACER:7",
           "GST_DEBUG_FILE": str(log), "GST_DEBUG_NO_COLOR": "1"}
    started = time.time()
    r = subprocess.run(["gst-launch-1.0", "-q", *pipeline.split()], env=env, capture_output=True, text=True)
    result = {
        "codec": codec, "element": element, "pattern": pattern, "settings": settings,
        "bitrateKbps": BITRATE_KBPS, "pipeline": pipeline, "seconds": round(time.time() - started, 1),
    }
    if r.returncode != 0:
        result["error"] = (r.stderr or r.stdout)[-2000:]
        return result
    result["encodeMs"] = stats(tracer_latencies(log, "enc"))
    result["uploadMs"] = stats(tracer_latencies(log, "up")) if upload else None
    result["sourceToSinkMs"] = stats(pipeline_latencies(log))
    return result


def run_pyrowave(chroma: str, bytes_per_frame: int) -> dict:
    pix = "yuv420p" if chroma == "420" else "yuv444p"
    y4m = pathlib.Path(f"/tmp/src-{chroma}.y4m")
    frames = 120
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", f"testsrc2=size={W}x{H}:rate={FPS}",
                    "-frames:v", str(frames), "-pix_fmt", pix, str(y4m)], check=True)
    cmd = ["pyrowave-webgpu-encode", str(y4m), f"/tmp/out-{chroma}.pyrowave", str(bytes_per_frame),
           "--frames", str(frames), "--timestamps", "--gpu-input"]
    r = subprocess.run(cmd, capture_output=True, text=True, env={**os.environ, "PYROWAVE_WEBGPU_BACKEND": "vulkan"})
    y4m.unlink(missing_ok=True)
    result = {"codec": f"pyrowave-{chroma}", "bytesPerFrame": bytes_per_frame, "command": " ".join(cmd)}
    if r.returncode != 0:
        result["error"] = (r.stderr or r.stdout)[-2000:]
        return result
    timings = {}
    for line in (r.stdout + r.stderr).splitlines():
        m = re.match(r"\s+(.+?)\s+mean\s+([\d.]+) ms\s+median\s+([\d.]+) ms\s+p95\s+([\d.]+) ms", line)
        if m:
            timings[m.group(1).strip()] = {"mean": float(m.group(2)), "p50": float(m.group(3)), "p95": float(m.group(4))}
    result["timingsMs"] = timings
    result["log"] = [l for l in (r.stdout + r.stderr).splitlines() if l.strip()][:20]
    return result


def gpu_info() -> str:
    r = subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
                       capture_output=True, text=True)
    return r.stdout.strip() or "nvidia-smi unavailable"


def sh(cmd: list[str], env: dict | None = None) -> str:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, env={**os.environ, **(env or {})}, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as err:
        return f"{cmd[0]}: {err}"
    return r.stdout + r.stderr


def diagnose() -> dict:
    """What the container sees of the GPU: device nodes, driver libraries, the
    nvcodec plugin and its init log, Vulkan manifests and devices."""
    libs = sh(["ldconfig", "-p"])
    wanted_libs = ["libcuda.so.1", "libnvidia-encode.so.1", "libnvrtc.so", "libGLX_nvidia.so.0",
                   "libEGL_nvidia.so.0", "libnvidia-glvkspirv.so", "libvulkan.so.1"]
    manifests = {}
    for d in ("/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"):
        for f in sorted(pathlib.Path(d).glob("*.json")):
            manifests[str(f)] = f.read_text(errors="replace").strip()
    # Re-scan nvcodec in-process with its debug output on, to see why it
    # registered (or didn't register) elements.
    registry = pathlib.Path("/tmp/gst-diag-registry.bin")
    registry.unlink(missing_ok=True)
    init = sh(["gst-inspect-1.0", "nvcodec"], {"GST_REGISTRY": str(registry), "GST_REGISTRY_FORK": "no",
                                               "GST_DEBUG": "*nv*:4,*cuda*:4", "GST_DEBUG_NO_COLOR": "1"})
    plugin = sh(["gst-inspect-1.0", "nvcodec"])
    vulkan = sh(["vulkaninfo", "--summary"])
    return {
        "devices": sorted(str(p) for p in pathlib.Path("/dev").glob("nvidia*")),
        "libraries": {name: name in libs for name in wanted_libs},
        "nvcodecFeatures": [l.strip() for l in plugin.splitlines() if re.match(r"\s+nv\w+:", l)
                            or re.match(r"\s+cuda\w+:", l)],
        "nvcodecPlugin": plugin.strip().splitlines()[:12],
        "nvcodecInitLog": [l for l in init.splitlines() if re.search(r"\b(ERROR|WARN)\b", l)][-40:],
        "vulkanManifests": manifests,
        "vulkanDevices": [l.strip() for l in vulkan.splitlines()
                          if re.search(r"deviceName|deviceType|driverName|driverInfo", l)],
        "vulkanError": None if "deviceName" in vulkan else vulkan.strip().splitlines()[-15:],
    }


def print_diagnostics(d: dict) -> None:
    print("\nDiagnostics (also in the JSON report):")
    print(f"  /dev: {' '.join(d['devices']) or 'no nvidia device nodes'}")
    print("  driver libraries: " + ", ".join(f"{k} {'yes' if v else 'NO'}" for k, v in d["libraries"].items()))
    print(f"  nvcodec elements: {', '.join(f.split(':')[0] for f in d['nvcodecFeatures']) or 'none'}")
    if not d["nvcodecFeatures"]:
        for line in d["nvcodecPlugin"] + d["nvcodecInitLog"][-15:]:
            print(f"    {line[:200]}")
    print(f"  Vulkan manifests: {', '.join(d['vulkanManifests']) or 'none'}")
    for line in d["vulkanDevices"] or d["vulkanError"] or []:
        print(f"    {line[:200]}")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    report = {"capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "gpu": gpu_info(),
              "gstreamer": subprocess.run(["gst-launch-1.0", "--version"], capture_output=True, text=True).stdout.splitlines()[:1],
              "frames": FRAMES, "resolution": f"{W}x{H}@{FPS}", "diagnostics": diagnose(), "results": []}
    for codec in ("h264", "hevc", "av1"):
        for pattern in ("ball", "snow"):
            print(f"NVENC {codec} ({pattern})…", flush=True)
            report["results"].append(run_nvenc(codec, pattern))
    for chroma, budget in (("420", 604_166), ("444", 1_229_166)):
        print(f"PyroWave {chroma}…", flush=True)
        report["results"].append(run_pyrowave(chroma, budget))

    path = OUT / f"s1e-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\nGPU: {report['gpu']}\n")
    for r in report["results"]:
        if "error" in r:
            print(f"{r['codec']:>14} {r.get('pattern', ''):>5}: ERROR {r['error'][:300]}")
        elif "encodeMs" in r:
            e = r["encodeMs"]
            print(f"{r['codec']:>14} {r['pattern']:>5}: encode p50 {e.get('p50')} / p95 {e.get('p95')} ms "
                  f"({r['element']}, {' '.join(r['settings'])})")
        else:
            t = r["timingsMs"]
            print(f"{r['codec']:>14}: " + "; ".join(f"{k} p50 {v['p50']}" for k, v in t.items()))
    d = report["diagnostics"]
    cpu_vulkan = any(re.search(r"llvmpipe|lavapipe|CPU", l) for l in d["vulkanDevices"])
    if cpu_vulkan:
        print("\nWARNING: a CPU Vulkan device is present; PyroWave numbers may not be from the GPU.")
    if cpu_vulkan or any("error" in r for r in report["results"]):
        print_diagnostics(d)
    print(f"\nFull report: {path}")


if __name__ == "__main__":
    sys.exit(main())
