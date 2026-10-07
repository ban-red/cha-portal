# Spike S1c: PyroWave vs H.264 / HEVC / AV1, head to head

**Question.** S1b showed PyroWave decodes in Chrome on the M4 Pro, but at ~6–8 ms per 1440p frame at 60 fps, because the Apple GPU stays at low clocks. Hardware HEVC/AV1 move ~10× fewer bytes and decode on the fixed-function media engine. **On the baseline client, which path gets a frame on screen sooner?**

## Method

- **Same content.** 1440p60 `mandelbrot` (detailed and moving), encoded six ways by `tools/make-streams.py`:

| Stream | Encoder and settings | Actual rate | Typical frame |
|---|---|---|---|
| `pyrowave-420-290m` | PyroWave (from S1b), byte cap 604 KB | 290 Mbit/s | 604 KB |
| `pyrowave-444-590m` | PyroWave 4:4:4, byte cap 1.23 MB | 581 Mbit/s | 1.23 MB |
| `hevc-420-40m` | x265 ultrafast + zerolatency, no B-frames, CBR with a 1-frame VBV, one IDR | 50 Mbit/s | 99 KB |
| `hevc-420-80m` | same, 80 Mbit/s target | 84 Mbit/s | 173 KB |
| `h264-420-40m` | x264 ultrafast + zerolatency, same rate-control rules | 35 Mbit/s | 72 KB |
| `av1-420-40m` | libaom realtime (cpu-used 8, no lag), same rate-control rules | 39 Mbit/s | 83 KB |

- **Same transport.** The S1 server replays each stream from memory at 60 fps over WebTransport datagrams, using the `cha-stream/1` framing. A worker reassembles frames and timestamps them against the server's clock (ping-based sync).
- **Decoding:**
  - PyroWave: `@cha/pyrowave-webgpu`, decode and draw in one GPU submission.
  - Everything else: WebCodecs `VideoDecoder` (`optimizeForLatency`, hardware preferred), drawn with WebGPU from an external texture.
- **Per frame, measured in ms from the moment the server sent it:**
  - **last fragment** received;
  - **decoded**: the WebCodecs output callback, or for PyroWave, GPU done;
  - **drawn**: GPU done after drawing into the canvas;
  - **on screen**: the next animation-frame callback after drawing, i.e. the rendering update that includes it.
  - **Decode only** = decoded − last fragment.

**Not included, so add them when comparing:**
- **Capture and encode on the node.** File replay skips both. [S1e](../s1e-encode-latency/README.md) measured encode on the RTX 4090 at 1440p:
  - NVENC H.264/HEVC/AV1: 1.7–2.2 ms;
  - PyroWave through the WebGPU port: 1.0 ms (4:2:0) and 1.9 ms (4:4:4), of which only 0.2–0.3 ms is GPU work.
- **Scan-out after the rendering update.** Up to one 120 Hz refresh, the same for every codec.

## Running it

On the Mac, make the streams. This needs the S1b clips (`spikes/s1b-pyrowave-webgpu/tools/make-clips.sh`) and ffmpeg with x264, x265 and libaom:

```bash
python3 spikes/s1c-codec-compare/tools/make-streams.py
```

Sync the repo (without the S1b clips) to the node:

```bash
rsync -az --delete --exclude .git --exclude target --exclude node_modules --exclude dist --exclude clips --exclude .cache --exclude 'spikes/*/results' ./ gpu-node.lan:/home/cha/docker/cha-portal-node/
```

Rebuild and restart the server on the node:

```bash
ssh gpu-node.lan 'cd /home/cha/docker/cha-portal-node && docker compose -f spikes/s1-browser-pyrowave/compose.yaml up -d --build'
```

Start the page on the Mac:

```bash
bun run --cwd spikes/s1c-codec-compare/web dev
```

Open the printed `localhost` URL in **Chrome** and keep the tab **visible**.
1. Set the host to `gpu-node.lan`.
2. Click **Load streams**. The table shows which decoder each stream gets here.
3. Click **Run all streams** (6 × 15 s).
4. Click **Copy results JSON** and save it to `docs/benchmarks/s1c-<date>-<client>-<node>.json`.

