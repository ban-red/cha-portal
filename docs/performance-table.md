# Performance table

What to expect from each kind of node, from numbers we measured. Where we
haven't measured something, the cell says so instead of guessing.

Baseline client for every latency here: MacBook Pro M4, Chrome, wired 1 GbE
([PLAN.md](PLAN.md), standing decisions). Raw data is in
[`benchmarks/`](benchmarks/README.md).

## At a glance

| Node hardware | Device | Codecs it offers | Measured encode time | Sweet spot | Recommended codec | Status |
|---|---|---|---|---|---|---|
| **NVIDIA GeForce RTX 4090** (NVENC, CUDA, zero copy) | `nvidia` | H.264, HEVC, AV1, PyroWave | **1.7–2.2 ms** at 1440p60, any of the three hardware codecs; **1.6–2.4 ms** at 1440p120 | **1440p60 and 1440p120**; 4K not measured | **AV1** or **HEVC** on a fast link; **PyroWave 4:4:4** on a wired LAN for text-heavy desktops | Measured end to end |
| **AMD Radeon 780M** (Ryzen 7 8845HS, VCN 4, radeonsi, Mesa 26.0) | `vaapi` | H.264, HEVC, AV1 | **1.3 ms** at 720p; AV1 **2.3 ms** at 1080p, **3.4 ms** at 1440p; HEVC **3.25 ms** at 1440p | **1080p60 to 1440p60** | **AV1** (same speed as HEVC, best quality per bit, palette mode helps desktops); **HEVC** where the client has no AV1 | Measured, with the host clock rule below |
| **Intel UHD 630** (i7-8700B, QuickSync, iHD 26.1) | `vaapi` | H.264, HEVC (no AV1 encoder) | **3.9 ms** at 720p H.264 (first test, including RGB→NV12) | **720p to 1080p60**; 1440p not measured | **HEVC** if the client decodes it, else **H.264** | Works; fewer numbers |
| **Intel Arc, other AMD (RDNA, VCN 3/4), newer Intel iGPUs** | `vaapi` | whatever `EncSlice`/`EncSliceLP` the driver offers, of H.264, HEVC, AV1 | not measured | n/a | **AV1** if the probe lists it, else **HEVC** | Untested; the same code path, so expect it to work, but we haven't run it |
| **No GPU** | `cpu` | H.264 (x264), AV1 (SVT-AV1 when it loads) | not measured | Desktops and browsers at modest sizes; **never games** | **H.264** | Works; not benchmarked |

"Encode time" is the whole of the encoder call: RGB→NV12 conversion, encode
and read-back, per frame, p50.

## What you feel: click to screen

These come from the same browser, so they compare nodes, not browsers.

| Node | Click → shown (p50) | Browser | Notes |
|---|---|---|---|
| RTX 4090 | **15.8 ms** | Claude app browser (Chromium 152) | AV1 over WebTransport |
| RTX 4090, GPU shared with another job | 24.2 ms | same | NVIDIA time-slices; see below |
| Radeon 780M | **16.0 ms** (16.7 worst run) | same | AV1 over WebTransport; matches the 4090 |

The app's browser pane doesn't present like Chrome, so treat click → screen as
a comparison between nodes, not as the number a Chrome user gets.

## The latency budget on the baseline

For a 4090 into Chrome on an M4 over 1 GbE (S1c, S1d, S1e):

| Stage | Time |
|---|---|
| Encode (NVENC) | 1.7–2.2 ms |
| Send → on screen, WebRTC (encode not included) | 5.4–6.4 ms |
| Send → on screen, H.264 / AV1 / HEVC, WebCodecs | 6.9 / 8.0 / 8.0–8.7 ms |
| Wire time for one 1440p60 frame at 290 Mbit/s | about 5 ms |
| Wire time for one 1440p60 frame at 590 Mbit/s | about 10.5 ms |

Safari 26.5 on the same LAN, HEVC over WebRTC, 1672×1440: send → shown p50
5.3 ms, decode p50 2.8 ms.

On a 1 GbE link, wire time is the largest piece once the rate goes past
about 300 Mbit/s. That's why the hardware codecs are the default tier on the
baseline and PyroWave is an opt-in LAN mode.

## Which codec, by client

