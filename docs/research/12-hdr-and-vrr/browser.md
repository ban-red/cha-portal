# HDR video and variable refresh in web browsers for Cha Portal's low-latency player (as of November 2026)

> Evidence for [research note 12](README.md).

Scope: Cha Portal's three browser video paths, which are (1) WebRTC → `<video>`, (2) WebTransport → WebCodecs `VideoDecoder` → `MediaStreamTrackGenerator` → `<video>` and (3) PyroWave → WebGPU canvas. The baseline is Chrome 154 on an M4 MacBook Pro (ProMotion/XDR). This builds on `docs/research/05-web-transport-and-input.md` and `docs/research/11-upscaling-and-frame-generation/browser.md` without repeating them.

Method notes:
- Chromium findings marked "(source, main)" come from reading `chromium.googlesource.com` at `main` on 2026-11-09. Trunk is about two milestones ahead of Chrome 154, so behaviour in 154 may differ in detail.
- Evidence types are tagged: vendor claim, single-developer report, source-code reading, or independent measurement.
- Older items carry a date tag. Claims I could not confirm carry (unverified).

---

## 1. WebCodecs HDR: hardware decode of HEVC Main10 / AV1 10-bit, VideoFrame format and colorSpace, and whether VideoTrackGenerator → `<video>` keeps HDR

### Takeaway
- **Decode.** Chrome decodes HEVC Main10 in hardware through WebCodecs on macOS (VideoToolbox) and Windows. The resulting `VideoFrame` carries the bitstream's BT.2020/PQ colour space and HDR metadata.
- **Frame format.** These GPU-backed 10-bit frames show `format === null` in JS, and `copyTo()` cannot read them back. That does not matter for Cha, which hands the frame straight to the track generator.
- **Display.** No source confirms end to end that a PQ `VideoFrame` pushed into `MediaStreamTrackGenerator` is shown in EDR on macOS or as HDR10 on Windows. The Chromium plumbing suggests it should work, but Cha has to test it.
- **Other browsers.** Firefox's WebCodecs colour metadata is still buggy. Safari has a known 10-bit bug in its WebGPU external-texture import.