## Results: baseline LAN, 2026-10-03

- **Client:** Chrome 154 on the M4 Pro MacBook Pro, visible tab, wired 1 GbE.
- **Server:** `gpu-node.lan` (quinn Cubic).
- **Run:** 15 s per stream (900 frames).
- **Raw data:** [`docs/benchmarks/s1c-2026-10-03-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/s1c-2026-10-03-m4pro-chrome154-gpu-node.json).

Every WebCodecs stream decoded on the hardware video engine. Times are ms from send, p50 / p95.

| Stream | Rate | Frames shown | Last fragment | Decoded | Drawn | **On screen** | Decode only |
|---|---|---|---|---|---|---|---|
| H.264 (x264 zerolatency) | 35 Mbit/s | 900 / 900 | 1.2 / 2.3 | 2.0 / 3.4 | 3.5 / 5.3 | **6.9** / 11.3 | 0.8 / 1.3 |
| AV1 (libaom realtime) | 39 Mbit/s | 900 / 900 | 1.2 / 2.2 | 2.5 / 3.9 | 3.9 / 5.8 | **8.0** / 13.5 | 1.2 / 1.7 |
| HEVC (x265 zerolatency) | 50 Mbit/s | 900 / 900 | 1.4 / 2.8 | 2.4 / 4.1 | 3.8 / 5.6 | **8.0** / 11.5 | 0.9 / 1.5 |
| HEVC | 84 Mbit/s | 900 / 900 | 2.0 / 3.2 | 3.1 / 4.6 | 4.5 / 6.2 | **8.7** / 12.0 | 1.1 / 1.7 |
| PyroWave 4:2:0 | 290 Mbit/s | 899 / 900 | 6.1 / 9.0 | 8.5 / 12.0 | 8.5 / 12.0 | **11.4** / 16.3 | 2.2 / 4.0 |
| PyroWave 4:4:4 | 581 Mbit/s | 890 / 900 | 12.5 / 19.2 | 16.2 / 22.9 | 16.2 / 22.9 | **20.0** / 26.1 | 3.5 / 4.4 |

### Verdict

- **On the baseline client and network, hardware codecs reach the screen first.**
  - H.264, AV1 and HEVC are on screen at 6.9–8.7 ms (p50). PyroWave 4:2:0 is at 11.4 ms and 4:4:4 at 20.0 ms.
  - PyroWave's gap is almost entirely **wire time**: the last fragment lands 6.1 ms after send at 290 Mbit/s, against 1.2–2.0 ms for the hardware streams. Decode isn't the problem.
- **Counting encode doesn't close the gap** (updated with [S1e](../s1e-encode-latency/README.md)).
  - This section first estimated NVENC at ~2–5 ms and predicted a rough tie for PyroWave 4:2:0.
  - Measured on the RTX 4090, NVENC takes 1.7–2.2 ms and the PyroWave port 1.0 ms.
  - With encode added: H.264 ~8.9 ms, HEVC ~9.8, AV1 ~10.0, PyroWave 4:2:0 ~12.4, 4:4:4 ~21.9 (p50).
  - Hardware codecs stay ~2.5–3.5 ms ahead on 1 GbE. PyroWave 4:4:4 doesn't compete on 1 GbE.
- **PyroWave decode is faster when the network is busy.**
  - In this run, decode-only was 2.2 ms (4:2:0) and 3.5 ms (4:4:4). In S1b's synthetic paced runs, submit→done was 8.0 / 8.6 ms.
  - Hypothesis, not yet verified: steady 300–600 Mbit/s of network receive keeps the M4 Pro in a higher performance state, which lifts GPU clocks too.
- **Wire time is where PyroWave can win, but only modestly for 4:2:0.** A 604 KB frame takes ~4.8 ms to serialize on 1 GbE, ~1.9 ms on 2.5 GbE and ~0.5 ms on 10 GbE. Estimated totals with S1e's encode:

| Link | PyroWave 4:2:0 (port / native encode) | H.264 / HEVC |
|---|---|---|
| 2.5 GbE | ~9.5 / ~8.8 ms | ~8.6 / ~9.5 ms: roughly a tie |
| 10 GbE | ~8.1 / ~7.4 ms | ~8.4 / ~9.3 ms: PyroWave ~1–2 ms ahead |

  The stronger reasons to pick PyroWave are quality (4:4:4 text, no motion artifacts), intra-only robustness, and partial-frame decode.
- **Loss:**
  - PyroWave 4:4:4 lost 10 of 900 frames, and 4:2:0 lost 1, to datagram loss at 290–581 Mbit/s. The hardware streams lost none.
  - This harness drops incomplete frames whole. PyroWave can decode a partial frame if packets are block-aligned and critical packets are protected (plan §3.3), so the real player would show a local blur instead of dropping.
- **The draw step is overhead to attack.** Drawing a WebCodecs frame through an external texture costs ~1.4 ms (decoded → drawn), and there are another ~3–4 ms until the next rendering update. Worth comparing the `<video>` / `VideoTrackGenerator` and WebRTC paths (S1d).

### What changes

- **Codec choice:** the default tier on this client is hardware HEVC/AV1/H.264, with PyroWave chosen when the per-client probe says it wins. That means ≥ 2.5 GbE, a desktop-class client GPU, or 4:4:4 text work where quality matters more than ~3 ms. The plan's "measure, don't assume" rule (§3.2) now has data behind it.
- **PyroWave stays first-class:** it is implemented end to end, bit-exact, and works in Chrome today. It just isn't the automatic default on 1 GbE + Apple laptop.

## S1d: present paths (same page)

S1c showed ~1.4 ms to draw a WebCodecs frame through a WebGPU external texture, then ~3–4 ms to the next rendering update. S1d asks which path gets a hardware-decoded frame to the compositor soonest. The **Present path** selector and **Run matrix** button (H.264 / HEVC / AV1 × every path) compare:

| Path | Transport | Decode | Present | "To compositor" measured by |
|---|---|---|---|---|
| `webgpu` | WebTransport | WebCodecs | WebGPU external texture → canvas | next rAF callback after GPU done |
| `video` | WebTransport | WebCodecs | `MediaStreamTrackGenerator` → `<video>` | rVFC `presentationTime` (+ `expectedDisplayTime`) |
| `canvas2d` | WebTransport | WebCodecs | 2D canvas `drawImage` (desynchronized) | next rAF callback after the draw |
| `webrtc` | WebRTC RTP (str0m, `playout-delay` 0/0, abs-capture-time) | browser's WebRTC decoder | `<video>` | rVFC `presentationTime` / `expectedDisplayTime`; receive and decode times from rVFC `receiveTime` / `processingDuration` |

`webrtc` is the Phase 1 default path (§2.3 of the plan), so this also checks that str0m can send HEVC and AV1 to Chrome. It can: all three codecs worked on loopback.

To run it on the LAN, the node needs the updated server, which adds `POST /webrtc/media`, and UDP 4434 open. Then select **Run matrix** in visible Chrome (12 × 15 s) and copy the JSON.

## S1d results: baseline LAN, 2026-10-03

- **Client:** Chrome 154 on the M4 Pro, visible tab, wired 1 GbE.
- **Server:** `gpu-node.lan`, replaying the same three streams as S1c.
- **Run:** 15 s per run (900 frames).
- **Raw data:** [`docs/benchmarks/s1d-2026-10-03-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/s1d-2026-10-03-m4pro-chrome154-gpu-node.json).
- **Decoders:** all hardware. WebCodecs reported hardware support; on WebRTC, `chrome://webrtc-internals` showed `ExternalDecoder (VideoToolboxVideoDecoder)`, power-efficient.
- **Frames:** every run showed 899–900 of 900, with no loss.

