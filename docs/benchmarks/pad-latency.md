# Pad input → decoded frame

What the player does with a pad press, timed end to end in software: the page sends button A as the session's own pad, the test-pattern environment turns a square in the middle of its picture green, and the page timestamps the decoded video frame that shows it.

**It is not controller-to-glass.** It leaves out the controller's USB (or Bluetooth) hop at the start and the display's scan-out at the end (a 60 Hz panel adds 0 to 16.7 ms, 8.3 ms on average, to what a person sees). It also stops at *decoded*, not presented: the Claude app's browser pane doesn't paint, so `requestAnimationFrame` stalls, but decoding runs. Add presentation and the display to compare with a camera measurement.

Raw data: [`pad-2026-10-10-electron152-gpu-node-testpattern.json`](pad-2026-10-10-electron152-gpu-node-testpattern.json).

## What is timed

```
t0   page: pad message (A down, slot 0, type xbox) handed to the player's send path
     player → streamer (control channel)           virtual Xbox 360 pad (uinput) → /dev/input/event*
     cha-testpattern reads the event, paints the centre square green on its next frame callback
     compositor → encoder → network → browser decoder
t1   page: the first decoded VideoFrame whose centre is green arrives (VideoFrame, before it is drawn)
```

`t1 - t0` is one number per press; both stamps are `performance.now()` in the same page, so no clock offset. The send path is the one real pads use (`Player.sendInput`, the same `pad` message `ControllerManager` sends, via `window.__chaHarness`); no physical pad is involved, so the datagram change shows up in the same path.

Frames are read with a `MediaStreamTrackProcessor` on the `<video>` element's track: on WebTransport that is the track the player writes decoded frames into, on WebRTC the browser's own decoded track. A 16×16 patch at the picture's centre is copied out (`VideoFrame.copyTo`, RGBA) only while a press is waiting, off the read loop, so a slow read doesn't delay the stamps. Each press is followed by 120 ms of hold, a release, a wait until the square is dark again, and a random 100-400 ms gap (seeded: the same `seed` replays the same gaps). The first presses are discarded until the environment has noticed the new virtual pad (it looks for a pad once a second).

Where the time goes, from the numbers below: the lowest samples (about 10 ms at 60 fps, 8-9 ms at 120) are the pipeline with no waiting; the median sits about half a frame interval above that, because the environment draws on frame callbacks and the encoder takes frames on its cadence. So at 60 fps the median is roughly floor + 8 ms, at 120 fps floor + 4 ms. What causes the tail wasn't investigated.

## Baseline, before pad input moves to datagrams

2026-10-10. Client: the Claude app's built-in browser, user agent
`Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Claude/2.31226.1 Chrome/152.0.7977.130 Safari/537.36`, on a MacBook Pro over wired 1 GbE. **Decoded size 1920×1440** (the pane's size, not 1440p). Node: the RTX 4090 test node on the same LAN, test-pattern environment, NVENC, through the Mac's dev portal. Pad messages go over the control channel as they do today (not datagrams). Each cell is the median of the runs' values; a run is 100 presses; 3 runs per configuration (6 for WebTransport AV1 60). Nothing missed in any run. With 100 presses p99 is the single worst press, so read p95 as the tail and p99 as "the worst one".

| Transport | Codec | fps | p50 ms (runs) | p95 ms | p99 ms (worst of 100) | Fastest ms |
|---|---|---|---|---|---|---|
| WebTransport | AV1 | 60 | **18.6** (17.1–19.1) | 25.6 | 44.6 | 9.8 |
| WebTransport | H.264 | 60 | **18.9** (18.3–19.0) | 26.7 | 68.4 | 9.6 |
| WebTransport | H.264 | 120 | **13.0** (12.4–14.4) | 20.8 | 45.1 | 8.4 |
| WebTransport | AV1 | 120 | **13.6** (13.5–14.1) | 20.4 | 60.7 | 9.8 |
| WebRTC | AV1 | 60 | **20.7** (19.7–20.8) | 27.7 | 57.9 | 10.3 |
| WebRTC | H.264 | 60 | **19.0** (18.8–19.8) | 26.7 | 63.3 | 9.8 |
| WebRTC | AV1 | 120 | **15.1** (14.9–15.3) | 20.6 | 49.8 | 9.8 |
| WebRTC | H.264 | 120 | **14.1** (13.9–14.2) | 20.8 | 43.5 | 8.4 |

The decoded frame rate was the stream's (60.0 or 120.0 fps, frame intervals p95 17.2-17.5 ms at 60 over WebTransport, 19.4-19.8 over WebRTC, and 9.0-9.3 / 11.9-12.3 at 120). Nearly every run has one press in the 40-80 ms range; it is not tied to a codec or transport. The patch read-back takes 5-8 ms (median), under a frame at 60 fps and about one frame at 120; it runs off the read loop so it doesn't move the stamps.

Other caveats:

- The pane is not Chrome. Presentation, power and focus behave differently (the stats overlay is paused in it), and decode scheduling may too. Use it to compare before and after, not as the number a Chrome user gets.
- 120 fps is the environment's compositor and encoder rate. The test pattern repaints only what changed, so the encoder works on near-static frames, much easier than a game's. These numbers are the floor of the pipeline, not what a real game costs.
- One browser, one node, one LAN, one afternoon; `readMs` and the frame intervals are in the JSON.

## Re-run it (the after-datagrams run must be the same)

Needs the dev portal (`bun run dev`, API on :7677 with the node connected, SPA on :7678) and the node's `cha/env-test-pattern:dev` image with the pad-A square (rebuilt 2026-10-10; the previous image is tagged `:pre-probe`). Don't start anything on the node by hand.

1. Launch a test-pattern environment as the dev user:
   ```bash
   bun scripts/pad-latency.ts create        # prints the session URL and the environment id
   ```
2. Open that URL in the **Claude app's browser pane** (the same client as the baseline), sign in with the dev login (`fetch('/api/auth/dev-login', {method: 'POST', headers: {'content-type': 'application/json'}, body: '{}'})` from the page, then reload), and wait until the picture shows. Click the pane's tab so it is the fronted one.
3. Pick the configuration in the toolbar: codec, frame rate and transport (WebTransport / WebRTC); wait about 5 s after each change. The dev harness exists only in dev builds.
4. Run three times, each 100 presses. The pane's script tool times out after 45 s, so start the job and poll:
   ```js
   window.__runs = null;
   (async () => {
     const runs = [];
     for (let i = 0; i < 3; i++) runs.push(await window.__chaHarness.padProbe({ count: 100, seed: i + 1 }));
     window.__runs = runs;
   })();
   // about 2.5 minutes later:
   window.__runs.map(r => [r.latencyMs.p50, r.latencyMs.p95, r.latencyMs.p99, r.latencyMs.min, r.frames.fps, r.decoded.width + 'x' + r.decoded.height])
   ```
   `padProbe` returns `latencyMs {p50, p95, p99, min, max, mean}`, `all` (every sample), `missed`, `frames.fps`, `readMs`, `decoded`, `userAgent`. Options: `count`, `gapMs: [lo, hi]`, `holdMs`, `timeoutMs`, `seed`.
5. Record the user agent, `decoded` size and the configuration with the numbers, as in the table above, in a new `pad-<date>-electron152-gpu-node-testpattern.json`.
6. Delete the environment: `bun scripts/pad-latency.ts delete <environment id>`.

`padProbe` is in `web/packages/player/src/pad-probe.ts` (the harness method is in `harness.ts`); the environment side is the green square in `crates/cha-testpattern/src/draw.rs`.
