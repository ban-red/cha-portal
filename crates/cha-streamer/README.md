# cha-streamer

One environment's media engine (plan §2.2, [ADR 0004](../../docs/adr/0004-own-engine-no-wolf.md)). The app runs as a client of our own headless Wayland compositor. Composited frames go to NVENC zero-copy through our own binding ([`cha-nvenc`](../cha-nvenc)), or to PyroWave on the LAN ([`cha-pyrowave`](../cha-pyrowave)), then to the browser over WebRTC (str0m) or WebTransport. Sound comes in through our own PulseAudio-protocol server and goes out as Opus. Keyboard and mouse come back into the compositor's seat; gamepads become virtual controllers (Xbox 360, DualSense or Steam Controller). There is no GStreamer, FFmpeg, PulseAudio, PipeWire or Wolf.

```
app ─Wayland─▶ compositor (Smithay, GLES on the render node)
                 │ 240 Hz: frame callbacks + presentation feedback → apps draw at 240 fps
                 │  60 Hz: composite (only if something changed) into a GBM buffer
                 ▼
        output pool (4 GBM buffers, each registered with CUDA once)
                 │ newest-frame-wins mailbox per codec
                 ▼
        encoder thread per codec (NVENC: P1 / ultra-low-latency, CBR, 1-frame VBV,
                 │                 infinite GOP, zero reorder delay)
                 ▼
        WebRTC session (str0m, playout-delay 0) ──▶ browser
                 ▲
                 └── input (control DataChannel) → the seat: pointer, relative motion, buttons, wheel, keys
```

## Devices (`--device nvidia|vaapi|cpu`)

What composites and encodes (`device.rs`; the portal side is `docs/devices.md`). The default is `nvidia`, as before.

| Device | Composites on | Encodes with | Codecs |
|---|---|---|---|
| `nvidia` | EGL on `--render-node` | NVENC through CUDA, zero-copy | H.264, HEVC, AV1, PyroWave |
| `vaapi` | EGL on `--render-node` (Mesa: iris, radeonsi) | VA-API (libva at run time), dmabuf imported with no copy | H.264, HEVC Main 8-bit (AV1 not written yet) |
| `cpu` | Mesa's llvmpipe (software EGL; no render node, no `/dev/dri`) | x264, SVT-AV1 | H.264, AV1 (when SVT-AV1 loads) |

- **One encoder trait** (`encoder/mod.rs`, `VideoEncoder`): encode a frame (keyframe on request), resize, change the bitrate or frame rate in place, reference invalidation where the encoder has it, and its frame index and timings. The media code (`media.rs`) is the same for every device. An encoder that can't invalidate references says so and the caller sends a keyframe. `Device::encoder` picks the implementation (`device::backend`: kind and codec).
- **Codecs offered** are `--codecs` (in its order) intersected with what the device makes; `hello`, `/streams` and the pages see only those. PyroWave stays NVIDIA-only (it is Vulkan and could run on Intel and AMD later).
- **`--probe-device <kind>[:<render node>]`** prints `{"kind","name","vendor"?,"renderNode"?,"codecs":[…],"cores"?}` on stdout and exits, before logging starts, so the last stdout line is the JSON; a failure exits non-zero. `nvidia` opens a session per codec to see which this GPU takes; `vaapi` asks libva (`vaQueryConfigEntrypoints`: a codec counts with an `EncSlice` or `EncSliceLP` entrypoint on a profile of it); `cpu` is x264 present (it is needed to start), SVT-AV1 if it loads (then `av1` follows `h264`), and the core count. It needs nothing writable. The node asks for `cpu` as it does for VA-API, in the same throwaway container (no device), and keeps `h264` only when the image doesn't answer.

**The CPU device.**
- **Compositing:** Smithay's `GlesRenderer` on Mesa's software EGL device (`EGL_MESA_device_software`, found by enumeration). No dmabuf global: software clients draw into shm. The scene goes into one renderbuffer (so only what changed is repainted, as the damage tracker sees an age of 1), is read back with `glReadPixels` (a PBO, mapped) and converted to planar 4:2:0 straight from the mapping into a free buffer of the pool: no extra copy. The pool's buffers are plain memory; a slot frees when the encoder lets go, as on a GPU.
- **Conversion** (`encoder/i420.rs`): BT.709, limited range (what NVENC signals), integers; the luma loop is a plain map that the compiler vectorises, chroma averages each 2×2 block, and bands of row pairs run on up to 4 scoped threads.
- **x264** (`encoder/x264.rs`) is opened with `dlopen` (`libx264.so.165` on Ubuntu 26.04; 160 to 165 and the unversioned name are tried) and set with option strings, so none of its parameter struct is mirrored except the size at the front (checked against the defaults). x264 is GPL-2.0-or-later, compatible with this project's AGPL-3.0; it is not linked, and the image installs the distro's package.
  - `zerolatency` (no B-frames, no lookahead, sliced threads, so all cores work on one frame), no scene-cut keyframes, infinite GOP: a keyframe (IDR) only when asked, intra refresh off, headers repeated on every keyframe, a one-frame VBV at the bitrate (`bitrate / fps`), BT.709 signalled.
  - Preset `superfast` up to 1080p60, `ultrafast` above it (`CHA_X264_PRESET` overrides, for measuring); threads are the cores the container may use, at most 16.
  - A bitrate change is a `x264_encoder_reconfig` in place. A new size or frame rate is a new session (x264 takes both at open), so it starts with a keyframe; no reference invalidation (a keyframe instead).
  - Above 1080p60 a warning says the CPU will struggle. The size comes from the page, so nothing is capped.