Times are ms from the server's send, p50 / p95. **To compositor** is:
- for the canvas paths, the next animation-frame callback after the draw;
- for `<video>` and WebRTC, `requestVideoFrameCallback`'s `presentationTime`.

| Codec | Path | Last packet | Decoded | **To compositor** | Expected display | Chrome decode avg |
|---|---|---|---|---|---|---|
| H.264 | WebGPU external texture | 1.8 / 2.9 | 2.9 / 4.3 | **8.9** / 13.0 | – | – |
| H.264 | track generator → `<video>` | 1.9 / 3.6 | 3.2 / 5.6 | **3.3** / 5.8 | 16.0 / 19.9 | – |
| H.264 | 2D canvas | 1.2 / 2.4 | 1.4 / 3.2 | **2.9** / 8.8 | – | – |
| H.264 | WebRTC RTP | 3.1 / 4.5 | 5.0 / 6.5 | **5.4** / 8.0 | 18.0 / 22.9 | 1.91 |
| HEVC | WebGPU external texture | 1.6 / 3.0 | 1.8 / 3.8 | **7.3** / 11.9 | – | – |
| HEVC | track generator → `<video>` | 2.8 / 4.8 | 3.3 / 5.8 | **3.4** / 5.9 | 15.5 / 20.4 | – |
| HEVC | 2D canvas | 2.0 / 3.4 | 2.0 / 3.9 | **3.5** / 9.5 | – | – |
| HEVC | WebRTC RTP | 3.6 / 6.3 | 5.4 / 8.3 | **5.9** / 9.9 | 18.4 / 24.2 | 1.85 |
| AV1 | WebGPU external texture | 1.6 / 2.9 | 1.3 / 2.9 | **7.5** / 11.4 | – | – |
| AV1 | track generator → `<video>` | 2.5 / 3.9 | 2.6 / 4.4 | **2.7** / 4.5 | 14.4 / 19.0 | – |
| AV1 | 2D canvas | 1.7 / 2.6 | 1.2 / 2.6 | **2.3** / 9.1 | – | – |
| AV1 | WebRTC RTP | 3.5 / 4.7 | 6.1 / 7.3 | **6.4** / 8.5 | 18.6 / 23.4 | 2.51 |

