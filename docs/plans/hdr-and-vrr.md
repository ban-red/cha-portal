# HDR and variable refresh: the implementation plan

How to build [research note 12](../research/12-hdr-and-vrr/README.md)'s ranked list, step by step. Written 2026-11-09 from the code at `ea5b445`; `file:line` references are as of then, and where the code changes, the code wins. Each step says what changes, where, how it is tested, and the gate it has to pass before the next step builds on it.

Where a step can be checked:
- **here**: any Linux machine, with `cargo test`, `bun test` and the Python tests (`scripts/check.sh`). No GPU.
- **node**: the RTX 4090 node, or the Radeon 780M / UHD 630 for VA-API, in the streamer's dev container.
- **Mac**: an M4 or M4 Pro MacBook Pro, for Cha Player (its Metal code builds only on macOS) and for Chrome and Safari on the XDR panel.
- **rig**: the glass-to-glass rig (C2.4), which doesn't exist yet.

## Where it stands (2026-11-09)

Built, with their tests passing here; none yet run on the node or a Mac:

| Step | What is built | Still to do |
|---|---|---|
| 1.1 | The node's wait (`commit_to_composite_ms_*`, `commit_to_encoded_ms_*` in `stats`; *Wait on node* in both players' panels and the copy report; `commit_wait_us_*` in the compositor's log) | Read it in a Steam session and a Chrome session on the node (the gate); Cha Player's part was written without a Mac |
| 1.2 | The browser's *Colour* and *Display* rows (WebCodecs' `colorSpace`, PyroWave's header, `dynamic-range`, the refresh measured from rAF), in the report and recordings | Cha Player's side (`CVPixelBuffer` format, `NSScreen` EDR values); 1.3's Perfetto recipe and the rig |
| 2.1 | One table of colour tags (`encoder/color.rs`) for every encoder, today's values by default, `CHA_SDR_TRANSFER` and `CHA_SDR_MATRIX` to try others | 2.2's patch pattern and `scripts/color-check.py`, then the measurements of 2.2–2.4 |
| 3.1, 3.2 | `compositor/schedule.rs` and `--pacing auto\|fixed\|commit`: gamescope's window composited on its commits with a variable refresh in its feedback; desktops on the encode tick as before; a keep-alive for the GameStream host | The gate on the node; `hello.stream.pacing` and `/info` (the log says the pacing today) |
| 3.3 | `--adaptive-sync` on Steam's gamescope, with a test of its command line | `-r` at the session's ceiling with live rate changes (3.5), gamescope from upstream 3.17.0, `xprop GAMESCOPE_VRR_FEEDBACK` on the node |
| 3.4 | `fixed_frame_rate_flag = 0` in the H.264 SPS | — |

Next: step 4 (the refresh rates offered per client), then 6.1 and 6.3's header writers, which can also be built and tested here.

Abbreviations: COMP = `crates/cha-streamer/src/compositor/mod.rs`, HAND = `crates/cha-streamer/src/compositor/handlers.rs`, OUT = `crates/cha-streamer/src/compositor/output.rs`, MEDIA = `crates/cha-streamer/src/media.rs`, CTRL = `crates/cha-streamer/src/control.rs`, WT = `crates/cha-streamer/src/wt.rs`, NV = `crates/cha-nvenc/src/nvenc.rs`, BS = `crates/cha-streamer/src/encoder/bitstream.rs`, PT = `web/packages/player/src/player.ts`, PYRO = `web/packages/player/src/pyro.ts`, SPEC = `web/packages/ui-spec/stats-panel.json`.

## 0. Decisions this plan rests on

