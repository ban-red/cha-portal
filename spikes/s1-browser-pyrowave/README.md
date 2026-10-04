# Spike S1: browser PyroWave receive

**Question (docs/PLAN.md §9, Phase 0):** on the baseline client (Chrome on an M4 MacBook Pro, wired 1 GbE), can a browser receive PyroWave-shaped traffic for **1440p60** fast and cleanly enough to make browser PyroWave worth shipping?

This half of the spike covers the **transport**. A Rust server sends fixed-size intra frames, burst out at each frame boundary like PyroWave's encoder would, using the real `cha-stream/1` datagram framing (`crates/cha-proto`):

- **WebTransport datagrams** (`wtransport`/quinn, self-signed cert pinned via `serverCertificateHashes`), received in a dedicated Worker.
- **WebRTC DataChannel**, unordered with `maxRetransmits: 0` (`str0m`, ICE-lite), received on the main thread.

The page reassembles frames and reports:

- receive rate and datagram loss;
- completed, incomplete and missing frames;
- frame **spread** (last fragment minus first fragment);
- **clock-synced one-way latency** to first and last fragment;
- completion jitter;
- the sender's own counters (frames it had to skip for backpressure, QUIC RTT/cwnd, SCTP buffer).

The second half, S1b (WebGPU PyroWave decode), is in [`../s1b-pyrowave-webgpu`](../s1b-pyrowave-webgpu/README.md).

## Gate

Per run, all must hold:

| Check | Threshold |
|---|---|
| Receive rate (median 1 s window) | ≥ 97% of target |
| Datagram loss | < 0.5% |
| Frames fully received | ≥ 99.5% |
| Frame spread p99 | ≤ wire time + 3 ms |
| One-way latency to last fragment, p99 | ≤ wire time + 6 ms |
| Frames skipped by the sender | 0 |

**Why "wire time":** a frame can't arrive faster than the link serializes it, natively or in a browser. On 1 GbE (about 940 Mbit/s of goodput):

| Preset | Mbit/s | Frame size | Wire time / frame | Share of frame interval |
|---|---|---|---|---|
| 1080p60 4:2:0 couch | 220 | 458 KB | 3.9 ms | 23% |
| 1440p60 4:2:0 couch | 290 | 604 KB | 5.1 ms | 31% |
| **1440p60 4:4:4 desk (gate)** | 590 | 1.23 MB | **10.5 ms** | 63% |
| 1440p120 4:2:0 couch | 580 | 604 KB | 5.1 ms | 62% |
| Stress | 800 | 1.67 MB | 14.2 ms | 85% |

> **Design consequence:** on 1 GbE, desk-quality 4:4:4 spends about 10 ms per frame just crossing the wire, roughly 2× the 4:2:0 preset. PyroWave's ~0.1 ms encode/decode doesn't help with that. The PyroWave tier therefore needs a **quality ↔ latency control**: a per-frame byte cap below "transparent". The bitrate target is not a fixed preset.

## Running it

You need the node (a Linux box on the LAN) and the client (the Mac with Chrome).

**On the node**, with a copy of the repository (no Rust toolchain needed; Docker builds it):

```bash
docker compose -f spikes/s1-browser-pyrowave/compose.yaml up -d --build
```

Set `S1_CC=bbr` (or `new-reno`) in front of that command to change the QUIC congestion controller. Or run natively with Rust ≥ 1.93: `cargo run --release -p s1-server -- --cc bbr`.

Open **TCP 4480** (info/signaling), **UDP 4433** (WebTransport) and **UDP 4434** (WebRTC) on the node's firewall. Other server flags: `--dgram-send-buffer`, `--sctp-buffer`, `--advertise <ip>`.

**On the Mac:**

```bash
bun install
```

```bash
bun run --cwd spikes/s1-browser-pyrowave/web dev
```

Open the printed `http://localhost:<port>` URL in **Google Chrome**. It must be `localhost`: WebTransport needs a secure context, and a localhost page can reach LAN IPs without Chrome's Local Network Access prompt.

1. Set **Server host** to the node's LAN name or IP.
2. Click **Run gate sweep**. It runs 5 presets × 2 transports at 15 s each, about 3 minutes.
3. Click **Copy results JSON** and save the output as `docs/benchmarks/s1-<date>-<client>-<node>.json`.

Hover a red **fail** cell to see which checks failed. Repeat with `S1_CC=bbr` and with **BYOB reader** ticked to see whether either changes the outcome.

## Results: baseline LAN, 2026-10-03

