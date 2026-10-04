# Benchmarks

Raw results from spike and benchmark runs. Each file is the JSON a harness exports, named `<spike>-<date>-<client>-<node>.json`. Analysis lives with the spike that produced it.

| File | Spike | Client → node | Summary |
|---|---|---|---|
| [s1-2026-10-03-m4pro-chrome154-gpu-node.json](s1-2026-10-03-m4pro-chrome154-gpu-node.json) | [S1](../../spikes/s1-browser-pyrowave/README.md) | M4 Pro + Chrome 154 → gpu-node.lan, 1 GbE | WebTransport: 100% of frames, 0% loss up to 811 Mbit/s; str0m DataChannel fails |
| [s1-2026-10-03-m4pro-chrome154-gpu-node-bbr.json](s1-2026-10-03-m4pro-chrome154-gpu-node-bbr.json) | [S1](../../spikes/s1-browser-pyrowave/README.md) | same, server `S1_CC=bbr` | Same medians; p99 3–15× worse than Cubic, plus loss and sender drops. BBR rejected. |
| [s1b-2026-10-03-m4pro-chrome154.json](s1b-2026-10-03-m4pro-chrome154.json) | [S1b](../../spikes/s1b-pyrowave-webgpu/README.md) | M4 Pro + Chrome 154 (local decode) | Bit-exact. 1440p GPU decode 1.3 / 2.5 ms back to back, 6.2 / 6.6 ms paced at 60 fps (4:2:0 / 4:4:4) |
| [s1b-2026-10-03-m4pro-chrome154-paced120.json](s1b-2026-10-03-m4pro-chrome154-paced120.json) | [S1b](../../spikes/s1b-pyrowave-webgpu/README.md) | same, paced at 120 fps | GPU decode 4.0 / 5.4 ms (4:2:0 / 4:4:4); ~106–111 fps sustained |
| [s1c-2026-10-03-m4pro-chrome154-gpu-node.json](s1c-2026-10-03-m4pro-chrome154-gpu-node.json) | [S1c](../../spikes/s1c-codec-compare/README.md) | M4 Pro + Chrome 154 → gpu-node.lan, 1 GbE | On screen p50: H.264 6.9, AV1 8.0, HEVC 8.0–8.7, PyroWave 4:2:0 11.4, 4:4:4 20.0 ms (before encode) |
| [s1e-2026-10-03-gpu-node-rtx4090.json](s1e-2026-10-03-gpu-node-rtx4090.json) | [S1e](../../spikes/s1e-encode-latency/README.md) | gpu-node.lan, RTX 4090 (node only) | 1440p60 encode p50: NVENC H.264 1.9–2.0, HEVC 1.7–1.9, AV1 1.8–2.2 ms; PyroWave 0.21 / 0.33 ms GPU, 1.0 / 1.9 ms via the WebGPU port (4:2:0 / 4:4:4) |
| [s1d-2026-10-03-m4pro-chrome154-gpu-node.json](s1d-2026-10-03-m4pro-chrome154-gpu-node.json) | [S1d](../../spikes/s1c-codec-compare/README.md) | M4 Pro + Chrome 154 → gpu-node.lan, 1 GbE | Send → compositor p50: track generator → `<video>` 2.7–3.4 ms, 2D canvas 2.3–3.5 (p95 ~9), WebRTC 5.4–6.4, WebGPU external texture 7.3–8.9 |
| [s3-2026-10-03-m4pro-chrome154-gpu-node-wolf.json](s3-2026-10-03-m4pro-chrome154-gpu-node-wolf.json) | [S3](../../spikes/s3-gateway/README.md) | M4 Pro + Chrome 154 → gateway + Wolf on gpu-node.lan, 1 GbE | Gateway hop 1–3 µs; Wolf HEVC 2.7–5.3 ms to compositor; Wolf's all-intra H.264 decodes in 9.3 ms (2.5 ms with P-frames) |
