# Spike S3: Moonlight → WebRTC gateway

**Question.** Phase 1 ("Portal into Wolf") streams Wolf's environments to the browser without our own media engine. Can a gateway on the node take Wolf's GameStream output and pass the encoded frames, untouched, into a WebRTC video track with playout-delay 0? What does the extra hop cost? (Plan §9, Phase 0: S3.)

## How it works

```
Wolf (NVENC) ──GameStream, loopback──▶ s3-gateway ──WebRTC RTP, LAN──▶ Chrome
                RTSP/ENet/RTP+FEC        moonlight-common-rust          playout-delay 0
                                         → str0m, no transcoding
```

- **Moonlight side:** [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust), pinned to `104ddf1` (GPL-3.0-or-later, which combines with our AGPL).
  - It is a Sans-IO Moonlight client: it pairs, launches the app, and reassembles each frame after FEC.
  - `OwnedVideoFrame::raw()` is the whole access unit (Annex-B for H.264/HEVC). The gateway hands it to str0m as is.
- **Browser side:** str0m sends that access unit as an RTP video track with playout-delay 0/0 and abs-capture-time. This is the same sender as S1d's `webrtc_media.rs`, made async and fed by the host instead of a file.
  - A browser keyframe request (PLI/FIR) becomes a Moonlight `RequestIdr`.
- **Pairing:** done once. The identity is kept in `--state-dir`.
  - With `--wolf-socket`, the gateway answers its own pair request through Wolf's API (`/api/v1/pair/pending` → `/api/v1/pair/client`), so nobody types a PIN.
  - Without it, the gateway logs a PIN to enter on the host.
- **Measuring:** the gateway speaks the S1 server's signalling API (`/info`, `/streams`, `/webrtc/media`). So the S1c/S1d page measures it with its `webrtc` path:
  1. Point the page at the gateway's port (4490).
  2. Load streams. You get one stream per codec: `wolf-h264`, `wolf-hevc`, `wolf-av1`.
  3. Run matrix.
- **What the gateway reports per run** (in the `stats`/`done` control messages, which the page saves):

| Field | Meaning |
|---|---|
| `first_frame_ms` | From launch to the first host frame (app start included) |
| `hop_us_p50` / `hop_us_p99` | From "frame complete from the host" to "handed to str0m" |
| `host_latency_ms_p50` / `host_latency_ms_p99` | The host's own capture → encoded time, when it reports one |
| `keyframes` | Keyframes sent |
| `keyframe_requests` | Browser keyframe requests |

## Known limits (moonlight-common-rust today)

- **No video decryption.** The gateway asks for no encryption. That's fine on a loopback hop.
- **No reference-frame invalidation (RFI).** Loss recovery is IDR-only.
- **No AV1.** Codec negotiation never selects AV1; it falls back to H.264. The gateway offers H.264 and HEVC only.
- **One stream at a time.** A GameStream host runs one session per client.

## Running it (on the node)

Needs the NVIDIA Container Toolkit's CDI spec, as for [S1e](../s1e-encode-latency/README.md). From the repository copy on the node:

```bash
docker compose -f spikes/s3-gateway/compose.yaml up -d --build
```

The gateway pairs with Wolf on first start and keeps the identity in a volume. Its log lists Wolf's apps.

Then adjust Wolf's config. The script is idempotent and restarts Wolf. It:
- switches NVENC H.264 to P-frames (see below);
- adds a heavier **Test snow** app (noise, frames as large as the bitrate allows).

```bash
sh spikes/s3-gateway/tools/configure-wolf.sh
```

On the Mac:

1. Open the S1c/S1d page in visible Chrome.
2. Set the host to the node and the port to `4490`.
3. Load streams, then Run matrix.

Remove everything, volumes included, with:

```bash
docker compose -f spikes/s3-gateway/compose.yaml down -v
```

### What the compose file deploys

**Wolf, slimmed down for a video-only test.** The image is `ghcr.io/games-on-whales/wolf:stable`, pinned by digest (built 2026-09-29).
- **App:** its built-in **Test ball**. It needs no app container, but is still scaled and encoded with NVENC at the size the gateway asks for.
- **GPU:** through CDI.
- **Not included:** the Docker socket, the `/dev` bind mount, uinput/uhid and udev rules. Steam and desktops need them; Test ball doesn't.
- **Network:** host networking on the Moonlight ports (TCP 47984/47989/48010, UDP 47999/48100/48200). It also advertises itself over mDNS, so LAN Moonlight clients will see it.
- **Pairing:** Wolf's HTTP `/pin/` endpoint is unauthenticated on the LAN, which is fine for a homelab spike.

**The gateway**, on TCP 4490 and UDP 4491. It reaches Wolf over loopback and its API socket.

### Wolf details that matter for the results

From Wolf's source at `stable` (`facb8e0`):

- **H.264 is all-intra by default** (`nvh264enc preset=low-latency-hq ... gop-size=0`). Every frame is a keyframe, which Chrome decodes ~4× slower (below). `configure-wolf.sh` gives it HEVC's settings: an infinite GOP with P-frames (`p1`, ultra-low-latency, CBR).
- **No VUI reorder hint.** Wolf doesn't write `bitstream_restriction` / `max_num_reorder_frames`. On Chrome's WebRTC path that doesn't matter: frames spend 0.4–1 ms in the jitter buffer and are rendered as soon as they decode. A rewrite would only matter for a WebCodecs path.
- **Wolf issue #501:** RTX 4090 crash on re-entering a running session. The gateway closes the app after every run (`/cancel`), so each run launches fresh. If it still bites, set `WOLF_USE_ZERO_COPY=FALSE`.

## Findings (2026-10-03)

