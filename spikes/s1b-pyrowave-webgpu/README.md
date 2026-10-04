# Spike S1b: PyroWave decode in the browser (WebGPU)

**Question (docs/PLAN.md §9, Phase 0):** S1 showed Chrome can *receive* PyroWave rates over WebTransport. Can it also *decode* PyroWave on the GPU and draw it, fast enough and with correct pixels, on the baseline client (Chrome on the M4 MacBook Pro)?

## What's here

- **[`web/packages/pyrowave-webgpu`](../../web/packages/pyrowave-webgpu/README.md) (`@cha/pyrowave-webgpu`):** a TypeScript PyroWave decoder for WebGPU.
  - The WGSL shaders are vendored unchanged from [imbcmdth/pyrowave@webgpu](https://github.com/imbcmdth/pyrowave/tree/webgpu) (`5e80f92`, MIT).
  - The host side (block layout, packet parser, dequant/iDWT dispatch planning) is ported from that branch's C++.
  - It decodes into packed 8-bit planes in a GPU buffer. `YuvRenderer` draws them straight to a canvas: no readback and no copies.
  - Needs WebGPU `subgroups`, so Chrome/Edge only for now.
- **`web/`:** the bench page. It has three modes:
  - **Validate:** decode frame 0 and compare it with the native decoder's output.
  - **Throughput:** back-to-back decodes, 3 frames in flight.
  - **Paced 60 fps:** decode and draw on the display's frame clock, like a stream would.

  It reports CPU parse time, GPU time per stage (timestamp queries), and submit → GPU-done time.
- **`tools/make-clips.sh`:** builds the fork's native encoder/decoder (wgpu-native) and encodes the 1440p test clips at the S1 byte budgets. It also writes frame-0 references with `tools/make-reference.py`.

## Running it (on the Mac)

```bash
spikes/s1b-pyrowave-webgpu/tools/make-clips.sh
```

```bash
bun run --cwd spikes/s1b-pyrowave-webgpu/web dev
```

Open the printed `localhost` URL in **Google Chrome**, keep the tab **visible**, and click **Run all clips** (about 1.5 minutes). Then click **Copy results JSON** and save it to `docs/benchmarks/s1b-<date>-<client>.json`.

The clips (`testsrc2` and `mandelbrot`, 2560×1440, 60 frames) come in two budgets:
- 4:2:0 at 604 KB/frame (290 Mbit/s at 60 fps);
- 4:4:4 at 1.23 MB/frame (590 Mbit/s).

## Results: M4 Pro, Chrome 154 (visible tab), 2026-10-03

Raw data: [`docs/benchmarks/s1b-2026-10-03-m4pro-chrome154.json`](../../docs/benchmarks/s1b-2026-10-03-m4pro-chrome154.json). GPU time comes from timestamp queries; submit→done is wall clock until the queue reports the work finished. All times in ms.

| 1440p clip | vs native | **Back to back** GPU p50 / p95 | **Paced 60 fps** GPU p50 / p95 | Paced submit→done p50 / p95 |
|---|---|---|---|---|
| testsrc2 4:2:0 (604 KB) | bit-exact | 1.33 / 1.74 | **6.15 / 6.36** | 8.0 / 8.7 |
| mandelbrot 4:2:0 (604 KB) | bit-exact | 1.33 / 1.64 | **6.15 / 6.44** | 7.9 / 8.8 |
| testsrc2 4:4:4 (1.23 MB) | bit-exact | 2.45 / 2.78 | **6.74 / 10.46** | 8.7 / 12.5 |
| mandelbrot 4:4:4 (1.23 MB) | bit-exact | 2.47 / 2.63 | **6.57 / 10.94** | 8.5 / 12.8 |

In both modes the inverse wavelet transform is ~90% of the GPU time, and CPU packet parsing is 0.1–0.3 ms. An earlier run in the Claude app's hidden browser pane (Chromium 152) matched the back-to-back numbers within 5%. Its paced numbers were invalid because rAF was throttled to ~1 Hz.

**Paced at 120 fps** (Chrome 154, 10 s; raw data in [`…-paced120.json`](../../docs/benchmarks/s1b-2026-10-03-m4pro-chrome154-paced120.json)):

| 1440p clip | Frames decoded (of 1200) | GPU p50 / p95 | Submit→done p50 / p95 |
|---|---|---|---|
| testsrc2 4:2:0 | 1062 (~106 fps) | **4.04** / 6.17 | 5.9 / 8.4 |
| testsrc2 4:4:4 | 1111 (~111 fps) | **5.40** / 6.30 | 7.0 / 9.0 |

The higher duty cycle keeps the GPU at higher clocks: per-frame decode drops from 6.2 to 4.0 ms (4:2:0) and from 6.6 to 5.4 ms (4:4:4). That is still 2–3× the back-to-back figure. The loop sustained only ~106–111 of 120 fps, so 1440p120 PyroWave is at the edge of what this client can decode and draw inside an 8.3 ms frame.

### Verdict (decode half of S1)

- **Correct: yes.** Bit-exact against the native decoder for 4:2:0 and 4:4:4.
- **Fast enough: yes, but not "~0.1 ms" on this client.**
  - With the GPU warm, a 1440p frame costs 1.3 ms (4:2:0) or 2.5 ms (4:4:4).
  - **At a real 60 fps cadence the Apple GPU stays at low clocks, and the same work takes 6.2 / 6.6 ms (p50), or 8 / 8.6 ms from submit to done.** Every stage slows down by the same factor, about 4.6× for 4:2:0, which points to GPU frequency rather than our code.
  - The gate's "decode < 1 ms" was set from desktop-GPU numbers. It fails on this client.
- **Latency budget, 1440p60, baseline (Chrome + M4 Pro + 1 GbE), before present:**

| | Last fragment arrives (S1) | + decode done (S1b paced) | **≈ total, p50** |
|---|---|---|---|
| PyroWave 4:2:0, 290 Mbit/s | 5.6 ms | 8.0 ms | **≈ 13.6 ms** |
| PyroWave 4:4:4, 590 Mbit/s | 11.1 ms | 8.6 ms | **≈ 19.7 ms** |

  Then present adds up to one 120 Hz refresh (0–8.3 ms) plus compositing.

**What this means.** On *this* client, PyroWave's advantage over hardware codecs is no longer obvious. A low-latency HEVC/AV1 stream at 50–100 Mbit/s spends about 1 ms on a 1 GbE wire. It decodes on the Mac's fixed-function media engine, which doesn't depend on GPU clocks. So it might match or beat PyroWave's ~13.6 ms. PyroWave still has qualities hardware codecs don't:
- no keyframes, and loss shows up as a local blur rather than a stall;
- 4:4:4 text clarity;
- constant quality and latency regardless of content;
- no NVENC session limit on the node.

The tier choice should therefore be **measured per client**, not assumed. S1c should run PyroWave and HEVC/AV1 (WebCodecs) **head to head, glass to glass**, on this rig.

Levers that could pull PyroWave's decode back down:
1. Raise the GPU's duty cycle. A 1440p120 stream gives the GPU twice the work at the same per-frame cost; whether that raises clocks is measurable now with **Paced rate = 120**.
2. Store the pyramid in `r16float`: half the memory traffic, and what the Metal port found fastest on Apple.
3. Use fewer, larger iDWT dispatches. At low clocks, per-dispatch overhead and barriers weigh more.
4. On macOS, a native client using the Metal port. The GPU clock behaviour is the same, but the Metal port's shaders are already tuned for Apple.

## Next

- [x] Paced run in visible Chrome 154. Results above.
- [x] Paced at 120 fps: partly. Decode drops to 4.0 / 5.4 ms, but only ~106–111 fps are sustained.
- [ ] S1c: PyroWave vs WebCodecs HEVC/AV1, glass to glass on the LAN, same rig.
- [ ] Subgroup-free dequant path, for Safari and Firefox on Windows/macOS.
- [ ] Try an `r16float` pyramid where `texture-formats-tier1` is available.
