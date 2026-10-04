# Spike S6: PyroWave's picture against the hardware codecs

**Question.** On the baseline (1 GbE, M4 Pro), PyroWave reaches the screen later than NVENC's codecs (S1c/S1e: ~12 ms for 4:2:0 against ~5 ms) and needs 7–15× the bitrate. Is its picture enough better on desktop content (text, UI, code) to be worth offering anyway, and which content shows it?

## What it measures

| Step | How |
|---|---|
| Content | `page.html`, rendered by headless Chrome at 2560 px wide: a docs-style page with a sidebar of small UI text, prose with links and a table, and syntax-coloured code on a dark panel (colour detail is where 4:2:0 and 4:4:4 differ). |
| Scenarios | 1440p60 crops of the tall screenshot: `static` (60 frames of one view), `scroll` (120 frames at 600 px/s) and `flick` (120 frames at 2400 px/s). |
| Source | RGB → YCbCr BT.709 limited range, 4:4:4; 4:2:0 copies are downsampled from it. Every codec gets the same YCbCr. |
| H.264, HEVC | ffmpeg's `h264_nvenc`/`hevc_nvenc` with the streamer's settings: preset P1, ultra-low-latency tune, CBR 40 Mbit/s, a one-frame VBV, no B-frames, infinite GOP (one keyframe, then P-frames). Decoded by ffmpeg. |
| PyroWave | `pyrowave-quality.c`: libpyrowave (the streamer's build, `89f7e47`) encodes each frame within the streamer's budget (604,166 bytes for 4:2:0, 1,208,333 for 4:4:4 at 1440p60), cuts it into 1100-byte packets as the streamer does, and decodes it from those packets. |
| Metrics | Against the 4:4:4 source, after upsampling 4:2:0 output: mean PSNR per plane (Y, Cb, Cr), the worst frame's luma PSNR, frame 0's luma PSNR (H.26x's keyframe), and SSIM. |
| Crops | Frame 60 of `scroll`, two 400×225 regions (prose, code) enlarged 3×: source, H.264, HEVC, PyroWave 4:2:0, PyroWave 4:4:4. |

No losses are simulated: this is the picture a viewer gets on a clean LAN.

## Running it (on the node)

Build the streamer's runtime image first (S6 takes libpyrowave from it), then from the repository root (creating `results/` first keeps the output yours rather than root's):

```bash
mkdir -p spikes/s6-quality/results
```

```bash
docker compose -f spikes/s6-quality/compose.yaml run --rm --build s6-quality
```

It needs about 4 GB of scratch space under `spikes/s6-quality/results/` while it runs, and keeps only `summary.md`, `results.json`, `crops.png` and the rendered `page.png`.

## Results (gpu-node, RTX 4090, 2026-10-04)

[`docs/benchmarks/s6-2026-10-04-gpu-node-rtx4090-quality.json`](../../docs/benchmarks/s6-2026-10-04-gpu-node-rtx4090-quality.json). Mean over each scenario's frames; dB against the 4:4:4 source.

| Scenario | Codec | Mbit/s | PSNR Y | PSNR Cb / Cr | Worst frame Y | SSIM (all planes) |
|---|---|---|---|---|---|---|
| static | H.264 | 40.0 | 41.9 | 35.7 / 36.2 | 30.6 (keyframe) | 0.958 |
| static | HEVC | 18.8 | 42.2 | 35.8 / 36.2 | 29.7 (keyframe) | 0.958 |
| static | PyroWave 4:2:0 | 290 | 41.9 | 36.2 / 36.5 | 41.9 | 0.960 |
| static | PyroWave 4:4:4 | 580 | 48.7 | 45.8 / 46.4 | 48.7 | 0.995 |
| scroll | H.264 | 40.0 | 42.8 | 36.4 / 37.0 | 30.6 (keyframe) | 0.964 |
| scroll | HEVC | 10.8 | 43.0 | 36.5 / 37.0 | 29.7 (keyframe) | 0.964 |
| scroll | PyroWave 4:2:0 | 290 | 46.5 | 36.8 / 37.3 | 41.4 | 0.966 |
| scroll | PyroWave 4:4:4 | 580 | 54.5 | 50.8 / 51.5 | 48.5 | 0.998 |
| flick | H.264 | 40.0 | 42.8 | 36.9 / 37.5 | 30.6 (keyframe) | 0.968 |
| flick | HEVC | 13.1 | 43.0 | 36.9 / 37.6 | 29.7 (keyframe) | 0.968 |
| flick | PyroWave 4:2:0 | 290 | 50.1 | 37.3 / 38.1 | 41.8 | 0.972 |
| flick | PyroWave 4:4:4 | 580 | 59.5 | 55.7 / 56.4 | 48.5 | 0.999 |

- **Chroma is decided by subsampling, not by the codec.** Everything 4:2:0 lands at 36–38 dB in Cb and Cr: H.264, HEVC and PyroWave alike. PyroWave 4:4:4 is 10–19 dB higher. On screen (the crops, 3×), coloured text on the dark code panel has slightly smeared edges in every 4:2:0 stream and none in 4:4:4; at 1× it's subtle.
- **The hardware codecs' keyframe is the weak frame.** With a one-frame VBV, the keyframe gets ~83 KB at 40 Mbit/s and comes out at ~30 dB, visibly soft. The P-frames after it catch up to ~42–43 dB within the first second. It happens at the start, after a loss and after a codec switch. Every PyroWave frame is as good as the rest.
- **Luma under motion favours PyroWave.** Static, PyroWave 4:2:0 ties the hardware codecs (~42 dB). Scrolling, it's 3.5–7 dB ahead, since each frame gets its whole budget. 4:4:4 is 6–16 dB ahead of everything.
- **HEVC doesn't need its 40 Mbit/s here.** NVENC's CBR without filler used 11–19 Mbit/s on this content and still matched H.264 at 40 (H.264 pads to the rate). A higher cap wouldn't buy chroma back; 4:2:0 is the limit.

## Verdict

- **PyroWave 4:4:4 is the one worth offering:** near-lossless text and UI (≥ 48 dB every frame, colour intact), at 580 Mbit/s and ~20 ms to the screen on 1 GbE (S1c), against ~5 ms and ~42 / 36 dB for HEVC. An opt-in "LAN quality" mode for text-heavy desktop work, not a default.
- **PyroWave 4:2:0 isn't, on 1 GbE:** the same chroma as HEVC, sharper only in motion, at 15–27× HEVC's bitrate and twice its latency. It stays for fast links and the native client.
- **For the hardware codecs,** the soft keyframe is the thing to fix: intra refresh spreads a refresh over several frames instead of one starved keyframe (P2.5). 4:4:4 hardware decode (HEVC Range Extensions, AV1 High) would close the colour gap where browsers support it.