| Client | Pick | Why |
|---|---|---|
| Chrome on macOS / Windows (any node with AV1) | **AV1** | Best picture per bit; decodes in hardware on most current machines |
| Chrome with no AV1 hardware decode | **HEVC**, then **H.264** | Older GPUs |
| Safari | **HEVC** | AV1 decode on Safari needs M3 / A17 Pro or newer |
| Chrome on Linux | **AV1** or **H.264** | No HEVC; hardware decode is patchy |
| Anything odd | **H.264** | Always offered |
| Wired LAN, text-heavy desktop, 4090 node | **PyroWave 4:4:4** | Near-lossless text (49–60 dB luma); needs about 440–570 Mbit/s at 1440p60 and WebGPU in the client |
| WAN | AV1 → HEVC → H.264 by what the client decodes | 5–50 Mbit/s with FEC and rate control |

Picture quality at 1440p60, on a Chrome-rendered docs page (S6): H.264 and HEVC
42–43 dB luma, 36–38 dB chroma; PyroWave 4:2:0 3.5–7 dB more luma in motion.
On the 780M, AV1 holds 34.5–34.7 dB frame after frame at 20 Mbit/s 720p, so its
references don't drift.

## Things that change the numbers

- **AMD needs a host rule for its video clocks.** On `auto` the 780M's video
  engine idles at its lowest clock under a stream: HEVC at 1440p took
  **7.05 ms**. With `deploy/node/host/73-cha-amd-video-clocks.rules` (vclk and
  dclk at their top step) it takes **3.25 ms** (p99 3.4), with no measurable
  change in power (about 9 W package either way) and the 3D clock untouched.
  `create-node.sh` sets it up for Proxmox hosts.
- **Sharing the GPU costs time on NVIDIA.** With another job (ComfyUI) running,
  every encode waits out that job's timeslice: about 2 ms more per codec, and
  click → shown went from 15.8 to 24.2 ms. A node that streams wants its GPU
  to itself.
- **AMD AV1 pads the picture.** radeonsi codes to a multiple of 64 wide by 16
  high, so 1080p comes out as 1920×1082. The streamer reports the true size and
  the player crops to it (WebCodecs visible rect, WebRTC `object-view-box`).
- **AMD H.264 needs the slice header packed by us.** radeonsi otherwise ignores
  a requested IDR after the first picture. This is on by default on Mesa.
- **Intel's HEVC needs a bigger buffer.** iHD's rate control gives an IDR a
  smaller share, so HEVC runs with a ten-frame HRD buffer, H.264 with four.
- **NVENC's low-latency CBR makes about two thirds of its target rate.** Plan
  the link for that.
- **120 fps.** On the 4090, H.264, HEVC and AV1 encode in 1.6–2.4 ms at 120 fps,
  inside the 8.3 ms a frame has. A 120 fps frame is about 67 kB against 79 kB
  at 60. Not measured on AMD or Intel.

## Not measured yet

- 4K on any node.
- A real game on the 780M (Steam's Big Picture runs, with gamescope on RADV and
  HEVC at 2560×1440, but no game has been benchmarked).
- Click → screen in Chrome (as opposed to the app's browser) on the 780M.
- UHD 630 at 1080p and 1440p; Intel HEVC timings.
- Any Arc GPU, and iHD's AV1 path (written from the header's contract, never run).
- CPU nodes.
- PyroWave on Intel and AMD (it is Vulkan compute and could run there; today it's
  NVIDIA only).
- A real controller on the AMD node (virtual pad input was checked).

## Sources

- [`crates/cha-streamer/README.md`](../crates/cha-streamer/README.md): VA-API
  section, "Measured on the Radeon 780M" and the NVENC results.
- [`benchmarks/p2amd-2026-10-09-mini-titan-780m-node.json`](benchmarks/p2amd-2026-10-09-mini-titan-780m-node.json):
  clocks on auto vs forced.
- [`benchmarks/p2amd-2026-10-09-electron152-mini-titan-780m-probe.json`](benchmarks/p2amd-2026-10-09-electron152-mini-titan-780m-probe.json):
  click → shown, 780M vs 4090.
- [`benchmarks/s1e-2026-10-03-gpu-node-rtx4090.json`](benchmarks/s1e-2026-10-03-gpu-node-rtx4090.json),
  [`benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json`](benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json),
  [`benchmarks/phase2-exit.md`](benchmarks/phase2-exit.md).
- [`PLAN.md`](PLAN.md) §3.2 (codec ladder, S1c/S1e/S6/S7/S8) and the AMD and
  Intel node status entries.
- [`devices.md`](devices.md): the device kinds.
