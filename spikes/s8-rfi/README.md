# Spike S8: reference frame invalidation against the browser's decoders

**Question.** When frames are lost on the way, P2.5 can resync with a P-frame instead of a keyframe: NVENC's reference frame invalidation (RFI). After it, does the browser's WebCodecs decoder carry on bit-exactly from the recovery frame, for H.264, HEVC and AV1?

## What RFI is here

Each frame predicts from one reference, but the encoder keeps a DPB of recent frames (`cha-nvenc`'s `DPB_FRAMES`, now 8). When frames are lost on the way, `Encoder::invalidate_from(index)` calls `nvEncInvalidateRefFrames` for each lost frame, and the next frame refers only to an older one the page has.

- **Flag:** that frame is marked RECOVERY on the wire (`cha-proto` header flag `1 << 6`) for the session whose page lost frames from at or after the invalidation point. A page waiting to resync may start from it as from a keyframe.
- **Fallback:** a keyframe, when the loss is further back than the DPB or before the last keyframe.

## What it measures

| Part | How |
|---|---|
| Streamer | The S7 bench's (`spikes/s7-wan/compose.yaml`): test pattern with 20 % noise at the bench page's size (1792×1008: the picture follows the page's element), 60 fps, ~40 Mbit/s target, on the RTX 4090. NVENC reports RFI support (`NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION`) for all three codecs. |
| Dump | `CHA_DUMP_RFI=<dir>,<at>,<lost>` (here `/tmp/rfi,120,3`, set through `DUMP_RFI` in the compose file) writes each codec's first frames to `<dir>/<codec>/NNNNN.bin`, plus `index.jsonl` (`{index, key, recovery, lost}` per frame). It acts as if frames 120–122 were lost: before encoding frame 123 it invalidates 120–122, so 123 is a recovery frame. |
| Encoders | A codec's encoder is made by the first page session on it, so the bench page switches codec live, one session per codec. |
| Page | `index.html`, served as `python3 -m http.server 5192 --directory spikes/s8-rfi`, reads the dumps from `results/` (gitignored). For each codec it decodes with WebCodecs twice, all frames and without 120–122, with `prefer-hardware` and again with `prefer-software`. |
| Metric | The luma plane of frames 123–138 from the two decodes: PSNR, where "identical" is bit-exact. |
| Control | The same, skipping three frames nothing was invalidated for (116–118), and comparing frame 119. It shows the page can tell a broken reference from a sound one. |

## Running it

On the node, from the repository root, after the dev loop has built the binaries (`deploy/streamer/compose.dev.yaml`, as for S7), start the bench with the dump on:

```bash
DUMP_RFI=/tmp/rfi,120,3 ADVERTISE=<node LAN address> docker compose -f spikes/s7-wan/compose.yaml up -d --force-recreate
```

On the Mac, open the S7 bench page (`bun run dev` in `spikes/s7-wan/web`, port 5191), which starts on HEVC:

```bash
open "http://localhost:5191/?host=gpu-node.lan&codec=hevc&transport=webtransport"
```

Then switch codec from the console, a few seconds on each (a dump stops about 60 frames after the recovery frame): `bench.player.switchCodec("h264")`, then `bench.player.switchCodec("av1")`.

Copy the dumps out of the container, on the node:

```bash
docker cp cha-s7-wan-1:/tmp/rfi /tmp/s8-rfi
```

and from there to the Mac, from the repository root (`results/` then holds `h264/`, `hevc/` and `av1/`):

```bash
rsync -az gpu-node.lan:/tmp/s8-rfi/ spikes/s8-rfi/results/
```

Serve the page and open `http://localhost:5192/`; the log has a line per codec and decoder, and the numbers are in `window.s8`.

## Results (2026-10-04, RTX 4090 → M4 Pro + Claude app's browser, Chromium 152)

The first run, with a 5-frame DPB (the lost three fit in either). See the [JSON](../../docs/benchmarks/s8-2026-10-04-electron152-gpu-node-rfi.json).

| Codec | Decoder | Without 120–122, from 123 | Control (without 116–118) |
|---|---|---|---|
| H.264 | hardware | identical | 17.9 dB, no error |
| H.264 | software | identical | 17.9 dB, no error |
| HEVC | hardware | identical | decode error |
| HEVC | software | not offered by Chromium | — |
| AV1 | hardware | identical | 14.5 dB, no error |
| AV1 | software | identical | 14.5 dB, no error |

Sizes of the frames around the loss, in bytes:

| Codec | Keyframe | P-frame (119) | Recovery (123) | Next (124) |
|---|---|---|---|---|
| H.264 | 116,347 | 49,651 | 48,117 | 48,474 |
| HEVC | 149,630 | 47,876 | 48,658 | 48,075 |
| AV1 | 251,114 | 76,464 | 76,579 | 75,359 |

A second run, with the 8-frame DPB (`DPB_FRAMES` now), came out the same: identical on every decoder, the control at 16.2 dB (H.264), 15.6 dB (AV1) and a decode error (HEVC), and recovery frames of 52,040 (H.264), 46,291 (HEVC) and 76,616 (AV1) bytes against keyframes of 116,868, 158,252 and 240,809.

- **NVENC made a P-frame for 123 in all three codecs**, not a keyframe.
- **The decoders carry on exactly.** Frames 123–138 match the decode that had all the frames, hardware and software.
- **The control shows what a missing reference does.** Without frames nothing was invalidated for, H.264 and AV1 decode on with no error and show a different picture (17.9 and 14.5 dB), and hardware HEVC raises a decode error.
- **A recovery frame costs what any P-frame does.** A keyframe costs 2.2–3.4 times as much, over the two runs. It also gets a one-frame VBV here, so on a busier picture it costs quality as well as bytes ([S6](../s6-quality/README.md)).

## Verdict

**RFI is the resync path on all three codecs.** The picture after the recovery frame is the one the page would have had with no loss, at the cost of one P-frame.

**The caution:** H.264 and AV1 decoders don't raise an error on a missing reference; they show garbage, silently (the control above). So the page must never feed a frame past a loss except a keyframe or a recovery frame. The WebTransport worker never does.

**Intra-refresh wasn't pursued.** It's also in P2.5's scope, but it feeds frames past a loss and lets the picture heal, which these decoders show as garbage until it heals. RFI recovers exactly.

## Live

RFI is now wired through the streamer and the page's worker (see [`cha-streamer`](../../crates/cha-streamer/README.md#rate-control-and-fec-p25)). HEVC over WebTransport on the S7 bench, in-order jitter, the Claude app's browser. Gaps are the longest wait between two frames; "before" is the keyframe resync S7 measured.

| Scenario | With RFI | Keyframe resync before |
|---|---|---|
| loss, 1 % onset | one event, ~120 ms gap, 12–14 frames lost (two runs) | 234 ms, 8 lost |
| loss, 3 % | 0 lost, 24–29 ms | 0 lost, 18 ms |
| wan (25 Mbit/s, 30 ± 5 ms, 0.5 %), onset | 8 lost, 117–120 ms | 7 lost, 131 ms |
| wan, steady | 0 lost, 25 ms in one run; one 4-frame, 119 ms event in the other | 0 lost, 26 ms |
| drop, 60 → 10 Mbit/s | 17 lost, 294 ms | 15 lost, 255 ms |

- **No keyframe fallbacks on loss.** The loss runs made 3–4 recoveries and the wan runs 2–3, none of them falling back to a keyframe (`rfi_keyframes=0`).
- **It halves the gap at a 1 % onset**, from 234 to ~120 ms, though more frames are lost (12–14 against 8). On the WAN scenario's onset the gain is ~10 ms.
- **The drop is a keyframe either way.** The overflow loses more frames than the DPB reaches back by the time the page's request arrives, so the encoder sends a keyframe (`rfi_keyframes=1`).
- **A recovery frame needs a keyframe's parity.** An earlier version, without it and without the page asking again at once when the recovery frame itself was lost, did worse than keyframes at the onset (gaps of 340–380 ms): the unprotected recovery frames were lost with the rest, and the page fell back to a keyframe 250 ms later.

**4K.** Eight references are more than levels 5.x allow at 3840×2160, so NVENC signals level 6.0 there (H.264 and HEVC; 5.0 at the page's size). The encoders start, the page plays 4K at 59–60 fps with nothing lost, and S8 on 4K dumps of H.264 and HEVC is identical on every decoder again (controls: H.264 19.7 dB, HEVC a decode error; AV1 at 2560×1440, 15.3 dB).