### Cited Findings
**Chrome codec support**
- WebCodecs HEVC decode: 8-bit from Chrome 107 and 10-bit (Main10) from Chrome 108, hardware only. On macOS, "Chrome supports PQ, HDR10 (PQ with static metadata), HLG", and macOS does the EDR mapping itself (third-party maintainer's compatibility matrix). — [StaZhu/enable-chromium-hevc-hardware-decoding](https://github.com/StaZhu/enable-chromium-hevc-hardware-decoding)

**Chromium's macOS VideoToolbox decoder (source, main)** — [video_toolbox_video_decoder.cc](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/mac/video_toolbox_video_decoder.cc); [video_toolbox_frame_converter.cc](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/mac/video_toolbox_frame_converter.cc)
- Colour space source: each output frame's colour space comes from the bitstream (`accelerator_->GetVideoColorSpace()`). If that is invalid, it falls back to the `VideoDecoderConfig` colour space, then to BT.709.
- HDR metadata: the config's `hdr_metadata` is merged with per-picture dynamic HDR metadata and attached with `frame->set_hdr_metadata()`.
- IOSurface tagging: the frame converter tags the IOSurface-backed SharedImage with the CVImageBuffer's colour space. When "Chrome and macOS do not agree on the color space" it removes `SHARED_IMAGE_USAGE_SCANOUT` to force compositing (crbug.com/343014700).
- WebGPU flag: it marks the SharedImage `WEBGPU_READ` only when the IOSurface pixel format is WebGPU-compatible.

**Blink `VideoFrame` mapping (source, main)** — [webcodecs/video_frame.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/modules/webcodecs/video_frame.cc)
- The `media::VideoPixelFormat` → JS `VideoPixelFormat` mapping covers I420/I420P10/P12, I422*, I444*, NV12 and RGBA/BGRA variants.
- I found no P010 case, and `PIXEL_FORMAT_RGBAF16` maps to `{}` (null). A hardware P010 frame therefore most likely reports `format: null` (inference from the mapping).
- `CopyToFormat()` contains: "Readback is not supported for high bit-depth formats" for non-mappable (GPU-only) frames. `copyTo()` and `allocationSize()` on such a frame fail.

**Spec**
- [older: 2021–2022] A WebCodecs contributor said HDR/WCG were out of scope for WebCodecs v1. Colour-space values align with H.273. The lack of HDR canvas was named as the main blocker at the time. — [w3c/webcodecs #384](https://github.com/w3c/webcodecs/issues/384)

**Firefox bugs (2026)**
- `VideoFrame.colorSpace` for every software-decoded frame (Linux, Windows, macOS) is guessed rather than taken from the decoder, while Chrome reports the decoder's values. — [Mozilla bug 2057759](https://bugzilla.mozilla.org/show_bug.cgi?id=2057759)
- A `VideoFrame` built from an `HTMLVideoElement` has an empty `colorSpace`. A BT.709 guess would drop HDR. — [Mozilla bug 2048686](https://bugzilla.mozilla.org/show_bug.cgi?id=2048686)
- `VideoEncoder` emits a hard-coded BT.709 `decoderConfig.colorSpace` even for BT.2020/PQ input. — [Mozilla bug 2056028](https://bugzilla.mozilla.org/show_bug.cgi?id=2056028)
- WebRender handling of Rec2100PQ P010 on Windows HDR displays is a separate open bug. — [Mozilla bug 1977744](https://bugzilla.mozilla.org/show_bug.cgi?id=1977744)
- [older: 2022] 10-bit AV1 output was colour-shifted because the packing did not match P010's MSB layout. — [Mozilla bug 1763782](https://bugzilla.mozilla.org/show_bug.cgi?id=1763782)

**Safari / WebKit**
- [2026] WebKit's `importExternalTexture` misreads 10-bit hardware-decoded frames as 8-bit. One project copies those frames instead of importing them. — [Silo-Server PR #2544](https://github.com/Silo-Server/silo-server/pull/2544) (single project report; already in Cha note 11)
- Safari 27 lets a page override a hardware decoder's colour space (per Cha note 05, citing [WebKit features for Safari 27.0](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/); not re-verified here).
- A WebKit WPT `VideoTrackGenerator` test that renders into a `<video>` was a constant failure on macOS Sequoia. — [WebKit bug 289211](https://bugs.webkit.org/show_bug.cgi?id=289211)

**VideoTrackGenerator status**
- `VideoTrackGenerator` (worker-only) is the standard. Chrome's `MediaStreamTrackGenerator` is an older, non-standard, main-thread API. — [Mozilla WebRTC blog](https://blog.mozilla.org/webrtc/unbundling-mediastreamtrackprocessor-and-videotrackgenerator/); [MDN VideoTrackGenerator](https://developer.mozilla.org/en-US/docs/Web/API/VideoTrackGenerator)
- Firefox's implementation is tracked as unshipped. — [Mozilla bug 1749533](https://bugzilla.mozilla.org/show_bug.cgi?id=1749533)
- Cha's player uses Chromium's `MediaStreamTrackGenerator` on the main thread (`web/packages/player/src/player.ts` lines 62–84, 1613, 1770).

### Inferences
**Codec strings.** Cha's current strings are all 8-bit. For HDR10 the decoder needs 10-bit strings:
- HEVC Main10: `hev1.2.4.L153.B0` (general_profile_idc 2).
- AV1 Main 10-bit: `av01.0.13M.10`.
- Optionally VP9 profile 2: `vp09.02.51.10`.
- Optionally pass `colorSpace: {primaries:"bt2020", transfer:"pq", matrix:"bt2020-ncl", fullRange:false}` in `VideoDecoderConfig` as a fallback. Chromium prefers the bitstream VUI and falls back to the config colour space (VideoToolbox source above).
- Probe each with `VideoDecoder.isConfigSupported`. The exact strings follow the codec registry conventions but were not re-checked against a source in this pass (unverified).

**Does HDR survive into `<video>`? (inference)**
- The JS `VideoFrame` wraps the same `media::VideoFrame` that already carries the colour space and `hdr_metadata`, so writing it into the generator should keep both.
- On macOS, the IOSurface is tagged with the CVImageBuffer's colour space and stays scanout-eligible. That makes a CALayer overlay with EDR plausible.
- The SCANOUT-drop fallback means a colour-space disagreement could silently force a composited, and possibly tone-mapped, path.
- Verify with Perfetto (`viz`, `gpu` and `media` categories) and by looking for an EDR highlight on the XDR panel.

**`format: null` is harmless for Cha** because the player never reads pixels back.
- It does break any debug tooling that calls `copyTo()` on 10-bit frames.
- It would break a WebGPU path that expects NV12 zero-copy (see §3).

**Priority order for HDR on path 2:** Chrome macOS (likely workable) → Windows Chrome (P010 + HDR swap chain, §2) → Safari (no `MediaStreamTrackGenerator` on the main thread; it needs worker `VideoTrackGenerator`, which is unverified for HDR) → Firefox (colour metadata bugs, no generator).

### Gaps
- I found no first-hand report or crbug that confirms or refutes HDR/EDR display of PQ `VideoFrame`s through `MediaStreamTrackGenerator`/`VideoTrackGenerator` → `<video>` on macOS or Windows. This is the single most important item for Cha to test.
- AV1 10-bit hardware decode via WebCodecs on M4 (VideoToolbox supports AV1 on M3+): I found no source on whether Chrome's VideoToolbox AV1 path outputs 10-bit P010 with PQ tags.
- Linux VA-API 10-bit WebCodecs: I found no 2026 source. Mozilla's VA-API VP9 profile 2 bug ([1705940](https://bugzil.la/1705940)) is Firefox-specific and old.
- Windows D3D11/D3D12 WebCodecs P010 → `VideoFrame` colour-space fidelity was not confirmed from source in this pass.

---

## 2. WebRTC HDR: HEVC/AV1/VP9 10-bit in Chrome and Safari, the colour-space RTP header extension, whether HDR renders from a WebRTC track, SDP parameters, and cloud gaming services

### Takeaway
- **Chrome's WebRTC can carry and render HDR only through its hardware-decoder adapter.** The colour-space header extension (or VUI) drives a decoder reinitialisation into the HDR colour space.
- **The software fallbacks are 8-bit only.** libwebrtc's dav1d AV1 decoder rejects anything that is not 8-bit.
- **HEVC.** It shipped in Chrome WebRTC around M136 as hardware-only, with Xbox Cloud Gaming as the stated driver. No source documents Main10/HDR over it.
- **Cloud gaming.** No cloud gaming service documents HDR in a browser. GeForce NOW ships 10-bit SDR colour in Chrome and Edge on Windows only.

### Cited Findings
**The colour-space header extension**
- `http://www.webrtc.org/experiments/rtp-hdrext/color-space` "is used to communicate color space information and optionally also metadata that is needed in order to properly render a high dynamic range (HDR) video stream". — [webrtc color-space README](https://webrtc.googlesource.com/src/+/main/docs/native-code/rtp-hdrext/color-space/README.md)
  - The one-byte form carries colour info only. The two-byte form adds mastering-display and content-light-level metadata (28 data bytes).
  - It is marked experimental.
- [older: 2019] libwebrtc: "The color space set through the RTP header extension overrides the color space in the VUI. The check for HDR should look at the resulting color space." (bug webrtc:10575). — [webrtc commit 2ad2fabd](https://webrtc.googlesource.com/src/+/2ad2fabdcf5017a4a5ccdc9c20ab955117498e9c)

**Chromium's WebRTC decoder adapter (source, main)** — [rtc_video_decoder_adapter.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/platform/peerconnection/rtc_video_decoder_adapter.cc)
- `ShouldReinitializeForSettingColorSpace()` reinitialises the hardware `media::VideoDecoder` when an incoming `EncodedImage`'s colour space differs from the config. The code comment says: "This also includes reinitialization for the HDR use case, i.e. config_.profile() is media::VP9PROFILE_PROFILE2."
- `HasSoftwareFallback()` returns false for HEVC ("HEVC does not have SW fallback").

**libwebrtc software AV1 decoder (source, main)**
- `Dav1dDecoder::Decode` contains `if (dav1d_picture.p.bpc != 8) { // Only accept 8 bit depth. ... return WEBRTC_VIDEO_CODEC_ERROR; }`. It also accepts only I420 or I444 layouts. — [dav1d_decoder.cc](https://webrtc.googlesource.com/src/+/main/modules/video_coding/codecs/av1/dav1d_decoder.cc)

**Chrome on Windows (older commits)**
- [older: 2019] Chromium allowed DXGI `R10G10B10A2` HDR10 swap chains for BT.2020/PQ input even without HDR metadata, "because HDR metadata is an optional extension for WebRTC streams". — [Chromium commit 4130a38e](https://chromium.googlesource.com/chromium/src/+/4130a38e27c173f4789fd8b87e73ae082e1599e2)
- [older: ~2019] A libwebrtc/Chromium change converted 8-bit H.264 buffers to 10-bit when the colour space says HDR, because "8-bit H264 HDR content is not rendered correctly in Chrome on Windows" (per search-result summary; which commit was not pinned down, unverified).

**HEVC in Chrome WebRTC**
- [2025-03] Intent to Ship "H265 (HEVC) codec support in WebRTC", estimated Chrome 136 on all six Blink platforms. — [blink-dev intent](https://groups.google.com/a/chromium.org/g/blink-dev/c/3h8lL8a377c)
  - Send and receive are gated by `WebRtcAllowH265Send` and `WebRtcAllowH265Receive`.
  - "We will not provide a software implementation to fall back to".
  - A Microsoft engineer framed decode as needed for Xbox Cloud Gaming clients.
  - The visible thread does not mention Main10, HDR, or the `profile-id`/`level-id`/`tier-flag` values.
- WebRTC HEVC decode requires Chrome ≥ 136.0.7077.0. — [StaZhu matrix](https://github.com/StaZhu/enable-chromium-hevc-hardware-decoding)

**Safari / WebKit**
- [older: 2020] WebKit added VP9 profile 2 (10-bit) in WebRTC. It converts libwebrtc I010 buffers to bi-planar 10-bit CVPixelBuffers for software VP9 HDR decode. — [WebKit r265073](https://trac.webkit.org/r265073); [WebKit bug 217673](https://www2.webkit.org/show_bug.cgi?id=217673)
- The GTK/WPE ports still failed the profile-2 test afterwards. — [WebKit bug 218221](https://www2.webkit.org/show_bug.cgi?id=218221)

**Cloud gaming services**
- GeForce NOW release highlights (vendor claim, undated entries):
  - v2.0.64: 10-bit colour precision in the native Windows/macOS/SHIELD apps.
  - v2.0.69: expanded "to streaming from Edge and Chrome browsers on Windows, Chrome browser on Chromebook, Samsung TV, and LG TV". It applies "even if you do not have an HDR display or if a game does not support HDR".
  - v2.0.67: HDR on Windows-on-Arm laptops (beta).
  - No browser HDR is listed. — [GeForce NOW release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- GeForce NOW HDR needs Ultimate and an HDR10 monitor. The requirements describe it for native apps. — [GeForce NOW system requirements](https://www.nvidia.com/en-gb/geforce-now/system-reqs/)
- [2026-08] Firefox on Windows became a supported GeForce NOW browser (1440p/120). Nothing was said about HDR. — [HotHardware](https://hothardware.com/news/nvidia-geforce-now-lands-firefox-1440p-120-fps-streaming)
- Xbox Cloud Gaming lists Edge and Chrome on Windows/ChromeOS/Android and Safari on Apple devices. I found no HDR-in-browser statement. — [Xbox support: supported browsers](https://support.xbox.com/help/games-apps/cloud-gaming/supported_browsers)

### Inferences
**Path 1 on Chrome**
- The only HDR-capable codecs are those with hardware decode behind `RTCVideoDecoderAdapter`: HEVC Main10 (macOS VideoToolbox, Windows D3D11), VP9 profile 2, and AV1 10-bit where hardware AV1 exists (M3/M4 Macs, recent GPUs).
- If the adapter falls back to software (dav1d or libvpx), a 10-bit AV1 stream fails to decode.
- Cha should signal BT.2020/PQ in the VUI or sequence header **and** send the colour-space header extension. The two-byte form carries the HDR10 static metadata.
- str0m needs to support negotiating and writing that extension (not checked here).

**SDP parameters (unverified for Chrome's offer).** These follow the RTP payload format RFCs, not a Chrome source:
- HEVC Main10: `profile-id=2` (RFC 7798).
- AV1: `profile=0` (Main covers 10-bit) with `level-idx`/`tier` (AV1 RTP spec).
- VP9: `profile-id=2`.
- Cha should read `RTCRtpReceiver.getCapabilities('video')` on each target to see which fmtp lines Chrome actually advertises.

**Measured as Cha's fastest path,** path 2 (WebCodecs) is the better first HDR target. It sidesteps the decoder-adapter reinit logic and the software-fallback risk, and Cha controls the colour metadata directly.

**Industry precedent:** no major cloud gaming service has shipped browser HDR, so Cha would be ahead of them. The closest shipped feature is GeForce NOW's 10-bit SDR in Chromium on Windows. It shows 10-bit decode through a browser WebRTC/`<video>` path is production-viable on Windows.

### Gaps
- No source states whether Chrome's WebRTC offer includes HEVC `profile-id=2` (Main10) or 10-bit AV1 entries, or whether HEVC Main10 receive works over WebRTC on macOS.
- No 2025–2026 first-hand report of HDR/EDR display from a WebRTC remote track in Chrome or Safari on macOS was found.
- Whether Safari 26's WebRTC decodes HEVC Main10 with PQ and shows EDR is unknown.
- Whether GeForce NOW's or Xbox's browser clients use WebRTC or another path for the 10-bit mode is not documented.

---

## 3. WebGPU HDR: canvas `toneMapping: "extended"` with rgba16float, Safari 26, Firefox, `importExternalTexture` on HDR frames, rec2100 canvas colour spaces, 2D canvas float16, and FP16 present cost

### Takeaway
- **Chrome (128/129+).** WebGPU canvases can show HDR on macOS and Windows with `format: "rgba16float"` and `toneMapping: {mode: "extended"}`. ChromeOS lagged at ship time. Cha's PyroWave path can therefore show HDR10 by decoding PQ in its shader and writing extended values into an FP16 canvas.
- **Safari 26** claims "HDR support in WebGPU Canvas", but a WebKit engineer reported in 2025 that macOS accepted the setting without displaying HDR.
- **Imported video.** `importExternalTexture` in Chromium only zero-copies NV12 frames into sRGB/P3, so 10-bit HDR frames take a copy path. HDR handling in the zero-copy shader is still a TODO.
- **Rec2100 canvas spaces** remain proposals, so Cha cannot use them.

### Cited Findings
**WebGPU spec (editor's draft)** — [WebGPU ED](https://gpuweb.github.io/gpuweb/) §21.4–21.5, §3.11.1, §6.4.1
- `GPUCanvasConfiguration` has `colorSpace: PredefinedColorSpace = "srgb"`, `toneMapping: GPUCanvasToneMapping = {}` (mode `"standard"` by default) and `alphaMode = "opaque"`.
- `"standard"`: values outside the screen's SDR range "are projected to the standard dynamic range" (often by clamping to [0, 1]).
- `"extended"`: values "in the extended dynamic range of the screen are unchanged", for example values up to 4.0 on a display with [0, 4] headroom.
- Support for `toneMapping` "is detected using `getConfiguration()`".
- Colour space conversions: "Colors are not lossily clamped during conversion". Values outside [0, 1] survive conversion, and high-bit-depth and extended-range sources "are preserved through color space conversion".
- `GPUExternalTextureDescriptor`: `{ source: HTMLVideoElement | VideoFrame, colorSpace: PredefinedColorSpace = "srgb" }`, described as "The color space the image contents of source will be converted into when reading". An external texture from a `VideoFrame` expires when the frame is closed.

**Chrome ship status**
- Intent to Ship "WebGPU extended range (HDR) support", Chrome 128, 2024-07-16. — [blink-dev](https://groups.google.com/a/chromium.org/g/blink-dev/c/rBQIRHUEAe8)
  - "ChromeOS platform support for HDR images (and canvas) has not been completed".
  - Safari and Firefox signals were positive in the WebGPU WG.
- "New in WebGPU 129" documents the canvas tone-mapping mode: `rgba16float` + `toneMapping: { mode: "extended" }`. — [Chrome for Developers](https://developer.chrome.com/blog/new-in-webgpu-129)
- Chromium's `GPUCanvasContext` parses `toneMapping` into the swap buffers' HDR metadata and reports it via `getConfiguration()`. `kRgba16Float` is an accepted context format (source, main). — [gpu_canvas_context.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/modules/webgpu/gpu_canvas_context.cc)

**Safari / WebKit**
- Safari 26.0 release notes (2025-09-15): "Added HDR support in WebGPU Canvas" and "Added support for WebGPU". — [Safari 26.0 release notes](https://developer.apple.com/documentation/safari-release-notes/safari-26-release-notes)
- [2024-10] WebKit commit: with extended tone mapping, the canvas uses `ContentsFormat::RGBA16F` to display in HDR. — [webkit-changes](https://lists.webkit.org/pipermail/webkit-changes/2024-October/308038.html)
- [2025-08-29] WebKit bug 272702 — [WebKit bug 272702](https://bugs.webkit.org/show_bug.cgi?id=272702):
  - Google's Gregg Tavares reported that on macOS (Safari Technology Preview "Release 226") `getConfiguration()` returns `toneMapping: 'extended'` but the canvas is **not** shown in extended range. He said it does work on iOS with WebGPU HDR enabled.
  - Apple's Mike Wyrzykowski replied that the macOS display issue was not known. The bug was closed as a duplicate of the fixed bug 279816.
  - I found no later confirmation that macOS Safari 26.x displays it.

**Firefox**
- [older: 2023–2025] Firefox lacked `importExternalTexture`/`GPUExternalTexture`. — [Mozilla bug 1827116](https://bugzilla.mozilla.org/show_bug.cgi?id=1827116)
- Firefox WebGPU reached Windows (141) and Apple Silicon macOS (147). — per Cha note 11, citing [AlternativeTo](https://alternativeto.net/news/2026/1/firefox-147-brings-apple-silicon-webgpu-support-improved-video-playback-and-pip-and-more)
- I found no source for Firefox canvas `toneMapping: "extended"`.

**Chromium `importExternalTexture` (source, main)** — [external_texture_helper.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/modules/webgpu/external_texture_helper.cc)
- Zero-copy requires a SharedImage with `WEBGPU_READ` usage, **`media_video_frame->format() == PIXEL_FORMAT_NV12`**, device support, and a destination colour space of `kSRGB` or `kP3`.
- The code comment reads "TODO(crbug.com/40227105): Support HDR color space and color range in generated wgsl shader to enable all color space for zero-copy path."
- Colour conversion constants come from skcms 7-parameter transfer functions and a primaries matrix (src → XYZD50 → dst).
- [older: Chrome 116] `VideoFrame` became a valid `importExternalTexture` source. — [New in WebGPU 116](https://developer.chrome.com/blog/new-in-webgpu-116)

**HDR canvas proposals**
- The ColorWeb-CG HDR canvas proposal adds `"rec2100-hlg"`, `"rec2100-pq"` and `"rec2100-display-linear"` to `PredefinedColorSpace`, which would make a canvas HDR. — [ColorWeb-CG hdr_html_canvas_element.md](https://github.com/w3c/ColorWeb-CG/blob/main/hdr_html_canvas_element.md)
- [older: 2021–2022] Chrome prototyped those spaces behind a flag. — [Cameron, public-colorweb](https://lists.w3.org/Archives/Public/public-colorweb/2022Jun/0000.html)
- The `hdr-canvas` library uses 2D `colorType: "float16"`, with `pixelFormat: "float16"` as the pre-Chrome-133 name, and earlier needed Chrome 108+ with an experimental flag. — [hdr-canvas README](https://unpkg.com/hdr-canvas@0.0.13/README.md) (third-party library; shipping status unverified)
- [2025-11-21] Cameron: HDR exposure must be aligned across CSS, canvas, WebGL and WebGPU. The most pressing open decision is linear versus log2 headroom encoding (ColorWeb-CG #129). — [public-colorweb](https://lists.w3.org/Archives/Public/public-colorweb/2025Nov/0001.html)

**Present cost**
- I found no published measurement of FP16 versus 8-bit WebGPU canvas present cost.
- Cha's measured canvas path is 7.3–8.9 ms, against 2.7–3.4 ms for `<video>` with SDR 8-bit. — [Cha note 11 §5](../../../docs/research/11-upscaling-and-frame-generation/browser.md)

### Inferences
**PyroWave HDR recipe (path 3, Chrome macOS/Windows)**
1. `ctx.configure({device, format: "rgba16float", colorSpace: "display-p3" or "srgb", toneMapping: {mode: "extended"}, alphaMode: "opaque"})`, then check `getConfiguration().toneMapping.mode === "extended"`.
2. In the final PyroWave pass, apply the PQ EOTF to get absolute nits.
3. Divide by an SDR reference white, usually 203 nits. That matches Chromium's PQ-0.581 = 203-nit convention in §4.
4. Convert BT.2020 → P3 or sRGB primaries, which leaves out-of-gamut values negative or above 1.
5. Re-encode with the sign-preserving sRGB transfer, because `PredefinedColorSpace` "srgb" and "display-p3" are gamma-encoded spaces.

Caveats for the recipe:
- Values above 1.0 are then shown up to the panel's current headroom and clipped above it.
- Cha must do its own highlight roll-off, because no headroom API has shipped (§4).
- The transfer-encoding detail is inferred from the PredefinedColorSpace definitions; validate it against ccameron's WebGPU HDR sample.

**FP16 canvas cost (back-of-envelope, not measured).** At 3456×2234 the FP16 back buffer is about 61.8 MB against 30.9 MB for 8-bit, or about 7.4 GB/s per direction at 120 fps. That is small next to M4-class memory bandwidth, so the risk is more likely an overlay or format penalty in the compositor than bandwidth. Cha should measure FP16 versus BGRA8 canvas present latency with rVFC-equivalent timestamps.

**Path 2 with WebGPU post-processing (for example upscaling, note 11 "Option A")**
- For a 10-bit HDR `VideoFrame`, Chromium falls off the NV12 zero-copy path into a copy.
- The output space can only be "srgb" or "display-p3". Per spec, values must not be clamped, but PQ is not a 7-parameter skcms curve, so whether Chromium's copy path tone-maps PQ to SDR or preserves extended range is unknown.
- On Safari, 10-bit frames are misread (Silo PR).
- HDR plus WebGPU post-processing of decoded video is therefore the riskiest combination. Keep HDR `VideoFrame`s on the plain `<video>` path.

**Safari path 3:** treat macOS WebGPU HDR display as unconfirmed. A runtime check of `getConfiguration()` is not sufficient per the WebKit bug; only visual or EDR verification is.

### Gaps
- Whether Chrome's `importExternalTexture` copy path preserves PQ highlights above 1.0 or tone-maps them was not established from source or tests.
- I found no measurement of FP16 versus 8-bit canvas present latency, and nothing on whether Chrome on macOS can promote an rgba16float WebGPU canvas to a CALayer overlay.
- Safari 26.x macOS WebGPU HDR display after the August 2025 report is unknown.
- Chrome Linux: I found no source on WebGPU extended tone mapping or HDR output on Linux/Wayland.
- The shipping status of 2D canvas `colorType: "float16"` and the rec2100 canvas colour spaces in Chrome 154 was not confirmed on chromestatus.

---

## 4. Detection and limits: `dynamic-range` media queries, MediaCapabilities HDR fields, headroom APIs, CSS `dynamic-range-limit`, and SDR-display tone mapping

### Takeaway
- **Capability detection is coarse but usable.** Use `matchMedia('(dynamic-range: high)')` for the display and `VideoDecoder.isConfigSupported` with 10-bit codec strings for decode. MediaCapabilities HDR fields exist in the spec, but browser support is uneven.
- **Headroom.** No shipped API exposes the current HDR headroom. Chrome's `ScreenDetailed` headroom attribute is still behind a flag and in privacy debate.
- **`dynamic-range-limit`** (Chrome 136, Safari 26 partial) can silently force a `<video>` or WebGPU canvas to SDR if any ancestor sets `standard`.
- **SDR displays.** Chrome tone-maps HDR video, but the quality varies by platform.

### Cited Findings
**Media queries**
- Chromium shipped the HDR CSS media queries (`dynamic-range`, `video-dynamic-range`). — [blink-dev Intent to Ship: HDR CSS Media Queries](https://groups.google.com/a/chromium.org/g/blink-dev/c/OpUsOWnnN6c) (milestone not re-verified)

**MediaCapabilities**
- The spec defines `transferFunction` ("srgb", "pq", "hlg"), `colorGamut` and `hdrMetadataType` ("smpteSt2086", "smpteSt2094-10", "smpteSt2094-40") in `decodingInfo()`. — [Media Capabilities WD 2025-02-12](https://www.w3.org/TR/2025/WD-media-capabilities-20250212/); [HDR explainer](https://github.com/w3c/media-capabilities/blob/main/hdr_explainer.md)
- The explainer's undated status note says Safari implemented these, Chrome was a work in progress and Firefox had not started. — [HDR explainer](https://github.com/w3c/media-capabilities/blob/main/hdr_explainer.md)
- A Chromium WPT commit "Implement MediaCapabilities: HDR decoding query" exists. — [WPT commit](https://chromium.googlesource.com/external/w3c/web-platform-tests/+/refs/tags/merge_pr_21791%5E%21/)
- Safari 26 fixed `MediaCapabilitiesDecodingInfo.configuration` being empty when `supported` is false. — [Safari 26.0 release notes](https://developer.apple.com/documentation/safari-release-notes/safari-26-release-notes)
- [2026-04] A WebKit bug notes that Chrome 146 reports AV1 decode as smooth and powerEfficient on an M4 MacBook Pro. — [WebKit bug 311593](https://bugs.webkit.org/show_bug.cgi?id=311593) (per search summary)

**Headroom API**
- [2025-06-23] Christopher Cameron (Google) proposed adding the screen's HDR headroom to `ScreenDetailed` — [public-colorweb](https://lists.w3.org/Archives/Public/public-colorweb/2025Jun/0000.html):
  - It goes on `ScreenDetailed` rather than `Screen` because it is "a potential fingerprinting vector".
  - It has its own change event, because the value changes with brightness.
  - Its use is custom tone mapping.
  - It has been "shipping behind a flag for several years".
- [2025-11] Google's Mike Wasserman asked whether the attribute should need an opt-in such as `getScreenDetails({includeHDRHeadroom: true})`. Kai Ninomiya noted it is a getter, so the value is only exposed when read. — [public-colorweb 2025-11](https://lists.w3.org/Archives/Public/public-colorweb/2025Nov/0002.html); [Ninomiya reply](https://lists.w3.org/Archives/Public/public-colorweb/2025Nov/0003.html)
- Linear versus log2 headroom encoding is unresolved (ColorWeb-CG #129). — [Cameron 2025-11-21](https://lists.w3.org/Archives/Public/public-colorweb/2025Nov/0001.html)

**CSS `dynamic-range-limit`**
- Values are `standard`, `no-limit` and `constrained`. — [MDN](https://developer.mozilla.org/en-US/docs/Web/CSS/dynamic-range-limit); [CSS Color HDR L1](https://www.w3.org/TR/css-color-hdr-1)
- Chrome shipped it in 136 (April 2025) and Safari in 26.0. — [ics.media](https://ics.media/en/entry/251024/); [blink-dev intent](https://groups.google.com/a/chromium.org/g/blink-dev/c/cG_ZCeQaWQs)
- Safari 26 supports only `standard` and `no-limit`, non-animatable. — [Safari 26.0 release notes](https://developer.apple.com/documentation/safari-release-notes/safari-26-release-notes)
- [2025-03] In CSSWG HDR breakouts, Chrome's engineer said changing the initial value would break images and the already-shipped WebGPU canvas. Canvas is treated "as a snapshot at a fixed headroom for now". The spec's initial value is `no-limit`. — [CSSWG minutes 2025-03-26](https://lists.w3.org/Archives/Public/www-style/2025Mar/0027.html); [CSSWG 2025-03-12](https://lists.w3.org/Archives/Public/www-style/2025Mar/0014.html)

**Tone mapping on SDR displays**
- [older: ~2022] Eric Chan's HDR diagnostics page reports that Chrome tone-maps from about version 106 on. On HDR displays Chrome maps 10,000 nits to the display's current peak, so PQ content looks darker than HLG (single expert's independent test page). — [HDR diagnostics](https://people.csail.mit.edu/ericchan/hdr/diagnostics.html)
- A Chromium issue notes SDR tone mapping works on macOS Big Sur+ and on Windows via `VideoProcessorBlt` on Intel Icelake+ GPUs. — [crbug 40184904](https://issues.chromium.org/40184904)
- A Chromium commit makes HLG 0.75 equal PQ 0.581 (= 203 nits), which "maps to SDR white" where no tone mapping happens. — [Chromium commit 4ec54d51](https://chromium.googlesource.com/chromium/src/+/4ec54d519e9469ab0f6d010b51da89914fc009ad)
- A ColorWeb-CG issue proposes a standard, global, metadata-driven HDR→SDR tone map. — [ColorWeb-CG #113](https://github.com/w3c/ColorWeb-CG/issues/113)

**Firefox on Windows**
- [2026] Firefox HDR video on Windows: Nightly 148 (January 2026), with stable reported as Firefox 153 (2026-07-21). — [Gigazine](https://gigazine.net/gsc_news/en/20260715-firefox-windows-hdr-video/); [Mozilla bug 2025322](https://bugzilla.mozilla.org/show_bug.cgi?id=2025322)
  - The bug is marked fixed for 152, which conflicts with the 153 date.
  - It requires Windows HDR on and a discrete AMD/NVIDIA GPU. Intel+NVIDIA laptops were excluded at first.
  - "no attempt has been made to read and use HDR metadata".

### Inferences
**Detection ladder for Cha:**
1. `matchMedia('(dynamic-range: high)').matches` decides whether to request HDR at all.
2. `VideoDecoder.isConfigSupported({codec: 'hev1.2.4.L153.B0', hardwareAcceleration: 'prefer-hardware'})` (or the AV1 10-bit string) gates path 2.
3. For path 3, configure the canvas and read back `getConfiguration().toneMapping`.

Without a headroom API, assume the XDR panel's headroom varies (about 1× to 16× SDR white, depending on brightness). Send HDR10 static metadata so the OS/browser tone mapper has MaxCLL/MaxFALL.

**Defensive CSS:** set `dynamic-range-limit: no-limit` explicitly on the `<video>` and canvas elements, so that a page-level `standard` cannot quietly force SDR.

**On SDR displays**, prefer that the node sends SDR rather than relying on browser tone mapping. Browser tone mapping is undocumented, varies by platform, and Firefox does not use the metadata.

### Gaps
- I found no current support table for the MediaCapabilities HDR fields in Chrome 154 or Firefox, or for whether they apply to `type: "webrtc"`.
- The exact chrome://flags name for the `ScreenDetailed` headroom attribute was not found.
- Whether `dynamic-range-limit` affects a `<video>` fed by `MediaStreamTrackGenerator` the same way as file playback is unverified.

---

## 5. Refresh rate and VRR: Chrome on ProMotion, Safari's 60fps preference, Chrome on Windows G-Sync/FreeSync, Firefox, track-generator `<video>` scheduling, rVFC fields and 90-on-120 judder

### Takeaway
**Chrome on macOS**
- Chrome drives a CADisplayLink on macOS 14+, but does not use true variable refresh.
- For fixed-rate panels, including ProMotion as Chromium treats it, it can only emulate lower rates by skipping vsyncs in powers of two: 120 → 60 → 30 Hz.
- When **only** video is updating, viz's `OnlyVideoMatcher` picks the largest supported interval with a "simple cadence" to the video's frame rate. 60 fps video can drop the compositor to 60 Hz. For 90 fps on 120 Hz no simple cadence exists, so it stays at 120 Hz with a 1-1-2 cadence (judder).

**Safari** still defaults to "Prefer Page Rendering Updates near 60fps" for page rendering as of early 2026.

**Windows and Linux.** I found no Chromium Windows VRR/G-Sync mode for video or canvas. Linux Wayland VRR has known Chromium bugs.

### Cited Findings
**Chromium macOS begin-frame source (source, main)** — [external_begin_frame_source_mac.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/service/frame_sinks/external_begin_frame_source_mac.cc)
- `GetSupportedFrameIntervals()`: "When CADisplayLink can support any preferred refresh rate, return an empty list". However, `hw_takes_any_refresh_rate_` is only ever assigned `false` in this file, next to a TODO.
- Otherwise: "For displays with fixed refresh rates, try to emulate lower rate options (2^n sub-multiples) by skipping VSyncs" (interval, interval×2, … up to 1/14 Hz).
- `SetPreferredInterval()` converts the decided interval into `vsync_subsampling_factor_ = adjusted_interval.IntDiv(min_refresh_interval_ - kDeltaAlmostEqual)`. It is traced as `ExternalBeginFrameSourceMac::SetPreferredInterval` with arg `vsync_subsampling_factor` in category `gpu`.
- Display links are created per `CGDirectDisplayID`. The timer fallback can be forced with `kForceMacVSyncTimerForDebugging`.

**Chromium Mac display link (source, main)**
- `ca_display_link_mac.mm` uses `CADisplayLink` under `API_AVAILABLE(macos(14.0))`, with a `CADisplayLinkInGpu` feature path. — [ca_display_link_mac.mm](https://chromium.googlesource.com/chromium/src/+/main/ui/display/mac/ca_display_link_mac.mm)
- [older: Chromium 138] The header notes that the system usually picks a rate that divides the display's maximum, and that fixed-rate requests use the same min, max and preferred values. — [Debian mirror of ca_display_link_mac.h](https://sources.debian.org/src/chromium/138.0.7204.183-1/ui/display/mac/ca_display_link_mac.h)
- [older] A Mac Observer report on the Gerrit change said Chrome would move from CVDisplayLink to CADisplayLink on Sonoma+ so 24 Hz video looks smooth and 120 Hz content is crisp. — [Mac Observer](https://macobserver.com/?p=190223)

**Chromium viz frame-interval logic (source, main)**
- `MatchContentIntervalType` (used by `OnlyVideoMatcher`) — [frame_interval_matchers.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/service/display/frame_interval_matchers.cc):
  - It returns nothing if any frame sink "has other updates" or other content types, or if the videos' intervals differ.
  - With `FixedIntervalSettings` it picks the largest supported interval for which `media::VideoCadenceEstimator::HasSimpleCadence(supported, content)` holds. Otherwise it returns `default_interval`.
- `VideoConferenceMatcher` handles two or more videos. `MixedFixedIntervalMatcher` (LCM of mixed content) is behind `kMixedFixedIntervalMatcher`, disabled by default. — [frame_interval_matchers.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/service/display/frame_interval_matchers.cc); [viz features.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/common/features.cc)
- `VideoFrameSubmitter` tags every video compositor frame with `ContentFrameIntervalType::kVideo` and `video_frame_provider_->GetPreferredRenderInterval()`, and sets `has_only_content_frame_interval_updates = true`. — [video_frame_submitter.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/platform/graphics/video_frame_submitter.cc)
- On Mac, `kVSyncAlignedPresentationForScrolling` is enabled by default ("presentation should be delayed until the next DisplayLink callback. Currently only for frames that handle interaction"). The general `kVSyncAlignedPresentation` is **disabled** by default, and `kUseDisplayRefreshRateForTimer` is enabled. — [viz features.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/common/features.cc)

**Chromium on other platforms (source, main)**
- On Windows, `ExternalBeginFrameSourceWin` is used when DirectComposition is available, with the comment "Vsync updates are required to update the FrameIntervalDecider with supported refresh rates". ChromeOS gates multi-rate support on `IsCrosContentAdjustedRefreshRateEnabled()`. Android has `kUseFrameIntervalDeciderAdaptiveFrameRate` (disabled by default). — [root_compositor_frame_sink_impl.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/service/frame_sinks/root_compositor_frame_sink_impl.cc); [viz features.cc](https://chromium.googlesource.com/chromium/src/+/main/components/viz/common/features.cc)
- I found no `BASE_FEATURE` for Windows VRR, tearing or G-Sync in `ui/gl/gl_features.cc`. The Windows items there are `kDCompOnD3D12` (disabled) and the vsync/compositor-clock flags. — [gl_features.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/gl/gl_features.cc)

**Other browsers and OSes**
- [older: 2022] ChromeOS had VRR behind `chrome://flags#enable-variable-refresh-rate`. — [Chrome Unboxed](https://chromeunboxed.com/variable-refresh-rate-coming-to-chrome-os/)
- [older] Edge (Chromium) had an experimental "Boost screen refresh rate when scrolling" flag for VRR panels. — [Winaero](https://winaero.com/edge-will-get-smoother-scrolling-on-displays-with-vrr-support)
- [older: 2019] Windows 10 1903 added an OS-level VRR toggle for full-screen DX11 games. — [Microsoft DirectX blog](https://devblogs.microsoft.com/directx/os-variable-refresh-rate/)
- KDE bug: "The presentation time protocol is buggy with VRR on Chromium". Chromium on Wayland fails to vsync properly with VRR "Always"/"Automatic" in fullscreen. — [KDE bug 478938](https://bugs.kde.org/show_bug.cgi?id=478938)
- [older] Single-user forum report: a 144 Hz monitor's measured refresh wandered to about 143.5–143.8 Hz in browser tests only when G-SYNC was on. This hints that G-SYNC engaged in a windowed browser. — [Blur Busters forum](https://forums.blurbusters.com/viewtopic.php?p=31144)

**Safari**
- "Prefer Page Rendering Updates near 60fps" is on by default. Turning it off lets Safari render at up to 120 Hz on ProMotion Macs and iPhones. — [MacRumors how-to](https://www.macrumors.com/how-to/enable-smoother-120hz-browsing-in-safari/); [iPhoneSoft, 2026-01-26](https://iphonesoft.fr/2026/01/26/activer-defilement-fluide-120-hz-safari-mac-ipad-iphone?AMP=1)
  - Sources disagree on whether it affects scrolling or only animations and rAF.
  - [older: 2022] It has been the default since macOS 12.4. — [ifun.de](https://www.ifun.de/promotion-darstellung-in-safari-muss-manuell-aktiviert-werden-188061/)
- The flag affects only Safari, not WKWebView apps. — [WebKit bug 294338](https://www2.webkit.org/show_bug.cgi?id=294338)

**macOS platform caveat**
- x86_64 apps under Rosetta can be clamped to 60 Hz on ProMotion; arm64 builds are not. — [Apple Developer Forums](https://developer.apple.com/forums/thread/800540)

**rVFC**
- `requestVideoFrameCallback` metadata includes `presentationTime`, `expectedDisplayTime`, `presentedFrames`, `processingDuration`, `captureTime`, `receiveTime` and `rtpTimestamp`. — [rVFC spec](https://wicg.github.io/video-rvfc/) (as cited in Cha note 05)

### Inferences
**What Chrome on ProMotion does for Cha's paths (inferred from source)**

| Stream rate | Display behaviour | Effect on Cha |
|---|---|---|
| 120 fps | Stays at 120 Hz | 1:1 cadence, no judder |
| 60 fps, page otherwise idle | `OnlyVideoMatcher` can pick a 16.7 ms interval. The Mac begin-frame source then subsamples vsyncs ×2, so the compositor latches at 60 Hz. | A frame arriving just after a latch waits up to 16.7 ms instead of 8.3 ms; the mean added wait roughly doubles, from about 4.2 to about 8.3 ms. |
| 90 fps | `HasSimpleCadence(8.33 ms, 11.1 ms)` fails, as do the 16.7/33 ms candidates, so it stays at the 120 Hz default. Each 3 frames span 4 vsyncs, shown for 8.3, 8.3 and 16.7 ms instead of 11.1 ms each. | Visible periodic judder at 30 Hz. ProMotion's hardware adaptive refresh is not used, because Chromium never requests arbitrary rates. |

- **The 60 fps case would be a latency regression.** Any on-page rAF loop or CSS animation breaks `has_only_content_frame_interval_updates` and keeps 120 Hz. A stats overlay or a tiny always-animating element could therefore pin 120 Hz. That is counter-intuitive but likely effective.
- **Recommendation: prefer 120 fps (or 60) over 90 fps** on 120 Hz ProMotion clients in Chrome. Treat 90 fps as a Windows/VRR-monitor option only.
- **Safari:** the default 60 fps preference throttles page rendering updates (rAF, canvas). That would cap PyroWave's canvas at about 60 Hz unless the user turns off the setting.
- **Safari `<video>`:** whether a `<video>` showing a MediaStream is also throttled is unverified. It likely is not if it is presented via its own layer (inference).
- **Windows:** with G-Sync/FreeSync "windowed + fullscreen" enabled in the driver, a borderless-fullscreen Chrome window may get driver-level VRR, but nothing in Chromium requests or paces for it.
- **Verify in Perfetto:** `ExternalBeginFrameSourceMac::SetPreferredInterval` (`vsync_subsampling_factor`) and the `FrameIntervalMatcherInputs` / `Viz.FrameIntervalDecider.ResultMatcherType` events show which rate Chrome chose during a Cha session.

### Gaps
- I could not read `webmediaplayer_ms_compositor.cc`; the fetch failed. That leaves three open questions:
  - What `GetPreferredRenderInterval()` returns for MediaStream video, and whether it reports the measured frame rate or "no preference", which decides whether `OnlyVideoMatcher` engages for Cha's paths 1 and 2.
  - Whether frames with WebRTC playout-delay 0 (or generator frames) bypass the smoothing `VideoRendererAlgorithm` and render the latest frame ASAP.
  - Whether presentation for MediaStream video is vsync-aligned.
- Whether `FrameIntervalDecider` matching is active by default on Mac in Chrome 154 (the Mac code implements the hooks) was not confirmed.
- I found no source that Chrome on macOS ever runs a ProMotion panel at a non-divisor rate (for example 90 Hz) for video.
- Safari 26's own video presentation pacing on ProMotion and its 2026 default for the 60 fps preference: 2026 how-to articles still describe turning it off manually, but there is no Apple statement.
- Firefox's frame pacing on ProMotion and with VRR was not researched; I found no sources.

---

## 6. Measurements of browser HDR decode/present latency and of judder/latency with and without VRR

### Takeaway
I found **no published independent measurement** of browser HDR decode or present latency, of FP16 canvas present cost, or of browser judder or latency with versus without VRR. The only hard numbers available are Cha's own SDR measurements, single-blog throughput figures and anecdotal forum reports. Cha will have to produce these numbers itself.

### Cited Findings
- Cha's own SDR measurements: about 3 ms send→compositor on the WebCodecs → `MediaStreamTrackGenerator` → `<video>` path, and 7.3–8.9 ms for the WebGPU canvas present path versus 2.7–3.4 ms for `<video>`. — [Cha note 11 §5](../../../docs/research/11-upscaling-and-frame-generation/browser.md) (internal measurement)
- [2025–2026] Chrome on an M4 MacBook Pro draws about 1230 fps via `importExternalTexture` versus about 1120 fps via `ImageBitmapRenderingContext` (single-blog throughput, 8-bit). — [WebCodecs Fundamentals: Rendering](https://webcodecsfundamentals.org/basics/rendering/) (via Cha note 11)
- [older: 2025-10] Constructing a `VideoFrame` from an OffscreenCanvas in Safari costs far more CPU than from a regular canvas. — [WebKit bug 300579](https://bugs.webkit.org/show_bug.cgi?id=300579) (via Cha note 11)
- TestUFO's G-SYNC page is a software interpolation simulation of VRR, not a measurement of browser VRR. — [TestUFO G-SYNC](https://www.testufo.com/gsync?embed=1)
- [older] The only VRR-in-browser evidence found is a single-user report of refresh-rate fluctuation with G-SYNC on. — [Blur Busters forum](https://forums.blurbusters.com/viewtopic.php?p=31144)

### Inferences
**Measurement plan.** Cha's existing photodiode and rVFC harness (note 05 §T10) should add:
- (a) HEVC Main10 PQ versus HEVC Main 8-bit on path 2, same content, Chrome macOS. Compare rVFC `presentationTime − receiveTime/captureTime` and photodiode latency.
- (b) PyroWave canvas `bgra8unorm` versus `rgba16float` + extended, same content.
- (c) 60/90/120 fps streams on ProMotion, with a Perfetto trace of `vsync_subsampling_factor`, measuring frame-time variance from rVFC `presentedFrames` deltas.
- (d) Windows Chrome on a G-Sync monitor, windowed versus borderless fullscreen, with driver "G-SYNC for windowed mode" on and off.

**Where HDR latency might come from.** The 10-bit hardware decode on VideoToolbox should cost about the same as 8-bit (same fixed-function block). Any HDR latency penalty would therefore most likely come from the compositor, not the decoder: a lost overlay leading to GPU composition with tone mapping (inference).

### Gaps
- No HDR-versus-SDR latency data exists for any browser.
- No VRR-versus-fixed judder or latency data exists for any browser.
- No published FP16 canvas present-cost data exists.

---

## 7. Synthesis: can each Cha Portal path show an HDR10 stream, per browser?

### Takeaway
Ranked by confidence on the primary target (Chrome macOS):

| Rank | Path | Verdict on Chrome macOS |
|---|---|---|
| 1 | Path 3 (PyroWave → WebGPU canvas) | Feasible today in Chrome: rgba16float + extended tone mapping, with Cha doing PQ decode and roll-off in its own shader. It pays the known extra canvas present cost. |
| 2 | Path 2 (WebCodecs → `MediaStreamTrackGenerator` → `<video>`) | Likely feasible with 10-bit codec strings. The frame carries PQ colour and metadata end to end in Chromium's code, but EDR display is unverified and must be tested. It is the best latency candidate. |
| 3 | Path 1 (WebRTC) | Only with hardware 10-bit decode plus the colour-space header extension. AV1 10-bit has no software fallback, and HEVC Main10 over WebRTC is undocumented. |

On Safari and Firefox all three paths are weaker or unverified.

### Cited Findings
- **Path 3 support:** WebGPU canvas extended tone mapping shipped in Chrome 128/129 (not ChromeOS at ship time). — [blink-dev intent](https://groups.google.com/a/chromium.org/g/blink-dev/c/rBQIRHUEAe8); [New in WebGPU 129](https://developer.chrome.com/blog/new-in-webgpu-129)
- **Path 3 code today:** Cha's PyroWave canvas is configured with `getPreferredCanvasFormat()` and `alphaMode: "opaque"`, with no `toneMapping` (`web/packages/player/src/pyro.ts` lines 79–80). On macOS the preferred format is 8-bit, so it is SDR-clamped today (spec default `"standard"`). — [WebGPU ED §21.5](https://gpuweb.github.io/gpuweb/)
- **Path 2 plumbing:** Chromium's VideoToolbox decoder propagates bitstream colour space and HDR metadata into frames and IOSurfaces. GPU 10-bit `VideoFrame`s have no JS pixel format and no readback. — [video_toolbox_video_decoder.cc](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/mac/video_toolbox_video_decoder.cc); [webcodecs/video_frame.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/modules/webcodecs/video_frame.cc)
- **Path 1 limits:** HDR reinit happens via the hardware adapter, and dav1d accepts 8-bit only. — [rtc_video_decoder_adapter.cc](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/platform/peerconnection/rtc_video_decoder_adapter.cc); [dav1d_decoder.cc](https://webrtc.googlesource.com/src/+/main/modules/video_coding/codecs/av1/dav1d_decoder.cc)
- **Safari:** WebGPU HDR canvas is claimed in 26.0 but was reported not displayed on macOS in 2025. 10-bit `importExternalTexture` is misread. — [Safari 26.0 release notes](https://developer.apple.com/documentation/safari-release-notes/safari-26-release-notes); [WebKit bug 272702](https://bugs.webkit.org/show_bug.cgi?id=272702); [Silo PR #2544](https://github.com/Silo-Server/silo-server/pull/2544)
- **Firefox:** WebCodecs colour metadata is guessed or empty in several paths. Windows HDR video arrived in mid-2026 without metadata use. — [Mozilla bug 2057759](https://bugzilla.mozilla.org/show_bug.cgi?id=2057759); [Gigazine](https://gigazine.net/gsc_news/en/20260715-firefox-windows-hdr-video/)

### Inferences
**Recommended order of work**
1. Path 2 HDR prototype on Chrome macOS: HEVC Main10 PQ with HDR10 SEI → `hev1.2.4.L153.B0` → `MediaStreamTrackGenerator` → `<video style="dynamic-range-limit:no-limit">`. Check for EDR highlights and Perfetto overlay status.
2. Path 3 FP16 + extended canvas with a PQ→extended-sRGB/P3 shader. Measure the FP16 present cost.
3. Windows Chrome path 2 with Windows HDR on: P010 → HDR10 swap chain.
4. Path 1 only if a WebRTC-only client must have HDR.

**Refresh-rate policy**
- On ProMotion in Chrome, stream at 120 (or 60) fps rather than 90 to avoid 1-1-2 judder.
- Watch for Chrome subsampling to 60 Hz on 60 fps streams when the page is otherwise idle; it would add up to one extra 8.3 ms vsync of wait.
- In Safari, warn users that the default 60 fps rendering preference caps the canvas path.

### Gaps
- The decisive unknowns are:
  - HDR/EDR display of generator-fed `<video>` in Chrome macOS and Windows.
  - Chrome's `importExternalTexture` behaviour for PQ.
  - Whether `OnlyVideoMatcher` engages for MediaStream video.
  - Safari's macOS WebGPU HDR display.
- All of them need hands-on testing; no public source resolves them.