- **SVT-AV1** (`encoder/svtav1.rs`) is opened with `dlopen` (`libSvtAv1Enc.so.2` on Ubuntu 26.04, which ships 2.3.0; `.4`, `.3` and the unversioned name are tried first, and a library older than 2.x is refused) and set by name through `svt_av1_enc_parse_parameter`, so none of `EbSvtAv1EncConfiguration`'s layout (it changes with the release) is mirrored: the struct is room the library fills with its defaults. Only the picture and packet headers are mirrored (v2.3.0's layout, checked by a test). SVT-AV1 is BSD-3-Clause-Clear with the AOM patent licence, compatible with AGPL-3.0; the image installs the distro's `libsvtav1enc2`, `SVT_LOG=2` keeps its banner out of the logs, and without the library the device offers H.264 only.
  - Low-delay prediction structure (a picture in, a packet out), no lookahead, CBR with a buffer of one frame (at least the library's 20 ms) and at most two, infinite GOP: a keyframe (IDR) only when asked, no scene-cut ones, a sequence header on every keyframe. **Screen content mode is forced on** (palette and intra block copy; on synthetic flat blocks it halved both the time and the bits of mode off at the same target, and the quality at equal bits wasn't compared). BT.709 limited, 8-bit 4:2:0, level 5.1 and main tier forced, so the stream is the `av01.0.13M.08` that `/streams` announces (the NVENC stream's too); a test reads the sequence header back.
  - Preset 11, the fastest in low delay in 2.3 (12 and 13 are mapped to it; 10 costs about twice as much); `CHA_SVTAV1_PRESET` overrides it. Threads are the library's own: it picks its level of parallelism from the cores the process may use (6 of 6 on 16 cores).
  - The packets are temporal units in the low-overhead format (a temporal delimiter OBU, then the sequence header on a keyframe, then the frame), what WebCodecs' AV1 decoder takes; dav1d decodes the whole stream, and from any keyframe on.
  - **Bitrate** takes effect on a keyframe in 2.3 (a rate-change event with the picture), so a new target is held for the next keyframe, and a cut of a fifth or more, or a rise of half or more, asks for one at once (not more than one a second for that). Smaller moves wait: the rate control's 1/32 steps would otherwise be a keyframe each. A new size or frame rate is a new session (a keyframe); no reference invalidation (a keyframe instead).
- **Measured** (the dev machine's i7-13700K, 16 threads, shared with other work, release build, llvmpipe drawing 2400 changing blocks, `cargo test -p cha-streamer --release -- --ignored --nocapture cpu_device_cost`; "busy" is a moving gradient with noise on every pixel; the same target for both codecs, 22.5 Mbit/s at 1080p and 40 at 1440p; "cores" is the encoder alone at 60 fps):

| Size | Draw and read back | Convert (4 threads) | x264 (p50, cores, Mbit/s) | SVT-AV1 (p50, p99, cores, Mbit/s) |
|---|---|---|---|---|
| 1080p60, blocks | 10 ms | 1.4 ms | 2.4 ms (`superfast`), 1.0–1.4, 17 | 11 ms, 59 ms, 1.8, 22 |
| 1080p60, busy | 11 ms | 1.0 ms | 2.0 ms, 0.9, 19.5 | 7–8 ms, 125 ms, 5.6, 26 |
| 1440p60, blocks | 8 ms | 1.9 ms | 2.0 ms (`ultrafast`), 0.7–0.9, 30 | 16 ms, 63 ms, 2.4, 38 |
| 1440p60, busy | 14 ms | 1.6 ms | 2.1 ms, 0.9, 36 | 11 ms, 196 ms, 9.3, 47 |

  The p99 of 120 frames here is a keyframe (two in the run): AV1's costs 60 ms at 1080p, 125 to 200 ms on a busy picture, against x264's 4 to 35 ms. On the busy picture AV1's mean is 25 ms at 1080p and 40 at 1440p, so it doesn't hold 60 fps there; on a desktop picture it holds 1080p60 with room (11 ms of 16.7) and 1440p60 at the edge (16 ms). **AV1 on the CPU costs about 4 to 8 times x264's time and 2 to 3 times its cores on a desktop picture, for the same bit rate at much better quality (screen content tools); use it where the browser or the link wants AV1, not for the CPU's sake.** Drawing here is mostly llvmpipe filling thousands of rectangles; a real desktop repaints far less. On a machine with fewer cores the encoders and llvmpipe compete for them.

**VA-API** (`encoder/vaapi.rs` and `encoder/vaapi/`, `encoder/bitstream.rs` and `bitstream/`): H.264 and HEVC Main 8-bit; HEVC was tested on an Intel UHD 630 (iHD), AMD is untested.
- **Input, no copy.** The compositor's GBM dmabuf is imported as an RGB VA surface (`vaCreateSurfaces` with `DRM_PRIME_2`, cached per output buffer). The driver's video processor converts it to NV12 (BT.709, limited range) on the GPU, and the encoder reads that. While the output pool picks its format and modifier it asks the VA driver to import a real buffer of each candidate (`vaapi::accepts_import`), so it only allocates what the driver takes; if no candidate imports, the stream doesn't start and says why. There is no copying fallback.
- **Low latency.** CBR with a four-frame HRD buffer (HEVC: ten, because iHD's HEVC rate control gives an IDR a smaller share of the same buffer; `CHA_VAAPI_HRD_FRAMES=n` overrides); one slice per picture; no B-frames, POC type 0, one reference, an infinite GOP with an IDR on request; `EncSliceLP` when the driver has it (`CHA_VAAPI_ENTRYPOINT=slice|lp` forces one); High, else Main, else Constrained Baseline. Bitrate and frame rate change in place (rate control buffers with the `reset` flag on the next picture); a size change makes new surfaces and starts with an IDR.
- **Headers are ours** (`bitstream.rs`, from the H.264 spec; exp-Golomb writer, SPS with a BT.709 limited-range VUI and `bitstream_restriction` with `max_num_reorder_frames = 0`, PPS, slice header, Annex-B framing). Where the driver takes packed headers (Intel's iHD) it gets our SPS and PPS on every IDR. Where it writes its own (Mesa's radeonsi), its SPS is parsed and written again with our VUI, and a missing SPS or PPS is added, so the output is the same either way. `CHA_VAAPI_PACKED_HEADERS=off` forces the second path; `=slice` also packs the slice header (an experiment: with rate control the driver picks the QP). `CHA_VAAPI_PACKED_EMULATION=driver` hands the packed headers over without emulation prevention bytes and has the driver add them, in case a driver ignores `has_emulation_bytes`.
- **Codecs.** H.264 and HEVC Main 8-bit; `Device::open` offers what the driver encodes *and* we have written (`vaapi::BUILT`).
- **HEVC** (`vaapi/hevc.rs`, `bitstream/h265.rs`, from the H.265 spec): the same low-latency shape (one slice, IP only, one reference, CBR, an IDR on request with VPS, SPS and PPS in front), `EncSliceLP` before `EncSlice` (the UHD 630 has only `EncSlice`). The VPS/SPS/PPS and the slice header are packed by us (iHD fails without a packed slice header). The coding tree is 32×32 over 8×8 coding blocks (what `VAConfigAttribEncHEVCBlockSizes` says, if the driver answers; iHD doesn't); temporal MVP and strong intra smoothing are on, SAO and AMP off. `cu_qp_delta` is on: iHD's rate control changes the QP per coding tree block, and without the flag the slice header's QP is wrong and the picture is garbage. `CHA_VAAPI_HEVC_TOOLS=sao,amp,sdh,tskip,nocuqp,nottmvp,nossm` switches tools against these defaults; `CHA_VAAPI_PACKED_HEADERS=headers` leaves the slice header to the driver.
- **Self-test** on a machine with the GPU: `CHA_ENCODE_TEST=120 cha-streamer --probe-device vaapi:/dev/dri/renderD128` encodes 120 generated 1280×720 pictures (`CHA_ENCODE_TEST_CODEC=hevc` for HEVC) (a bitrate change after a quarter, an IDR requested at the half), checks every access unit (NAL types, an IDR first with SPS and PPS, the SPS size) and prints the result on stderr before the JSON; it fails non-zero otherwise. The same as a test: `CHA_VAAPI_NODE=/dev/dri/renderD128 cargo test -p cha-streamer vaapi_self_test -- --ignored --nocapture`. `LIBVA_MESSAGING_LEVEL=2` makes libva say more.

## How it works

- **Two clocks.**
  - A timerfd ticks at the compositor rate (`--compositor-fps`, default 240, rounded up to a multiple of `--fps`: 270 for 90 fps, so every encoded frame falls on a tick). Every tick sends frame callbacks and presentation feedback, so apps draw at that rate. S2 showed this halves click → screen latency.
  - Only every encode tick (`--fps`, default 60; 90 and 120 are options, see *Frame rate*) composites, and only if something changed. Nothing is composited that won't be encoded.
  - Chrome paces itself to presentation feedback, not frame callbacks: with feedback at 60 Hz it drew at 60 fps even with 240 Hz callbacks.
- **Zero-copy.**
  - The output pool is four GBM buffers in the GPU's own tiling (`ARGB8888`).
  - Each buffer is registered once with CUDA (an EGL image), and once with NVENC per encoder. Encoding a frame maps and encodes it; nothing is copied.
  - A buffer is reused when every encoder has dropped its frame.
- **Encoders.**
  - One thread per codec, started by the first viewer of that codec and kept warm afterwards.
  - A one-frame mailbox means a slow encoder skips frames instead of queueing them.
  - A keyframe request with nothing new on screen re-encodes the last frame.
  - On resize the session is reconfigured in place, up to 3840×2160.
- **Window policy (kiosk).** Every toplevel fills the output: maximized, or fullscreen when it asks. Decorations are server-side, i.e. none. Dialogs are centered. Popups get proper grabs.
- **Protocols:**
  - compositor, xdg-shell, xdg-decoration;
  - shm, linux-dmabuf v5 with feedback naming the render node;
  - seat (keyboard, pointer), relative-pointer, pointer-constraints;
  - data-device, primary-selection;
  - viewporter, presentation-time, single-pixel-buffer, xdg-output.

  Xwayland comes with XFCE in P1.4.

## WebTransport (P2.3)

The Chromium fast path, `cha-stream/1` (plan §3.1), beside WebRTC on its own UDP port (`--wt-port`):

- **Media:** each encoded frame goes out as datagrams with `cha-proto`'s 16-byte header (frame id, fragment index and count, keyframe flag, send time); Opus frames one datagram each. The player reassembles them in a worker and decodes with WebCodecs into track generators feeding `<video>` and `<audio>`, the fastest path to the screen in S1d.
- **Control:** the session's first bidirectional stream, with the same JSON lines as WebRTC's DataChannel (input, pings, resize), plus `keyframe` and `rfi` requests.
- **No silent eviction:** a frame QUIC's send buffer can't take whole isn't sent, and nothing after it is until a keyframe or a recovery frame (asked for at once, as under RFI below). The page also asks when a frame can't be completed or a gap shows.
- **Congestion:** our own rate control and FEC (P2.5, below).
- **Certificate:** self-signed, 13 days, ECDSA P-256; browsers accept it by its SHA-256 (`serverCertificateHashes`), which the portal hands out with the URLs (one per node address, each carrying the media token).
- **Several viewers** (P2.6, below): WebTransport sessions coexist on the one endpoint, and WebRTC ones share their one port.
- **Codec switches in place** (`{"t":"codec","codec":…}` on the control stream). The session subscribes to the other encoder and keeps sending the current stream until that encoder's first frame. From there it sends the new codec under the next `stream` number (the header's stream byte), and the page starts that stream afresh. Answered with `{"t":"codec","codec":…,"stream":…}`, or an `error`. In the app's browser, every switch among H.264, HEVC, AV1 and both PyroWave modes left the picture without a gap over 35 ms, cold encoders included (a reconnect costs a new handshake and a blank picture).
- **Stats** carry composited → encoded and encoded → sent percentiles per report window, so a codec's cost shows within half a second of switching to it.
- **First numbers** (the app's browser, which wasn't painting, so no click → screen yet): H.264 at 1616×1256 decodes in 1.5–1.6 ms with nothing lost; click → sound 55 ms p50, against 85–90 over WebRTC (no NetEq buffer).

## Frame rate (60 to 120 fps)

The baseline stays 1440p60 on 1 GbE; 90 and 120 are options. `--fps` sets the rate at the start (the node passes the app's choice), and a page can change it while the stream runs.

- **Bitrate.** The defaults are for 60 fps (`framerate.rs` holds the rules):
  - **NVENC codecs:** `--mbps` × (fps / 60)^0.75, so ×1.36 at 90 and ×1.68 at 120 (40 → 67 Mbit/s). Frames are cheaper at a higher rate (less changes between them), so the bitrate grows slower than the rate. The result is the session's starting rate and its ceiling; rate control climbs to it.
  - **PyroWave:** `--pyrowave-mbps` is the budget at 60 fps. It is per frame, so the total grows linearly with the rate and a frame keeps its quality, up to 600 Mbit/s, what 1 GbE carries with room for audio and framing. 4:2:0 at 120 fps is 580 Mbit/s, under it. 4:4:4 is 580 at 60 fps and would be 1160 at 120: capped, so its frames get half the bytes (logged once). PyroWave has no bitrate to trim: rate control only holds frames while the queue drains.
- **Live change** (`{"t":"fps","fps":60|90|120}`, any transport, only from the session with the controls). The compositor retimes its clock and composites at the new rate. Each encoder follows on its next frame: `nvEncReconfigureEncoder` changes the frame rate and bitrate in place (the VBV is one frame of bits, and NVENC's timing follows), with no keyframe and no gap, so there is no encoder to restart as a codec switch has. A reconfigure that fails restarts that encoder from a keyframe. The sessions see the new rate on their next 50 ms tick and move rate control's ceiling to the scaled bitrate (`RateControl::rescale`): a target that the path never held back goes to it, a target the path had cut keeps its level (a higher ceiling is climbed to) or follows a lower ceiling down in proportion. Answered with `{"t":"fps","fps":N}`, or `{"t":"fps","fps":N,"error":"…"}` with the rate still running. Every viewer sees the new rate in the next `stats` message (about every 500 ms), which carries `fps`, as does the `hello`'s `stream` and `GET /info`. The page offers it only for desktop apps: a gamescope app (Steam) can't change its refresh while running.
- **What scales with the rate, and what is fixed in time** (the audit for 60 to 120 fps):

| Where | Rule | At 120 fps |
|---|---|---|
| NVENC bitrate, VBV (one frame of bits), frame rate in the encoder init | scaled (above); VBV and timing follow `fps` | 67 Mbit/s, VBV 70 kB |
| PyroWave frame budget | per frame, total scaled and capped at 600 Mbit/s | as above |
| Encode interval (compositor), re-encode throttle (a keyframe or recovery with nothing new on screen: once per frame) | per frame | 8.3 ms |
| Keyframes, IDR, GOP | none: infinite GOP, keyframes only on request | — |
| Send-queue hold (`HOLD_MS`) | fixed in time, 25 ms (1.5 frames at 60). In frames it would halve and hold over queues rate control doesn't call one yet (soft back-off starts at 20 ms) | 3 frames |
| Queue seen by rate control (`backlog_ms`) | in time: frames queued × the frame interval | — |
| Frame-size average (hold's estimate) | fixed in time: weight 0.9 per frame at 60 fps, 0.95 at 120 (~170 ms) | — |
| FEC: chance a frame is lost past its parity | per second, not per frame: 10⁻⁴ at 60, 0.5 × 10⁻⁴ at 120 (a stall once in 167 s at either) | 0.5 × 10⁻⁴ |
| FEC: keyframes and recovery frames (10⁻⁶, 0.3 % loss) | fixed: rare, not per-frame | — |
| Rate control (40 and 20 ms queue thresholds, 15 ms delay growth, 100 ms reports, 300 ms and 1.2 s settling, 400 ms stall grace, 50 % a second climb) | fixed in time, all of it (`rate.rs`): a queue means the same latency at any rate | — |
| Pacing tick (50 ms), keyframe retry (300 ms), stuck subscriber (1 s), PyroWave heal (250 ms) | fixed in time | — |
| RFI: DPB of 8 frames (`DPB_FRAMES`) | fixed in frames, so its reach in time halves: ~117 ms at 60, ~58 ms at 120 (less the loss report's trip). It can't grow: AV1 has eight reference slots, and a bigger DPB at 1440p passes the 12 frames H.264 and HEVC levels allow decoders. A loss further back costs a keyframe, as before | ~58 ms |
| RFI ring of sent frames (256) | fixed in frames, and enough: invalidation reaches 7 frames back, so a longer memory names frames nothing can use. 4.3 s at 60, 2.1 s at 120 | 2.1 s |

- **Measured** (RTX 4090 in the dev container, `cha-nvenc`'s ignored test `switches_to_120_fps_in_place_and_keeps_up`, 1440p ARGB from CUDA memory, two noisy frames alternating, no compositor): H.264, HEVC and AV1 encode in 1.6–2.4 ms (p50) at both rates, p99 at most 2.5 ms, inside the 8.3 ms a 120 fps frame has. The in-place switch from 60 to 120 fps with the bitrate scaled made 480 frames with no keyframe. A 120 fps frame is about 67 kB against 79 kB at 60 (the picture here is synthetic noise, so only the ratio means anything).

## Rate control and FEC (P2.5)

The WAN tier over WebTransport (plan §3.1 rules 1, 2 and 6). Spike S7 ([`spikes/s7-wan`](../../spikes/s7-wan/README.md)) drives it through netem.

- **Receiver truth.** Every 100 ms the page's worker reports:
  - how long frames took from send to complete there (the median of the last 150 ms, clock-synced);
  - what arrived (the last 200 ms);
  - the share of datagrams, data and parity, that never did (the last second).

  QUIC's RTT and loss count stand in only until the first report. Its RTT lags by seconds on a draining queue, and it counts reordered packets as lost.
- **Rate control** (`rate.rs`), every 50 ms, delay first and loss last:
  - **Cut:** a queue over 40 ms (delay growth over its 10 s minimum, or our own send queue) cuts the rate to 0.85 of what arrived. No second cut until frames sent after the first can have reported: the queue plus 300 ms, or 1.2 s for loss alone (the page counts an overflow's loss that long).
  - **Back off:** over 15 ms, 10 % down.
  - **Hold:** at 2–10 % loss. Random loss isn't congestion; FEC covers it.
  - **Climb:** after a calm second, 50 % a second, or 10 % near where the path last pushed back. Never past twice what goes out: NVENC's low-latency CBR makes about two thirds of its target, and a target far above what's sent would let the next busy picture burst into the bottleneck.
  - **Longer path:** a "queue" that holds while the rate falls by a third for 2 s is a longer path, and the delay floor moves up to it.
  - **Stall:** next to nothing arriving, or the reports stopping, is mostly a busy browser, so it's waited out for up to 400 ms. What arrives after it is a burst whose delays show the queue, and a cut goes to what arrives then.
- **Encoder.** The target goes to NVENC in place, without an IDR (`nvEncReconfigureEncoder`), when it moves by more than 1/32. Video gets 92 % of the rate, less what parity takes.
  - **Hold:** while more than 25 ms of frames (1.5 at 60 fps) wait in QUIC's send buffer, the encoder holds, skipping frames rather than queueing them. A subscriber held for over a second (a stuck page) stops holding the shared encoder.
  - **Keyframes:** a keyframe re-encode happens at most once per frame interval.
- **Congestion window** (`congestion.rs`): our own quinn controller, a fixed 8 MB window that ignores loss, since rate control paces the media. Cubic halved its window at every random loss and starved the stream at 1 % loss.
- **FEC** (`cha-proto::fec`): systematic Reed-Solomon over GF(2⁸) with a Cauchy matrix, in blocks of up to 128 data fragments.
  - **How much:** per block (header byte 3), the least parity that keeps a frame's odds of being lost under 10⁻⁴ at 60 fps (scaled by 60 / fps; 10⁻⁶ for keyframes and recovery frames, the binomial tail), and never more than half its data.
  - **For what loss:** 1.5× the loss measured while the path was calm, at least 0.3 %, held for 5 s after the last loss. A queue's overflow is rate control's to fix; parity would only add to it.
  - **Keyframes and recovery frames** always have parity for at least 0.3 % loss: one is mostly asked for because something was lost, and a resync point has to arrive. Before recovery frames had it, one sent at the onset of loss was often lost itself.
  - **Wire:** parity datagrams, flagged `PARITY` with indices after the data, carry the frame's length and a shard. The page rebuilds a frame as soon as any k of a block's k + m shards are in.
- **Resync (RFI).** A lost frame costs a P-frame instead of a keyframe: the next frame refers around it.
  - **Request:** the page's worker sends `{"t":"rfi","id":N}` on the control stream, N being the first lost frame's id. The session maps it to the encoder's frame index (a ring of the last 256 frames it sent: 4.3 s at 60 fps, 2.1 at 120, more than reference invalidation can use), records it as the page's loss point, and asks the encoder to invalidate from there (`Media::request_invalidate`).
  - **Encoder:** `nvEncInvalidateRefFrames` for each frame from the loss on; the DPB holds 8 frames (`cha-nvenc`'s `DPB_FRAMES`; ~117 ms at 60 fps, ~58 ms at 120). A loss more than 7 frames back, or before the last keyframe, gets a keyframe instead. With nothing new on screen, the last frame is encoded again around the loss. Frames the sender drops itself (QUIC's buffer full) take the same path rather than always a keyframe.
  - **Flag:** the next frame is `RECOVERY` for every session whose loss point is at or after the invalidation point; the others get it as a normal frame. It has keyframe-grade parity.
  - **Page:** the worker drops frames until a keyframe or a `RECOVERY` frame that answers its loss. It asks again at once if that frame is itself lost (overtaken by later frames, or silent for 250 ms), and asks for a keyframe if nothing came 250 ms after asking.
  - **Log:** the encoder's 10 s line has `recoveries=` and `rfi_keyframes=` (the fallbacks).
  - **Checked** in S8 ([`spikes/s8-rfi`](../../spikes/s8-rfi/README.md)): H.264, HEVC and AV1 decode bit-exactly from a recovery frame, in hardware and software. A recovery frame costs what a P-frame does, a keyframe 2–3 times as much.
  - **S7, HEVC** (in-order jitter): the gap at a 1 % loss onset is ~120 ms (234 ms with keyframes), 117–120 ms at the WAN scenario's onset (131), with no keyframe fallbacks. The drop is still a keyframe (294 ms, against 255): the overflow loses more frames than the DPB reaches back by the time the request arrives. A first version, with no parity on recovery frames and no immediate re-ask, did worse than keyframes at the onset (340–380 ms): the unprotected recovery frames were lost, and the page fell back to keyframes 250 ms later.
  - **WebRTC** keeps PLI and keyframes: Chrome's receiver has no RFI message, and NACK recovers most losses.
- **Path MTU.** When QUIC lowers its datagram size (a black-hole fallback), the next frame is cut to the new size.

**S7 results** (HEVC unless noted; test pattern with 20 % noise, 60 fps at the bench page's size, about 1792×1008; [JSON](../../docs/benchmarks/s7-2026-10-04-electron152-gpu-node-wan.json)):

| Scenario | Link | Result |
|---|---|---|
| WAN | 25 Mbit/s, 30 ± 5 ms, 0.5 % loss | 60.8 fps at 21.7 Mbit/s; after the first 3 s, 3 frames lost and the longest gap 51 ms. AV1: 60.3 fps at 21.5 Mbit/s, longest gap 25 ms after the onset |
| Loss | 20 ms; 1 %, then 3 % | 3 %: nothing lost, 693 frames rebuilt, longest gap 18 ms. 1 %: 8 frames lost at the onset, none after |
| Drop | 60 → 10 → 60 Mbit/s, 20 ms | 8.5 Mbit/s on the 10; one 255 ms gap at the drop; back over 20 Mbit/s 6 s after the link |

Before P2.5, 3 % loss brought the stream down to 10 fps and then 1, and a drop to 10 Mbit/s froze it for good.

**WebRTC** uses the same rate control, with the page's reports coming over the control DataChannel. No FEC; Chrome's NACKs and retransmissions recover losses.
- **Send times** come from each frame's RTP timestamp, the encode time on the session clock. The `sent` messages share the DataChannel, whose retransmissions hold them back under loss, just when the delay matters.
- **Delay:** the page reports the lower quartile of presented frames' send → complete (`receiveTime`), not the median. A frame that needed a retransmission completes a round trip late, and at a few percent loss most frames do; a queue delays them all.
- **Updates:** only a report with a delay updates the rate. A page that presents no frames (hidden) holds it.
- **Resync bound:** when frames stop decoding for 300 ms while data still arrives, the page asks for a keyframe. Otherwise Chrome waits seconds on retransmissions a congested path doesn't deliver.
- **Not str0m's GCC** (S7): it paces media at 1.1× its estimate (~14 ms per frame on a LAN), and its estimate stalls at 1.5× what NVENC's undershooting CBR sends.
- **S7, WebRTC, HEVC:**
  - unshaped: 24 Mbit/s at 60 fps, as before;
  - WAN (in-order jitter): 59.4 fps at 17 Mbit/s, no freezes;
  - 3 % loss: 58 fps at 14.6 Mbit/s, no freezes;
  - the drop: one 0.5 s freeze, back to full rate 5 s after the link.

**Open:**
- A loss further back than the DPB (a queue's overflow, as at the drop) still costs a keyframe. Intra-refresh isn't pursued: it feeds frames past a loss, which H.264 and AV1 decoders show as garbage until it heals, and RFI recovers exactly (S8).
- Loss starting before parity: the first ~300 ms of it costs frames.

## PyroWave (P2.4)

The LAN tier (plan §3.2–3.3): Themaister's wavelet codec, intra-only, a few tenths of a millisecond of GPU work per frame, at hundreds of Mbit/s. WebTransport only. Offered as `pyrowave420` (games) and `pyrowave444` (desktops) when libpyrowave loads and the render node's PCI ids are known; the player offers it where WebGPU has subgroups.

- **Encoder** ([`cha-pyrowave`](../cha-pyrowave), our binding to `libpyrowave-shared`, loaded at run time): its own Vulkan device on the same GPU, picked by PCI vendor and device id. One device per streamer, shared by both modes (their calls take turns), made in the background at start-up. Making one takes ~0.4–0.6 s and stalls the GPU's other work (NVENC, the compositor) for ~0.2 s, which a first switch to PyroWave mid-session would show as a freeze. The warm device costs ~57 MiB of VRAM (an idle streamer: 394 → 451 MiB); leave PyroWave out of `--codecs` to skip it. Each output buffer is imported once as a dma-buf with its DRM modifier. The encoder takes RGB and converts to YCbCr on the GPU, then splits the frame into packets of about 1100 bytes that each decode on their own.
- **Modifiers.** NVIDIA's GL picks compressed modifiers that its Vulkan driver can't import, so the output pool leaves those out (they gain nothing for a buffer that is read once).
- **Budget:** `--pyrowave-mbps` (default 290) for 4:2:0 at 1440p and 60 fps, scaled with the picture's area; 4:4:4 gets twice as much. It is per frame, so the total grows with `--fps`, to 600 Mbit/s at most (*Frame rate*).
- **No keyframes.** Every frame stands alone, so a lost datagram blurs its region of one frame and nothing waits for a resync. When the screen goes still, the last frame is sent once more after 250 ms (a heal), so a loss doesn't stay on screen.
- **Wire.** One PyroWave packet per datagram. Its blocks are atomic, so a packet can come out larger than asked; it is then split across datagrams flagged `CONTINUES` (more follow) and `CONTINUED` (a tail). The player assembles whole packets, decodes everything that arrived by the 60 ms deadline, and drops older frames.
- **Browser:** `@cha/pyrowave-webgpu` decodes into an offscreen WebGPU canvas; each frame becomes a VideoFrame for the same track generator and `<video>` as the hardware codecs, so presentation, stats and the probe work alike.
- **Failures are visible.** If the encoder can't start, the session closes with the reason, and the next viewer gets a fresh try. `cha-node --doctor` makes the device the way a streamer does (`cha-streamer --probe-pyrowave`). NVIDIA's Vulkan driver (`libGLX_nvidia`) links libX11 and libXext, so the image carries both.

**First numbers** (RTX 4090, 1 GbE, 2026-10-04):

| | 4:2:0 | 4:4:4 |
|---|---|---|
| 2560×1440, raw WebTransport session from the app's browser | 267 Mbit/s, ~650 datagrams per frame, every frame complete | 574 Mbit/s at 58.5 fps, 702 of 703 frames complete |
| 1920×1440 through the portal, decoded and shown in the app's browser | 218–221 Mbit/s at 60 fps; 12 frames lost at start (the resize), none after | 442 Mbit/s at 60 fps, none lost |

- GPU work per frame: 0.14 ms at 1920×1440 4:2:0 (scale, DWT, quantize, analyze, resolve, pack), about 0.22 ms at 1440p.
- Wall clock per encode: 2.5 ms p50, 2.7–3.0 ms p99, while a ComfyUI job held the GPU. Of that, recording and submitting take 0.14 ms and packetizing 0.05 ms. The other 2.3 ms is waiting for the GPU: one full timeslice of the other job's, every frame, since each encode is submitted just after the job gets the GPU back. NVENC under the same load waits 1.8–3.8 ms p50 and up to 5.4 ms p99, since its RGB input conversion runs on the same shaders. S1e's idle-GPU figure for the whole wait plus readback was 0.4 ms. See *Shared GPUs* below.
- The decode call in the browser: 0.35–0.5 ms (CPU side; the GPU work isn't timed yet).
- Send → shown and click → screen need a painted browser (the app's pane mostly wasn't).

## Shared GPUs

A homelab GPU often runs other work (image generation, LLMs). NVIDIA time-slices the GPU between processes, so whenever another process has work queued, the compositor's render and each encode wait for that process's timeslice to end: up to ~2.3 ms each on the RTX 4090 at the default timeslice. Fewer GPU contexts per frame mean fewer waits. Sharing one Vulkan device between the compositor and PyroWave (plan §3.3) would make it one.

- **The operator's lever:** a shorter compute timeslice, `sudo nvidia-smi compute-policy --set-timeslice=1` (SHORT; 0 restores the default). It costs the other jobs some throughput, and it doesn't persist across reboots.
- **Queue priority:** PyroWave can ask for a high-priority (async compute) queue, but on driver 595.71 the first encode on it never completes, so we stay at the default priority until that's understood.

## Sound (P1.6)

```
app ─libpulse─▶ our PulseAudio-protocol server ($XDG_RUNTIME_DIR/pulse/native, sink "cha")
                 │ per stream: decode, downmix to stereo, resample to 48 kHz (polyphase sinc)
                 ▼
        mixer thread, 10 ms clock ── Opus (libopus, our binding; CELT-only, 128 kbit/s VBR)
                 ▼
        audio track (its own MediaStream: no lip-sync hold on video) ──▶ browser (NetEq at its minimum)
```

- **Why a server of our own.** Every Linux audio stack speaks the PulseAudio protocol: libpulse, PipeWire's and SDL's backends, Chrome, Firefox, Wine. One small server replaces PulseAudio or PipeWire in the container, and the samples go straight to our mixer. The `pulseaudio` crate (MIT) supplies the wire format; the server is ours (`src/audio/pulse.rs`).
- **What it has.** One sink, playback streams, volumes and mutes, subscriptions (so mixers like pavucontrol work), exact latency and timing replies. No sources (microphones), sample cache or modules yet. Samples arrive over the socket; there is no shared memory.
- **Flow control** follows PulseAudio's: a stream is kept `tlength` ahead (REQUEST), starts once `prebuf` is queued (STARTED) and stops again when it runs dry (UNDERFLOW). A stream that leaves `tlength` to the server gets 60 ms, not PulseAudio's 2 s. Nothing gets less than 20 ms (two mixer ticks).
- **The clock runs with or without a viewer.** Apps, and media players that pace video by audio, need a sink that consumes. Encoding happens only while a session listens.
- `--no-audio` turns it all off.

## Viewers (P2.6)

Up to four sessions watch an environment at once (`viewers.rs`, plan §3.4). Viewers with the same codec share its encoder. WebTransport and WebRTC sessions coexist, in any mix.

- **WebRTC viewers share one UDP port** (`rtc_hub.rs`). The streamer binds `--webrtc-port` once per host address, for as long as any WebRTC session exists; every session has its own str0m `Rtc` (its own ICE credentials, DTLS and tracks) in its own task. A reader task takes each datagram off the sockets and routes it by the sender's address. An address no session has claimed (a browser's first STUN check, or a NAT rebinding) is offered to every session, and the one whose `Rtc::accepts` takes it (STUN by its ICE username, everything else by the nominated address) claims the address; later datagrams go to it alone. A session that is sent something it doesn't accept lets the address go. Output goes out of the socket whose address str0m names. The last session to leave frees the port; a session started right after retries the bind for about 160 ms before settling for another port.
- **Rate control is per viewer, the encoder follows the slowest.** Each session runs its own rate control from its own page's reports and sets its own pace; a shared encoder runs at the lowest rate any live subscriber takes and holds when one is backed up (a viewer stuck for a second stops counting). Every WebRTC viewer gets the same encoded frames, written to its own video track, and its own Opus track.
- **No takeover on reconnect.** A new WebRTC connection no longer stops the previous one. A tab that vanishes without closing keeps its seat until str0m sees ICE disconnect, so a reconnect during that window counts against the limit of four.

- **One has the controls (the floor):** its keyboard, mouse, gamepads, resizes, cursor mode and clipboard reach the environment, and only it gets the apps' clipboard. The others watch: what they send is ignored.
- **Who:** the newest owner session takes the floor on joining; an admin's only when no owner is watching. When the controller leaves, the newest owner or admin (owners first) gets it. Owners and admins can take it (`{"t":"take_control"}`). Share links' roles (ADRs 0014, 0015): a `player` is one gamepad slot and never holds the floor; a `viewer` watches only and is refused `take_control`; a `controller` takes the floor when nobody or another controller holds it, never from an owner or admin, takes an empty one on joining, and is never given one on a leave. The holder, if an owner or admin, hands it to a controller with `{"t":"give_control","to":<session id>}`.
- **Presence and idle time.** A session is *active* from joining. A browser page sends `{"t":"presence","active":true|false}` (any role) when its control channel opens and whenever it changes: active while the page is visible or its sound is audible (playing, not muted, volume above 0), so a forgotten hidden tab stops holding its seat. Native and GameStream clients never send it and stay active. `Viewers` keeps the idle clock: `idle_since` is set while no session is active (from the start while nobody has come; when the last active session went quiet or left) and cleared while any is. `GET /info` has `"viewers"` (all sessions), `"active_viewers"` and `"idle_secs"` (0 while any session is active); the portal's idle shutoff reads `idle_secs`. Presence moves neither the floor nor the count.
- **Nothing stays held.** Whenever the floor moves (the controller's session ends, or another takes it), the streamer lifts every key and pointer button the compositor still holds down and puts every pad back to rest (`gone`: sticks centered, buttons up). The old controller's page can't send the release, and the character would run on forever.
- **Seats don't leak.** A WebRTC session that isn't connected (ICE and DTLS) 15 s after its offer ends and frees its seat; a WebTransport session that opens no control stream within 5 s of being accepted is closed (code 3) and frees its seat. Both are logged.
- **Pages hear** `{"t":"floor","control","viewers"}` on every change (with `"can_take":true` when that session could take the floor now), and the session holding it also `{"t":"viewers","list":[{"id","role","slot"?}]}`, the other sessions. A watching page gets `{"t":"pointer","x","y","drawn"}` at encode ticks, and draws the controller's pointer over the picture when the controller's page draws its own cursor (`drawn` false).
- Tested with an owner and an admin on one Chrome environment: the admin joined watching ("2 watching", the owner's pointer drawn where it was), took the controls, the owner took them back, and the admin's leaving left the owner in control. Both shared one HEVC encoder.
- **Late viewers and the pads.** The streamer keeps the last lightbar colour, player-LED mask and adaptive-trigger effect per pad (`PadMemory`). A session that gains the controls, by joining as the controller or by taking them, is sent them at once (`led`, `players`, `trigger`), on either transport. Rumble and haptics are moments, not state, and aren't replayed.
- **Unit-tested, not live:** two str0m clients' STUN checks reach only their own viewer's `Rtc` (`session.rs` tests); the router's claim, release and leave rules (`rtc_hub.rs`); the pad replay (`control.rs`). A live two-viewer WebRTC run (Chrome with Safari or Firefox) hasn't been done.

## Clipboard (P2.6)

Text both ways, on the control channel (`{"t":"clipboard","text":…}`), up to 1 MiB:

- **Environment → browser:** when an app sets the clipboard, the compositor asks it for the text (UTF-8 first) at the next tick. Smithay reports a selection before storing it. A thread reads the pipe, and the text is published to the sessions, which send it to the page. The player writes it to the device's clipboard, or on the next click if the browser wants a gesture.
- **Browser → environment:** the page sends its clipboard just before a paste shortcut's keys, so the paste uses it. The text becomes the compositor's own selection, written on a thread to each app that pastes.
- **Wayland apps** (Chrome, Firefox) take part directly.
- **X11 apps** (XFCE, under rootful Xwayland; Steam, under gamescope's Xwayland, which doesn't pass the clipboard through) keep their clipboard inside the X server, which only the app's container reaches. The `xfce` and `steam` images run a helper, `cha-x11-clipboard` ([`crates/cha-x11-clipboard`](../cha-x11-clipboard), x11rb), as the desktop's user. It talks to the streamer over a Unix socket in the shared volume (`/run/cha/clipboard`, owned by the app's uid; `src/x11_clipboard.rs`), in `cha_proto::clipboard`'s frames: kind, sequence number, length, UTF-8 text.
  - **X → page:** the helper watches CLIPBOARD's owner (XFixes). On a new one it reads `TARGETS`, then the text as `UTF8_STRING` (or Latin-1 `STRING`), INCR included, up to 1 MiB, and sends a `copied` frame. The streamer publishes it where Wayland copies go, so the sessions forward it to the page. Every copy goes up, the same text again too, since the device's clipboard may have changed since; the helper's own taking of the clipboard (for the page's text) isn't a copy, so the page's text doesn't echo back.
  - **Page → X:** a `set` frame makes the helper own CLIPBOARD (with a server timestamp, checked afterwards), then answer `ack`. It serves `TARGETS`, `TIMESTAMP`, UTF-8 text (`UTF8_STRING`, `TEXT`, `text/plain;charset=utf-8`, `text/plain`) and Latin-1 `STRING` (`?` for what it lacks), by INCR in 64 KiB chunks past the server's request limit. `Media::set_clipboard` waits for the `ack` (30 ms at most; not at all with no helper connected) before returning, so the paste's V key, which follows on the same ordered channel, reaches Xwayland after the X selection is owned. The wait is on the session's task, never the compositor's.
  - Plain threads: one accepts, each helper has a reader and a writer. The helper reconnects every second if the streamer restarts, and exits with the X server. Only CLIPBOARD is bridged, not PRIMARY (Wayland apps' isn't either).
  - `cha-x11-clipboard --get` and `--put TEXT` are an ordinary X client's reading and owning, for tests and ops.
- Tested in XFCE end to end through the portal: copies from xfce4-terminal and Thunar reach the page, and the page's text pastes into both, 293 KB (by INCR both ways) and accents, CJK and emoji included, byte-exact.
- Tested in the Chrome environment: text with accents and emoji pasted into the omnibox from the page, then copied back, each exactly once. The helper is tested against a real X server (Xvfb) and a pretend streamer: copies both ways, accents and emoji, INCR both ways (300 KB), refusal over 1 MiB, and `xclip`'s reads of every target and its Latin-1 `STRING` ownership.

## Cursor (P2.6)

In desktop mode the page draws the cursor (`{"t":"cursor","client":true}`), so it moves with no stream delay. The compositor then leaves it out of the picture, and pointer motion over still content no longer costs a frame: 60 moves across a static Chrome page gave 2 frames, where each move used to be composited and encoded. With a locked pointer (games) the page asks for it back in the picture; each session starts that way.

- **Shapes** go out on change as `{"t":"cursor"}`. A named cursor is a CSS keyword: we implement the cursor-shape protocol, whose names are CSS's, and Chrome and GTK 4 use it, so `text` over a text field costs a few bytes. Apps that draw their own cursor surface (Xwayland, GTK 3, Qt) send the image as straight-alpha RGBA, base64, once per session per image (later only its `id`); larger than 256 px goes as `default`.
- **The page** sets the element's CSS cursor. Images go through `image-set()` at the stream's scale, so a 30 px cursor in a 2× stream shows at 15 CSS px, crisp.
- Tested: Chrome's cursor-shape names (`default`, `text` over the omnibox), and XFCE's arrow image through rootful Xwayland.

## Frame tap (`--frame-tap`)

A way for a process on the node to look at the screen without a viewer connected: a health check, a thumbnail, an agent.

- **Off unless asked for.** `--frame-tap <socket>` serves the latest picture on a Unix socket (mode 0600, the streamer's user). `--frame-tap-size` (default `640x360`, at most `1920x1080`) and `--frame-tap-fps` (default 15) set the picture and the most it takes a second.
- **Idle until a reader asks.** Nothing is captured until the first request, and capturing stops 3 s after the last. With no viewer the compositor draws only when a capture is due (about `--frame-tap-fps` times a second); with a viewer it takes the picture from the composites that happen anyway.
- **Protocol** (one request per line):
  - `FRAME [after]` waits up to 300 ms for a frame newer than `after` (the newest at the time of the request if it is left out), then answers `OK <seq> <width> <height> <bytes> <age_ms>` and `bytes` of RGB, top row first. A screen that isn't changing makes no new frames, so the newest comes back with its age. `NONE` means no frame has been captured yet.
  - `INFO` answers one JSON line: the size, rate and newest sequence number.
  - `QUIT`, or closing the socket, ends the connection.
- **How it takes the picture.** On a GPU device the buffer just composited is scaled (linear filter) into a small buffer of its own and only that is read back: the output pool, the CUDA registrations and the encoders are not touched. The scale and readback are queued on one tick and mapped on a later one, so the compositor thread never waits on the GPU. On the CPU device the picture is averaged down from the readback it already has.
- **Cost** (RTX 4090 with a game running on the same GPU, release build, 640x360 at 15 Hz, a reader asking all the time): the capture takes the compositor thread about 0.4 ms at p50 and 1.4 ms at p99 (queueing about 0.1 ms, mapping about 0.3 ms). The compositor logs `taps`, `tap_us_p50` and `tap_us_p99` with its other numbers every 10 s. Not yet measured with a viewer streaming at the same time.
- It exposes the screen to whatever can open the socket: keep the path somewhere only that user can reach.
- **Driving a gamepad** (`--frame-tap-pad <0-3>`, its own opt-in, and it needs `--frame-tap` and gamepads, i.e. `--input-dir`). `PAD <json>` sets the state of that virtual pad, the way a session's pad message does: `PAD {"b":[<buttons 0..1>],"a":[lx,ly,rx,ry],"ty":"xbox","hold_ms":250}` answers `OK`, or `ERR <why>`. The index is the streamer's, whatever the message says. The state stands for `hold_ms` (default 250, at most 2000) and then the pad goes back to rest: when a newer `PAD` arrives the hold starts again, `PADOFF` lets go at once, and so does the connection that sent it closing. One writer at a time: a browser session with the controls sends the same pad indices.

## Setup status (P2.1)

What an app's long first-run setup is doing, for the pages to show while the picture is still black (Steam's first launch downloads ~500 MB). The contract is in [`images/README.md`](../../images/README.md): the app replaces `/run/cha/status`, one JSON object, atomically.

- **Reading.** A plain thread (`src/status.rs`) reads the file every 250 ms: an open and a few dozen bytes, with no inotify binding to maintain. `label` is required (the rest is optional: `done`, `total`, `unit`); `done` is held to `total`, long text is cut, and a missing file, `{}` or no label clears the status. A file that doesn't parse or is over 4 KiB changes nothing, so an app that doesn't write atomically costs at most a moment's old status. It is published on a `watch` channel, like the clipboard.
- **Sending.** Every session, WebRTC and WebTransport alike, sends `{"t":"status","label","done","total","unit"}` when its control channel opens (if there is a status) and on each change, and `{"t":"status"}` with no label once it is cleared. All viewers get it, not only the controller. The player's `onStatus` and the portal's progress notice take it from there.

## Performance overlay

An app with a MangoHud overlay (Steam, drawn by mangoapp inside gamescope, so it is in the picture) lets the page change its level live. The streamer owns no overlay itself: it edits the app's config, `/run/cha/mangohud.conf` in the shared runtime dir (`src/overlay.rs`; the Steam image points `MANGOHUD_CONFIGFILE` at it). mangoapp re-reads the file when it is replaced, within about 100 ms.

- **The file says whether there is an overlay.** Apps that don't write it have none, and the streamer never creates it.
- **Formats.** `no_display` is level 0 (off). `mangoapp_steam` then `preset=N`, N in 1 to 4, is level N: FPS only, horizontal bar, extended, full (MangoHud's presets, which a user may redefine in `~/.config/MangoHud/presets.conf`). Any other content, or a file over 4 KiB, is the user's own config and is reported as `"custom"`.
- **Setting.** `{"t":"overlay","level":0..4}`, any transport, only from the session with the controls. The streamer writes a temp file beside the config and renames it over it (mode 0644, owned by the app's uid when there is one). It answers `{"t":"overlay","level":N}`, or `{"t":"overlay","level":N,"error":"…"}` with the level still set; with no overlay the answer is only `{"t":"overlay","error":"this app has no performance overlay"}`. Levels outside 0 to 4 are refused.
- **Reporting.** The `hello`'s `stream` and every `stats` message (about every 500 ms) carry `overlay`: a number 0 to 4 or `"custom"`. The stats line leaves it out when there is no file; the hello has `null`. The file is read on each report (a few bytes, no inotify), so every viewer sees a change, whoever made it, within about 500 ms.

## Gamepads (P1.6)

- The page sends each pad's Gamepad API state (standard mapping) when it changes. The streamer turns each pad into a **virtual Xbox 360 controller** through `/dev/uinput` (045e:028e, the xpad driver's ranges), the pad SDL, Steam and Wine know best.
- The kernel creates the devices on the host, so the app gets them through two volumes the streamer fills (`--input-dir`): the `eventN`/`jsN` nodes (the app's `/dev/input`) and udev database entries marking them joysticks (the app's `/run/udev`; Chrome, Firefox and Wine find pads through udev). The app's device cgroup allows input devices; these are the only ones in its `/dev/input`.
- Hotplug events don't reach the app's network namespace, so `--gamepads` (default 1) are made at start and kept. SDL is told to skip udev (`SDL_JOYSTICK_DISABLE_UDEV=1` in the base image) and watches `/dev/input`, so it sees later pads too.
- **Extra pad state.** The `pad` message may also carry `ty` (the family the page recognized, not `t`, which is the control envelope's type: `xbox`, `playstation`, `switch`, `steam`, `generic`), `gyro` (rad/s), `accel` (m/s²), `touch` (`[{id,x,y,down}]`, 0..1) and `bat` (0..1). They are optional and kept on `PadState`; the Xbox 360 pad has no use for them, and a malformed one is ignored rather than costing the buttons. The DualSense and Steam Controller devices use them; so do the extra buttons 17..23 (touchpad click or right trackpad click, left trackpad click, back paddles, L5/R5, mute), which are optional like the rest.
- **Pad kinds.** `--pad-kind xbox360|dualsense|steam` (default `xbox360`) chooses what every pad of the environment is. `dualsense` and `steam` are made through `--uhid` (default `/dev/uhid`; empty for none) and need the host's `uhid` module and, for the DualSense, the kernel's `hid-playstation` (it loads on demand; the Steam Controller needs only `hid-generic`). With no usable uhid they fall back to `xbox360` with a warning. `GET /info` has `pad_kind` (what it ended up as).
- **uhid devices** (`src/uhid.rs`): `UHID_CREATE2`, `UHID_INPUT2` and `UHID_DESTROY`, and a thread per device answering the kernel (`GET_REPORT`, `SET_REPORT`, `OUTPUT`; `START`, `STOP`, `OPEN` and `CLOSE` are only logged). Input reports go out when a state arrives and every 8 ms (DualSense) or about 4 ms (the 2026 Steam Controller; the original 9 ms) in between, because clients wait for the first report and real controllers don't stop reporting. Once the driver has bound a device, its nodes are found in sysfs by the `phys` it was created with (`cha/pad<i>/<tag>`) and shared like the Xbox pads' (`eventN`/`jsN` in `<input-dir>/dev`, udev entries in `<input-dir>/udev/data` marking each as a joystick, touchpad or accelerometer), and each `hidraw` node is made in `<input-dir>/dev/hidraw/<name>`, owned by the app's uid, with its own udev entry. They are listed in `GET /info` as `"hidraw": [{"name","major","minor"}]` (only the pads made so far; a pad made later than start isn't in the node's mounts).
- **DualSense** (`src/dualsense.rs`, wired USB `054c:0ce6`): the real USB report descriptor ([nondebug/dualsense](https://github.com/nondebug/dualsense), as inputtino checked it), input report 0x01 from the state (sticks, triggers, hat and buttons including touchpad click, mute and PS, gyroscope and accelerometer with the 0.33 us timestamp, two touch points, the battery), and the feature reports `hid-playstation` and SDL read: calibration 0x05 (ours: gyroscope 1/16 degree/s and accelerometer 1/8192 g per unit, no bias, so the page's rad/s and m/s² map 1:1), pairing info 0x09 (a fake MAC, `02:ca:fe:` and three bytes of the run's) and firmware info 0x20. Other feature reports the descriptor declares answer zeros. Output report 0x02 becomes `rumble` (motors 0..255; `ms` 60000 until the stop, both firmware generations' flags), `led` (lightbar), `players` (LED mask) and `trigger` (the 11-byte effect block per side); a repeat of what was last sent isn't sent again. The Bluetooth report 0x31 isn't handled. The host kernel's own writes at probe (blue lightbar, player LED) happen before any session exists and aren't replayed to one that connects later.
- **Steam Controller** (`src/steam_controller.rs`): the 2026 one over Bluetooth, `28de:1303` (SDL's "Triton", the kernel's "Ibex"; bus `BUS_BLUETOOTH`), made by `steam_controller::VARIANT`. Its wired id (`1302`) would meet what the original wired one did: SDL and Steam take only a controller's USB interface 2 and `hidapi` finds none for a uhid device. One vendor collection (ours: a real one's was not at hand), plain numbered reports: input `0x45` (SDL's `ID_TRITON_CONTROLLER_STATE_BLE`; 45 bytes: sequence, 32-bit buttons, the triggers, both sticks, both pads' x, y and pressure, the IMU's microsecond clock, accelerometer, gyroscope), `0x43` (battery, 14 bytes: the charge state, the level and the voltages) and `0x79` (wireless status, "connected"), the last two a second apart; output `0x80` (rumble: each motor's 16-bit speed, resent by SDL every 40 ms; only a change becomes a `rumble` event) and `0x81` (a trackpad pulse, `haptic`; side 1 left, 0 right, 2 both as the kernel's `steam_haptic_pulse` sends it); feature report 1 (63 bytes after the id), the command messages of the original (SDL's lizard-mode and IMU settings, 0x83 attributes, 0xAE string attributes). Layouts and masks after SDL's `SDL_hidapi_steam_triton.c` and `controller_structs.h` (zlib) and the kernel's `hid-steam.c` (GPL-2.0, read only); the page's `steam-triton.ts` reads the same bytes. The d-pad, the paddles (L4, R4, L5, R5), the quick-access button and the pad clicks are real buttons (extras 17..23 and 12..15 of the page's state), and the right stick is a stick. The serial is thirteen characters, `FXA` and ten hex digits of the run's number (Steam's support pages: 13 characters beginning with `FXA`); the unit serial (attribute 1) and the board's (0) are the same. Every kind of feature command Steam sends is logged once at `info` (`steam controller: first command of its kind`, with its body and whether we handle it), the rest at `debug`. The host kernel (6.17) has no `hid-steam` entry for `1303` (`HID_BLUETOOTH_DEVICE` in newer ones): `hid-generic` binds it, with a `hidraw` node and no input devices. The `uniq` is a Bluetooth address (`02:ca:fe:…`), and the udev entries say `ID_BUS=bluetooth`, `ID_MODEL_ID=1303`. Guesses: the descriptor, the pad pressure, the battery's voltage, the stick-touch bits, the attributes' values and the serial's digits.
- **The original Steam Controller** (behind the same `Variant`, Bluetooth `28de:1106` or wired `28de:1102`): a Bluetooth one has no interfaces, and SDL takes it by its bus; `0x1105` and `0x1106` are the same to SDL. One vendor collection, report id 3, 19-byte input and feature reports, which carry 18-byte segments led by a header byte (`0x80`, `0x40` on the last, the segment's number in the low bits; SDL's `SDL_hidapi_steam.c`, `SteamControllerPacketAssembler`). Input is the Bluetooth state message (SDL's `UpdateBLESteamControllerState`: first nibble 4, a flag word, then the buttons per SDL's masks, the two trigger bytes, the stick, the left pad, the right pad, the accelerometer and the gyroscope; 31 bytes, two segments; no quaternion). Feature commands come in segments and the reply is read back a segment per read, up to the one flagged last. Handled are get attributes 0x83, string attribute 0xAE (a 20-character field, zero padded; the serial is `F` and nine hex digits), set settings 0x87 (kept) and get settings 0x89, load defaults 0x8E (clears them), get digital mappings 0x82 (none) and haptic pulse 0x8F (`haptic`); every other command is acknowledged and does nothing. The stick and the left trackpad have fields of their own; the right stick, when deflected, is a finger on the right pad. It has no d-pad (Steam's default layout makes the left trackpad one), which the page's d-pad buttons don't reach: they set the quadrant bits only. Steam opened it and Steam Input's output reached a game (Cyberpunk 2077 under Proton), 2026-10-05.
- **hid-steam and hidraw** (the wired variant only). While something has the `hidraw` node open (Steam), the kernel's `hid-steam` unregisters its own input devices, and registers new ones (new `eventN`) when it is closed; the evdev nodes shared with the app are those of the first.
- **Pad messages.** Besides `rumble`, sessions forward `{"t":"haptic","i","side","amp","on_us","off_us","count"}`, `{"t":"led","i","r","g","b"}`, `{"t":"players","i","mask"}` and `{"t":"trigger","i","side","effect":[11 bytes]}` to the controller only, on both paths, under the same ~60 per second per pad and kind limit (a rumble stop or a haptic count of 0 is never held back).
- **Rumble.** The Xbox pads advertise force feedback (`FF_RUMBLE`, `FF_PERIODIC` with sine, square and triangle, `FF_GAIN`; 16 effects, as the xpad driver's memless layer does). A thread per pad answers the kernel's uinput requests: `UI_FF_UPLOAD` and `UI_FF_ERASE` (the app's ioctl waits for the answer), and turns the app's play/stop events into the motors' drive (the playing effects added up, scaled by the gain; periodic effects drive both motors from their magnitude). Sessions forward it as `{"t":"rumble","i","lo","hi","ms"}` (`lo` the strong motor, `hi` the weak one, 0..1; `ms` 0 stops) to the controller only, both on WebRTC and WebTransport, at most ~60 per second per pad (the newest wins; a stop is never held back). An effect with no length plays until stopped and is announced for 60 s.

### Steam's virtual Xbox pad (`--pad-kind steam`)

- Steam hands games its own Xbox pad (`28de:11ff`), which it makes through `/dev/uinput`; the app has none (with it, a game could make keyboards and mice on the node's kernel). In the Steam image an `LD_PRELOAD` shim (`images/steam/uinput-shim/`, C, i386 and amd64) catches libc's `open` of `/dev/uinput` and `/dev/input/uinput` and returns a `SOCK_SEQPACKET` connection to `<runtime dir>/uinput.sock` (mode 0660, the app's uid); with no streamer behind it the open fails with `ENOENT` as before. The shim turns every ioctl, write and read on that descriptor, in either uinput API (`UI_DEV_SETUP`/`UI_ABS_SETUP`, or the legacy write of a `uinput_user_dev`), into the ABI-neutral packets of `src/uinput_proto.rs` (the byte layouts are documented there and in the shim's `proto.h`) and logs each call to `/run/cha/uinput-shim.log` (appended, 1 MiB).
- The broker (`src/uinput_broker.rs`, started for the `steam` kind only) validates each request (`src/uinput_policy.rs`; unit-tested without a kernel) and makes the device with the streamer's own `/dev/uinput`. The socket is reachable by every process of the app, games included, so the validator is the boundary. Allowed: event types SYN, KEY, ABS and FF; keys `BTN_SOUTH`..`BTN_THUMBR` and the four `BTN_DPAD_*`; axes `ABS_X`..`ABS_RZ` and `ABS_HAT0X/Y`, each within ±65535; `FF_RUMBLE`, `FF_PERIODIC` with its waveforms, `FF_GAIN`; at most 16 effects, 64 events per write and 20 000 events per second (burst 2048). The identity is ours: `28de:11ff`, bus USB, phys `cha/pad<4+N>` (past our own pads, so the host's udev rule matches), the client's version and its name as printable ASCII (at most 79 characters; the default is "Steam Virtual Gamepad"). One connection is one device, at most 4 at once. A refusal is `EINVAL` to the client and a warning in the streamer's log, once per kind of refusal (later ones at debug).
- The device's nodes and a udev entry (`ID_INPUT_JOYSTICK=1`, `ID_VENDOR_ID=28de`, `ID_MODEL_ID=11ff`) go into the app's volumes like the Xbox pads'; `UI_GET_SYSNAME` answers the real kernel name. The connection ending (Steam exits or crashes) or `UI_DEV_DESTROY` removes the nodes and entries and destroys the device (Steam makes and destroys one per game launch).
- **Force feedback** goes through the client: the kernel's `UI_FF_UPLOAD`/`UI_FF_ERASE` requests (an app's `EVIOCSFF`/`EVIOCRMFF` waits for them) are relayed as packets, the shim hands them to Steam as the `EV_UINPUT` events a real uinput gives and takes its `UI_BEGIN_FF_*`/`UI_END_FF_*` calls; the answer completes the kernel's request, or `EIO` if none comes within 2 s. Plays, stops and the gain are relayed as events. These pads' rumble never reaches the page from here: Steam drives the Steam Controller's motors itself, over `hidraw`.
- **Tests.** `cargo test -p cha-streamer uinput` (policy, wire codec; with a usable `/dev/uinput`, also real devices through a test client, and the force feedback round trip when the test can open the evdev node) and `images/steam/uinput-shim/run-tests.sh` (builds the shim and its test programs for i386 and amd64, and runs them through the shim against the real broker). Status: tested against the node's kernel, not yet with Steam.

## GameStream media (ADR 0009, G2)

A Moonlight client's session can be served by the streamer: the node's GameStream host (in the agent, G3) does pairing, the app list and RTSP, and gives each launched session to the environment's streamer, which serves its control (ENet), video and audio streams. It is behind the **`gamestream` cargo feature**, off by default (the streamer image builds with it: `cargo build -p cha-streamer --features gamestream`). Everything is in `src/gamestream/`; `signal.rs` has only the optional arguments and one block that starts the module and merges its routes. Without the feature nothing of it is compiled, and the browser transports never see it.

- **Switching it on.** `--gamestream-ports <video>,<control>,<audio>` (three distinct UDP ports, bound on every address when a session starts) and the secret of the local API in the environment variable **`CHA_GAMESTREAM_SECRET`** (at least 16 characters; never an argument, and removed from the environment at start so the apps this streamer starts don't inherit it). Ports without a secret is a startup error. The agent passes both when `CHA_GAMESTREAM` is on.
- **The local API** (on the signalling listener, which the agent keeps on `127.0.0.1`), every request with `Authorization: Bearer <secret>` (wrong or missing: 401):
  - `POST /gamestream/session` with a `cha_gamestream::SessionHandoff` as JSON: binds the ports and starts the session; `200 {}`. 422 when the stream asks for something this environment doesn't make (a codec it doesn't offer, surround sound, HDR), 500 for a port that can't be bound.
  - `DELETE /gamestream/session/{id}`: stops it and frees the ports (404 for another id).
  - `GET /gamestream/status`: `{"running": bool, "session_id": 7}`.
  - One session per environment. A second `POST` for the **same client** (a resume) stops the first and starts the new one; one for another client is a 409. A session whose client left or timed out ends by itself, and the environment is free again.
- **What a session does to the environment.** It is one more viewer: it joins as an owner, so it takes the floor (browser viewers keep watching but lose the controls, and the usual floor rules apply to it), and its input only counts while it holds the floor. It sets the environment's size to the client's (the size applied, after `fit_size`'s limits, is logged) and the frame rate to the nearest of 60, 90 and 120. The encoder is shared (ADR 0009), so those changes, and the bitrate the client asks for (the session's pace, which caps others on the same codec), are everyone's. H.264, HEVC and AV1 are offered; HDR and 4:4:4 are not. On the end, the keys and pads it held are released.
- **Video** is the encoder's frames as they are (Annex-B or OBUs); a keyframe request or a reference invalidation from the client goes to the encoder (`request_keyframe`, `request_invalidate`), the frame's `captured` time is its compositing time.
- **Audio** is the mixer's Opus. Moonlight asks for 5 ms or 10 ms packets: 10 ms are the mixer's own frames; **5 ms** come from a second Opus encoder the mixer makes when the first such listener arrives (`Audio::subscribe_frames(5)`: each 10 ms tick is encoded as two 240-sample frames; nobody else's frames change, and with no 5 ms listener the encoder isn't run). Any other length is refused. Only stereo is made: a client that asks for 5.1 or 7.1 is refused with a message saying so (G4).
- **Input.** Windows virtual keys become evdev codes (`src/gamestream/input.rs`, US layout: letters, digits, F1 to F24, navigation, the numpad, left and right modifiers, punctuation, media keys); the numpad's Enter reaches the host as the main Enter (the key code on the wire has no extended flag). The absolute mouse is scaled from the client's view to the output's pixels, relative motion passes through, and a wheel notch (120) is 100 px with Moonlight's "up" being a negative y. Pads (up to four) become the W3C standard mapping the pads take (XInput's button flags, triggers 0..1, sticks -1..1 with Y down), and a pad that leaves the client's mask goes back to rest. Touch, pen, typed text and a pad's touchpad, motion and battery are ignored, noted once in the log.
- **Feedback.** What apps do to the pads goes to the client: rumble (a rumble the app gave a length is stopped by a timer, since Moonlight rumbles until told), the lightbar, adaptive trigger effects; at most about 60 messages a second per pad and kind, a stop never waits, and only while the client has the floor. Players' LEDs and Steam haptics have no Moonlight message.

## Running it (node, dev loop)

The dev container builds and runs the streamer from the synced repository. The GPU comes from CDI, and the container uses host networking.

```bash
docker compose -f deploy/streamer/compose.dev.yaml up -d --build
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev cargo build --release -p cha-streamer
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev /target/release/cha-streamer --listen 0.0.0.0 --run 'google-chrome-stable --no-sandbox --ozone-platform=wayland --no-first-run --force-device-scale-factor=1 --user-data-dir=/tmp/chrome-profile --kiosk file:///src/spikes/s2-compositor/bench/page/live.html'
```

- Chrome runs with `--no-sandbox` only because the dev container runs as root. The environment images in P1.4 run apps unprivileged.
- Signalling is on TCP 7660 and WebRTC on UDP 7661.
- On its own like this, starting a stream needs the token printed at startup, kept in `/state/token`. Measure with the S1c/S1d page, as for S2: host `gpu-node.lan`, port 7660, the token, the WebRTC present path. The streams are `live-hevc`, `live-h264` and `live-av1`.

## Run by the agent (P1.5)

The agent starts one streamer container per environment, with signalling on `127.0.0.1` only: `--listen 127.0.0.1 --portal-key <key> --environment-id <id>`.

- **Brokering.** A browser's offer reaches the streamer through the portal and the agent. It carries a **media token**: Ed25519, signed by the portal, valid for 60 s, for this environment only. The streamer checks it offline against the portal's key.
- **Reconnects.** Viewers coexist (Viewers, P2.6); a new connection doesn't stop an old one, which leaves when its browser closes or ICE disconnects. A reconnect gets a fresh keyframe, and the environment keeps running in between.
- **Interactive sessions** have no time limit (`secs=0`). They end when the browser leaves.
- **Keyboard focus.** A new window gets the keyboard once it first shows a buffer. Chrome ignores a keyboard `enter` for a surface it hasn't drawn, and focusing the same surface again later sends nothing.

## Results (RTX 4090, Chrome 154 in the compositor, 2026-10-03/04)

**Browser side** (Chrome 154 on the M4 Pro, 1 GbE, the S1c/S1d page's WebRTC path with its click probe, 2026-10-04; raw data: [`docs/benchmarks/p13-2026-10-04-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/p13-2026-10-04-m4pro-chrome154-gpu-node.json)). All times are ms, p50 / p95; S2 M2 for comparison:

| Stage | HEVC | H.264 | AV1 | S2 M2 (HEVC / H.264) |
|---|---|---|---|---|
| Composited → encoded (node, p50 / p99) | 1.77 / 4.49 | 2.01 / 5.25 | 2.08 / 2.54 | 1.85 / 1.89 |
| Sent → browser compositor | 3.86 / 7.31 | 4.47 / 8.13 | 6.10 / 10.2 | 4.15 / 3.84 |
| **Node compositor → Mac compositor** (sum of p50s) | **≈ 5.65** | ≈ 6.5 | ≈ 8.2 | ≈ 6.0 / ≈ 5.8 |
| Browser decode only | 1.90 / 2.20 | 2.50 / 3.00 | 3.80 / 4.60 | |
| **Click → browser compositor** | **24.6** / 40.4 | 27.4 / 44.1 | 24.2 / 40.6 | 34–35 (240/60) |
| … of which Chrome reacting in our compositor | 17.9 / 33.9 | 20.3 / 37.0 | 19.4 / 36.4 | 29 |
| Frames lost / dropped | 0 / 0 | 0 / 0 | 1 lost (NACK'd, 1 PLI) | |

- **About 6 ms from the node's screen to the Mac's, and ~25 ms from click to screen.** Click → screen is 10 ms faster than S2 at the same 240 Hz / 60 fps. Chrome draws to presentation feedback sent every compositor tick, and each composite takes the newest buffer it committed.
- **HEVC is the fastest on this client.** H.264 decodes ~0.6 ms slower in Chrome on the M4, and AV1 ~1.9 ms slower.

**Node side, A/B against S2's GStreamer streamer (`gst-wayland-display` → `nvh265enc`).**
- Both ran back to back with Chrome at 240 fps on S2's live page, at 2560×1440, 60 fps, 40 Mbit/s.
- The GPU was shared with ComfyUI and llama.cpp jobs, at up to 100% utilization and 346 W. That load sets the tail latency, so only back-to-back runs compare.
- Raw data: [`docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json`](../../docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json).

| HEVC, compositor → encoded (ms) | p50 | p99 |
|---|---|---|
| S2 (GStreamer), S2 M1 on a quiet GPU | 1.72 | 1.83 |
| **cha-streamer**, GPU busy | **1.75** | 4.25 |
| S2 (GStreamer), GPU busy (straight after) | 1.78 | 4.32 |

**Where the time goes** (cha-streamer, HEVC, µs, p50 / p99), from the encoder thread's breakdown:

| Stage | p50 | p99 |
|---|---|---|
| composited → picked up | 16 | 686 |
| CUDA view of the buffer | 4 | 9 |
| NVENC map | 9 | 19 |
| submit | 71 | 165 |
| **wait for NVENC** | **1598** | **3915** |

**Other numbers:**
- **Composite:** 0.3–0.4 ms of GPU time at p50, ~2.3 ms at p99 under the shared load.
- **New viewer:** first frame 50–67 ms with a cold encoder (NVENC session init 31–59 ms), **10–11 ms** with a warm one. S2 kept every encoder always on: 14–17 ms.
- **All three codecs:** H.264, HEVC and AV1 all reach the browser over WebRTC; S2 had no AV1.
- **Resize:** 2560×1440 → 1920×1080 → 1280×720 → 2560×1440 mid-stream reallocates the pool and sends a keyframe each time. 897 of 902 frames went out, so each resize skipped about one frame.
- **Under heavy load** (another process at ~350 W), the NVENC wait grows by ~2 ms for every codec.

**Where the tail comes from.**
- With RGB input, NVENC converts to YUV with a CUDA kernel. That kernel waits behind other processes' CUDA work on a busy GPU, and S2 has the same exposure.
- Doing the conversion ourselves removes that wait. Probed on this stack:
  - GL can render R8, but CUDA's EGL interop won't import an R8 image;
  - NV12 can't be a GL render target;
  - GBM ignores a linear request and returns an implicit layout that GL can't bind.

  So the path is a Vulkan compute pass that writes NV12 into a buffer CUDA imports (external memory), and NVENC reads it as `CUDADEVICEPTR` NV12 (`cha-nvenc` already takes `InputFormat::Nv12`). PyroWave needs the same Vulkan import of the composite (plan §3.3), so both land together.