1. **Steam sessions and desktop sessions pace differently, and the streamer decides which.** A session whose toplevel is gamescope (xdg `app_id` `gamescope`) gets commit-driven compositing and a zero refresh; every other client keeps the 240 Hz `Fixed` feedback that Chrome paces itself from (note 12, "Composite on gamescope's commit"). `--pacing auto|fixed|commit` on the streamer, default `auto`, so no wire or catalog change is needed and `fixed` brings back today's behaviour.
2. **Bit depth is part of the codec name.** Ten-bit streams are offered as `hevc-10` and `av1-10` (and later `pyrowave420-10`, `pyrowave444-10`) beside today's names. The streamer runs one encoder thread per codec name already (MEDIA `subscribe`, the per-codec mailbox), so 8- and 10-bit viewers of one environment get separate encoders, codec switching in place (`{"t":"codec"}`) works unchanged, and an older page never asks for a name it doesn't know.
3. **HDR is a mode of the environment, not of a viewer.** One compositor output feeds every encoder, so whether it is composed in HDR is decided once: when the content is HDR and at least one viewer's display is (note 12, "SDR content and SDR clients"). In HDR mode the 10-bit codecs carry BT.2020/PQ; an SDR viewer gets an encode tone-mapped to SDR from the same composite (step 10.3).
4. **One colour description travels with every stream.** The streamer's `hello` gains `stream.color` (`{"depth":8|10,"primaries":"bt709"|"bt2020","transfer":"srgb"|"bt709"|"pq","matrix":"bt709"|"bt2020nc","range":"limited"|"full"}`) and, in HDR, a `{"t":"hdr","mdcv":{…},"cll":{…}}` line on the control stream whenever the metadata changes. Clients use these, not decoder attachments, as moonlight-qt does (note 12, "HDR10 with static metadata"). A missing `color` means today's stream.
5. **Clients tell the node what their display is.** A `{"t":"display","hdr":bool,"peakNits":n,"sdrWhiteNits":n,"headroom":x,"refreshHz":n,"vrr":bool}` line after `hello`, and again when the window moves or the headroom changes. The streamer ignores what it doesn't use; an older streamer ignores the line.
6. **Measure before building past each gate.** Every later step's gate is a number nobody has published (note 12, the gaps table). Steps 1 and 2 come first because they also check the SDR streams Cha ships today.

## 1. Record each frame's timing and colour (rank 1)

### 1.1 How long an app's frame waits on the node — *here, then node*

Today the streamer measures composited → encoded → sent, but not how long a picture had been drawn before the compositor took it, which is the wait note 12 puts at about 16.7 ms on average at 60 fps in Steam sessions.

- **HAND `commit`**: when a toplevel's root surface commits a new buffer, record `Instant::now()` on the compositor state as `last_commit` (only commits with a buffer, not pure state commits; the cursor surface and sync subsurfaces excluded, as `dirty` already distinguishes them).
- **COMP `composite`**: carry the newest `last_commit` taken since the previous composite into the published `Frame` as `committed: Option<Instant>` (MEDIA `Frame`), `None` when the composite was forced with nothing new (keyframe re-encodes, the frame tap).
- **MEDIA `EncodedFrame`** gains `committed: Option<Instant>`, copied by each encoder thread.
- **Stats**: CTRL `StreamerStats` gains `commit_to_composite_ms_p50/p99` and `commit_to_encoded_ms_p50/p99` over the report window, computed where `composite_to_encoded` is (WT and the WebRTC session). The compositor's own once-a-minute log line (OUT `Stats::maybe_log`) gets the same percentiles.
- **Panel**: a `Waits` row under Latency in SPEC (`"commit_to_encoded_ms"`, "How long the app's newest picture waited on the node before it was encoded"), shown when the value is present; both players read it from the `stats` message; shared cases in `stats-panel-cases.json`.
- **Tests (here)**: a percentile helper over `Option<Instant>` pairs; serde of the new fields skipped when absent (older pages unaffected); the ui-spec cases.
- **Gate (node)**: a Steam session at 60 fps shows the commit → composite wait the arithmetic predicts (0–16.7 ms, a fixed phase per session); a Chrome session shows it at or under one encode interval.

### 1.2 What colour each frame arrived in — *here, Mac*

- **Browser**: PT records each decoded `VideoFrame.colorSpace` (WebCodecs) or the PyroWave sequence header's `ColorInfo` (PYRO already formats it at line 133), and the stream's `hello.stream.color` once step 6 sends it. A `Colour` row in SPEC: `"{depth}-bit {transfer}"`, with a warning when the decoder's colour disagrees with the stream's.
- **Display**: `matchMedia('(dynamic-range: high)')`, the measured refresh (median `requestAnimationFrame` interval over 2 s at start), and Safari's 60 fps preference when rAF stays at 60 on a display that reports 120. A `Display` row: `"{refresh_hz} Hz, {dynamic_range}"`.
- **Cha Player**: the `CVPixelBuffer`'s format (8 or 10 bits), `kCVImageBufferTransferFunctionKey` and the screen's `maximumExtendedDynamicRangeColorComponentValue` / `maximumPotential…` and refresh interval, in the same rows. Mac-only code; the row formatting lives in `crates/cha-player/src/ui/stats/` and is tested here.
- **Recordings**: both rows go into the browser's recordings (`recording.ts`) and the native copy report.

### 1.3 Chrome's frame interval and the glass-to-glass rig — *Mac, rig*

