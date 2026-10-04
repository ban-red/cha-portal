# cha-streamer

One environment's media engine (plan §2.2, [ADR 0004](../../docs/adr/0004-own-engine-no-wolf.md)). The app runs as a client of our own headless Wayland compositor. Composited frames go to NVENC zero-copy through our own binding ([`cha-nvenc`](../cha-nvenc)), then to the browser over WebRTC (str0m). Keyboard and mouse come back into the compositor's seat. There is no GStreamer, FFmpeg or Wolf.

```
app ─Wayland─▶ compositor (Smithay, GLES on the render node)
                 │ 240 Hz: frame callbacks + presentation feedback → apps draw at 240 fps
                 │  60 Hz: composite (only if something changed) into a GBM buffer
                 ▼
        output pool (4 GBM buffers, each registered with CUDA once)
                 │ newest-frame-wins mailbox per codec
                 ▼
        encoder thread per codec (NVENC: P1 / ultra-low-latency, CBR, 1-frame VBV,
                 │                 infinite GOP, zero reorder delay)
                 ▼
        WebRTC session (str0m, playout-delay 0) ──▶ browser
                 ▲
                 └── input (control DataChannel) → the seat: pointer, relative motion, buttons, wheel, keys
```

## How it works

- **Two clocks.**
  - A timerfd ticks at the compositor rate (`--compositor-fps`, default 240). Every tick sends frame callbacks and presentation feedback, so apps draw at that rate. S2 showed this halves click → screen latency.
  - Only every encode tick (`--fps`, default 60) composites, and only if something changed. Nothing is composited that won't be encoded.
  - Chrome paces itself to presentation feedback, not frame callbacks: with feedback at 60 Hz it drew at 60 fps even with 240 Hz callbacks.
- **Zero-copy.**
  - The output pool is four GBM buffers in the GPU's own tiling (`ARGB8888`).
  - Each buffer is registered once with CUDA (an EGL image), and once with NVENC per encoder. Encoding a frame maps and encodes it; nothing is copied.
  - A buffer is reused when every encoder has dropped its frame.
- **Encoders.**
  - One thread per codec, started by the first viewer of that codec and kept warm afterwards.
  - A one-frame mailbox means a slow encoder skips frames instead of queueing them.
  - A keyframe request with nothing new on screen re-encodes the last frame.
  - On resize the session is reconfigured in place, up to 3840×2160.
- **Window policy (kiosk).** Every toplevel fills the output: maximized, or fullscreen when it asks. Decorations are server-side, i.e. none. Dialogs are centered. Popups get proper grabs.
- **Protocols:**
  - compositor, xdg-shell, xdg-decoration;
  - shm, linux-dmabuf v5 with feedback naming the render node;
  - seat (keyboard, pointer), relative-pointer, pointer-constraints;
  - data-device, primary-selection;
  - viewporter, presentation-time, single-pixel-buffer, xdg-output.

  Xwayland comes with XFCE in P1.4.

## Running it (node, dev loop)

The dev container builds and runs the streamer from the synced repository. The GPU comes from CDI, and the container uses host networking.

```bash
docker compose -f deploy/streamer/compose.dev.yaml up -d --build
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev cargo build --release -p cha-streamer
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev /target/release/cha-streamer --run 'google-chrome-stable --no-sandbox --ozone-platform=wayland --no-first-run --force-device-scale-factor=1 --user-data-dir=/tmp/chrome-profile --kiosk file:///src/spikes/s2-compositor/bench/page/live.html'
```

- Chrome runs with `--no-sandbox` only because the dev container runs as root. The environment images in P1.4 run apps unprivileged.
- Signalling is on TCP 4495 and WebRTC on UDP 4496.
- Until the portal brokers sessions (P1.5), starting a stream needs the token printed at startup. It is kept in `/state/token`.
- Measure with the S1c/S1d page, as for S2: host `gpu-node.lan`, port 4495, the token, the WebRTC present path. The streams are `live-hevc`, `live-h264` and `live-av1`.

## Results (RTX 4090, Chrome 154 in the compositor, 2026-10-03)

**Node side, A/B against S2's GStreamer streamer (`gst-wayland-display` → `nvh265enc`).**
- Both ran back to back with Chrome at 240 fps on S2's live page, at 2560×1440, 60 fps, 40 Mbit/s.
- The GPU was shared with the owner's ComfyUI and llama.cpp, at up to 100% utilization and 346 W. That load sets the tail latency, so only back-to-back runs compare.
- Raw data: [`docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json`](../../docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json).

| HEVC, compositor → encoded (ms) | p50 | p99 |
|---|---|---|
| S2 (GStreamer), S2 M1 on a quiet GPU | 1.72 | 1.83 |
| **cha-streamer**, GPU busy | **1.75** | 4.25 |
| S2 (GStreamer), GPU busy (straight after) | 1.78 | 4.32 |

**Where the time goes** (cha-streamer, HEVC, µs, p50 / p99), from the encoder thread's breakdown:

| Stage | p50 | p99 |
|---|---|---|
| composited → picked up | 16 | 686 |
| CUDA view of the buffer | 4 | 9 |
| NVENC map | 9 | 19 |
| submit | 71 | 165 |
| **wait for NVENC** | **1598** | **3915** |

**Other numbers:**
- **Composite:** 0.3–0.4 ms of GPU time at p50, ~2.3 ms at p99 under the shared load.
- **New viewer:** first frame 50–67 ms with a cold encoder (NVENC session init 31–59 ms), **10–11 ms** with a warm one. S2 kept every encoder always on: 14–17 ms.
- **All three codecs:** H.264, HEVC and AV1 all reach the browser over WebRTC; S2 had no AV1.
- **Resize:** 2560×1440 → 1920×1080 → 1280×720 → 2560×1440 mid-stream reallocates the pool and sends a keyframe each time. 897 of 902 frames went out, so each resize skipped about one frame.
- **Under heavy load** (another process at ~350 W), the NVENC wait grows by ~2 ms for every codec.

**Where the tail comes from.**
- With RGB input, NVENC converts to YUV with a CUDA kernel. That kernel waits behind other processes' CUDA work on a busy GPU, and S2 has the same exposure.
- Doing the conversion ourselves removes that wait. Probed on this stack:
  - GL can render R8, but CUDA's EGL interop won't import an R8 image;
  - NV12 can't be a GL render target;
  - GBM ignores a linear request and returns an implicit layout that GL can't bind.

  So the path is a Vulkan compute pass that writes NV12 into a buffer CUDA imports (external memory), and NVENC reads it as `CUDADEVICEPTR` NV12 (`cha-nvenc` already takes `InputFormat::Nv12`). PyroWave needs the same Vulkan import of the composite (plan §3.3), so both land together.
