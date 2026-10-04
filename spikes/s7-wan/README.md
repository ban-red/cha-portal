# Spike S7: the WAN tier under netem

**Question.** Over WebTransport, does P2.5's rate control and FEC keep a stream adaptive on a lossy or throttled WAN, with no stall over 1 s (the P2.5 exit)? And what goes wrong on the way there?

## What it measures

| Part | How |
|---|---|
| Streamer | `cha-streamer` with the test pattern, as the dev loop builds it, in a container of its own (`compose.yaml`, the dev image plus `iproute2`). The pattern is 20 % noise (`NOISE`, `CHA_TESTPATTERN_NOISE`), enough to fill the encoder at 25 Mbit/s, so rate control has something to control. Session ceiling: 40 Mbit/s. |
| Link | `netem.sh` shapes the container's own interface (server → browser; `NET_ADMIN` in its namespace, nothing on the host). A token bucket at the root sets the rate, with a 3 KB burst, so it splits QUIC's GSO super-packets into real ones; netem under it adds delay, jitter and loss and holds a 150 ms buffer. Steps change in place, so queued packets survive them. Jitter keeps packets in order, as a real queue does (netem gets the rate too). `S7_REORDER=1` lets each packet take its own delay instead: at a packet a millisecond, ±5 ms reorders nearly all of them. |
| Scenarios | `scenarios/*.txt`, one step per line (`<second> <rate> <netem arguments>`): `wan` (a minute of 25 Mbit/s, 30 ± 5 ms, 0.5 % loss), `loss` (20 ms; 1 %, then 3 %, then none), `drop` (60 Mbit/s, a sixth of it for 20 s, then back). |
| Client | `web/`, a bench page with the real `@cha/player` in the Claude app's browser (Chromium 152) on the M4 Pro (1 GbE). Each second it records frames decoded (read from a clone of the video track, so it counts whether or not the page is painted), the longest wait between two frames (the worker's clock), what arrived, frames lost and frames FEC rebuilt, and send → complete. |

The page records over 60–70 s while `netem.sh` plays a scenario.

## Running it

On the node, from the repository root, after the dev loop has built the binaries (`deploy/streamer/compose.dev.yaml`):

```bash
ADVERTISE=<node LAN address> docker compose -f spikes/s7-wan/compose.yaml up -d --build
```

On the Mac, the bench page (`bun run dev` in `spikes/s7-wan/web`, port 5191):

```bash
open "http://localhost:5191/?host=gpu-node.lan&codec=hevc&transport=webtransport"
```

Start recording from the console, `bench.run(62)` (results in `bench.last`), then play a scenario on the node:

```bash
spikes/s7-wan/netem.sh spikes/s7-wan/scenarios/wan.txt
```

Stop the bench with `docker compose -f spikes/s7-wan/compose.yaml down`.

## Results (2026-10-04, RTX 4090 → M4 Pro + Claude app's browser, Chromium 152)

WebTransport throughout. All runs are HEVC except one, the WAN scenario on AV1. The first runs had netem reorder the jittered packets, the harsher case, before in-order jitter became the default; the last row is the default. See the [JSON](../../docs/benchmarks/s7-2026-10-04-electron152-gpu-node-wan.json).

| Scenario | Phase | fps | Mbit/s | Frames lost | Rebuilt by FEC | Longest gap |
|---|---|---|---|---|---|---|
| wan | onset (3 s) | 58.3 | 7.0 | 3 | 119 | 105 ms |
| wan | 25 Mbit/s, 30 ± 5 ms, 0.5 % | 60.8 | 21.7 | 3 | 3,441 | 51 ms |
| wan, AV1 | the whole minute | 60.3 | 21.5 | 16 (in the first 2 s) | 3,573 | 233 ms at the onset, 25 ms after |
| wan, in order | onset (3 s) | 56.3 | 20.0 | 7 | 20 | 131 ms |
| wan, in order | 25 Mbit/s, 30 ± 5 ms, 0.5 % | 60.5 | 22.0 | 0 | 486 | 26 ms |
| loss | clean | 60.0 | 24.5 | 0 | 0 | 21 ms |
| loss | 1 % | 62.0 | 25.5 | 8 (at the onset) | 364 | 234 ms |
| loss | 3 % | 60.0 | 24.6 | 0 | 693 | 18 ms |
| drop | 60 Mbit/s | 60.0 | 24.3 | 0 | 0 | 31 ms |
| drop | 10 Mbit/s | 59.4 | 8.5 | 15 (at the drop) | 0 | 255 ms |
| drop | 60 Mbit/s again | 60.1 | 21.5 | 0 | 0 | 20 ms (back over 20 Mbit/s after 6 s) |

The baseline before P2.5 used the same bench at a fixed 40 Mbit/s, with QUIC's Cubic. At 3 % loss it fell to 10 fps and then 1. A drop to 10 Mbit/s froze the picture for good: a keyframe storm into a full queue.

**Verdict: the exit is met over WebTransport.** On the WAN scenario both AV1 and HEVC hold 60 fps at ~86 % of the link. The longest wait is about a quarter second, when loss starts or the link drops, and 18–51 ms otherwise.

## What it took (in the order the bench showed it)

1. **The baseline's freeze.** Each lost frame asked for a keyframe, and each keyframe went into the queue the last one had filled.
   - Keyframe re-encodes are now limited to one per frame interval.
   - The encoder holds (skips frames) while QUIC's send buffer has more than 1.5 frames.
   - The bitrate changes in place, without an IDR.