- A short recipe in [`docs/benchmarks/README.md`](../benchmarks/README.md): a Perfetto trace with the `viz` and `gpu` categories, reading `ExternalBeginFrameSourceMac::SetPreferredInterval` and its `vsync_subsampling_factor`, for a stream at 60, 90 and 120 fps with the page idle.
- The rig itself is C2.4's: frame IDs drawn by the test pattern, a photodiode or high-speed camera. It is hardware and stays in the backlog; steps 3 and 5 need it for their latency gates, steps 2, 4 and 6 do not.

## 2. Settle the SDR colour tags the encoders send today (rank 2)

### 2.1 One table of colour tags — *here*

Today each encoder writes its own: NV `vui()` and the AV1 colour config say BT.709 primaries and matrix with the sRGB transfer (13), limited range; BS's H.264 VUI, `bitstream/h265.rs`, `bitstream/av1.rs`, `x264.rs` (`--transfer bt709`) and `svtav1.rs` say transfer 1 (BT.709).

- **`crates/cha-streamer/src/encoder/color.rs`** (new): `ColorTags { primaries, transfer, matrix, full_range }` with the H.273 code points, `ColorTags::SDR_NVENC` and `ColorTags::SDR` as today's values, and `ColorTags::from_env()` reading `CHA_SDR_TRANSFER=srgb|bt709` and `CHA_SDR_MATRIX=bt709|bt601`, so a node owner can try each on a client without a rebuild (the streamer's other `CHA_*` switches work the same way).
- Every writer takes its tags from there: BS (H.264 VUI), `bitstream/h265.rs` (VUI), `bitstream/av1.rs` (`color_config`), `x264.rs` (the `colorprim`/`transfer`/`colormatrix` option strings), `svtav1.rs` (`color-primaries`, `transfer-characteristics`, `matrix-coefficients`), and `cha-nvenc` through a new `Config::color` field instead of the constants in NV `vui()` (`cha-nvenc` keeps no dependency on the streamer: the field is its own small struct).
- **Tests (here)**: each header writer's existing round-trip tests read the tags back; a test that today's defaults are byte-for-byte what each writer emitted before (so this step changes no stream); the env parsing.

### 2.2 Find out which tags are right — *node, Mac*

- **Patches**: the VA-API self-test (`CHA_ENCODE_TEST`) and an ignored `cha-nvenc` test gain `CHA_ENCODE_TEST_PATTERN=patches`: sRGB red, green, blue, cyan, magenta, yellow, a 0–255 grey ramp and a near-black ramp, as BGRx. With `CHA_ENCODE_TEST_DUMP` the stream is written out; `scripts/color-check.py` (new, stdlib only) decodes it with `ffmpeg -f rawvideo` and prints each patch's decoded RGB under each matrix and transfer, against the source.
- **On Chrome macOS**: the test-pattern image gains the same patches (`crates/cha-testpattern`), streamed through each encoder, compared by eye and with a screenshot's pixel values.
- **Gate**: a fix ships only where a shift is seen. If NVENC's built-in conversion is BT.601, either tag the matrix 6 on NVENC's RGB input (what FFmpeg does) or make step 8.2's own conversion the SDR path too. If the CPU and VA-API encoders show the 11-level midtone shift NVENC had, their transfer tag moves to 13.

### 2.3 VA-API's video processor gets explicit colour properties — *node*

`crates/cha-streamer/src/encoder/vaapi/ffi.rs` sets a `SRGB` surface standard and a `BT709` output standard, which Mesa resolves to unequal transfers, so AMD's in-encoder conversion (EFC) is probably never used (note 12, "VA-API splits by GPU generation").
- Set `VAProcColorStandardExplicit` with `VAProcColorProperties` on both sides: input RGB 1/13/0, output 1/13/1 (or whatever step 2.2 settles), equal transfers, so the 780M can take EFC.
- **Gate (node)**: `CHA_ENCODE_TEST` on the 780M: encode time per frame against today's 1.3 ms at 720p, and the patch check unchanged. Ship only if it is no slower and the colours are no worse.

### 2.4 Cha Player's sRGB layer — *Mac*

wgpu's trunk fixes "oversaturated sRGB surface colors on wide-gamut displays" (#10286). Check with the patch pattern on the MacBook's P3 panel against the same stream in Chrome. If Cha Player is oversaturated, set the `CAMetalLayer`'s colour space to sRGB through the hal now, or wait for the wgpu release that carries the fix (step 7.4 moves to wgpu 30 anyway).

## 3. Steam sessions at the game's cadence (rank 3)

### 3.1 The compositor's scheduler as its own module — *here*

**`crates/cha-streamer/src/compositor/schedule.rs`** (new), pure and clock-injected, so every rule is a unit test:

