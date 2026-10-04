# 05: Browser platform for ultra-low-latency streaming, and how commercial cloud gaming does it

Research pass for **Cha Portal** (portal.cha.sh). Written 2026-10-03. Every claim has a source link inline. Anything I could not confirm from a primary source is tagged **(unverified)**.

Browser versions current on 2026-10-03:
- **Chrome 154** is stable. It shipped 2026-09-22. Chrome now ships every **2 weeks**: 155 on 10-06, 156 on 10-20 ([chromiumdash](https://chromiumdash.appspot.com/fetch_milestone_schedule), [BCD browsers/chrome.json](https://github.com/mdn/browser-compat-data/blob/main/browsers/chrome.json)).
- **Firefox 157** is stable. It shipped 2026-09-29, and Firefox is also on a 2-week cadence ([product-details](https://product-details.mozilla.org/1.0/firefox_versions.json)).
- **Safari 27.0** is stable. It shipped 2026-09-14 ([BCD browsers/safari.json](https://github.com/mdn/browser-compat-data/blob/main/browsers/safari.json)).

Support data comes from MDN browser-compat-data (BCD) `main` as fetched today, plus chromestatus and Chromium/libwebrtc source.

---

## TL;DR

1. **WebTransport is "Baseline 2026" on paper, but not in practice on Safari.** Safari 26.4 shipped it in March 2026 ([WebKit](https://webkit.org/blog/17862/webkit-features-for-safari-26-4/), [caniuse](https://caniuse.com/webtransport)). Two problems remain:
   - WebKit's implementation **deadlocks after about 16 MiB of stream data or about 7,600 streams per session**. This is [WebKit bug 319818](https://bugs.webkit.org/show_bug.cgi?id=319818), still NEW. Because of it, moq.dev **disables WebTransport on every WebKit engine** ([source](https://github.com/moq-dev/moq/blob/main/js/net/src/connection/browser.ts)).
   - Safari's handshake also needs draft-14/16 flow-control capsules that several server libraries don't send ([hyperium/h3#347](https://github.com/hyperium/h3/issues/347)).
   - **Treat WebTransport as a Chromium and Firefox ≥153 path for LAN and power users, not the universal default.**
2. **WebRTC stays the universal, NAT-proof baseline.** It is also the only transport that commercial browser cloud gaming uses:
   - GeForce NOW uses WebRTC to ICE-lite servers, with NVST data channels that include a *partially reliable* gamepad channel ([OpenNOW](https://github.com/OpenCloudGaming/OpenNOW)).
   - Xbox Cloud Gaming uses WebRTC media plus `input`, `control`, `message` and `chat` data channels ([xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player)).
   - In Chrome, the sender stamping **`playout-delay` min=0/max=0** switches libwebrtc to "render ASAP" (`UseLowLatencyRendering`). Blink then uses `LowLatencyVideoRendererAlgorithm` ([timing.cc](https://webrtc.googlesource.com/src/+/main/modules/video_coding/timing/timing.cc)).
3. **For LAN and WAN parity with restrictive NATs, the fallback chain has to be:**
   1. WebRTC over direct UDP
   2. WebRTC over TURN-UDP
   3. TURN-TCP or TURN-TLS on port 443
   4. WebSocket over 443

   WebTransport is an *upgrade* when the node is directly reachable. It has no ICE and no NAT traversal, and Chromium has **no HTTP/2 fallback** ([chromestatus](https://chromestatus.com/feature/5094058497277952)).
4. **Decode.** WebCodecs is everywhere except Firefox for Android:
   - Chrome 94, Firefox 130, Safari 16.4. AudioDecoder arrived in Safari 26.
   - Real-world coverage in 2026: AV1 is about 91% on Chrome and Firefox, but only about 27–33% on Safari. HEVC is near-universal on Safari, about 57–97% on Chromium, and almost nil on Firefox ([dataset](https://webcodecsfundamentals.org/datasets/codec-analysis-2026/)).
   - For 1-in-1-out hardware decode, the bitstream must carry H.264/HEVC VUI `bitstream_restriction` with `max_num_reorder_frames=0`, as Sunshine does ([cbs.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/cbs.cpp)). Without it, hardware decoders buffer about 4 frames ([w3c/webcodecs#732](https://github.com/w3c/webcodecs/issues/732)).
5. **Render.** Use WebGPU `importExternalTexture(VideoFrame)` in a worker. Fall back to WebGL2 with `desynchronized:true` (honoured on Windows and ChromeOS only), or to MediaStreamTrackGenerator/VideoTrackGenerator feeding `<video>`.
   - WebGPU ships in Chrome on Windows, macOS and ChromeOS, and on Linux only for Intel Gen12+ (Chrome 144) and NVIDIA on Wayland (Chrome 147). **AMD on Linux still needs a flag.**
   - WebGPU ships in Firefox on Windows (141) and on Apple Silicon Macs (145/147). **Firefox has no WebGPU on Linux.**
   - WebGPU ships in Safari 26.
   - **Subgroups are Chrome-only (134+)**, which affects the Pyrowave decoder ([gpuweb status](https://github.com/gpuweb/gpuweb/wiki/Implementation-Status), BCD).
6. **Input.**
   - Pointer Lock `unadjustedMovement`:
     - Chrome 88, not on Linux.
     - Firefox 152 and Safari 18.4.
     - Chrome for Android 144.
     - No pointer lock at all on iOS.
   - Keyboard capture is **split**:
     - Chrome has `navigator.keyboard.lock()`.
     - Firefox 151 and Safari 26.4 have `requestFullscreen({keyboardLock:"browser"})`. Chrome doesn't support that form; it is an Interop 2027 proposal.
   - Gamepad:
     - Chromium polls internally at **4 ms (250 Hz)** ([source](https://chromium.googlesource.com/chromium/src/+/main/device/gamepad/gamepad_provider.cc)).
     - Event-driven `rawgamepadinputchange` is in an origin trial from M149 to M157.
     - Trigger rumble: Chrome 126+ only.
     - Gyro, touchpad, adaptive triggers and lightbar need WebHID, which is Chromium desktop only.
7. **iPhone is the weakest client.** It has no element fullscreen, no pointer lock, no keyboard lock and no gamepad haptics, and WebTransport is unusable there. Ship a PWA, use WebRTC, and use touch/trackpad modes.
8. **LAN gotcha: Chrome Local Network Access (LNA).** It now gates **WebSocket and WebTransport** to private IPs from a public origin (Chrome 147+), with a permission prompt. WebRTC is out of scope. A public `portal.cha.sh` connecting to `192.168.x.x` nodes will trigger the prompt ([LNA blog](https://developer.chrome.com/blog/local-network-access), [radar#29](https://github.com/getsentry/browser-updates-radar/issues/29)).
9. **L4S/ECN.**
   - Chrome **reports ECN in QUIC ACKs by default** ([chromestatus](https://chromestatus.com/feature/5205722919600128)). A WebTransport server can therefore run an ECN- or L4S-aware controller today.
   - In WebRTC, RFC 8888 feedback and SCReAMv2 are still **field trials** in libwebrtc ([field_trials.py](https://webrtc.googlesource.com/src/+/main/experiments/field_trials.py)).
10. **Recommended stack (details in §9).**
    - **Tier A, default everywhere:** WebRTC from a str0m- or Pion-based node. Video on an RTP track with playout-delay 0/0. Input on unordered/unreliable plus reliable data channels.
    - **Tier B, Chromium and Firefox, direct reachability:** WebTransport datagrams with a custom framing, WebCodecs or WebGPU decode (Pyrowave), and an app-level delay-based CC.
    - **Tier C:** TURN-TLS on 443, then WebSocket.
    - **Measure:** frame-ID strip plus a per-frame timestamp trace plus a photodiode click-to-photon rig.

---

## 1. Browser support matrix (2026-10-03)

Legend: ✅ = shipped by default (version shown). ⚠️ = partial or caveat. ❌ = not supported. Data is from [BCD](https://github.com/mdn/browser-compat-data) unless another source is linked. "Edge" mirrors Chrome.

| Capability | Chrome / Edge (desktop) | Chrome Android | Firefox desktop | Firefox Android | Safari macOS | Safari iOS/iPadOS |
|---|---|---|---|---|---|---|
| WebTransport | ✅ 97 | ✅ | ✅ 114 (<153: only two concurrent remote-initiated streams, [moq note](https://github.com/moq-dev/moq/blob/main/js/net/src/connection/browser.ts)) | ✅ | ⚠️ 26.4: flow-control stall ([WebKit 319818](https://bugs.webkit.org/show_bug.cgi?id=319818)) | ⚠️ 26.4: same |
| WT `serverCertificateHashes` | ✅ 100 | ✅ | ✅ 125 | ✅ | ✅ 26.4 per BCD (independent reports unclear (unverified)) | ✅ 26.4 per BCD |
| WT `congestionControl` option | ❌ (chromestatus "Proposed", [5180947573112832](https://chromestatus.com/feature/5180947573112832)) | ❌ | ✅ 114 (effect unverified) | ✅ | ✅ 26.4 | ✅ 26.4 |
| WT HTTP/2 fallback | ❌ ([chromestatus](https://chromestatus.com/feature/5094058497277952)) | ❌ | ❌ (preview only) | ❌ | ✅ (WebKit 26.4 notes) | ✅ |
| WT `getStats()` | ❌ (BCD) | ❌ | ⚠️ throws | ⚠️ | BCD lists 26.4; moq.dev reports none (unverified) | same |
| WebCodecs VideoDecoder | ✅ 94 | ✅ | ✅ 130 | ❌ | ✅ 16.4 | ✅ 16.4 |
| WebCodecs AudioDecoder (Opus) | ✅ 94 | ✅ | ✅ 130 | ❌ | ✅ 26 | ✅ 26 |
| WebGPU | ✅ Win, macOS, ChromeOS 113. Linux: Intel Gen12+ 144, NVIDIA on Wayland 147, AMD needs a flag | ✅ 121 (Android 12+) | ⚠️ Win 141. Apple Silicon 145 on macOS 26, 147 on all versions. No Linux, no Intel Mac | ❌ (flag) | ✅ 26 | ✅ 26 |
| WebGPU `subgroups` | ✅ 134 | ✅ 134 | ❌ | ❌ | ❌ (position "Support") | ❌ |
| `importExternalTexture(VideoFrame)` | ✅ 116 (zero-copy) | ✅ | ⚠️ 144 (Windows) | ❌ | ✅ 26 | ✅ 26 |
| Canvas `desynchronized` | ⚠️ 81, effective on Windows and ChromeOS only | ? | ❌ | ❌ | ❌ | ❌ |
| MediaStreamTrackGenerator (window) | ✅ 94 | ✅ | ❌ | ❌ | ❌ | ❌ |
| VideoTrackGenerator (worker) | ❌ | ❌ | ❌ | ❌ | ✅ 18 | ✅ 18 |
| `requestVideoFrameCallback` | ✅ 83 | ✅ | ✅ 132 | ✅ | ✅ 15.4 | ✅ 15.4 |
| RTCRtpReceiver `jitterBufferTarget` | ✅ 124 | ✅ | ✅ 115 | ✅ | ✅ 27 | ✅ 27 |
| playout-delay RTP header extension | ✅ (render ASAP at 0/0) | ✅ | ✅ negotiation since 71 ([bug 1585009](https://bugzilla.mozilla.org/show_bug.cgi?id=1585009)) | ✅ | reported to work (unverified) | (unverified) |
| RTCRtpScriptTransform (encoded transform V2) | ✅ 141 (legacy `createEncodedStreams` 86) | ✅ | ✅ 117 | ✅ | ✅ 15.4 | ✅ 15.4 |
| RTCDataChannel transferable to worker | ✅ 130 | ✅ | ✅ 144 | ✅ | ✅ 15 | ✅ 15 |
| WebRTC HEVC | ✅ 136 (hardware only, [chromestatus](https://chromestatus.com/feature/5153479456456704)) | ✅ 136 | ❌ | ❌ | ✅ (shipped per chromestatus) | ✅ |
| Pointer Lock | ✅ 37 | ✅ 144 | ✅ 50 | ? | ✅ 10.1 | ❌ |
| Pointer Lock `unadjustedMovement` | ⚠️ 88 (no Linux) | ? | ✅ 152 | ? | ✅ 18.4 | ❌ |
| `pointerrawupdate` | ✅ 77 (secure context from 142) | ✅ | ✅ 148 | ? | ❌ | ❌ |
| `getCoalescedEvents` / predicted events | ✅ 58 / 77 | ✅ | ✅ 59 / 89 | ✅ | ✅ 18.2 | ✅ 18.2 |
| `navigator.keyboard.lock()` | ✅ 68 | n/a | ❌ | n/a | ❌ | ❌ |
| `requestFullscreen({keyboardLock})` | ❌ | ❌ | ✅ 151 | ? | ✅ 26.4 | ❌ |
| Keyboard Map (`getLayoutMap`) | ✅ 69 | ✅ | ❌ | ❌ | ❌ | ❌ |
| Element fullscreen | ✅ 71 | ✅ | ✅ 64 | ✅ | ✅ 16.4 | ⚠️ iPad only: undismissable overlay, swipe-down exits. **None on iPhone** |
| Gamepad API | ✅ | ✅ | ✅ | ✅ 32 | ✅ 10.1 | ✅ |
| `vibrationActuator` dual-rumble | ✅ 68 | ⚠️ Android 12+ in development | ❌ | ❌ | ✅ 16.4 | ❌ |
| `trigger-rumble` | ✅ 126 | ❌ | ❌ | ❌ | ❌ (Safari "In development") | ❌ |
| `rawgamepadinputchange` | Origin trial M149–M157 ([blink-dev](http://www.mail-archive.com/blink-dev@chromium.org/msg17328.html)) | OT | ❌ | ❌ | ❌ | ❌ |
| WebHID (DualSense gyro, touchpad, triggers, lightbar) | ✅ 89 (dedicated workers from 131) | ❌ | ❌ | ❌ | ❌ | ❌ |
| Screen Wake Lock | ✅ 84 | ✅ | ✅ 126 | ✅ | ✅ 16.4 | ✅ 18.4 (Home Screen apps from 18.4) |
| Window Management (`getScreenDetails`) | ✅ 100 | ❌ | ❌ | ❌ | ❌ | ❌ |
| Async Clipboard read/write | ✅ 66/76 (permission) | ✅ | ✅ 125/127 (gesture plus paste prompt) | ✅ | ✅ 13.1 (gesture) | ✅ |
| `clipboardchange` event | ✅ 144 (needs sticky activation or permission from 145) | ? | ❌ | ❌ | ❌ | ❌ |
| AudioContext `outputLatency` | ✅ 102 | ✅ | ✅ 70 | ✅ | ✅ 18.4 | ✅ 18.4 |
| Local Network Access prompt | ✅ fetch 142, WS/WT 147, WebRTC exempt | ✅ | ❌ | ❌ | ❌ | ❌ |

---

## 2. Transport

### 2.1 WebRTC: the latency knobs that matter

**Playout delay: the single most important knob for Chromium receivers.**
- The `http://www.webrtc.org/experiments/rtp-hdrext/playout-delay` extension is set by the **sender** (our node). A value of 0/0 means "render as soon as possible" ([libwebrtc doc](https://webrtc.googlesource.com/src/+/main/docs/native-code/rtp-hdrext/playout-delay/README.md)).
- In current libwebrtc, `VCMTiming::VideoDelayTimings::UseLowLatencyRendering()` is true when `min_playout_delay == 0 && max_playout_delay <= 500 ms`. In that case `RenderTime()` returns `Timestamp::Zero()` ("Render as soon as possible"), and the frame carries `use_low_latency_rendering` and `max_composition_delay_in_frames` ([timing.cc](https://webrtc.googlesource.com/src/+/main/modules/video_coding/timing/timing.cc)).
- Blink renders such frames through `LowLatencyVideoRendererAlgorithm`. That algorithm enters a drain mode that drops every second frame if the queue grows. Its `average_frame_duration()` is **hard-coded to 60 fps** (TODO crbug 1138888) ([header](https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/modules/mediastream/low_latency_video_renderer_algorithm.h)). Validate 120/144 fps behaviour empirically **(unverified impact)**.
- The extension must be negotiated in SDP (`a=extmap`). If the line disappears, the value is ignored ([lazyharu](https://lazyharu.com/en/webrtc-playout-delay/)).
- Server-side support:
  - **str0m** has `Extension::PlayoutDelay` and `AbsCaptureTime`.
  - **Pion** has `playoutdelayextension.go` and `abscapturetimeextension.go` in `pion/rtp`.
- Firefox negotiates the extension since 71 ([bug 1585009](https://bugzilla.mozilla.org/show_bug.cgi?id=1585009)). Firefox uses libwebrtc's timing too, but its compositor path is separate **(unverified that it renders ASAP)**.

**`jitterBufferTarget`** is the receiver-side hint: Chrome 124, Firefox 115, and new in **Safari 27** ([WebKit 27](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/)).
- Setting it to `0` asks for the minimum buffer. Moonlight Web Stream does exactly `receiver.jitterBufferTarget = 0` (plus the legacy `playoutDelayHint`).
- It does **not** by itself trigger the render-ASAP path. Use both.
- Safari 27 also added a `targetLatency` receiver attribute ([WWDC26 post](https://webkit.org/blog/17967/news-from-wwdc26-webkit-in-safari-27-beta/)).

**Loss recovery.** RTX/NACK is standard everywhere.
- libwebrtc **always accepts `flexfec-03` as a receive codec**. Sending it is behind `WebRTC-FlexFEC-03-Advertised` ([webrtc_video_engine.cc](https://webrtc.googlesource.com/src/+/main/media/engine/webrtc_video_engine.cc)). So a node can send FlexFEC to Chrome if Chrome's receive capabilities include it (Pion ships a `flexfec` interceptor).
- RED/ULPFEC is also in the default format list.
- Safari and Firefox FlexFEC receive: **(unverified)**.
- For game streaming on the WAN, use NACK first (RTT is small) and add FEC when loss exceeds about 1%, or on links with long RTT.

**Congestion control.**
- Every browser runs libwebrtc's GCC with transport-wide CC (TWCC) feedback. The *sender* (our node) owns the controller, which is good: we can choose.
- str0m has TWCC plus BWE. Pion has `gcc`, `twcc`, `rfc8888`, `ccfb` and `pacing` interceptors (pion/interceptor `pkg/`).
- RFC 8888 CCFB (the ECN-capable feedback L4S needs) is still the `WebRTC-RFC8888CongestionControlFeedback` field trial (expiry 2027-02-01). SCReAMv2 exists in libwebrtc as the `WebRTC-Bwe-ScreamV2` trial (expiry 2026-12-30) ([field_trials.py](https://webrtc.googlesource.com/src/+/main/experiments/field_trials.py)).
- **Conclusion: L4S over browser WebRTC is not available by default in Oct 2026.** An MMSys'26 prototype showed 10× lower queuing delay with RFC 8888 plus Prague in-browser ([ACM](https://dl.acm.org/doi/10.1145/3793853.3798195)).

**Encoded transforms.** `RTCRtpScriptTransform` is in all engines (Chrome 141 for the V2 spec; Chrome 86 legacy `createEncodedStreams`).
- Useful for per-frame metadata: frame IDs and server timestamps appended to the payload and stripped on receive. This is ideal for latency tracing.
- Not useful for custom codecs: frames still go to the built-in decoder.

**Data channels for custom codecs or bypassing the jitter buffer.**
- `ordered:false, maxRetransmits:0` gives UDP-like semantics over SCTP/DTLS.
- Channels are transferable to workers (Chrome 130, Firefox 144, Safari 15), which keeps the main thread out of the receive path.
- Parsec's early web client sent video over data channels ([Parsec blog](https://parsec.app/blog/game-streaming-tech-in-the-browser-with-parsec-5b70d0f359bc)). Xbox and GFN use data channels for input.
- Keep messages smaller than the path MTU (about 1,150 B) so one lost SCTP chunk doesn't drop a whole message.
- SCTP throughput ceilings at 200+ Mb/s (Pyrowave rates) are **(unverified); benchmark them**.
- Chrome is prototyping SCTP Negotiation Acceleration (SNAP) to remove two RTTs from data-channel setup ([chromestatus](https://chromestatus.com/feature/5137946677215232), Proposed).

**ICE and NAT for nodes.**
- Use **ICE-lite** on nodes with public or LAN IPs, as GFN does ([OpenNOW-vita](https://github.com/OpenCloudGaming/OpenNOW-vita)).
- Chrome hides the browser's host candidates behind mDNS `.local` names. A server-side ICE agent therefore normally learns the browser via **peer-reflexive** candidates. Make sure the node accepts prflx candidates.
- Offer **ICE-TCP passive** candidates on the node (Pion `ICETCPMux`; str0m supports TCP) for UDP-blocked networks.
- Fall back to TURN on UDP 3478, TCP 3478 and TLS 443 (Moonlight Web Stream documents the same ladder: [README](https://github.com/MrCreativ3001/moonlight-web-stream)).

### 2.2 WebTransport

**Status**
- Chrome 97, Firefox 114 and Safari 26.4, so Baseline since March 2026 ([caniuse](https://caniuse.com/webtransport), [webrtc.ventures](https://webrtc.ventures/2026/04/webtransport-is-now-baseline-what-it-means-for-real-time-media/)).
- IETF `draft-ietf-webtrans-http3` is at **-16** (2026-07-06), and `-http2` at -15 ([datatracker](https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/)).

**Upcoming Chromium API work** (all "Proposed" on chromestatus):
- `congestionControl`
- datagram writable streams with `sendGroup`/`sendOrder` prioritization ([5183354944225280](https://chromestatus.com/feature/5183354944225280))
- `reliability`/`supportsReliableOnly` (M158)
- `draining`
- BYOB datagram readable

**Safari realities (most important)**
- **Handshake.** Network.framework sends a *hybrid* SETTINGS set: H3_DATAGRAM, draft-07 `0xc671706a` max sessions, and draft-14 `WT_INITIAL_MAX_*`. It does **not act on initial flow-control settings**: it waits for `WT_MAX_DATA` / `WT_MAX_STREAMS_*` capsules on the CONNECT stream. Without them, `ready` never resolves, or `createBidirectionalStream()` resolves but carries nothing ([h3#347](https://github.com/hyperium/h3/issues/347), [fails-components#490](https://github.com/fails-components/webtransport/issues/490)).
  - It also requires `SETTINGS_WT_MAX_SESSIONS` (`0x14e9cd29`) ≥ 1.
  - **webtransport-go** implemented draft-16 with `WT_MAX_DATA`/`WT_MAX_STREAMS` capsules in June–July 2026 (commits #311, #332, #333, #340). It is the most likely Safari-compatible server **(not tested)**.
  - **wtransport** (Rust) only advertises the draft-07 `0xc671706a` setting (`wtransport-proto/src/settings.rs`), so it probably fails with Safari **(inferred)**.
  - **h3-webtransport** is known broken with Safari.
- **Long-session stall.** [WebKit bug 319818](https://bugs.webkit.org/show_bug.cgi?id=319818) "[WebTransport] Flow control never refills" is NEW, P2, filed 2026-07-20. Safari doesn't return flow-control credit when streams FIN/RESET, so sessions freeze after about 16 MB or about 7,600 streams. moq.dev therefore gates on `engine === WebKit || OS === iOS` and falls back to WebSocket ([browser.ts](https://github.com/moq-dev/moq/blob/main/js/net/src/connection/browser.ts), [doc.moq.dev](https://doc.moq.dev/lib/js/)).
  - Datagrams are not QUIC flow-controlled, so a datagram-heavy design would last longer on Safari, but the control streams still leak credit. **Don't rely on Safari WebTransport until the bug is fixed.**
- **No usable error messages.** Safari leaves `WebTransportError.message` empty (moq `error.ts`).

**Firefox:** before 153 it allowed only **two concurrent remote-initiated streams** ([bug 2046262](https://bugzilla.mozilla.org/show_bug.cgi?id=2046262), via moq).

**`serverCertificateHashes`** (MDN [constructor](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport)):
- Requires `allowPooling:false`.
- Requires an X.509v3 certificate with **validity under 2 weeks**.
- **ECDSA P-256** is the interop baseline. **RSA is not allowed.**
- Hash is SHA-256.
- Chrome 100, Firefox 125, Safari 26.4 (BCD).
- For homelab nodes without public DNS, the node mints a short-lived self-signed P-256 certificate and rotates it about weekly. The portal hands the hash to the browser over the authenticated control channel.
- Alternative: give each node a real certificate via ACME DNS-01 under a cha.sh subdomain that resolves to the node's private IP (the plex.direct pattern). Watch out for **DNS-rebind protection** on home routers (dnsmasq, pfSense, Pi-hole), which blocks public names resolving to RFC1918 **(well-known behaviour, unverified for specific defaults)**.

**Corporate networks.** WebTransport needs outbound UDP 443.
- Chromium has **no HTTP/2 fallback**, so on UDP-blocked networks or behind explicit HTTP proxies it simply fails. Expect proxies not to tunnel QUIC **(unverified for MASQUE-capable proxies)**.
- Safari *does* fall back to HTTP/2 (WebKit 26.4 notes), but then it is TCP with head-of-line (HOL) blocking.

**No NAT traversal.** WebTransport is client→server. A homelab node behind NAT needs one of three things:
- a port forward or UPnP
- an overlay (Tailscale/WireGuard)
- a **public relay**: the node keeps an outbound QUIC connection to a relay on the portal host, which forwards datagrams and streams

WebRTC with TURN covers this without extra infrastructure, which is the main reason it stays tier A.

**Chrome LNA.** Since Chrome 147, WebSocket and WebTransport connections from a public origin to a local or private address need the "local network" permission. Chrome 142 already did this for fetch. Chrome 156 removes the enterprise opt-out policy. WebRTC is out of scope ([LNA](https://developer.chrome.com/blog/local-network-access), [radar#29](https://github.com/getsentry/browser-updates-radar/issues/29)).
- A self-hosted portal on the same LAN (private-to-private) is unaffected.
- A public portal reaching LAN nodes gets one prompt per origin.
- Chrome 154 is adding `targetAddressSpace` for WebSocket ([chromestatus](https://chromestatus.com/feature/4779920606756864)).

**ECN.** Chrome's QUIC stack reports ECN codepoints in ACK frames on Apple, Windows, Linux and Android ([QUIC ACK-ECN](https://chromestatus.com/feature/5205722919600128)). For server→browser video, the node can mark ECT(1) and run an L4S-style controller using the browser's ACK-ECN feedback. This is the only practical L4S path to a browser today.

**Congestion control over WebTransport.** QUIC DATAGRAM frames *are* congestion controlled by the server's QUIC CC (RFC 9221), so the server's QUIC CC choice matters:
- quinn ships `cubic`, `new_reno` and `bbr` (`quinn-proto/src/congestion/`).
- quiche ships Google's gcongestion BBR2 **(unverified version)**.
- Cubic will happily fill bloated buffers. Layer a **media-aware rate controller** on top:
  - **GCC-style.** The browser worker timestamps each datagram on arrival (`performance.now()`), batches `(seq, arrival_ts)` feedback every 20–50 ms over a datagram or stream, and the node runs a delay-gradient estimator. Port it from Pion `gcc` (MIT), str0m BWE (MIT/Apache) or SCReAM (BSD, [EricssonResearch/scream](https://github.com/EricssonResearch/scream); SCReAMv2 is `draft-ietf-ccwg-rfc8298bis-screamv2-01`).
  - Use QUIC-level `rtt`/`cwnd`/ECN-CE stats from the server stack as a second signal.
  - BBR (`draft-ietf-ccwg-bbr-06`) is a reasonable QUIC-level default.
  - JS arrival timestamps include network-service→renderer IPC jitter, so smooth over multiple packets.
- Moonlight/Sunshine itself uses a fixed bitrate plus Reed-Solomon FEC, with no real-time CC (see 03). That is fine on a LAN and fragile on the WAN.

**Framing.** Max datagram payload is about 1,200 B minus QUIC/H3 overhead. Expose `datagrams.maxDatagramSize` and fragment frames like RTP (frame-id, frag-idx, frag-count, FEC group).
- The alternative is moq-lite style: **one unidirectional stream per frame or GOP** with `sendOrder` priority and resetting stale streams. That is good for Chrome and Firefox, **bad on Safari** (stream-count stall).
- BrowserPane uses reliable envelopes for keyframes and control, and raw datagrams for H.264 delta fragments ([BrowserPane](https://github.com/ITmedes/BrowserPane), AGPL-3.0).

### 2.3 WebSocket fallback

TCP has HOL blocking but works through every proxy on 443. **Selkies 2.0 now defaults to WebSockets plus WebCodecs**, with WebRTC opt-in ([docs](https://selkies-project.github.io/selkies/)). Moonlight Web Stream also offers a WebSocket transport as its restrictive-network fallback.
- Use the same framing as WebTransport so client code is shared.
- Add app-level frame-ACK backpressure and aggressive drop-to-keyframe (see 04 §"frame-ACK backpressure").

### 2.4 Media over QUIC (MoQ)

**Status**
- `draft-ietf-moq-transport-22` (2026-10-01) is a WG document. The milestone to request IESG publication is Dec 2026 ([datatracker](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/)).
- `draft-lcurley-moq-lite-06` (2026-09-24).
- Cloudflare runs MoQ relays (draft-14/16) with a provisioning API ([blog](https://blog.cloudflare.com/moq-relays/)).
- Eleven vendors showed interop at NAB 2026 ([forasoft](https://www.forasoft.com/learn/video-streaming/articles-streaming/media-over-quic-moq)).

**moq-dev/moq** (MIT/Apache, very active, `moq-relay-v0.17.0`):
- npm `@moq/net`, `@moq/hang`, `@moq/watch`, `@moq/publish`.
- WebTransport on Chrome and Firefox ≥153, **WebSocket fallback on Safari and iOS**.

**Verdict.** MoQ solves **fan-out and relays**, which is not Cha Portal's problem (1:1, homelab or small group). Its stream-per-group model is also the worst case for WebKit's bug.
- Borrow ideas: priorities, group/frame model, the relay for NAT'd nodes.
- Don't adopt MoQ as the interactive transport.
- Revisit for spectator or "watch my session" mode.

---

## 3. Decode

### 3.1 WebCodecs VideoDecoder

**`optimizeForLatency:true`** only minimizes frames held before output. In Chromium it mostly disables frame-threading in **software** decoders. Hardware decoders may still hold frames.
- Reported issue: H.264 Main from iOS needs 4 frames queued after each IDR on hardware decoders. Baseline profile, or `prefer-software`, fixes it ([w3c/webcodecs#732](https://github.com/w3c/webcodecs/issues/732)).
- A 3-second delay on macOS with `avc1.64001F` is still open ([#899](https://github.com/w3c/webcodecs/issues/899), 2025-07).

**Fix it in the bitstream.** Sunshine rewrites SPS VUI with `bitstream_restriction_flag=1`, `max_num_reorder_frames=0` and `max_dec_frame_buffering=max_num_ref_frames`, and does the equivalent for HEVC ([cbs.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/cbs.cpp)). Do the same in the node encoder path. Also:
- no B-frames
- Annex-B with in-band SPS/PPS on IDR
- low-delay HRD
- for AV1, `reduced_still_picture_header`-free low-delay config and no frame reordering, i.e. `enable_order_hint` without hidden frames **(encoder-specific)**

**`hardwareAcceleration`.**
- Probe `isConfigSupported` with `'prefer-hardware'` per codec at startup.
- On Linux Chrome, hardware decode via VA-API is spotty **(unverified per-distro)**, so expect software (dav1d/FFmpeg) and budget CPU.
- Safari VideoToolbox decodes AV1 **only on hardware with AV1 decode (M3/A17 Pro and newer)**, with no software fallback. That explains Safari's about 27–33% AV1 support in 2026 real-user data ([dataset](https://webcodecsfundamentals.org/datasets/codec-analysis-2026/)).

**Codec coverage (7.65M sessions, Jan–Oct 2026, [webcodecsfundamentals](https://webcodecsfundamentals.org/datasets/codec-analysis-2026/))**
- H.264 baseline: 99.8%.
- VP9: 99.87%.
- AV1 8-bit: about 91% on Chrome, Edge and Firefox desktop; 0% on Firefox Android; about 27% Safari macOS and 33% iOS.
- HEVC:
  - Safari: about 85–91%.
  - Chrome Win 84%, macOS 97%, Linux 58%.
  - Edge Win **57%** (licensing).
  - Firefox about 0–2% ([HEVC page](https://webcodecsfundamentals.org/codecs/hevc.html)).
- AV1 ∪ HEVC = 99.6%.

**4:4:4 and 10-bit.**
- HEVC RExt hardware decode in Chromium:
  - Windows: Intel Gen10+ 8/10-bit 4:2:2/4:4:4, plus 12-bit on Gen12+; partial on NVIDIA Turing–Ada, full on Blackwell.
  - Apple Silicon: 8–10-bit 4:0:0/4:2:0/4:2:2/4:4:4.
  - WebCodecs HEVC 8-bit from Chrome 107, 10-bit from 108.
  - WebRTC HEVC from 136.
  - Source: [StaZhu matrix](https://github.com/StaZhu/enable-chromium-hevc-hardware-decoding).
- AV1 4:4:4 (High profile) has essentially no hardware decode.
- **Pyrowave supports 4:4:4 natively** ([README](https://github.com/Themaister/pyrowave)). That makes it the text-crisp LAN option.
- HDR: VideoFrame `colorSpace` exists everywhere. Safari 27 lets you override a hardware decoder's color space ([WebKit 27](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/)). Canvas/WebGPU HDR output (`toneMapping: extended`) is a separate track **(not researched in depth)**.

### 3.2 Pyrowave in the browser

Cross-reference: [02-pyrowave-and-codecs.md](02-pyrowave-and-codecs.md).

**Decoder shader requirements** (from `pyrowave/shaders`):
- `wavelet_dequant.comp` uses `subgroupInclusiveAdd`, `subgroupShuffleUp`, ballot/vote, plus 8- and 16-bit storage.
- `idwt.comp` optionally uses fp16.
- WGSL has no 8/16-bit storage types. Emulate with `u32` packing.
- WebGPU `subgroups` (Chrome 134+) covers inclusive-add and shuffle-up. Firefox and Safari need a workgroup-shared-memory scan path.
- `shader-f16`: Chrome and Safari 26, not Firefox.

**Bitrates of about 200+ Mb/s** are the design point (README). The bottleneck will be:
- browser datagram or data-channel receive throughput and per-packet JS overhead (about 20k pkts/s at 1,200 B). **Must benchmark** WebTransport datagrams vs unreliable data channels in a worker.
- the GPU upload: `device.queue.writeBuffer` per frame. Fine at about 25–30 MB/s.

**Where it works:** Chrome on all desktop OSes with WebGPU; Safari macOS 26+ (no subgroups); Firefox Windows and macOS ARM. **Not** Firefox Linux or Chrome Linux AMD without a flag.

### 3.3 Audio

Opus decode:
- WebRTC audio track. NetEq adapts its own buffer; robust, but typically tens of ms.
- Or WebCodecs `AudioDecoder` (Safari 26+), feeding an **AudioWorklet with a SharedArrayBuffer ring buffer** ([ringbuf.js](https://github.com/padenot/ringbuf.js), MPL-2.0). SAB requires **cross-origin isolation** (COOP `same-origin` plus COEP `require-corp`/`credentialless`). Plan headers for this from day one; it also unlocks 5 µs timers.
- Use `AudioContext({latencyHint:'interactive'})`. Chrome 58 and Safari 14.1 accept the hint; Firefox ignores it.
- Read `baseLatency` and `outputLatency` (Chrome 102, Firefox 70, Safari 18.4) to report audio delay. Use 5–10 ms Opus frames and a 10–30 ms adaptive target. Drop or stretch to stay in sync.

---

## 4. Render

| Path | Copy | Latency notes | Where |
|---|---|---|---|
| WebRTC track → `<video>` | zero-copy | With playout-delay 0/0, uses Chromium's low-latency renderer algorithm and can use hardware overlays | all |
| WebCodecs → **WebGPU `importExternalTexture`** in a worker on OffscreenCanvas | zero-copy on Chrome | About 1 ms import on desktop and about 3 ms on phones ([webrtcHacks](https://webrtchacks.com/video-frame-processing-on-the-web-webassembly-webgpu-webgl-webcodecs-webnn-and-webtransport/)). Presents at the next compositor frame. No `desynchronized` for WebGPU canvases | Chrome; Safari 26; Firefox Windows |
| WebCodecs → WebGL2 `texImage2D(VideoFrame)` with `desynchronized:true` | usually GPU copy | `desynchronized` skips the renderer compositor queue and can do front-buffer or overlay scan-out. Effective on Windows and ChromeOS only ([Chrome blog](https://developer.chrome.com/blog/desynchronized), BCD) | Chrome Windows/ChromeOS best; others fine |
| WebCodecs → MediaStreamTrackGenerator → `<video>` | zero-copy | Goes through WebMediaPlayerMS. Whether it uses the low-latency algorithm for generated frames is **(unverified)** | Chromium only |
| WebCodecs in worker → **VideoTrackGenerator** → `<video>` | zero-copy | Safari's equivalent | Safari 18+ |
| WebCodecs → Canvas2D `drawImage` | copy | Simplest. Firefox path in Moonlight Web Stream | all |

**Moonlight Web Stream's pipeline table** is a good, tested fallback ladder ([pipeline.ts](https://github.com/MrCreativ3001/moonlight-web-stream/blob/main/web/stream/video/pipeline.ts), GPL-3.0):
1. data → VideoDecoder → MSTG → `<video>` (Chromium)
2. worker VideoTrackGenerator (Safari)
3. canvas (Firefox)
4. OpenH264-wasm → WebGL
5. MSE as the last resort

**Frame pacing and VRR.**
- Browsers present on the compositor's vsync. A frame that arrives just after a vsync waits up to one refresh: 8.3 ms at 120 Hz, 4.2 ms at 240 Hz.
- No web API drives VRR or exposes present timestamps for canvases **(unverified that no browser does VRR for canvases)**. `requestVideoFrameCallback` exposes `presentationTime` and `expectedDisplayTime` for `<video>` ([spec](https://wicg.github.io/video-rvfc/)).
- Practical guidance:
  - Present immediately on decoder output. Don't wait for rAF to *decode*. Only coalesce if two frames land in the same vsync.
  - Prefer 120 Hz+ client displays.
  - Run decode, transport and render in one dedicated worker so main-thread jank can't stall frames.

---

## 5. Input

### Mouse
- Pointer Lock with `{unadjustedMovement:true}` gives raw, un-accelerated deltas: Chrome 88 (not Linux), Firefox 152, Safari 18.4. Chrome for Android has pointer lock since 144. **iOS has none.**
- Pointer lock needs a user gesture. Chrome has a developer-trial "Keyboard Lock and Pointer Lock permissions" prompt ([chromestatus](https://chromestatus.com/feature/5142031990259712)), which is a future UX risk.
- `pointerrawupdate` delivers mouse events above the frame rate (Chrome 77, Firefox 148). Use it, or `getCoalescedEvents()`, to send every delta.
- In absolute (desktop) mode, send normalized coordinates and render a local cursor for snappy feel, as Parsec described ([Parsec](https://parsec.app/blog/game-streaming-tech-in-the-browser-with-parsec-5b70d0f359bc)).

### Keyboard

**Lock.** Two APIs:
- Chrome: `navigator.keyboard.lock()`. Only effective in JS-initiated fullscreen; hold Esc for 2 s to exit; OS secure-attention sequences such as Ctrl+Alt+Del are never capturable ([spec](https://wicg.github.io/keyboard-lock/)).
- Firefox 151 and Safari 26.4: `element.requestFullscreen({keyboardLock:"browser"})`. Chrome doesn't support it yet ([Interop 2027 proposal #1409](https://github.com/web-platform-tests/interop/issues/1409), [WHATWG fullscreen](https://fullscreen.spec.whatwg.org/#keyboard-locking)).
- Feature-detect both. Safari auto-releases the lock on fullscreen exit or tab switch (WebKit 26.4 notes).
- Alt+Tab, Win and Cmd+Tab capture is OS- and browser-dependent **(unverified per platform)**. Provide an on-screen "send Win / Alt+Tab / Ctrl+Alt+Del" menu.

**Codes.** Send `KeyboardEvent.code`, the physical key mapped to a scancode/VK on the host.
- GFN's protocol sends **both the Windows VK and the PS/2 scancode**, because DirectInput/raw-input games read scancodes while message-queue apps read VKs ([OpenNOW-vita input_protocol.rs](https://github.com/OpenCloudGaming/OpenNOW-vita)).
- `navigator.keyboard.getLayoutMap()` (Chrome only) can label keys.
- Layout mismatch between client and host is a real UX issue. Offer "physical" mode (games) and "text" mode.

**IME and text.** Use a hidden `<textarea>` with `compositionstart/update/end` and `beforeinput`, and send committed Unicode text as a text event. Moonlight's protocol has a UTF-8 text packet. Keep raw keydown forwarding suppressed while `isComposing`.

### Gamepad
- **Polling.** Chromium's browser-process provider polls every **4 ms (about 250 Hz)** (`kPollingIntervalMilliseconds = 4`, [gamepad_provider.cc](https://chromium.googlesource.com/chromium/src/+/main/device/gamepad/gamepad_provider.cc)). The chromestatus "Gamepad polling at 250Hz" entry is stale.
- Pages typically read in rAF, which adds up to one frame. Poll `getGamepads()` from a `setInterval(4)` in addition to rAF, or use the **`rawgamepadinputchange`** origin trial (Chrome M149–M157, [blink-dev](http://www.mail-archive.com/blink-dev@chromium.org/msg17328.html), [explainer](https://microsoftedge.github.io/MSEdgeExplainers/GamepadEventDrivenInputAPI/explainer.html)). Firefox and Safari: no signal.
- **Transport.** Send full gamepad state snapshots with a sequence number on an **unreliable** channel at 120–250 Hz, plus a reliable "edge" message for button transitions. GFN uses `input_channel_partially_reliable` for gamepad state and reliable for descriptors ([OpenNOW features.md](https://github.com/OpenCloudGaming/OpenNOW/blob/main/docs/streamer-comparison/features.md)). xCloud uses one reliable ordered `input` channel and returns vibration reports on it ([xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player)).
- **Haptics.**
  - `vibrationActuator.playEffect('dual-rumble')`: Chrome 68 and Safari macOS 16.4.
  - `'trigger-rumble'`: Chrome 126 for compatible pads.
  - Firefox has no `playEffect`. iOS has none.
  - Android vibration is in development ([chromestatus](https://chromestatus.com/feature/5144383549079552)).
  - Moonlight Web Stream implements both effects (`web/stream/input.ts`).
- **DualSense/DS4 extras** (gyro, touchpad, adaptive triggers, lightbar) need **WebHID**: Chromium desktop 89+, dedicated workers 131+.
  - `navigator.hid.requestDevice()` needs a user gesture and shows a chooser. The grant persists per origin, can be revoked with `forget()`, and iframes need the `hid` permission policy.
  - Chromium's static HID blocklist only blocks FIDO and security keys, not Sony pads ([hid_blocklist.cc](https://chromium.googlesource.com/chromium/src/+/main/services/device/public/cpp/hid/hid_blocklist.cc)).
  - The Gamepad API keeps working alongside WebHID.
  - The Gamepad light-indicator and multitouch extensions are stalled or dev-trial.

### Other
- **Touch and pen.** Pointer events with `pressure`, `tilt` and `altitudeAngle`, `getCoalescedEvents` and `getPredictedEvents` (Safari 18.2+). iPhone needs touch-as-trackpad and an on-screen gamepad.
- **Clipboard.**
  - Client→host: listen for the `paste` event (`clipboardData`, no prompt), and use `navigator.clipboard.readText()` on focus where permitted. Firefox and Safari require a gesture plus a paste prompt.
  - Host→client: `writeText()` inside user activation, or with permission. Chrome 144 has a `clipboardchange` event.
- **Fullscreen.** Use Element fullscreen. On iPhone, ship as an installed **PWA** (standalone display) instead.
- **Screen Wake Lock.** Everywhere (iOS Home Screen apps 18.4+).
- **Multi-monitor.** Window Management (`getScreenDetails`, `requestFullscreen({screen})`) is Chrome only, from 100.
- **Timestamps.** `event.timeStamp` and `performance.now()` resolution is about 100 µs, or **5 µs when `crossOriginIsolated`** in Chrome. Firefox coarsens more. Enable COOP/COEP.

---

## 6. Server-side networking libraries (license matters: Cha Portal is AGPL/GPL)

**License compatibility.**
- MIT, BSD, Apache-2.0 and MPL-2.0 are all compatible with **AGPL-3.0/GPL-3.0**.
- GPL-3.0 code, such as Moonlight Web Stream, can be combined with AGPL-3.0 (§13 of both).
- **Pick GPLv3 or AGPLv3, not GPLv2-only**: Apache-2.0 dependencies (eturnal, quinn, wtransport, str0m, moq) are incompatible with GPLv2-only.

Activity below is the last commit date from `git log` and tags from `git ls-remote` on 2026-10-03.

### 6.1 QUIC / WebTransport

| Library | Lang / license | Version, activity | Safari WT | Verdict |
|---|---|---|---|---|
| [quinn](https://github.com/quinn-rs/quinn) | Rust, MIT/Apache | quinn-0.11.12, commit 2026-10-02 | n/a (QUIC only) | **Core QUIC for a Rust node.** CC: cubic, new_reno, bbr. Datagram buffers configurable |
| [wtransport](https://github.com/BiagioFesta/wtransport) | Rust, MIT/Apache (on quinn) | 0.7.2, commit 2026-09-22 | likely ❌ (draft-07 settings only) | Nice API. Needs a draft-14+ capsules patch for Safari. Fine for Chrome/Firefox |
| [h3 / h3-webtransport](https://github.com/hyperium/h3) | Rust, MIT | h3-webtransport 0.1.2, commit 2026-09-20 | ❌ open issue #347 | Avoid for WT for now |
| [webtransport-go](https://github.com/quic-go/webtransport-go) | Go, MIT (quic-go) | v0.13.0, commit 2026-09-21. **draft-16, WT_MAX_DATA/STREAMS capsules (Jul 2026)** | probably ✅ (untested) | **Best WT server if the node is Go.** quic-go has ECN, datagrams and pacing |
| [quiche / tokio-quiche](https://github.com/cloudflare/quiche) | Rust, BSD-2 | tokio-quiche 0.20.0, commit 2026-10-02 | n/a | Cloudflare-grade QUIC/H3. A source grep found only incidental WebTransport references (netlog event names, a `:protocol webtransport` test header), so there is no first-class WT server API. You would have to build WT on top of it |
| [msquic](https://github.com/microsoft/msquic) | C, MIT | v2.6.2, commit 2026-09-30 | n/a | Excellent QUIC (BBR, ECN). No built-in WebTransport |
| [fails-components/webtransport](https://github.com/fails-components/webtransport) | Node, BSD-3 | v1.6.8, commit 2026-10-03 | ✅ (has Safari capsule fix work) | Node option. HTTP/2 WT fallback |
| [aioquic](https://github.com/aiortc/aioquic) | Python, BSD-3 | 1.3.0, last commit 2025-10-11 | (PR #652 for WT_MAX_SESSIONS) | Prototyping only |

### 6.2 WebRTC stacks

| Library | Lang / license | Version, activity | Notes | Verdict |
|---|---|---|---|---|
| [str0m](https://github.com/algesten/str0m) | Rust, MIT/Apache, **sans-IO** | 0.24.1, commit 2026-10-03 | ICE (UDP+TCP), DTLS, SCTP data channels, TWCC+BWE, NACK, simulcast, playout-delay and abs-capture-time extensions. No TURN client, no adaptive jitter buffer, no codecs (by design) | **Top pick for a Rust node** (send side, server role) |
| [webrtc-rs/rtc](https://github.com/webrtc-rs/rtc) + [webrtc](https://github.com/webrtc-rs/webrtc) | Rust, MIT/Apache | v0.21.0-rc.2, commit 2026-10-03 | Sans-IO rewrite plus async wrapper. Used by Moonlight Web Stream (0.21) and OpenNOW-vita | Good alternative. API churn |
| [Pion](https://github.com/pion/webrtc) | Go, MIT | v4.2.22, commit 2026-10-02 | Most complete OSS stack. Interceptors: gcc, twcc, rfc8888, ccfb, flexfec, nack, pacing, jitterbuffer. pion/rtp playout-delay and abs-capture-time | **Top pick for a Go node.** Used by neko (Apache-2.0) |
| [libdatachannel](https://github.com/paullouisageneau/libdatachannel) | C++, **MPL-2.0** | v0.24.6, commit 2026-09-27 | Lightweight, includes a WebSocket client/server | Good for a C++ native thin client or a Sunshine/Wolf-side plugin |
| GStreamer `webrtcbin` / `webrtcsink` (gst-plugins-rs) | C / Rust, LGPL / MPL-2.0 | active **(not re-checked)** | `webrtcsink` has built-in CC and encoder control. Selkies used it historically | Fine if the node pipeline is GStreamer anyway |

### 6.3 STUN/TURN

| Server | Lang / license | Version, activity | Verdict |
|---|---|---|---|
| [coturn](https://github.com/coturn/coturn) | C, BSD-3 | 4.18.0, commit 2026-10-02 | De-facto standard: TURN UDP/TCP/TLS/DTLS, REST-API ephemeral credentials. Ship as an optional compose service on the portal host |
| [eturnal](https://github.com/processone/eturnal) | Erlang, Apache-2.0 | 1.12.3, commit 2026-10-02 | Simpler config, good container story |
| [Pion TURN](https://github.com/pion/turn) | Go, MIT | v5.1.2, commit 2026-09-24 | **Embeddable** in a Go portal or node. TURN-TLS on 443 |
| [turn-rs](https://github.com/mycrl/turn-rs) | Rust, MIT | commit 2026-09-14 | Embeddable in Rust; less battle-tested |

**Restrictive-NAT recipe** (LAN and WAN first-class):
- The portal host (public IP) runs TURN on UDP 3478, TCP 3478 and **TLS 443**, using SNI/ALPN multiplexing with the HTTPS reverse proxy or a second IP.
- Issue short-lived TURN credentials via the coturn REST API scheme.
- Nodes run ICE (full or lite) with host, srflx and ICE-TCP passive candidates.
- For WebTransport on NAT'd nodes, add a QUIC relay on the portal host, or require a port forward or overlay.

---

## 7. How commercial cloud gaming does it in the browser

| Service | Browser transport | Video | Input | Notes and sources |
|---|---|---|---|---|
| **GeForce NOW** (play.geforcenow.com) | WebRTC to **ICE-lite** servers. NVST WebSocket signaling. Media and input multiplexed on one UDP flow ([PAM'24](https://arxiv.org/abs/2401.06366)) | H.264, HEVC and AV1 (native client data; browser codec mix (unverified)). Browser: up to **1440p120** for Ultimate ([XDA](https://www.xda-developers.com/nvidia-geforce-now-browsers-1440p/)). Firefox on Windows supported since Aug 2026 ([NVIDIA](https://blogs.nvidia.com/blog/geforce-now-thursday-firefox/)) | Data channels: reliable `input_channel_v1` (keyboard VK+scancode, relative mouse), **`input_channel_partially_reliable` for gamepad state**. Control messages for IDR request, frame pacing and frame ACK ([OpenNOW docs](https://github.com/OpenCloudGaming/OpenNOW)) | PAM'24: browser sessions had under 20 ms network latency 70% of the time vs 90% for the native app, and the browser streamed at lower resolution |
| **Xbox Cloud Gaming** | WebRTC: audio sendrecv, video recvonly with codec preferences | H.264 (via `setCodecPreferences`) | Reliable ordered data channels `input`, `control`, `message` and `chat`. Vibration reports come back on `input` | [xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player) (reverse-engineered) |
| **Stadia** (defunct) | WebRTC in Chrome. RTP | VP9/H.264 | n/a | [Network analysis](https://arxiv.org/abs/2012.06774): up to 45 Mb/s |
| **Parsec web** (2018–) | WebRTC **data channels** carrying Parsec's own framing | MSE in Chrome's **low-delay push mode**. Chrome-only | DOM events, Pointer Lock (needs fullscreen), polled Gamepad API | [Parsec blog](https://parsec.app/blog/game-streaming-tech-in-the-browser-with-parsec-5b70d0f359bc). Current stack **(unverified)** |
| **Amazon Luna** | PWA on iOS ([Engadget](https://www.engadget.com/luna-amazon-cloud-gaming-interview-pwa-apple-173948922.html)). Transport **(unverified, likely WebRTC)** | (unverified) | (unverified) | |
| **Boosteroid** | Browser client. WebRTC **(unverified)** | **AV1 in browser** on devices with hardware AV1 (May 2025) ([blog](https://boosteroid.com/blog/2025/05/09/boosteroid-introduces-av1-codec-for-improved-streaming-efficiency/)) | (unverified) | |
| **Shadow** | Primarily a native client. Browser client status **(unverified)** | | | |
| **Steam Remote Play** | **No browser client** as of Oct 2026. Native only. June 2026 stable raised adaptive bitrate to 250 Mb/s ([report](https://www.linuxcompatible.org/story/valve-updates-stable-steam-client-with-250-mbit-s-remote-play-and-multicontroller-support)) | | | Use Moonlight/Wolf or our own pipeline for browser play |

**Typical latencies (treat with care).**
- NVIDIA marketing claims "as low as 35 ms" for GFN with AV1 and Reflex ([CloudDosage](https://clouddosage.com/geforce-now-av1/)).
- Independent 2025–26 tests on blogs range widely, from 25 to 90 ms click-to-photon, with no rigorous published methodology. I found no peer-reviewed glass-to-glass comparison of browser and native clients for 2025–26 **(gap)**.
- Moonlight/Sunshine users report **5–6 ms "streaming latency"** (host processing plus decode, excluding display) on wired LAN ([HN](https://news.ycombinator.com/item?id=40405214), [note.com](https://note.com/cute_agapan9087/n/n5e400bb52235?hl=en)).
- **Realistic browser targets for Cha Portal:**
  - LAN: about 1–2 frames plus display (12–25 ms at 120 Hz).
  - Metro WAN: RTT + about 15 ms.

**Takeaways from the incumbents:**
1. Everyone ships **WebRTC** in the browser. Nobody public ships WebTransport for interactive play.
2. Input goes over **data channels**, with gamepad state sent **partially reliable** (GFN).
3. The server side is **ICE-lite**, with tight control over encoder pacing, IDR requests and frame ACKs over a control channel.
4. Firefox and Safari are second-class but now supported (GFN Firefox 2026).

---

## 8. Existing open projects to learn from

| Project | Transport / decode | License | Relevance |
|---|---|---|---|
| [Moonlight Web Stream](https://github.com/MrCreativ3001/moonlight-web-stream) (v3.0 pre, commit 2026-10-03) | Sunshine → node → **WebRTC** (track or data channels: unordered, `maxRetransmits:0`, `maxPacketLifeTime:30`) or **WebSocket**, decoded with WebCodecs. Pipelines for MSTG, VideoTrackGenerator, canvas, OpenH264 and MSE. `jitterBufferTarget=0`. Trigger rumble | **GPL-3.0** (AGPL-combinable) | Closest reference for external Moonlight/Sunshine/Vibepollo endpoints (feature 8). Uses webrtc-rs 0.21 |
| [moonlight-web-stream-simplified](https://github.com/spacedouut/moonlight-web-stream-simplified) | Adds experimental **WebTransport** (streams plus datagrams, `max_in_flight_video_frames`) | (unverified) | WebTransport framing ideas |
| [BrowserPane](https://github.com/ITmedes/BrowserPane) | **WebTransport**: reliable envelopes for control and keyframes, datagrams for H.264 deltas. Tiles plus WebCodecs H.264. WebGL2. Chromium-only | **AGPL-3.0** | Hybrid tiles-plus-video for desktop environments |
| [Selkies 2.0](https://github.com/selkies-project/selkies) | **WebSocket + WebCodecs default**, WebRTC opt-in. pixelflux encoders (NVENC, VA-API, x264) | MPL-2.0 | Desktop container streaming (see 04) |
| [gawk](https://github.com/Tuhis/gawk) | WebTransport + WebCodecs, Go relay fleet, sub-500 ms | (unverified) | Viewer fan-out pattern |
| [neko](https://github.com/m1k1o/neko) | Pion WebRTC plus GStreamer | Apache-2.0 | Multi-user desktop/browser |
| [moq-dev/moq](https://github.com/moq-dev/moq) | MoQ-lite over WebTransport, with WebSocket fallback | MIT/Apache | Browser quirk database (`browser.ts`, `error.ts`) |

---

## 9. Recommendation: Cha Portal browser client stack

### 9.1 Architecture

```
Browser (main thread: UI, input capture, fullscreen/lock)
  └─ Dedicated Worker "session" (transport + decode + render)
       ├─ Transport adapter (one interface, 4 impls):
       │    A  WebRTC  (RTCPeerConnection: video/audio tracks + DataChannels)   ← default
       │    A' WebRTC  DataChannel-only (custom codec frames, e.g. Pyrowave)
       │    B  WebTransport (datagrams for media/input, bidi stream for control) ← upgrade
       │    C  WebSocket (same framing as B, TCP)                               ← last resort
       ├─ Decoder: WebCodecs VideoDecoder | Pyrowave-WGSL (WebGPU compute)
       └─ Renderer: WebGPU (OffscreenCanvas, importExternalTexture) | WebGL2(desync) | <video> via track
Node (docker compose): encoder → packetizer → {str0m|Pion WebRTC, quinn/webtransport-go WT, WS}
Portal: auth, signaling (WSS), TURN (coturn/eturnal/Pion, TLS 443), optional QUIC relay
```

Main-thread work stays tiny: input events are posted to the worker with a `MessagePort`, or sent straight on a data channel. The worker owns the transport. Data channels are transferable; WebTransport and WebCodecs are worker-native.

### 9.2 Fallback chain (decided per session by a fast probe race)

1. **LAN or direct and Chromium/Firefox ≥153.** Try **WebTransport (B)** to `node:443/udp`, authenticated with `serverCertificateHashes` or an ACME certificate. With Pyrowave this carries 4:4:4 at 200+ Mb/s. Otherwise AV1/HEVC/H.264 via WebCodecs. Expect the Chrome LNA prompt when the portal origin is public.
2. **Otherwise, or on Safari/iOS: WebRTC (A)** with ICE (host, srflx, prflx, ICE-TCP) and video on an RTP track.
   - Node stamps **playout-delay 0/0** and **abs-capture-time**. Client sets `jitterBufferTarget=0`.
   - Codec order is decided by `RTCRtpReceiver.getCapabilities` and MediaCapabilities: AV1 → HEVC (Safari, Chrome hardware) → H.264.
   - NACK always. FlexFEC/RED adaptive. GCC on the node.
   - Pyrowave over WebRTC uses **A'** (unreliable data channel) on capable browsers.
3. **UDP blocked:** WebRTC via **TURN-TLS on 443** (portal-hosted).
4. **Still failing (proxy):** **WebSocket (C)** over 443, with H.264 at reduced fps and bitrate, frame-ACK backpressure, and drop-to-IDR.

Session upgrades: start on whichever connects first, usually WebRTC, and hot-switch to WebTransport if the probe succeeds. Requires an IDR on the switch.

### 9.3 Per-browser caveats
- **Chrome/Edge:** best on all axes.
  - Linux: WebGPU only on Intel Gen12+ and NVIDIA on Wayland; no `unadjustedMovement`; hardware decode spotty. Fall back to WebGL2 and WebCodecs software.
  - Plan for LNA prompts and for the future pointer/keyboard lock permission prompts.
- **Firefox:**
  - No MSTG, so render to canvas.
  - No WebGPU on Linux, so no Pyrowave there.
  - No subgroups, so slower Pyrowave dequant.
  - Almost no HEVC; no WebHID or haptics.
  - WebTransport OK from 153.
  - Keyboard lock via `requestFullscreen({keyboardLock})` (151); `unadjustedMovement` 152.
- **Safari macOS:**
  - Use WebRTC. WebTransport is gated off until WebKit 319818 is fixed.
  - WebCodecs plus VideoTrackGenerator in a worker.
  - AV1 only on M3+, so prefer **HEVC** for Safari.
  - Keyboard lock via fullscreen option (26.4); pointer lock raw (18.4); dual-rumble.
  - `jitterBufferTarget`/`targetLatency` (27).
- **iOS/iPadOS:**
  - WebRTC only; HEVC.
  - Installed PWA for full-screen; no pointer or keyboard lock.
  - iPhone: no Element fullscreen. iPad: fullscreen with an undismissable overlay.
  - Gamepad without haptics; touch-trackpad and on-screen pad.
  - All iOS browsers are WebKit.

### 9.4 Codec, bitstream and node rules
- Low-delay encoder config: no B-frames, CBR/VBV of about 1 frame, slice or intra-refresh.
- **Write VUI `bitstream_restriction` with `max_num_reorder_frames=0`** for H.264/HEVC (Sunshine `cbs.cpp`).
- Request IDR over the control channel. Use LTR/reference invalidation where the encoder supports it.
- Put a per-frame header (frame-id, capture µs, encode µs) in an encoded-transform trailer (WebRTC) or the packet header (WT/WS).

---

## 10. Measurement methodology (glass-to-glass and per-stage)

### 10.1 Clocks
- Run an NTP-style ping on the control channel (min-RTT filtered, about 1 Hz) to estimate the node↔browser clock offset and drift. Use `performance.timeOrigin + performance.now()` on the client and `CLOCK_MONOTONIC` on the node, with a published mapping.
- Enable **cross-origin isolation** for 5 µs timers.
- On WebRTC, also use **abs-capture-time** (str0m `AbsCaptureTime`). `requestVideoFrameCallback` metadata gives `captureTime`, `receiveTime`, `rtpTimestamp`, `processingDuration`, `presentationTime` and `expectedDisplayTime` ([rVFC spec](https://wicg.github.io/video-rvfc/)).
- Chrome 145 exposes `VideoFrame.metadata().rtpTimestamp` for WebRTC frames ([chromestatus](https://chromestatus.com/feature/5186046555586560)).

### 10.2 Per-frame trace (log every frame, sample 1% in production)

| Stage | Where | How |
|---|---|---|
| T0 capture | node | compositor/PipeWire/KMS buffer timestamp |
| T1/T2 encode start/end | node | encoder callbacks (NVENC/VA-API/Pyrowave GPU timestamps) |
| T3 first packet sent / T4 last packet sent | node | pacer |
| T5 first packet received / T6 last packet received | browser worker | `performance.now()` on datagram or data-channel receipt; WebRTC: rVFC `receiveTime` |
| T7 `decode()` called / T8 output callback | worker | WebCodecs `output` callback (`decodeQueueSize` too); WebRTC: `totalDecodeTime`, `totalAssemblyTime` and `jitterBufferDelay` from `getStats()` |
| T9 submitted for composition | worker | WebGPU `queue.submit` time; `<video>`: rVFC `presentationTime` |
| T10 expected display | browser | rVFC `expectedDisplayTime` (video only); for canvas, next vsync estimate from rAF |
| T11 photons | physical | photodiode (below) |

- Embed the **frame-id** both in metadata and **visually**: a 32-bit binary strip of 8×8 or 16×16 black/white blocks in a corner, robust to compression. The client can read the strip back via a 1×N WebGPU readback in debug builds to detect drops, repeats and reorders.
- Use the strip in camera and photodiode tests to pair frames.
- In a server test-pattern mode, burn a millisecond clock into the frame for camera-based glass-to-glass tests: one high-speed camera (240–1000 fps) filming the source monitor and the client screen side by side.
- Use `chrome://webrtc-internals` and Perfetto (`chrome://tracing` with gpu, viz and media categories) to see compositor and vsync stages Chrome doesn't expose to JS.

### 10.3 Click-to-photon (end-to-end incl. input)
- **Rig.** A microcontroller (Teensy or RP2040) enumerates as a USB HID mouse or gamepad on the client and fires a click at t0. A photodiode taped to the client screen watches a test region.
- The node runs a test app that flips that region black→white on the click, rendered at the host refresh rate.
- Measure t0→threshold crossing. Repeat ≥200 times with random phase.
- Open hardware and tools: [OSLTT](https://github.com/OSRTT/OSLTT); NVIDIA LDAT and Reflex Analyzer monitors if available.
- **Baselines to measure:**
  1. The same app running locally on the client machine.
  2. Moonlight native → Sunshine/Wolf.
  3. Each Cha Portal tier: A, A', B, C.

  Do this on each browser at 60, 120 and 240 Hz, wired and Wi-Fi, LAN and with an emulated WAN (`tc netem` delay, jitter and loss; L4S-capable `tc` with DualPI2 for ECN tests).
- Report median, p95 and p99. Also report **frame-time variance** (stutter) and drop/repeat counts from the frame-id strip. Latency alone hides judder.

### 10.4 Input path breakdown
- Client: `event.timeStamp` → send time (worker) → node receive (clock-mapped) → injection (uinput/libei/Inputtino) → app sees it, via a test app timestamping the input.
- Gamepad: compare the rAF-polled, `setInterval(4)`-polled and `rawgamepadinputchange` paths (OT) on Chrome.

---

## 11. Risks and open questions

1. **Safari WebTransport** may stay broken for months (WebKit 319818 has no assignee). Plan Safari = WebRTC indefinitely.
2. **Browser receive throughput for Pyrowave rates** (200+ Mb/s) over WT datagrams or SCTP data channels is unmeasured. Benchmark in week 1. It may force H.264/HEVC/AV1 on WAN and Pyrowave on LAN/Chromium only.
3. **Chromium low-latency renderer and 120+ fps.** `LowLatencyVideoRendererAlgorithm` assumes 60 fps. Measure drops at 120/144/240.
4. **No WebGPU on Firefox Linux or Chrome Linux AMD by default.** That is a notable chunk of the homelab Linux-desktop audience.
5. **Chrome permission creep:** LNA (WS/WT), the planned keyboard/pointer-lock permission prompts. Design UX for them.
6. **WT server library choice for Rust.** wtransport lacks draft-14+ capsules. Either patch it (contribute upstream), use webtransport-go in a Go node, or put a small Go WT edge in front of a Rust core.
7. **L4S.** The WebTransport path can do ECN today (Chrome ACK-ECN). WebRTC can't by default. Decide whether to invest in an L4S/Prague controller on the WT path.
8. **Open:**
   - Does Safari honour playout-delay 0/0 like Chrome? Measure.
   - Firefox's actual render path under playout-delay?
   - Exact Safari `serverCertificateHashes` behaviour?
   - Corporate-proxy behaviour for TURN-TLS vs WSS?