### Verdict

- **Track generator → `<video>` is the fastest present path, and the steadiest.**
  - Hardware-decoded frames reach the compositor **2.7–3.4 ms** after send (p95 4.5–5.9).
  - The 2D canvas matches it at p50 (2.3–3.5 ms). Its p95 is worse (8.8–9.5 ms), because the frame waits for the next rendering update.
- **Don't draw hardware-decoded frames through WebGPU.**
  - The external-texture draw plus the wait for the next rendering update cost **4–5 ms more** than `<video>` (7.3–8.9 ms).
  - S1c used this path, so S1c's hardware-codec "on screen" numbers overstate the cost by ~4–5 ms.
  - WebGPU stays the path for PyroWave, whose decoder writes a GPU texture anyway.
- **WebRTC costs ~2.5–3 ms more than WebTransport + `<video>`** (5.4–6.4 ms p50). It splits roughly into thirds:
  - the last packet arrives ~1–1.5 ms later than over WebTransport;
  - Chrome's RTC decode averages 1.9–2.5 ms, against ≤ 1 ms through WebCodecs;
  - the jitter buffer holds frames 1–2 ms on average, even with playout-delay 0.
- **Expected display is ~12 ms after presentation** on both `<video>` paths. That is the compositor-to-panel pipeline at 120 Hz, and it applies to every path. The canvas paths don't report it.
- **Decode times at or below zero on the WebTransport paths** (decoded ≤ last packet) are a clock artefact. The worker stamps arrival and the main thread stamps decode, and their `performance.timeOrigin` values differ by up to ~0.5 ms. WebCodecs hardware decode is ≲ 1 ms here.

### What changes

- **Phase 1** (the Wolf gateway, S3) uses the WebRTC track: it works in every browser, including Safari. On the baseline that costs ~5.5–6.5 ms from send to compositor.
- **The Chromium fast path** is WebTransport → WebCodecs → track generator → `<video>`: ~3 ms.
  - `MediaStreamTrackGenerator` is Chrome's main-thread API. The standard `VideoTrackGenerator` runs in a worker; use it where available.
  - A 2D canvas is the fallback.
- **Revisiting S1c with the better path:**
  - Hardware codecs reach the compositor at ~3 ms after send, against ~11.4 ms for PyroWave 4:2:0 through WebGPU.
  - Counting encode (S1e: NVENC ~2 ms, PyroWave port ~1 ms), that is **~5 ms vs ~12.4 ms** on 1 GbE + M4 Pro. The default tier stays hardware codecs.