- `Pacing::{Fixed, Commit}` and `Schedule::new(pacing, cap: Duration)`.
- `Fixed`: today's rule from COMP `tick()` (composite on the encode tick if dirty or forced; realign after a stall), moved here unchanged.
- `Commit`: `on_commit(now)` marks content waiting; `due(now) -> Option<Instant>` says when to composite: at once if at least one cap interval has passed since the last composite, else at `last + cap`; after a composite, `next = max(next, now − cap/4) + cap` (Sunshine's late-frame rule, note 12) so a late frame doesn't push every later one.
- **Keep-alive**: Cha's own players already take silence: a still desktop sends nothing today, and the health check judges freezes only while the source sends at a rate (`web/packages/player/src/health.ts`, `freeze_*` in `ui-spec/health.json`). Stock Moonlight clients are the exception ("many clients expect a constant stream of video frames", Sunshine's reason for re-encoding at half the rate). So `keepalive: Option<Duration>` is off for Cha's transports and on, at half the stream rate, for sessions of the GameStream host; it re-encodes the last frame, the path keyframe requests already use.
- **Repeats**: `on_commit` takes whether the commit carried damage. A commit without damage doesn't count as content (gamescope's idle hold of the Steam UI repeats frames; whether its repeats carry damage is measured in 3.5).
- **Tests (here)**: a game at 60, 45 and 37 fps on a 60 fps cap composites each frame once and never twice within a cap interval; a game above the cap is held to the cap; a late commit composites at once and the next one at `+cap`; an idle session sends nothing, or at the keep-alive rate when one is set; `Fixed` reproduces today's tick decisions for 60, 90 and 120 at a 240 Hz clock.

### 3.2 Commit-driven compositing and the feedback gamescope reads — *here, then node*

- **COMP**: the compositor tick (240 Hz timerfd) stays for frame callbacks and the desktop feedback. In `Commit` mode a second timerfd is armed for `Schedule::due`, and a commit on the driving surface arms it at once; `tick()`'s encode branch moves into `Schedule`.
- **Which surface drives**: the toplevel whose xdg `app_id` is `gamescope` (`new_toplevel` in HAND sees it; gamescope's Wayland backend sets it). `--pacing auto` turns on `Commit` when such a toplevel maps and back to `Fixed` when it unmaps. Other surfaces (popups, the cursor) are composited with whatever the driving surface's commit triggers; a change to them alone composites at the next cap interval, as damage does today.
- **Feedback for the driving surface**: `Refresh::Variable(cap)` (a version-1 binder such as gamescope receives refresh 0; note 12, "Composite on gamescope's commit") and `presented` with the composite's own timestamp, not the 240 Hz tick's. A buffer replaced before any composite took it gets `discarded`. Every other surface keeps `Refresh::Fixed(tick_period)` on the tick, as today. Smithay's `OutputPresentationFeedback` takes the refresh per call, so the two policies are two calls per tick.
- **Frame callbacks** keep going on the 240 Hz tick for everything (gamescope requests none; desktops need them).
- **`--pacing`** in `crates/cha-streamer/src/main.rs`, documented in the streamer README's *How it works*; `GET /info` and the `hello`'s `stream` carry `"pacing":"fixed"|"commit"` for the stats panel.
- **Tests (here)**: the auto-detection on a fake toplevel's app id; that a `Fixed` session's feedback is byte-for-byte today's.
- **Gate (node)**: 1.1's wait falls from about one frame to under a millisecond or two in a Steam session; a Chrome session's numbers don't move.

### 3.3 gamescope with `--adaptive-sync`, `-r` at the ceiling, and 3.17.0 — *here, then node*

- **`images/steam/steam-gaming`**: add `--adaptive-sync`, and pass `-r` the stream's ceiling (`CHA_REFRESH_MAX`, new, which the node sets to the highest rate the session may switch to, 120; `CHA_REFRESH` stays the starting rate for Steam's own frame limit). The Python tests (`images/steam/test_steam_session.py` and siblings) check the command line.
- **`images/steam/Dockerfile`**: build gamescope from the upstream `3.17.0` tag instead of Ubuntu's 3.16.20 source, with `gamescope-wayland-mangoapp.patch` rebased onto it (3.17.0 reworked the vblank schedule so the VRR branch follows a newly ready frame, and carries the HDR fixes step 9 needs). `apt-get build-dep gamescope` still provides the build dependencies; the `+cha` version suffix and `apt-mark hold` stay.
- **`crates/cha-node/src/environments.rs`**: set `CHA_REFRESH_MAX` beside `CHA_REFRESH` (line 437).
- **Gate (node)**: `xprop -root GAMESCOPE_VRR_FEEDBACK` reads 1 under Cha; Steam's FPS limit of 45 gives 45 composites a second, not a 60/30 alternation; mangoapp still draws.

