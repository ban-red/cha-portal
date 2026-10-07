# Phase 2 exit pass: results

Runs from [`docs/plans/phase2-exit.md`](../plans/phase2-exit.md), each a
**Record 30 s** summary from the browser on the baseline MacBook Pro
M4 (wired 1 GbE) against gpu-node (RTX 4090).

## Safari 26.5, Google Chrome app playing a video, HEVC over WebRTC, 120 fps (2026-10-06)

Part of run 4 (Safari): not Steam yet, and one viewer.

| | |
|---|---|
| Browser | Safari 26.5 (`Version/26.5 Safari/605.1.15`), macOS |
| Transport, codec | WebRTC, HEVC (H265), 1672×1440 |
| Frame rate | target 120; the streamer sent 43.2/s on average (the page's video and what moved around it); shown p50 37.1, min 24.5 |
| Send → shown (880 frames) | p50 5.34 ms, p95 7.1 ms, p99 11.91 ms |
| Decode | p50 2.84 ms, p95 3.16 ms |
| RTT | 1 ms |
| Bitrate | 13.8 Mbit/s mean |
| Lost / recovered / dropped | 0 / 0 / 0 |
| Freezes | none |
| Audio buffer (NetEq) | mean 34.3 ms, max 50 ms |
| Health | worst B, final A; the one reason: sound buffering (40 ms) |

Read:

- Safari plays the stream with HEVC hardware decode and a send → shown p50 of
  5.3 ms, as low as WebRTC in Chrome (S1d: 5.4–6.4 ms).
- Shown p50 against the mean sent rate is not a like-for-like ratio (a
  percentile of per-second rates against a mean); the health grade, which
  compares them over the same span, found no stutter.
- The audio buffer is Safari's NetEq: 34–50 ms on a 1 ms LAN, and the sound
  was fine. The grade judged it by WebTransport's bar (our buffer, 0 on a calm
  LAN) and called it a minor issue; it now judges WebRTC's from 100 ms, so this
  run grades A.

## Safari 26.5, the same environment, H.264 over WebRTC, 120 fps (2026-10-06)

Switched from HEVC in the same session.

| | |
|---|---|
| Transport, codec | WebRTC, H.264, 1672×1440 |
| Frame rate | target 120; sent 49.2/s on average; shown p50 33.1, min 24.2 |
| Send → shown (1008 frames) | **invalid**: 119 175 ms, see below |
| Decode | p50 2.8 ms, p95 3.56 ms |
| RTT | 1 ms |
| Bitrate | 14.3 Mbit/s mean |
| Lost / recovered / dropped | 0 / 0 / 0 |
| Freezes | none |
| Audio buffer (NetEq) | mean 43.4 ms, max 45.6 ms |

The stream played well. The latency was a player bug: switching codec over
WebRTC opens a new session, whose clock starts again at zero, and the player
kept the old session's clock sync, so every frame read as ~119 s late (the time
between the two sessions' starts). Fixed: a closed session drops its clock
sync. Rerun for the number.
