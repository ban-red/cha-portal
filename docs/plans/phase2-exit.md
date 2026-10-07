# Phase 2 exit pass

*Phase 2 was closed by the owner on 2026-10-07 without these runs; they remain open checks (docs/PLAN.md).*

What closes Phase 2 (docs/PLAN.md §9, *Phase 2*), what is already met, and the
runs left for the owner on the baseline (MacBook Pro M4, wired 1 GbE). Every
run uses the session's **Record 30 s** (stats panel), and its Markdown summary
goes into `docs/benchmarks/phase2-exit.md`, with the browser's real UA and the
decoded size.

## The exit criteria

| Criterion | State |
|---|---|
| A Steam game playable with a controller in Chrome, on LAN | **Met** 2026-10-05 (Cyberpunk 2077, Steam Controller 2026 over WebHID, virtual Xbox 360 pad; Steam's logs show the game read that pad, not a virtual Steam Controller, which Proton games don't see yet: docs/controllers.md). |
| … in Firefox and Safari | Runs 3 and 4. |
| … over WAN | Run 5 (netem on the node; P2.5's suite met the stall bound). |
| A KDE Plasma desktop works | **Met** 2026-10-04 (P2.2). |
| PyroWave 1440p120 4:4:4 on wired LAN in Chrome | **Can't fit 1 GbE**: 4:4:4 was 574 Mbit/s at 60 fps, so ~1.15 Gbit/s at 120. Proposed instead: 1440p120 **4:2:0** (~530 Mbit/s) on 1 GbE (run 2), and 4:4:4 at 120 once a 2.5 GbE client exists. |
| Adaptive AV1 on a lossy/throttled WAN, no stall over 1 s | **Met** 2026-10-04 (S7: 25–51 ms gaps at 0.5 % loss; one 255 ms gap on a 60→10 Mbit/s drop). |

## Before the runs

- The node's GPU has room: ComfyUI holds ~16 GB of the 4090's 24 GB. Cyberpunk
  wants ~8–10 GB more. Stop ComfyUI's job (or the container) for runs 1–5.
- The node agent and streamer image are on the build with this pass's fixes
  (a recreate).

## Runs

1. **NVENC baseline, Chrome, LAN.** XFCE at 1440p60, HEVC over WebTransport,
   then over WebRTC. Pass: send → shown p50 below WebRTC's (S1d: 2.7–3.4 ms vs
   5.4–6.4), grade A. This also checks NVENC after the encoder refactor.
2. **120 fps, Chrome, LAN.** The same app at 120 fps (app card), HEVC, then
   PyroWave 4:2:0. Pass: shown fps ≥ 115 while the picture moves, nothing lost,
   grade A or B.
3. **Steam in Firefox, LAN.** A game with a controller through the Gamepad API
   (Firefox has no WebHID; use the Xbox 360 kind). Pass: playable, grade B or
   better, rumble.
4. **Steam in Safari, LAN.** As run 3 (WebRTC, H.264 or HEVC). Then open the
   same environment in Chrome too: two WebRTC viewers at once. Pass: both
   play; the watcher sees the picture.
5. **Steam over a WAN, Chrome.** netem on the streamer (S7's 25 Mbit/s, 30 ±
   5 ms, 0.5 % loss), AV1, controller. Pass: playable, no stall over 1 s,
   sound without clicks (the WebTransport audio buffer).
6. **Steam Controller 2026, real Steam.** The `steam` kind: Steam shows a
   connected controller, the d-pad works, haptics come back.

## After

- Results into `docs/benchmarks/phase2-exit.md`; PLAN.md's Phase 2 heading
  marked done with the date, and the PyroWave criterion amended if run 2 is
  4:2:0.
- Whatever fails becomes a Phase 3 item or a fix before closing.