The path works end to end on `gpu-node.lan` (RTX 4090), with the gateway and Wolf on the node and the page on the Mac. The gateway pairs with Wolf on its own (no PIN) and launches the app fresh for every run. The first frame reaches the browser **64–88 ms** after launch.

**Problems found and fixed in the gateway:**

| Symptom | Cause | Fix |
|---|---|---|
| Frames reached Chrome all with the same RTP timestamp | Wolf stamps every video packet with RTP timestamp `0` (`gst-plugin/video.hpp`: `packet->rtp.timestamp = 0x00`). Moonlight clients ignore it. | Stamp RTP time from the gateway's own arrival clock |
| Some runs stopped ~3.2 s in | `disconnect()` in moonlight-common-rust only queues the ENet disconnect, and the gateway dropped the stream before it went out. Wolf gives every session from one client the same id, so ~5 s later the old peer's timeout **paused the next session**. | Keep driving the stream after `disconnect()` until the ENet disconnect is sent (≤ 500 ms) |
| AV1 run decoded nothing | moonlight-common-rust never negotiates AV1 ("Av1 is not supported in this implementation currently") and silently falls back to H.264 | Only offer H.264/HEVC. Fail if the stream's format differs from the browser track's |
| Gateway hop of 0.4–0.5 ms on 60–80 KB frames | `frame.as_ref()` re-scans the whole access unit for NAL units (~5 ns/byte), only to find the keyframe flag | Take the keyframe flag from the packet header |
| ~20 µs hop on small frames | Tokio's LIFO slot: the woken WebRTC task waited while the Moonlight task drained the frame's FEC packets | Yield right after handing over a frame |

**The gateway hop is now 0–3 µs p50 and ≤ 22 µs p99**, from "frame complete from Wolf" to "handed to str0m", whatever the frame size.

### Results: Chrome 154 on the baseline LAN, 2026-10-03

- **Client:** Chrome 154, visible tab, on the M4 Pro over wired 1 GbE.
- **Node:** Wolf `stable` (`facb8e0`) on `gpu-node.lan` (RTX 4090), with its default all-intra H.264.
- **Stream:** 2560×1440 at 60 fps. The gateway requests 40 Mbit/s; Wolf uses ~31 Mbit/s for video after FEC.
- **Run:** 15 s per stream.
- **Raw data:** [`docs/benchmarks/s3-2026-10-03-m4pro-chrome154-gpu-node-wolf.json`](../../docs/benchmarks/s3-2026-10-03-m4pro-chrome154-gpu-node-wolf.json).

Times are ms from the gateway's send, p50 / p95. Chrome decode and jitter-buffer figures are its own `getStats()` averages.

| Stream | Frame size | Keyframes | Shown | Last packet | Decoded | **To compositor** | Chrome decode | Jitter buffer | Gateway hop p50 / p99 | Launch → first frame |
|---|---|---|---|---|---|---|---|---|---|---|
| Ball H.264 | 1.4 KB | every frame | 292/294 | 0.8 / 1.1 | 3.4 / 5.2 | **4.1** / 7.7 | 2.78 | 0.40 | 1 / 5 µs | 139 ms |
| Ball HEVC | 1.4 KB | 1 | 297/298 | 0.8 / 1.1 | 2.5 / 3.5 | **2.7** / 6.1 | 1.84 | 0.38 | 1 / 13 µs | 72 ms |
| Snow H.264 | 80.8 KB | every frame | 287/289 | 2.1 / 3.1 | 11.3 / 12.5 | **11.7** / 15.8 | 9.31 | 1.06 | 3 / 7 µs | 230 ms |
| Snow HEVC | 61.3 KB | 1 | 298/298 | 2.6 / 3.9 | 4.9 / 6.4 | **5.3** / 8.3 | 2.19 | 0.70 | 2 / 22 µs | 68 ms |

No frame numbers were missing between Wolf and the gateway, and frames arrived every 16.7 ms (p99 ≤ 17.1).

**H.264 with P-frames** (`configure-wolf.sh`), on the same content. Re-run in the hidden pane, which doesn't affect Chrome's decode-time stat: those runs matched the visible ones within 0.3 ms.

| Wolf H.264 | Frame size | Keyframes | Chrome decode avg | Send → decoded p50 |
|---|---|---|---|---|
| All-intra (Wolf default) | 80.8 KB | every frame | 9.31 ms | 11.3 ms |
| P-frames, `p1` ultra-low-latency | 63.2 KB | 1 | **2.47 ms** | **4.5 ms** |

### Verdict

- **The gateway is transparent.**
  - It adds 1–3 µs per frame (p99 ≤ 22 µs) and loses nothing.
  - Wolf HEVC through it reaches the compositor at 5.3 ms (busy content) and 2.7 ms (simple content) after the gateway's send. S1d's replayed HEVC took 5.9 ms over the same WebRTC path.
  - **The Phase 1 MVP path works:** Wolf → moonlight-common-rust → str0m → Chrome, in hardware and with no transcoding.
- **Phase 1 should run Wolf with P-frame H.264, or prefer HEVC.**
  - Wolf's all-intra H.264 costs ~9 ms of decode per busy 1440p frame in Chrome's WebRTC path.
  - With P-frames it drops to 2.5 ms, close to HEVC's 1.6–2.2 ms.
  - The Wolf adapter in `cha-node` should own Wolf's encoder config.
- **Not measured here: capture → encode inside Wolf.** Wolf doesn't report host processing latency in the frame header (`host_latency_ms` is empty). Glass-to-glass needs the S4 latency harness.
- **Upstream fixes worth sending:**
  - **moonlight-common-rust:** AV1 negotiation; a `disconnect()` that flushes the ENet disconnect; a cheaper keyframe check than the full `parse_nalus` scan.
  - **Wolf:** real video RTP timestamps; don't let a stale peer's timeout pause a client's newer session.
