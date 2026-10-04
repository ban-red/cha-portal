# Spike S2: the compositor (first milestone: gst-wayland-display + Chrome on NVIDIA)

**Question (plan §9, Phase 0: S2).** Phase 2's own engine (`cha-streamer`) needs a headless Wayland compositor that environments draw into. Its frames must reach the encoder (NVENC, VA-API, PyroWave) without copies, and it must be resizable at runtime. The candidates are:
- **gst-wayland-display** (Wolf's Smithay compositor, MIT): a GStreamer source, and also a Rust crate we could embed;
- **pixelflux** (Selkies, MPL).

**This milestone** covers the first candidate on the NVIDIA node, with **Google Chrome** (the first environment) as its client:
- Does it run headless in a container with the GPU from CDI?
- Does Chrome get GPU rendering under it?
- What does compositor → encoded cost per frame?
- How do the zero-copy paths compare with a copy path?

## What it runs

One container. `bench/bench.py` starts this pipeline and, for the Chrome runs, launches Chrome against the compositor's socket:

```
waylanddisplaysrc render-node=/dev/dri/renderD128 ! <source path> ! nvh26Xenc (p1, ultra-low-latency, CBR 40 Mbit/s) ! fakesink
```

Chrome runs in kiosk mode (Ozone/Wayland) and shows `bench/page/index.html`: a full-screen WebGL2 plasma with per-frame grain. Every pixel changes every frame, like a game or a video. The page logs its GL renderer and frame rate.

| Source path | Caps out of the compositor | Into the encoder |
|---|---|---|
| `cuda` | `memory:CUDAMemory`. The plugin imports its frames into CUDA itself (`cuda-device-id=0`, `cuda` feature). | directly |
| `dmabuf` | NV12 DMA-BUF (in-source Vulkan convert, `DRM_FORMAT=NV12`) | `dmabuftocuda`. Only runs if this GStreamer has that element. |
| `sysmem` | RGBx in system memory (copy path, for comparison) | `cudaupload ! cudaconvert` |

Each path runs twice, with no client and with Chrome, for H.265 and H.264. 15 s are measured after a 6 s warm-up. For each run the bench reports:
- **compositor → encoded** latency: a frame leaving `waylanddisplaysrc` to its encoded frame leaving the encoder, matched by PTS with pad probes, as in the repo's `benchmark/bench-e2e.py`;
- compositor and encoder frame rates;
- CPU use of the compositor+encoder process and of Chrome;
- NVENC's own session stats (`nvidia-smi`);
- Chrome's GL renderer and frame rate.

**Not measured yet:**
- Chrome's commit → the compositor frame that includes it, which is the other half of capture latency;
- runtime resize;
- input injection;
- the PyroWave (Vulkan) path.

Those are the next milestones.

## Running it (on the node)

Needs the NVIDIA Container Toolkit's CDI spec, as for [S1e](../s1e-encode-latency/README.md). From the repository copy on the node:

```bash
docker compose -f spikes/s2-compositor/compose.yaml run --rm --build s2-compositor
```

The first build takes several minutes. It installs Ubuntu 26.04, GStreamer 1.28, Rust, gst-wayland-display at `016b4fc` built with `--features cuda`, and Google Chrome stable.

The run takes about 6 minutes. It writes JSON to `spikes/s2-compositor/results/`. Nothing listens on the network.

Chrome runs with `--no-sandbox`, because the container runs as root. That's acceptable for a local test page. The real environment image runs Chrome as an unprivileged user with its sandbox.

## Results: milestone 1, gpu-node.lan, 2026-10-03

- **Stack:**
  - RTX 4090, driver 595.71.05, GPU through CDI;
  - GStreamer 1.28.2;
  - gst-wayland-display `016b4fc` built with `--features cuda`;
  - Google Chrome 154.0.8037.97.
- **Run:** 2560×1440 at 60 fps, 15 s measured per run.
- **Raw data:** [`docs/benchmarks/s2-2026-10-03-gpu-node-rtx4090-chrome154.json`](../../docs/benchmarks/s2-2026-10-03-gpu-node-rtx4090-chrome154.json).
- **Shared GPU:** ComfyUI was resident on the same GPU during the run.

Compositor → encoded is p50 / p95 / p99, in ms.

| Codec | Source path | Client | Compositor → encoded | Compositor / encoder fps | Compositor+encoder CPU | Chrome CPU | GPU util |
|---|---|---|---|---|---|---|---|
| HEVC | `cuda` (zero-copy) | none | **1.68** / 1.71 / 1.75 | 60 / 60 | 1.5% | – | 1% |
| HEVC | `cuda` (zero-copy) | Chrome | **1.72** / 1.78 / 1.83 | 60 / 60 | 2.5% | 7.6% | 2% |
| HEVC | `sysmem` (copy) | none | 2.69 / 2.76 / 2.80 | 60 / 60 | 16.1% | – | 9% |
| HEVC | `sysmem` (copy) | Chrome | 2.80 / 2.93 / 3.67 | 60 / 60 | 17.7% | 8.7% | 10% |
| H.264 | `cuda` (zero-copy) | none | **1.81** / 1.83 / 1.85 | 60 / 60 | 1.5% | – | 1% |
| H.264 | `cuda` (zero-copy) | Chrome | **1.82** / 1.88 / 1.93 | 60 / 60 | 2.6% | 7.7% | 2% |
| H.264 | `sysmem` (copy) | none | 2.83 / 2.88 / 2.92 | 60 / 60 | 16.0% | – | 9% |
| H.264 | `sysmem` (copy) | Chrome | 2.85 / 2.98 / 3.22 | 60 / 60 | 16.7% | 7.8% | 9% |

**Chrome under the compositor:**
- It renders on the GPU: `ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX 4090/PCIe/SSE2, OpenGL ES 3.2)`.
- It fills the 2560×1440 output in kiosk mode and holds 60 fps on the full-screen WebGL page.
- Its D-Bus errors are harmless in a container.

**`dmabuf`** (`dmabuftocuda`) didn't run: Ubuntu's GStreamer has no such element.

### Verdict (milestone 1)

- **gst-wayland-display works headless on NVIDIA in a container, with the GPU from CDI.** No X server, no host display, no udev rules, no privileges.
- **The zero-copy CUDA path is nearly free.**
  - Compositor → encoded is 1.7–1.8 ms at 1440p60, p99 ≤ 1.93 ms. NVENC's own average is 1.55 ms, so the compositor and CUDA hand-off add ~0.15 ms.
  - The process uses 1.5–2.6% of one core. With Chrome rendering, latency moves by 0.01–0.03 ms.
- **The copy path costs ~1 ms per frame and ~10× the CPU** (16–18%). It's the fallback where zero-copy isn't available.
- **Google Chrome is viable as the first environment** on this compositor: GPU-rendered, 60 fps, under 8% CPU.
- **For the plan:** gst-wayland-display passes on NVIDIA. Still open:
  - the same run on AMD/Intel (VA-API DMA-BUF);
  - Chrome commit → compositor frame;
  - runtime resize and input injection;
  - the PyroWave/Vulkan path (the `feat/vulkan-encode` branch);
  - pixelflux as the alternative.

### Next milestones

1. **Live stream to the browser.** Compositor → NVENC → our WebRTC session (S3's str0m code) → the S1d page, plus keyboard and mouse back into the compositor. That is a minimal `cha-streamer` and gives the first truly end-to-end numbers on our own engine.
2. **Runtime resize** (caps renegotiation mid-stream) and **Chrome commit → frame** latency.
3. The same bench on an AMD or Intel GPU, and pixelflux for comparison.

## Milestone 2: live stream with input (`server/`, the `s2-streamer` crate)

A minimal `cha-streamer`: the compositor and Chrome from milestone 1, streamed live to a browser with keyboard and mouse back.

```
Chrome ─Wayland─▶ waylanddisplaysrc ─CUDA─▶ tee ─▶ nvh265enc ─▶ h265parse ─▶ appsink ─┐
 (kiosk, GPU)      (compositor)               └──▶ nvh264enc ─▶ h264parse ─▶ appsink ─┤
                         ▲                                                            ▼
                         └── MouseMoveAbsolute / MouseButton / MouseAxis / KeyboardKey ◀── str0m WebRTC session ◀─▶ browser
```

**Media:**
- Each codec has an always-on NVENC branch: p1, ultra-low-latency, CBR, infinite GOP.
- The parser repeats the parameter sets before every keyframe.
- A new viewer, or a browser keyframe request (PLI/FIR), sends a force-key-unit event up its branch.
- Frames go out as a WebRTC video track: str0m, playout-delay 0, the same sender as S3.

**Input:**
- The page sends `{"t":"input",...}` messages on the `control` channel while the pointer is over the video:
  - pointer position, normalized to the picture;
  - buttons, wheel, and `KeyboardEvent.code`.
- The streamer maps them to the compositor's events: positions in output pixels, Linux `BTN_*` buttons, evdev `KEY_*` keys (`src/input.rs`).
- They reach the compositor as custom upstream events on its source pad.

**What it reports** (in the `stats`/`done` control messages, which the page saves):
- compositor → encoded per frame;
- encoded → sent per frame;
- frame interval;
- keyframes and keyframe requests;
- input counts.

The page adds send → decoded → compositor. Together they give **compositor → browser compositor**.

**Access:**
- The stream is a live, controllable browser on the LAN, so `POST /webrtc/media` needs a token.
- The token is checked before the body is read; a missing or wrong token gets 403.
- The streamer generates one at startup and logs it. Set it with `S2_TOKEN` to keep it fixed.

### Running it

On the node:

```bash
docker compose -f spikes/s2-compositor/compose.yaml --profile live up -d --build s2-streamer
```

To read the token from the log:

```bash
docker compose -f spikes/s2-compositor/compose.yaml logs s2-streamer | grep -o 'token [0-9a-f]*' | tail -1
```

On the Mac, open the S1c/S1d page (`bun run --cwd spikes/s1c-codec-compare/web dev`) in Chrome:

1. Set the host to `gpu-node.lan`, the port to `4495`, and the token.
2. Click **Load streams**. You get `chrome-hevc` and `chrome-h264`.
3. To measure: **Run matrix** (15 s per codec), then **Copy results JSON**.
4. To use it: set *Seconds per stream* to e.g. `600` and **Run** one stream. Move the mouse, click and type over the video. The page in the stream echoes the pointer, clicks, wheel and keys, and flashes white on every click.

Stop it with:

```bash
docker compose -f spikes/s2-compositor/compose.yaml --profile live down
```

### Results: milestone 2, Chrome 154 on the baseline LAN, 2026-10-03

- **Node:** `gpu-node.lan` (RTX 4090), with `s2-streamer` at 2560×1440, 60 fps, 40 Mbit/s CBR.
- **Content:** Google Chrome showing `live.html` (moving gradient and sweeping bar). The CBR filled ~71 KB per frame (35 Mbit/s).
- **Client:** Chrome 154, visible tab, on the M4 Pro over 1 GbE.
- **Run:** 15 s per codec, with input forwarded during the run.
- **Raw data:** [`docs/benchmarks/s2m2-2026-10-03-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/s2m2-2026-10-03-m4pro-chrome154-gpu-node.json).

All times are ms, p50 / p95 (p99 where noted).

| Stage | H.264 | HEVC |
|---|---|---|
| Compositor → encoded (node) | 1.89 / p99 2.10 | 1.85 / p99 2.07 |
| Encoded → sent (node) | 0.019 / p99 0.10 | 0.021 / p99 0.11 |
| Sent → last packet | 2.00 / 2.94 | 1.91 / 2.80 |
| Sent → decoded | 3.55 / 5.29 | 3.79 / 5.43 |
| **Sent → browser compositor** | **3.84** / 7.94 | **4.15** / 7.83 |
| Sent → expected display | 16.7 / 22.4 | 18.3 / 33.1 |
| **Compositor → browser compositor** (sum of p50s) | **≈ 5.8** | **≈ 6.0** |
| Chrome decode avg / jitter buffer avg | 2.25 / 0.88 | 2.10 / 0.80 |
| Frames shown / sent; lost; keyframes | 893 / 901; 0; 1 | 900 / 901; 0; 1 |
| Input events forwarded (unmapped) | 504 (0) | 899 (0) |

### Verdict (milestone 2)

- **Our own engine works end to end.**
  - The path: Chrome in the compositor on the node GPU → NVENC zero-copy → str0m WebRTC → Chrome on the Mac, with keyboard and mouse back into the compositor.
  - A new viewer gets its first frame (a forced keyframe) **14–17 ms** after subscribing.
- **About 6 ms from the node's compositor to the Mac's compositor (p50)** at 1440p60 on 1 GbE.
  - p95 is ~10 ms.
  - The panel shows the frame ~12–14 ms later (`expectedDisplayTime`), which is the Mac's compositor → panel pipeline at 120 Hz.
- **This WebRTC sender is ~1.5 ms faster than S1d's.** S1d's thread-based str0m loop (`webrtc_media.rs`) measured ~3.1 ms to the last packet and 5.4 ms to the compositor on similar frames. The async session used by S3 and S2 measures 2.0 and 3.8 ms. It is the base for `cha-streamer`'s WebRTC endpoint.
- **Not yet measured:**
  - Chrome's own render → compositor commit on the node (≤ 1 frame);
  - the input → screen round trip.

  The live page flashes white on every click so that a software harness can time it. That is the obvious next step for S4.

### GPU contention (same run, GPU shared with a busy llama.cpp server)

A second milestone-2 run happened while a `llama-server` (CUDA) held the RTX 4090 at ~93% utilization, drawing 332 W. The stream stayed clean: 901/901 frames shown, nothing lost. It got slower, though:

| p50 (p99/p95) | Idle GPU | GPU busy with llama.cpp |
|---|---|---|
| Compositor → encoded | 1.85–1.89 ms (p99 ~2.1) | 3.85–4.94 ms (p99 6.9–7.7) |
| Sent → browser compositor | 3.84–4.15 ms | 5.47–5.95 ms |
| Frame interval p99 | 17.0 ms | 19.2–20.2 ms |

Raw data: [`docs/benchmarks/s2m2-2026-10-03-m4pro-chrome154-gpu-node-gpu-busy.json`](../../docs/benchmarks/s2m2-2026-10-03-m4pro-chrome154-gpu-node-gpu-busy.json).

NVENC is its own engine, but the compositor's rendering and the CUDA hand-off time-slice with the other job. For the node design, this means:
- streaming sessions on a shared GPU need priority, for example CUDA/EGL context priority or MPS limits for the batch job;
- or GPU placement that keeps streaming off GPUs running heavy batch work. That belongs to the plan's placement work in Phase 3.

## Input → screen probe (first piece of S4)

The page can time the full round trip in software, without a camera:
- **Click:** with **Input → screen probe** ticked, the page sends a synthetic left click every 500 ms over the `control` channel (after a 2 s warm-up). It parks the remote pointer in the middle first.
- **Flash:** `live.html`, in the stream, turns the whole screen white on every click.
- **Detection:** the page samples a 4×4 patch of the bottom-right corner of every presented video frame (`requestVideoFrameCallback` → canvas). The first frame brighter than luma 200 closes that click.
- **Stages:** the streamer echoes when each probe click arrived (`{"t":"probe"}`). Every `sent` message also carries the frame's composited and encoded times (`c_us`, `e_us`). With the page's clock sync, each click splits into five stages:

| Stage | From → to |
|---|---|
| `clickToStreamer` | page → streamer (DataChannel) |
| `streamerToComposited` | input into the compositor → Chrome handles the click and paints → the compositor's next frame includes it |
| `compositedToEncoded` | NVENC |
| `encodedToSent` | the streamer hands the frame to str0m |
| `sentToPresented` | network, Chrome decode, the page's compositor |

`totalMs` is click → presented. `toDisplayMs` adds Chrome's expected display time, which is still short of photons: the panel's own latency isn't included. Results land in the run's JSON as `inputProbe`. Keep your hands off the video while it runs.

### Results: click → screen, 2026-10-03

**Through the stream** (the browser probe):
- **Setup:** Chrome 154 on the M4 Pro ← `s2-streamer` with a 60 fps compositor and 60 fps encode.
- **Run:** 25 clicks per codec.
- **Raw data:** [`docs/benchmarks/s4probe-2026-10-03-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/s4probe-2026-10-03-m4pro-chrome154-gpu-node.json).

| p50 (p95) | H.264 | HEVC |
|---|---|---|
| Page → streamer | 0.47 ms | 0.43 ms |
| **Streamer → flash composited** | **76.8 ms** (77.1) | **72.6 ms** (73.1) |
| Composited → encoded | 2.03 ms | 1.90 ms |
| Encoded → sent | 0.02 ms | 0.02 ms |
| Sent → browser compositor | 3.28 ms | 1.90 ms |
| **Click → browser compositor** | **82.6 ms** (83.0) | **76.9 ms** (77.3) |
| Click → expected display | 94.9 ms | 88.6 ms |

The streaming path costs ~6 ms. **Almost all of the round trip is Chrome reacting inside the compositor.**

**On the node, without a browser** (`S2_MODE=input` in `bench/bench.py`):
- Synthetic clicks go straight into the compositor.
- A pad probe finds the first white frame leaving it, or leaving a decimator.
- 30 clicks per case, a fresh Chrome per case.
- The probe page's flash lasts a fixed 100 ms, so it's visible at any frame rate.
- **Raw data:**
  - [Chrome flags](../../docs/benchmarks/s2input-2026-10-03-gpu-node-chrome-flags.json)
  - [compositor rates](../../docs/benchmarks/s2input-2026-10-03-gpu-node-compositor-rates.json)
  - [decimation](../../docs/benchmarks/s2input-2026-10-03-gpu-node-decimation.json)

| Case | Click → frame p50 (p95) | Compositor frames | Note |
|---|---|---|---|
| Compositor 60 fps | 74.5–76.0 ms (~83–86) | ~4.5 | Seven sessions, all 74–76 ms except one at 60 ms |
| `--disable-gpu-vsync` / `--disable-frame-rate-limit`, 60 fps | 60.2–62.2 ms | ~3.6 | Same as the baseline in that round: **Chrome flags don't help** |
| Compositor 120 fps | 13.6–39.6 ms | 1.6–4.8 | Large spread between Chrome sessions |
| Compositor 240 fps | 16.4–20.4 ms (~18–24) | ~4–5 | Consistent |
| **Compositor 240 fps → every 4th frame forwarded (60 fps)** | **24.2 ms** (32.6) | – | What an encoder at 60 fps sees |
| Compositor 240 fps → every 2nd frame forwarded (120 fps) | 28.5 ms (37.0) | – | |

**Confirmed through the stream.** The same browser probe, with `s2-streamer` at `--compositor-fps 240 --fps 60` (25 clicks per codec). Raw data: [`docs/benchmarks/s4probe-2026-10-03-m4pro-chrome154-gpu-node-240to60.json`](../../docs/benchmarks/s4probe-2026-10-03-m4pro-chrome154-gpu-node-240to60.json).

| p50 (p95) | H.264, 60 → 240/60 | HEVC, 60 → 240/60 |
|---|---|---|
| Page → streamer | 0.47 → 1.14 ms | 0.43 → 0.53 ms |
| **Streamer → flash composited** | 76.8 → **29.1 ms** (30.5) | 72.6 → **29.2 ms** (29.4) |
| Composited → encoded | 2.03 → 1.95 ms | 1.90 → 1.87 ms |
| Sent → browser compositor | 3.28 → 3.26 ms | 1.90 → 1.96 ms |
| **Click → browser compositor** | 82.6 → **35.4 ms** (36.5) | 76.9 → **33.6 ms** (34.9) |
| Click → expected display | 94.9 → 55.6 ms | 88.6 → 46.7 ms |

The stream stayed clean (895 and 901 of 901 frames shown, nothing lost), and the encoder and network still carry 60 fps.

### Verdict: input latency

- **Chrome inside the compositor reacts ~4.5 compositor frames after the click.** That is the usual input → BeginFrame → commit → raster → swap pipeline, plus the compositor's own frame. Chrome's vsync and frame-rate flags don't change it consistently. **The compositor's frame rate does.**
- **Run the compositor (and the app) fast, and encode only what the network needs.**
  - Compositor at 240 fps, every 4th frame encoded: click → forwarded frame **24 ms instead of 76 ms** on the node. Through the stream, **click → browser compositor is 34–35 ms instead of 77–83 ms**. The encoder and network still carry 60 fps.
  - The decimator decides on arrival: a pad probe drops frames before their slot is due, so there's no extra frame of delay. `capssetter` tells the encoders the real rate, so rate control budgets per encoded frame.
  - GPU cost: compositor plus Chrome at 240 fps took ~22% of the RTX 4090 on this always-animating page. Idle desktop content renders only on damage.
- **Implication for the plan.** `cha-streamer` should run its compositor at 2–4× the encode rate (`--compositor-fps`). The click → screen budget at 1440p60 on 1 GbE then splits roughly into:
  - ~24 ms app and compositor;
  - ~2 ms encode;
  - ~4 ms network, decode and present;
  - plus the client display's own latency.
- **Variance between Chrome sessions is large** (±10 ms at 120 fps). Comparisons need several sessions per configuration, and the latency harness (S4) should report it.
- **Shared GPU:** when ComfyUI or llama.cpp work ran at the same time, the compositor couldn't hold its rate and clicks were missed. The bench now records GPU utilization and flags starved cases.

## Milestone 3: runtime resize

**How it works:**
- **Streamer:** the compositor's output caps sit in a named `capsfilter`. `Media::resize(w, h)` sets new caps (clamped to 320×240 … 3840×2160, multiples of 8) and asks every encoder branch for a keyframe.
- **Pipeline:**
  - the source renegotiates and resizes its output;
  - the compositor reconfigures the app's window;
  - NVENC restarts at the new size;
  - the parser carries the new parameter sets to the browser.
- **Page:** sends `{"t":"resize","w":…,"h":…}` on the `control` channel; the streamer answers `{"t":"resized",…}`. With **Resize test** ticked, it steps the output through 1920×1080, 1280×720 and back to 2560×1440, 3 s apart. For each step it times request → first presented frame at the new size, and the longest gap between presented frames.
- **On the node,** `S2_MODE=resize` in the bench does the same without a browser.

**A gst-wayland-display bug, fixed in `patches/0001-resize-keep-unbounded-windows-full-size.patch`:**
- **Cause:** on renegotiation, the compositor caps each window at the client's `max_size`. In xdg-shell, 0 means "no maximum". Intersecting with a 0×0 rectangle sent Chrome a configure with **no size**.
- **Symptom:** after the first resize, Chrome fell back to a 500×46 window.
- **Fix:** clamp each dimension only when the client sets a real maximum, as the initial configure already does.

The Dockerfile applies the patch on top of `016b4fc`. It is worth sending upstream.

### Results (node, RTX 4090, Chrome 154, 2026-10-03)

**Raw data:** [patched](../../docs/benchmarks/s2resize-2026-10-03-gpu-node-rtx4090.json), [before the fix](../../docs/benchmarks/s2resize-2026-10-03-gpu-node-rtx4090-unpatched.json).

| Step | Compositor at new size | Chrome window resized | Encoded at new size | Longest gap between encoded frames |
|---|---|---|---|---|
| HEVC 2560×1440 → 1920×1080 | 45.6 ms | 33.3 ms | 48.1 ms | 30.1 ms |
| HEVC → 1280×720 | 37.8 ms | 26.6 ms | 39.6 ms | 28.7 ms |
| HEVC → 2560×1440 | 26.0 ms | 17.7 ms | 29.5 ms | 28.2 ms |
| H.264, same steps | 36.0–43.8 ms | 22.7–32.5 ms | 37.9–45.3 ms | 28.6–34.6 ms |

**Verdict:**
- **Runtime resize works.** A new size takes effect in 30–48 ms, end to end on the node. The stream skips about one frame, with no errors or encoder restarts that stall.
- **Before the fix,** the pipeline resized the same way, but Chrome's window collapsed. Any client that sets no maximum size hits the same bug.
- **Not yet done:**
  - the browser end to end (the page's **Resize test**);
  - following the viewer's window automatically: a ResizeObserver on the player, debounced, rounded to multiples of 8. That belongs in `@cha/player` in Phase 2.