2. **QUIC's RTT lags by seconds on a draining queue**, so rate control overshot. The page now reports send → complete, and cuts go to what arrived.
3. **netem took QUIC's GSO super-packets as single packets**, so a "1 %" loss dropped bursts of ten and its buffer held megabytes. The token bucket in front splits them.
4. **A hidden page's main thread is throttled.** Reports and frame timing moved into the worker.
5. **Cubic halved its window at every random loss**, starving the stream. Our own controller keeps a fixed window; rate control does the pacing.
6. **The browser's arrays are sparse.** FEC recovery skipped their holes and threw, which killed the datagram loop. Recovery now uses dense arrays and index loops, and one bad datagram can't stop the loop.
7. **A page that had stopped reading held the shared encoder** at its own rate. Holds older than a second now don't count.
8. **QUIC's black-hole fallback shrank the datagram size**, so every video datagram was refused. The next frame is now cut to the current size.
9. **Reordering looked like loss.** First QUIC counted reordered packets as lost. Then the page counted a fragment FEC rebuilt before it arrived (5 ms of jitter reorders a lot) as lost. More parity meant more early rebuilds, which meant more "loss": a loop to 50 % parity. The page now counts what never arrived: a frame's datagrams, data and parity, until 200 ms after its first.
10. **A linear parity share left too many frames short** at 0.5 % loss: two parity for 28 fragments fail about 4.5 times in 10⁴. Parity is now the least that keeps a frame's odds under 10⁻⁴ (binomial tail), and keyframes always get some.
11. **Parity for congestion loss feeds the congestion.** At the drop, the overflow's loss called for 4× parity, more than the link took, and the stream stayed at the floor for 20 s. FEC now follows only the loss seen on a calm path, and parity is capped at half the data.
12. **Climbing without evidence.** When the encoder doesn't fill its target, nothing shows what the path takes, so the climb stops at twice what goes out. It's twice, not WebRTC's 1.5: NVENC's low-latency CBR makes about two thirds of its target, so 1.5 never let it climb back after a drop.
13. **Stale signals cut twice.**
    - The page counts loss over a second, so one overflow was cut for three times. Loss alone now waits 1.2 s after a cut.
    - The page's delay median spanned 300 ms, so each probe of the link was cut twice. It now spans 150 ms, and a cut waits for the queue plus 300 ms.
14. **The browser stalls.** A ~270 ms pause in the (hidden) bench page looked like a 273 ms queue with nothing arriving, and rate control cut to the floor, taking 15 s to recover. Next to nothing arriving, or reports stopping, is now waited out for 400 ms. The worker also no longer gives up on a frame after 60 ms of silence; 250 ms now, while a later frame completing still marks a loss within 15 ms.

## WebRTC

The bench runs WebRTC too: `start.sh` binds the container's own address and announces the node's, as a router's port-forward would be.

- **Chrome's WebRTC receiver stalls under reordering.** On the wan scenario with reordering, it sent 15,788 NACKs for 284 packets actually lost. It then froze 48 times, 38 s of a 75 s run, with 4 keyframe requests: 29 fps, gaps up to 6 s. With in-order jitter: a NACK per lost packet, every one retransmitted, no freezes.
- **str0m's GCC (`enable_bwe`) doesn't fit our sender**, so it was tried and taken out:
  - **Pacing:** it paces all media at 1.1× its estimate, so a frame takes most of a frame interval to leave. That's ~14 ms more on a LAN where send → shown is ~5 ms.
  - **Stuck estimate:** it climbs to no more than 1.5× what it sees sent, and NVENC makes ~70 % of its target, so the estimate stalled near 8–12 Mbit/s even on an unshaped 1 GbE link. Probing needs the sender under ~20 % of the estimate (ALR) or a 15 s wait.
  - **Compensating doesn't help:** scaling the encoder's target up by its fill only filled str0m's pacer queue.

- **WebRTC's ABR is WebTransport's controller**, fed by the page's reports over the control DataChannel (see the streamer's README). Three things it took:
  - **Send times from the RTP timestamp.** The `sent` messages share the DataChannel, which held them back for seconds under loss. That blinded the controller: a 169 ms queue built with no cut, then a 3 s freeze.
  - **A keyframe when decoding stalls for 300 ms** while data arrives. At the drop, Chrome otherwise waited 3.1 s on retransmissions.
  - **The lower quartile of delay, not the median.** At 3 % loss most frames wait a round trip for a retransmission, and the rate slid to 3.9 Mbit/s.

  | WebRTC, HEVC, in-order jitter | fps | Mbit/s | Freezes (Chrome's count) |
  |---|---|---|---|
  | unshaped | 60 | 24.3 | 0 |
  | wan, steady | 59.4 | 17.3 | 0 |
  | loss: 1 % / 3 % | 59.1 / 58.3 | 25.7 / 14.6 | 0 |
  | drop: 10 Mbit/s | 57.6 | 8.4 | one, 0.5 s (back over 20 Mbit/s 5 s after the link) |

## Open

- **Loss before parity.** When loss starts on a clean link, the first ~300 ms of it (until the page's count shows it) costs frames and a keyframe gap of ~100–250 ms.
- **RFI or intra-refresh.** RFI is in, and S8 checked that every browser decoder carries on from its recovery frame ([S8](../s8-rfi/README.md)). Live on this bench it halves the gap at a 1 % loss onset (234 → ~120 ms); a loss further back than the DPB, as at the drop, still costs a keyframe. Intra-refresh wasn't pursued.
- **Real paths.** The bench is one netem box. Wi-Fi, cellular and real routes vary in ways it doesn't.