### 3.4 One false flag in the H.264 header — *here*

BS line 361 writes `fixed_frame_rate_flag = 1` with `time_scale` twice the rate. A commit-paced stream doesn't have a fixed rate. Write the flag as 0 (the timing info stays, as the cap's rate); `bitstream/h265.rs` makes no fixed-rate claim and the AV1 sequence header writes no timing info. Test: the SPS round-trip test reads the flag back as 0.

### 3.5 What still needs a spike — *node*

- Whether gamescope's idle-hold repeats carry damage (log commit damage during the Steam UI's idle); if not, a cheap hash of a downscaled frame before encode.
- NVENC bits per frame with irregular input at the cap-sized one-frame VBV: feed irregular intervals to `switches_to_120_fps_in_place_and_keeps_up` in `cha-nvenc`'s tests.
- Changing the stream's rate live for a gamescope app, now that `-r` is only an upper bound: `{"t":"fps"}` writes `GAMESCOPE_FPS_LIMIT` through the X helpers (`steam-touch-mode`'s connection) and the streamer README's "a gamescope app (Steam) can't change its refresh while running" goes.

## 4. A refresh rate per client (rank 4) — *here, Mac*

- **`web/packages/player/src/refresh.ts`** (new): `measureRefresh()` (rAF intervals, as 1.2) and `offeredRates(displayHz, browser)`: 60 and 120 on a 120 Hz display; 90 only when the display runs at a multiple of 90 or reports VRR; Safari at 60 Hz rAF on a 120 Hz display gets a hint to turn off "Prefer Page Rendering Updates near 60fps". Tests (here) with bun.
- **Portal** (`web/apps/portal/src/views/SessionView.vue`'s fps menu): show the offered rates, keep the others under "More" with a note on judder; the default for a game on a 120 Hz Mac becomes 120 where the node and link allow it, else 60.
- **Gate (Mac)**: 1.3's Perfetto trace confirms Chrome's interval for MediaStream video at 60, 90 and 120.

## 5. Native pacing in Cha Player (rank 5, C2.4) — *Mac, rig*

- **`crates/cha-player/src/present/`**: a `Pacer` trait with three implementations, chosen in Settings > Display (kept per install): `Immediate` (today), `DisplayLink` (`CAMetalDisplayLink` with `preferredFrameRateRange` at the stream's rate, newest frame wins, waiting at most half an interval, as moonlight-qt), and `OnArrival` (native full screen with Adaptive-Sync or ProMotion: present as soon as a frame is decoded, holding it at most half a frame to even out jitter).
- The display link's drawable is wrapped with `create_texture_from_hal` and drawn with wgpu, then presented on wgpu's Metal queue; `objc2-quartz-core` 0.3.2 has the bindings.
- The stats get decode-complete and presented times (the drawable's presented handler), a `Pacing` row, and whether the window is native full screen.
- **Tests (here)**: the hold and newest-frame logic as pure functions in a non-gated module.
- **Gate (rig)**: glass-to-glass lower or steadier than `Immediate`, windowed and full screen, as PLAN already requires; the ≤12 ms p50 target is probably reachable only in native full screen.

## 6. Ten-bit SDR end to end (rank 6)

### 6.1 The codec names and the colour description — *here*

- **`crates/cha-streamer/src/codec.rs`**: `VideoCodec::Hw(Codec, Depth)` with `Depth::{Eight, Ten}`; `name()`/`from_name()` add `hevc-10` and `av1-10`; `hw()` keeps returning the NVENC codec. Everything that matches on `VideoCodec` (MEDIA, WT, the WebRTC session, `signal.rs`) follows; WebRTC never offers a 10-bit name (note 12 rejects HDR over WebRTC, and libwebrtc's dav1d is 8-bit only).
- **`--codecs`** accepts the new names; the node's default list adds them after the 8-bit ones only where the device encodes them (6.2, 6.3).
- **`signal.rs` `/streams`**: `codecString` `hev1.2.4.L153.B0` and `av01.0.13M.10`, and `"depth":10`.
- **`hello.stream.color`** (decision 4) on WT and WebSocket, from `ColorTags` (2.1) and the depth.
- **cha-wire / the portal**: the portal passes the codec names through (`environments.rs` `connect`); its codec validation learns the two names. Older nodes don't list them, so nothing changes for them.
- **Tests (here)**: name round trips; `/streams` and `hello` JSON; the portal's connect validation.

### 6.2 NVENC at 10 bits — *node*

- **NV**: `Params` gains `depth`; HEVC uses the Main10 profile GUID with `pixelBitDepthMinus8 = 2` (NVENC's internal CUDA 8→10 conversion from ARGB on Ada); AV1 sets `inputPixelBitDepthMinus8 = 0`, `pixelBitDepthMinus8 = 2` (converted in hardware). Probe in `--probe-device nvidia` adds the 10-bit names when the session opens.
- **Gate (node)**: encode time at 1440p for both against today's 1.6–2.4 ms p50, with the ignored NVENC test; the stream decodes with ffmpeg as 10-bit with the tags from 2.1.

### 6.3 VA-API at 10 bits — *here, then node*

- **Headers (here)**: `bitstream/h265.rs` writes `general_profile_idc = 2` (Main10), `bit_depth_luma_minus8 = bit_depth_chroma_minus8 = 2` in the SPS; `bitstream/av1.rs` writes `high_bitdepth = 1` in `color_config` (profile 0 keeps 4:2:0). The writers' tests parse them back, and the self-test's checks (`CHA_ENCODE_TEST`) learn the 10-bit SPS and sequence header.
- **Surfaces (node)**: the video processor writes `VA_FOURCC_P010` instead of NV12; the encoder is opened with `VA_RT_FORMAT_YUV420_10`; `vaapi::BUILT` and the probe offer `hevc-10` where the driver has a Main10 `EncSlice*` entrypoint and `av1-10` where it has AV1 Profile 0 with 10-bit (`VAConfigAttribRTFormat` includes `YUV420_10`).
- **Gate (node)**: the self-test passes on the 780M (VCN 4: both) and reports encode time against 8-bit; the UHD 630 offers neither (note 12: no Main10 encode, no P010 from its video processor).

### 6.4 The browser decodes 10 bits — *here, then Mac*

- **PT**: `WEBCODECS` gains the 10-bit strings; the codec choice asks `VideoDecoder.isConfigSupported` for them and prefers them over 8-bit on the WebTransport path when supported and the node offers them; `decoderConfig` passes `colorSpace` from `hello.stream.color`. The `Colour` row (1.2) shows the depth.
- **Tests (here)**: the codec preference with a stubbed `isConfigSupported`; the `colorSpace` mapping.
- **Gate (Mac, Windows)**: hardware decode of both in Chrome macOS (HEVC Main10; AV1 10-bit only on M3 and later) and Windows; send→shown within noise of 8-bit.

### 6.5 Cha Player decodes 10 bits — *Mac*

- **`crates/cha-player/src/video/videotoolbox.rs` lines 216–219**: ask for `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange` and `…FullRange` as well. `render/color.rs` and `present/metal.rs` already handle 16-bit planes and the BT.2020 matrix, so nothing else changes for SDR.
- **Gate (Mac)**: a `hevc-10` stream decodes as P010 (the `Colour` row says 10-bit) with no change in decode time.

PyroWave's 10-bit modes wait for step 8, since its encoder wants an R16 intermediate that only the HDR conversion pass makes.

## 7. Which client paths show HDR10, from a recorded clip (rank 7)

Nothing on the node changes in this step: each client plays an HEVC Main10 PQ clip with HDR10 SEI, recorded once with ffmpeg and served by the dev harness.

### 7.1 The PQ maths, once, with a reference — *here*

- **`web/packages/pyrowave-webgpu/src/shaders/hdr.wgsl`** (new) and its twin in `crates/cha-pyrowave-wgpu`: the PQ EOTF and inverse, BT.2020 → BT.709 primaries (negatives kept), division by a reference white (203 nits by default), a highlight roll-off to a given headroom (BT.2390's EETF), and the sign-preserving sRGB encode for extended-range canvases.
- **`hdr.ts`**: the same functions in TypeScript as the reference; tests (here) against BT.2100's and BT.2390's published values (PQ 0.58 ≈ 203 nits, 10000 nits at 1.0, the EETF's knee) and the WGSL compiled by `naga` through the existing shader test (`crates/cha-upscale/tests/shader.rs` shows how).

### 7.2 Chrome: WebCodecs into `<video>` — *Mac*

- A dev page (`web/packages/player/src/dev-bridge.ts`'s harness, or a new `hdr-probe.ts`) that decodes the clip with WebCodecs and writes the frames into a `MediaStreamTrackGenerator` feeding a `<video>`, with `dynamic-range-limit: no-limit` on the element.
- **Gate**: the XDR panel shows highlights above SDR white; Perfetto shows the overlay kept; send→shown within ~1 ms of an 8-bit clip.

### 7.3 Chrome: PyroWave's canvas at `rgba16float` — *here, then Mac*

- **PYRO line 79–80**: when the stream's colour says PQ and `matchMedia('(dynamic-range: high)')` holds, configure the canvas with `format: "rgba16float"`, `toneMapping: { mode: "extended" }`, check `getConfiguration()`, and run 7.1's shader in `YuvRenderer` (`web/packages/pyrowave-webgpu/src/render.ts`, which today clamps). Without HDR, PQ content is tone-mapped to SDR in the same shader. The SGSR pass is skipped for HDR (note 12 rejects client upscaling on HDR).
- The clip for this path is a PyroWave PQ capture made with `cha-pyrowave`'s encoder on the node once (`CHA_ENCODE_TEST_DUMP`-style), checked into `spikes/` with its source.
- **Tests (here)**: the config choice; the renderer's uniforms for PQ.
- **Gate (Mac)**: visible EDR highlights; the FP16 present's cost against `bgra8unorm` measured with the existing timing.

### 7.4 Cha Player: an extended-linear surface on wgpu 30 — *Mac*

- **Upgrade**: wgpu 29.0.x → 30.0.1 and egui/egui-wgpu 0.35 → 0.36 or 0.37 (the first on wgpu 30), across the workspace (`cha-upscale`, `cha-pyrowave-wgpu` too); migrate `Queue::present`, `create_texture_from_hal`'s `initial_state`, and the new `SurfaceConfiguration::color_space`. Then `cargo hakari generate`.
- **Surface**: when the stream is PQ and `Surface::display_hdr_info` reports potential headroom above 1, configure `Rgba16Float` with `ExtendedSrgbLinear` (not `Bt2100Pq`, which crashes on Metal in 30.0.1 until #10175 is released). Otherwise today's `Bgra8Unorm`, with PQ tone-mapped to SDR.
- **Shader**: `render/video.wgsl` stops clamping in HDR and applies 7.1's functions, rolling off to the live headroom; `cha-upscale` is skipped for HDR; the PyroWave render pass gets the same treatment.
- **egui**: renders into an 8-bit offscreen texture, composited at EDR 1.0, so the UI keeps its colours on an extended-linear surface.
- **A/B**: the `CAEDRMetadata` path (a 10-bit PQ layer, `opticalOutputScale` 203) behind a setting, once a wgpu release carries #10175.
- **Gate (Mac)**: visible EDR highlights; headroom logged across brightness steps; decode-to-present unchanged against SDR.

## 8. HDR10 from the node, one surface at a time (rank 8)

### 8.1 Spikes before the build — *node*

- A test program on the 4090: GBM allocation of `ABGR16161616F` and `ABGR2101010` with the modifiers the GLES renderer offers; import into Vulkan; CUDA import of a Vulkan buffer (external memory). Note 12 calls all three unverified.
- gamescope with `--hdr-debug-force-output`, Cha treating its toplevel as BT.2020/PQ by convention, to get a real PQ game frame into the encoder before colour management exists (step 9).

### 8.2 The conversion pass — *node*

The streamer README already plans a Vulkan compute pass that writes NV12 into CUDA memory for NVENC and shares the composite's import with PyroWave (plan §3.3). Build it with three outputs:
- **SDR NV12** (8-bit, BT.709, the tags from 2.1), which also removes NVENC's CUDA conversion wait on a busy GPU (README, "Where the tail comes from").
- **SDR P010** for the 10-bit codecs.
- **HDR P010**: PQ and the BT.2020 non-constant-luminance matrix from the linear FP16 composite.
- `crates/cha-streamer/src/encoder/convert/` (new) with the shaders in WGSL or GLSL compiled at build time; tested (here) on lavapipe against a CPU reference, as `cha-upscale` is.

### 8.3 Each encoder takes P010 and the metadata — *node*

- **NVENC**: `InputFormat::P010` (`NV_ENC_BUFFER_FORMAT_YUV420_10BIT`, already in `crates/cha-nvenc/src/sys.rs`), VUI 9/16/9, `outputMasteringDisplay`/`outputMaxCll` and the per-picture `pMasteringDisplay`/`pMaxCll` on every IDR and every RFI-forced keyframe.
- **VA-API**: the encoder reads the P010 surface; MDCV/CLLI written by Cha as SEI (HEVC) or a metadata OBU (AV1), reusing `crates/cha-gamestream/src/hdr.rs`'s builders (moved into a shared module).
- **PyroWave**: PQ input in `cha-pyrowave` (`pyrowave.h` takes HDR10 PQ and wants R16 planes), and the 10-bit names `pyrowave420-10`/`pyrowave444-10`; the sequence header's `ColorInfo` says BT.2020/PQ.
- **Metadata on the wire**: decision 4's `{"t":"hdr"}` line; the GameStream host reports `hdr: true` and sends the 0x010e packet with the same values (`crates/cha-streamer/src/gamestream/`).
- **Gate (node)**: 10-bit encode latency against 8-bit; a PQ game frame (8.1's spike) reaches 7's proven client path with its highlights.

## 9. Colour management for Steam sessions (rank 9) — *here, then node*

- **Protocol**: vendor the MIT `wp-color-management-v1` module from `dividebysandwich/smithay`'s `ds-hdr-master` branch into `crates/cha-streamer/src/compositor/color_management/`, with its notice and a row in [PROVENANCE.md](../PROVENANCE.md); version 1 for gamescope ≤ 3.16.24 must also offer the deprecated `srgb` transfer (3.17.0 doesn't need it). Advertised only when the session's encoder can take 10 bits (Drakulix's rule, note 12).
- **Per-surface decoding**: a texture shader per image description (PQ, scRGB linear, gamma 2.2 with luminances, SDR) into a linear `Abgr16161616f` output pool (OUT `pick_format`), SDR white mapped to the client's (203 nits by default).
- **gamescope**: `--hdr-enabled` in `steam-gaming` when the environment is in HDR mode; `DXVK_HDR=1` in the game environment (Valve Proton has no switch of its own); 3.17.0's content-driven HDR, so the output is PQ only while an HDR game is on screen.
- **Tests (here)**: the protocol module's own tests, plus an image-description → shader-parameters table.
- **Gate (node)**: gamescope logs max > reference; its WSI offers HDR10 and scRGB swapchains; Steam shows its HDR toggle; a known HDR title reaches the encoder in PQ with the Steam overlay and mangoapp correct.

## 10. HDR only when it helps, and SDR for SDR viewers (rank 10)

- **10.1 The display line** (decision 5) from both players; the compositor's preferred image description takes the viewer's peak, minimum and SDR white, so games calibrate against the viewer's own display (HGIG for a virtual display).
- **10.2 Automatic HDR**: the environment enters HDR mode when an HDR viewer is connected and a surface is HDR, and leaves it when either stops (Valve's "Automatic"). Settings > Stream: Automatic, On, Off, in both players, kept per user.
- **10.3 SDR viewers of an HDR environment**: 8.2's pass tone-maps to SDR NV12 (one BT.2390 curve; the compositor knows the content's peak, so no peak detection) for their encoder.
- **10.4 Overrides**: a per-client SDR-brightness and peak override, instead of calibration screens.
- **Gate**: the Moonlight complaint list as a test plan: SDR not washed out next to HDR, HDR off for SDR content, the mode shown in the stats.

## 11. Desktop HDR and the other browsers (rank 11)

- Advertise headroom to desktop sessions only for HDR viewers (Chrome switches every window to FP16 when it sees headroom); HDR10 only while an HDR surface is visible, otherwise 10-bit SDR 4:4:4 (HEVC Rext on NVENC, PyroWave 4:4:4).
- Windows Chrome, then Safari (blocked on WebKit's WebGPU HDR display, its 10-bit external textures and its WebTransport), then Firefox.
- **Gate**: Chrome's FP16 compositing cost measured; text unchanged in an A/B; each browser's HDR verified by eye.

## 12. Research items (rank 12)

fifo-v1 for native Wayland clients on desktop sessions (signal barriers when a composite consumes the surface, with a fallback timer), commit-timing once the scheduler uses it, presentation-time v3 when Smithay has it, and 10-bit on CPU nodes once SVT-AV1's low-delay 10-bit cost is measured. None starts before a real client needs it.

## 13. Order of work and what each step changes on the wire

| Step | Changes the wire | Older nodes / pages |
|---|---|---|
| 1.1 | `stats` gains optional fields | ignored by older pages |
| 1.2, 4 | none (client only) | — |
| 2.1 | none (same bytes by default) | — |
| 2.2–2.4 | possibly the SDR tags | decoders follow the tags |
| 3 | `hello.stream.pacing`, `/info` | ignored by older pages |
| 6.1 | new codec names; `hello.stream.color`; `/streams` `depth` | older pages never ask for the names; older nodes never offer them |
| 8, 9, 10 | `{"t":"hdr"}`, `{"t":"display"}` | ignored both ways |

Steps 1–4 come first and in that order; 6 and 7 can run in parallel with 5; 8 needs 6 and 7's gates; 9 needs 8; 10 and 11 need 9.
