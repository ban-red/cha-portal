# Spike S1e: encode latency on the node GPU

**Question.** S1c compared PyroWave and hardware codecs with encode left out. How long does each take to *encode* a 1440p60 frame on the node's GPU? The first node is `gpu-node.lan` with an RTX 4090, driver 595.71.

## What it measures

| Path | How | Settings |
|---|---|---|
| NVENC H.264 / HEVC / AV1 | GStreamer `nvcodec` elements fed by a live 60 fps `videotestsrc`, CUDA upload first. The latency tracer reports every frame's time inside the encoder element and the upload. | 40 Mbit/s CBR, VBV ≈ one frame, fastest preset (`p1`), ultra-low-latency tune, no B-frames, infinite GOP. Each setting is applied only if this GStreamer's element exposes it; the report lists what was applied. |
| PyroWave 4:2:0 / 4:4:4 | The WebGPU port's native encoder (wgpu-native on Vulkan), frame already on the GPU (`--gpu-input`), 120 frames | S1 budgets: 604 KB and 1.23 MB per frame |

Two NVENC contents per codec: `ball` (simple motion) and `snow` (noise, worst case for rate control).

## Running it (on the node)

Docker on the node needs the NVIDIA Container Toolkit with a CDI spec. Toolkit ≥ 1.17 generates one at `/var/run/cdi/nvidia.yaml`; otherwise run `sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml`. Check it with:

```bash
docker run --rm --device nvidia.com/gpu=all ubuntu nvidia-smi
```

From the repository copy on the node:

```bash
docker compose -f spikes/s1e-encode-latency/compose.yaml run --rm --build s1e-encode
```

The first build takes a few minutes: Ubuntu 26.04, GStreamer 1.28, and the PyroWave tool built against wgpu-native. The run takes about a minute. It prints a summary and writes the full JSON to `spikes/s1e-encode-latency/results/`. If anything fails, it also prints what the container sees of the GPU: device nodes, driver libraries, the nvcodec plugin's elements and init log, and the Vulkan devices.

### Container notes

- **Ubuntu 26.04, not 25.04.** 25.04's GStreamer package ships no `nvcodec` plugin on x86-64 ([LP #2109413](https://bugs.launchpad.net/ubuntu/+source/gst-plugins-bad1.0/+bug/2109413)), and 25.04 is end of life.
- **The GPU comes in as a CDI device (`nvidia.com/gpu=all`), not `--gpus`.** With `--gpus` (compose `driver: nvidia`), NVENC works, but NVIDIA's Vulkan driver fails with `Could not get 'vkCreateInstance' via 'vk_icdGetInstanceProcAddr'`. That path leaves out `/dev/nvidia-modeset`, the ICD manifest, and newer driver libraries such as `libnvidia-present` (driver 595). The CDI spec injects the whole driver. Seen on `gpu-node.lan`: toolkit 1.20.1, driver 595.71.05, Docker 29.
- **Vulkan libraries in the image.** NVIDIA's Vulkan driver links against libglvnd, libEGL and libXext, which the image installs. It has no Mesa Vulkan drivers, so a CPU device can't be picked by accident.

## Caveats

- **PyroWave numbers here are an upper bound.** They come from the WebGPU port on wgpu-native; upstream's Vulkan `libpyrowave` is faster on NVIDIA (fork README: 0.12 ms GPU encode at 1080p). The port's own numbers on an RTX 4090 at 1080p were ~0.15 ms GPU and ~0.6 ms wall clock.
- **NVENC through GStreamer** includes the element's queueing. That is what our streamer would see if it uses GStreamer for hardware encode (plan §2.2), but it isn't the raw NVENC API floor.
- **Capture is not included.** Compositor → DMA-BUF → encoder import is a separate cost, measured in the live streamer (Phase 2).

## Results: gpu-node.lan, RTX 4090, 2026-10-03