Setup:
- **Client:** Chrome 154 on the M4 Pro MacBook Pro, wired 1 GbE.
- **Server:** `gpu-node.lan` (Docker, host networking, quinn Cubic).
- **Run:** 15 s per run, 1200-byte datagrams.
- **Raw data:** [`docs/benchmarks/s1-2026-10-03-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/s1-2026-10-03-m4pro-chrome154-gpu-node.json).

**WebTransport datagrams.** All figures are in ms unless marked.

| Preset | Target Mbit/s | Received Mbit/s | Loss | Frames ok | Wire time | Spread p50 / p99 | Latency to first fragment p50 / p99 | Latency to last fragment p50 / p99 | Gate |
|---|---|---|---|---|---|---|---|---|---|
| 1080p60 4:2:0 | 220 | 223 | 0.0006% | 899 / 900 | 3.9 | 3.9 / 7.6 | 0.35 / 1.78 | 4.3 / 8.2 | miss: spread p99 +3.7 ms over wire |
| 1440p60 4:2:0 | 290 | 294 | 0% | 900 / 900 | 5.1 | 5.2 / 8.0 | 0.41 / 0.82 | 5.6 / 8.5 | pass |
| **1440p60 4:4:4 (gate)** | 590 | 598 | 0% | 900 / 900 | 10.5 | 10.7 / 13.8 | 0.44 / 0.95 | **11.1** / 15.4 | miss: spread p99 +3.3 ms over wire |
| 1440p120 4:2:0 | 580 | 588 | 0% | 1800 / 1800 | 5.1 | 5.3 / 7.5 | 0.28 / 0.95 | 5.6 / 8.3 | pass |
| Stress | 800 | 811 | 0% | 900 / 900 | 14.2 | 14.5 / 16.1 | 0.37 / 1.21 | 14.9 / 16.8 | pass |

Received rates sit slightly above target because they include the 16-byte headers.

**WebRTC DataChannel (str0m):**
- 71–193 Mbit/s received, against targets of 220–800.
- 0.1–0.8% loss.
- 0.3–1.4 s of queueing.
- The server's control messages sat behind media in the SCTP queue, so clock sync and server stats for these runs are invalid; negative latencies in the JSON are that artifact. **Fail.**

### Verdict (transport half of S1)

- **WebTransport in Chrome passes in substance.**
  - At every rate up to 811 Mbit/s (86% of the link), 100% of frames arrived, with one incomplete frame in 6,300 caused by 2 lost packets.
  - The first fragment lands 0.3–0.45 ms after send.
  - Frames complete at wire time + 0.1–0.7 ms (p50) and wire time + 2.4–3.7 ms (p99).
  - Two presets miss the letter of the gate. Their p99 spread exceeds the +3 ms allowance by 0.3–0.7 ms, while the heavier 800 Mbit/s run came in at +1.9 ms. So the tail is scheduling jitter, not a bandwidth limit. A repeat with `S1_CC=bbr` and the BYOB reader should show whether it comes from the sender's bursts or the receiving worker.
- **The str0m DataChannel path fails** on the LAN as it did on loopback. In-browser PyroWave rides WebTransport, so Chromium and Firefox ≥ 153 only. Safari gets the hardware-codec tiers unless a libdatachannel retest says otherwise.

### Design consequences

- **4:4:4 costs latency on 1 GbE.** At 1440p60, frames complete in 5.6 ms (p50) at 290 Mbit/s 4:2:0 but 11.1 ms at 590 Mbit/s 4:4:4. 1440p**120** 4:2:0 at 580 Mbit/s also completes in 5.6 ms. On 1 GbE, spend bandwidth on frame rate before chroma for games. Keep 4:4:4 for text-heavy desktops. The quality ↔ latency byte cap in the plan is confirmed as necessary.
- **Bursts inflate quinn's RTT estimate.**
  - Smoothed RTT climbs from 3.5 to 12.9 ms as load rises, against a minimum RTT of 0.4–1.1 ms. That is the frame burst draining at wire speed.
  - Cubic's cwnd grows unbounded on a loss-free LAN (87 MB at 800 Mbit/s), so QUIC never paces it.
  - This supports Nestri's rule: use send-queue duration (`backlog_ms`) as the congestion signal, not loss or RTT alone.
  - Also test BBR, which paces at its bandwidth estimate and is kinder to other traffic sharing the link.

### Cubic vs BBR (quinn), same LAN, 2026-10-03

The WebTransport rows of the same sweep, rerun with `S1_CC=bbr`. Raw data: [`…-gpu-node-bbr.json`](../../docs/benchmarks/s1-2026-10-03-m4pro-chrome154-gpu-node-bbr.json). These exports didn't record whether the BYOB reader was ticked; the harness records it now. Latency columns are to the last fragment.

| Preset | CC | Frames ok | Sender drops | Lost packets | cwnd at end | Latency p50 / p95 / p99 / max (ms) |
|---|---|---|---|---|---|---|
| 1080p60 4:2:0 | Cubic | 899 / 900 | 0 | 2 | 1.1 MB | 4.3 / 6.2 / **8.2** / 11.9 |
| | BBR | 900 / 900 | 0 | 0 | 283 KB | 4.3 / 6.7 / **25.1** / 56.1 |
| 1440p60 4:2:0 | Cubic | 900 / 900 | 0 | 0 | 3.2 MB | 5.6 / 6.6 / **8.5** / 17.7 |
| | BBR | 900 / 900 | 0 | 0 | 131 KB | 5.7 / 7.5 / **51.7** / 107.9 |
| 1440p60 4:4:4 | Cubic | 900 / 900 | 0 | 0 | 42 MB | 11.1 / 12.1 / **15.4** / 26.2 |
| | BBR | 890 / 900 | 6 | 646 | 327 KB | 11.2 / 15.1 / **96.4** / 196.7 |
| 1440p120 4:2:0 | Cubic | 1800 / 1800 | 0 | 0 | 6.2 MB | 5.6 / 6.6 / **8.3** / 20.9 |
| | BBR | 1789 / 1800 | 9 | 20 | 356 MB (sampled mid-cycle) | 5.6 / 7.8 / **128.7** / 151.9 |
| Stress 800 | Cubic | 900 / 900 | 0 | 0 | 83 MB | 14.9 / 15.6 / **16.8** / 22.6 |
| | BBR | 887 / 900 | 6 | 59 | 366 KB | 14.9 / 26.1 / **78.1** / 177.8 |

**Verdict: don't use quinn's BBR for frame-burst media.** The median is identical, because the wire sets it. The tail gets 3–15× worse, and BBR adds loss and sender-side frame drops.

The mechanism, confirmed in `quinn-proto` 0.11.19's `congestion/bbr`:
1. BBR sizes cwnd to about 2× bandwidth-delay product. With a 0.5 ms LAN RTT that is 130–370 KB, **smaller than one frame burst** (0.46–1.7 MB), so each frame is spread over several round trips.
2. Every 10 s it enters **ProbeRTT for 200 ms** with cwnd cut to 0.75× BDP. That stalls whole frames for up to ~200 ms, which matches the max column.
3. ProbeBW's 1.25× pacing gain overshoots the 1 GbE bottleneck, which explains the 646 lost packets at 590 Mbit/s.

Cubic does well on a quiet LAN only because, with no loss, its cwnd grows without bound (42–83 MB), so it effectively never limits. That also means it offers no protection on a shared or lossy link.

**Consequence for `cha-streamer` (plan §3.1):** ship a **media-aware quinn congestion controller**.
- Its cwnd floor must be ≥ 1–2 frames.
- No ProbeRTT phases.
- Its rate comes from our receiver reports and send-queue duration, which also set the PyroWave byte cap and the hardware-encoder bitrate.
- Until then, use Cubic.

## Preliminary results: loopback, 2026-10-03

Setup: server and browser on the same M4 Pro (embedded Chromium 152, Claude app browser pane), 5 s runs, 1200-byte datagrams. **Loopback has no 1 GbE wire, so this checks function and browser ingest headroom only. It is not the gate.**

| Transport | Target | Received | Loss | Frames ok | Spread p50/p99 | Latency first / last p50 | Notes |
|---|---|---|---|---|---|---|---|
| WebTransport | 290 | 295 Mbit/s | 0% | 100% | 3.1 / 8.1 ms | 0.3 / 3.4 ms | ~30.7k datagrams/s |
| WebTransport | 590 | 598 Mbit/s | 0% | 100% | 6.5 / 15.1 ms | 0.3 / 6.8 ms | ~62k datagrams/s |
| WebRTC DC (str0m) | 290 | ~32 Mbit/s | 1.8% | n/a | 108 / 191 ms | ~600 ms | **245/300 frames skipped by sender**, latency up to ~1.2 s |

What this says so far:

- **WebTransport ingest in Chromium is healthy.** About 1.5 Gbit/s effective burst ingest into a Worker, no loss, and every frame arrived. On a real 1 GbE link the wire, not the browser, should be the binding limit. To be confirmed on the LAN with Chrome 154.
- **The `str0m` SCTP DataChannel path is not viable for PyroWave as it stands.** Its send side drains at only ~30–60 Mbit/s, so the buffer fills, frames are skipped, and control messages queue behind media. This holds even after the event loop drains all input and drives output after every packet. Chrome DataChannels are not known for high throughput either.
  - Next step: one comparison against a usrsctp/dcSCTP-based sender (libdatachannel) before ruling the path out entirely.
  - This **does not affect** the WebRTC baseline for H.264/HEVC/AV1, which uses RTP tracks, not SCTP.

## Next

- [x] Run the sweep on the real LAN (Mac + Chrome 154 → `gpu-node.lan` over 1 GbE). Results above.
- [x] Repeat with BBR: worse tails (see above). Cubic stays the default.
- [ ] Repeat the 1440p60 4:4:4 preset with the BYOB reader ticked (Cubic), now that the export records it.
- [x] S1b: PyroWave decode on WebGPU. See [`../s1b-pyrowave-webgpu`](../s1b-pyrowave-webgpu/README.md). Bit-exact. Per 1440p 4:2:0 / 4:4:4 frame on the M4 Pro: 1.3 / 2.5 ms of GPU time back to back, 6.2 / 6.6 ms paced at 60 fps.
- [ ] Secondary data points: Firefox (≥153) and Safari (expected to fail WebTransport, see research 05).
- [ ] DataChannel comparison with libdatachannel (usrsctp) to settle the Safari PyroWave question.