- **Node:** RTX 4090, driver 595.71.05, GStreamer 1.28.2, Ubuntu 26.04 container with the GPU passed in through CDI.
- **NVENC:** 600 frames per run at 1440p60. The first 30 are skipped as warm-up.
- **NVENC settings applied:** `preset=p1 tune=ultra-low-latency rc-mode=cbr bitrate=40000 max-bitrate=40000 vbv-buffer-size=666 bframes=0 gop-size=-1 rc-lookahead=0 zerolatency=true`.
- **PyroWave:** 120 frames of `testsrc2`.
- **Raw data:** [`docs/benchmarks/s1e-2026-10-03-gpu-node-rtx4090.json`](../../docs/benchmarks/s1e-2026-10-03-gpu-node-rtx4090.json).

**NVENC.** Times are ms per frame, p50 / p95.

| Codec | Content | Encode (in the encoder element) | CUDA upload | Source → sink |
|---|---|---|---|---|
| H.264 | ball | **1.89** / 1.96 | 0.31 | 2.23 / 2.58 |
| H.264 | snow | **1.99** / 2.02 | 0.31 | 2.34 / 2.43 |
| HEVC | ball | **1.73** / 1.78 | 0.32 | 2.08 / 2.39 |
| HEVC | snow | **1.89** / 2.00 | 0.31 | 2.23 / 2.37 |
| AV1 | ball | **1.81** / 1.88 | 0.32 | 2.17 / 2.46 |
| AV1 | snow | **2.20** / 2.24 | 0.31 | 2.53 / 2.61 |

**PyroWave (WebGPU port).** Times are ms per frame, p50.

| Mode | Bytes per frame | GPU passes | Wall clock | Of which: record + submit / wait + readback / packetize |
|---|---|---|---|---|
| 4:2:0 | 604 KB (at the cap) | **0.21** | **1.04** | 0.59 / 0.41 / 0.04 |
| 4:4:4 | 872 KB (cap 1.23 MB; `testsrc2` is easy content) | **0.33** | **1.92** | 1.12 / 0.71 / 0.09 |

The spread is tight: every p95 is within 0.15 ms of its p50.

### Verdict

- **NVENC on Ada is faster than S1c assumed.** S1c assumed 2–5 ms per frame; the measured encode is 1.7–2.2 ms at 1440p, and 2.1–2.6 ms from source to sink. HEVC is the quickest, and AV1 is the slowest on noise.
- **PyroWave's GPU work is tiny, and the wall clock is mostly overhead.**
  - The GPU passes take 0.21 ms (4:2:0) and 0.33 ms (4:4:4). The rest of the ~1–2 ms is wgpu command recording and submission, plus reading the bitstream back.
  - A native integration sharing the compositor's Vulkan device (plan §3.3) should land near ~0.3–0.6 ms.
- **With encode added, hardware codecs still lead on the baseline.** S1c's on-screen p50 plus this encode p50:

| Path | S1c on screen | + encode | **Total** |
|---|---|---|---|
| H.264 | 6.9 | 1.9–2.0 | **~8.9** |
| HEVC 50 Mbit/s | 8.0 | 1.7–1.9 | **~9.8** |
| AV1 | 8.0 | 1.8–2.2 | **~10.0** |
| PyroWave 4:2:0 | 11.4 | 1.0 (port) | **~12.4** |
| PyroWave 4:4:4 | 20.0 | 1.9 (port) | **~21.9** |

- **The tie S1c predicted doesn't happen.** S1c guessed PyroWave 4:2:0 would roughly tie once encode was counted. In fact the hardware codecs stay ~2.5–3.5 ms ahead on 1 GbE + M4 Pro. Even a ~0.3 ms native PyroWave encode only recovers ~0.7 ms of that. The rest is wire time, which only a faster link removes.
- **The plan stands.** Hardware HEVC/AV1/H.264 is the default tier on this baseline, and PyroWave is chosen by the per-client probe (plan §3.2), mainly for ≥ 2.5 GbE links.
- **Next lever for hardware codecs.** NVENC can emit slices before the whole frame is done (sub-frame output). That overlaps encode with sending and would trim part of the ~2 ms. Worth trying in the live streamer.
