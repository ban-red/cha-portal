# 03 — Moonlight / Sunshine / Apollo / Vibepollo ecosystem & the GameStream protocol

Research slice for **Cha Portal** (portal.cha.sh). Researched 2026-10-03. Repo metadata comes from the GitHub REST API and from shallow clones read on that date. Anything I could not check against a primary source is tagged **(unverified)**.

User decisions to factor in (from the coordinator): Cha Portal will be **copyleft OSS (AGPL/GPL)**, so linking or forking GPL-3 code is acceptable. The first target is **homelab / small groups**. **LAN and WAN are equally first-class.**

---

## TL;DR

- **The GameStream/Moonlight protocol is the de-facto standard for low-latency game and desktop streaming.** It has clients on every platform: PC, Android, iOS/tvOS, Xbox, Switch, TVs, Steam Link and Pi. Three of the newer hosts (Wolf, Moonshine, Punktfunk) speak it so that any Moonlight client works on day one. Cha Portal should speak it **in both directions**:
  - as a **client/gateway**, to reach external Sunshine, Apollo, Vibepollo and Wolf hosts from the browser;
  - as a **host façade on Cha Nodes**, so stock Moonlight, Artemis and Moonlight V+ clients, including Nonary's PyroWave-capable fork, can connect.
- **Vibepollo** (Nonary, GPL-3.0, Apollo fork, **v2.0.0 released 2026-09-30**) is the most feature-rich host. Its Sunshine-based twin, **Vibeshine**, ships the same features.
  - **PyroWave over the GameStream transport** (Windows and Linux), with a **documented wire contract**: new `ServerCodecModeSupport` bits, `bitStreamFormat=3`, "record framing", and FEC applied only to the critical wavelet band.
  - A **native WebRTC browser path** inside the host. Windows-only build; it passes H.264/HEVC/AV1 through without transcoding and carries input over a DataChannel.
  - A **~90-endpoint REST API** with **path+method-scoped bearer tokens**.
  - Up to **4 concurrent "Remote Monitor" clients**, a Linux host beta (Arch/CachyOS + KDE Plasma 6 Wayland + its own DKMS virtual-display kernel module), a free Windows virtual gamepad driver, and VRR pacing.
  - Caveats: about 99% AI-generated, a single maintainer, a very fast release cadence with frequent regressions, Windows-first, and no Docker.
- **Browser clients today:**
  - **MrCreativ3001/moonlight-web-stream** (GPL-3.0, Rust). Its v3 prereleases (Sept 2026) replaced moonlight-common-c with **moonlight-common-rust**, a **pure-Rust, Sans-IO, WASM-compilable** Moonlight implementation. It **passes host H.264/HEVC/AV1 frames straight through** into WebRTC RTP (webrtc-rs 0.21), with no transcoding. It adds playout-delay=0, NACK, FlexFEC and PLI→IDR, plus a WebSocket+WebCodecs fallback. It also drafts a WHIP/WHEP-style **"Moonlight over WebRTC" signaling spec**. This is the closest thing to what Cha Portal needs. **Fork it or depend on it.**
  - **linckosz/moonlight-web** (GPL-3.0, C++/Qt, created June 2026): an alternative gateway with its own capture engine, Wolf API pairing and session sharing.
- **moonlight-common-c** (GPL-3.0; an App Store §7 exception was added 2026-09-26) is the reference client core. It uses **process-global state, so one stream per process**; moonlight-web-stream v2 had to spawn a subprocess per stream. Prefer moonlight-common-rust for a multi-session gateway. Its gaps today: no AV1 parsing in the Rust proto (frames still pass through), **no video decryption**, and **no RFI**.
- **Rust/permissive host implementations exist** for the Node side:
  - **Moonshine** (hgaiser, **BSD-2**, Rust, a smithay compositor per session, Vulkan encode).
  - **Punktfunk** (**MIT/Apache-2.0**, Rust). GameStream host plus its own QUIC + GF(2¹⁶) Leopard-FEC protocol that "breaks the ~1 Gbps FEC wall".
  - **Wolf** (MIT, C++, Docker-native; covered by another slice).
- **Protocol hard limits that matter for Cha Portal:**
  - FEC is GF(2⁸) Reed-Solomon, capped at **4 blocks × 255 shards**. Sunshine **drops FEC** on frames above about 850 packets at 20% FEC, which is the "~1 Gbps wall".
  - Mode is **fixed at launch**; a mid-stream resolution change needs a relaunch or resume.
  - The cursor is composited into the video.
  - Stock Sunshine has **no NAT traversal**: LAN encryption is off by default and WAN means port-forwarding or a VPN.
  - No stock clipboard, mic or file transfer. Apollo, Foundation and Vibepollo each extend these, incompatibly.
- **Recommendation:**
  - **(a)** Build the browser path as a **server-side gateway colocated with the host** (on a Cha Node in the same LAN, or as a sidecar). It speaks Moonlight over the lossless LAN leg and **re-packetizes NALUs/OBUs into WebRTC** (default, with ICE/TURN for WAN) or **WebTransport+WebCodecs** (edge path). There is no transcoding.
  - Longer term, explore **moonlight-common-rust-in-WASM over a dumb UDP↔WebTransport relay** for end-to-end Moonlight semantics in the browser.
  - **(b)** Cha Nodes should **also expose GameStream**, as a compatibility front door: Wolf, Moonshine or Punktfunk class. Adopt **Vibepollo's PyroWave wire contract** for native PyroWave clients. Do not make GameStream the *only* native protocol, because of its WAN and FEC limits.

---

## 1. Landscape at a glance (as of 2026-10-03)

| Project | Role | Lang | License | Stars | Latest release (date) | Last push | Verdict for Cha Portal |
|---|---|---|---|---|---|---|---|
| [LizardByte/Sunshine](https://github.com/LizardByte/Sunshine) | Host (reference) | C++ | GPL-3.0 | 41,799 | v2026.914.233613 stable (2026-09-15); v2026.1003.171600 pre (2026-10-03) | 2026-10-03 | **Protocol-compatible** (external endpoint) |
| [ClassicOldSong/Apollo](https://github.com/ClassicOldSong/Apollo) | Host (Sunshine fork) | C++ | GPL-3.0 | 11,171 | v0.4.6 (2025-07-13); v0.4.7-alpha.1 (2025-08-12) | 2026-05-21 | Protocol-compatible; inspiration (permissions, OTP, virtual display, clipboard) |
| [Nonary/Vibepollo](https://github.com/Nonary/Vibepollo) | Host (Apollo fork) | C++ (+Vue UI) | GPL-3.0 | 1,218 | **2.0.0 (2026-09-30)** | 2026-10-03 | **Protocol-compatible + adopt PyroWave wire contract**; REST API integration |
| [Nonary/vibeshine](https://github.com/Nonary/vibeshine) | Host (Sunshine fork, Vibepollo twin) | C++ | GPL-3.0 | 466 | 2.0.0 (2026-09-30) | 2026-10-03 | Same as Vibepollo |
| [AlkaidLab/foundation-sunshine](https://github.com/AlkaidLab/foundation-sunshine) | Host (Sunshine fork) | C++ | GPL-3.0 | 7,065 | v2026.925.152547 (2026-09-25); pre 2026-10-01 | 2026-10-03 | Protocol-compatible; mic + runtime-bitrate extensions |
| [games-on-whales/wolf](https://github.com/games-on-whales/wolf) | Host (Docker, multi-session) | C++ | MIT | 2,213 | (see Wolf slice) | 2026-09-29 | Candidate Node engine (other slice) |
| [hgaiser/moonshine](https://github.com/hgaiser/moonshine) | Host (headless, per-session compositor) | Rust | **BSD-2** | 1,232 | v0.16.1 (2026-09-14) | 2026-10-02 | **Candidate Node GameStream engine / code donor** |
| [Punktfunk](https://git.unom.io/unom/punktfunk) ([GH mirror](https://github.com/rongrong666/punktfunk)) | Host + native clients + own QUIC protocol | Rust | **MIT OR Apache-2.0** | n/a | Cargo v0.24.0 | active (unverified date) | Candidate code donor (FEC, Windows IDD, GameStream host) |
| [moonlight-stream/moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c) | Client core lib | C | GPL-3.0 + App Store §7 exception | 576 | (no releases; submodule) | 2026-09-26 | Reference; depend only if single-session-per-process is OK |
| [MrCreativ3001/moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust) | Client core lib (Sans-IO) | Rust | GPL-3.0-or-later | (unverified) | v0.1.0 (git) | 2026-10-03 | **Depend-on / fork** |
| [moonlight-stream/moonlight-qt](https://github.com/moonlight-stream/moonlight-qt) | Client (PC) | C++/Qt | GPL-3.0 | 18,869 | v6.1.0 (**2024-09-17**; v6.2 pending) | 2026-10-03 | Reference client |
| [moonlight-stream/moonlight-android](https://github.com/moonlight-stream/moonlight-android) | Client | Java/C | GPL-3.0 | 7,209 | v12.2 (2026-09-12) | 2026-10-01 | Reference client |
| [moonlight-stream/moonlight-ios](https://github.com/moonlight-stream/moonlight-ios) | Client iOS/tvOS | ObjC | GPL-3.0 | 1,680 | App Store only | 2026-09-26 | Reference client |
| [moonlight-stream/moonlight-embedded](https://github.com/moonlight-stream/moonlight-embedded) | Client (Pi/embedded) + `libgamestream` | C | GPL-3.0 | 1,664 | v2.7.1 (2025-11-30) | 2026-06-06 | Reference for C HTTP/pairing |
| [moonlight-stream/moonlight-chrome](https://github.com/moonlight-stream/moonlight-chrome) | Client (Chrome NaCl) | C/JS | GPL-3.0 | 814 | — | **archived** (2024-02-13) | History only |
| [ClassicOldSong/moonlight-android (Artemis)](https://github.com/ClassicOldSong/moonlight-android) | Client (Apollo companion) | Java | GPL-3.0 | 4,074 | v20.2.6 (2025-08-08); v20.3.0-exp.9 (2025-08-30) | 2026-09-09 | Inspiration (UX), protocol ext reference |
| [qiin2333/moonlight-vplus](https://github.com/qiin2333/moonlight-vplus) | Client (Foundation companion) | Java | GPL-3.0 | 3,943 | v12.12.13.beta.3 (2026-09-30) | 2026-10-03 | Inspiration |
| [Nonary/moonlight-qt](https://github.com/Nonary/moonlight-qt) ("VRR Moonlight Client") | Client (PyroWave + VRR) | C++/Qt | GPL-3.0 | 99 | v6.1.0-vrr18 (2026-09-30) | 2026-10-03 | **Test client for Cha PyroWave-over-GameStream** |
| [MrCreativ3001/moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream) | Browser gateway | Rust + TS | GPL-3.0-or-later | 710 | v3.0.0-prerelease.7 (2026-09-16); stable v2.10.0 | 2026-10-03 | **Fork / depend-on** |
| [linckosz/moonlight-web](https://github.com/linckosz/moonlight-web) | Browser gateway + own host engine | C++/Qt + TS | GPL-3.0 | 96 | (created 2026-06-22) | 2026-10-03 | Inspiration (session sharing, Wolf pairing) |
| [Themaister/pyrowave](https://github.com/Themaister/pyrowave) | Codec | C++/Vulkan | MIT | 649 | — | 2026-10-03 | (other slice) |

Activity summary: Sunshine, Vibepollo/Vibeshine, Foundation, moonlight-qt (master), moonlight-web-stream, moonlight-common-rust, Moonshine and Moonlight V+ are all **very active** (commits within the last 48h). **Apollo is slow.** It has had no release since Aug 2025 and its last commits were May 2026. In April 2026 the maintainer posted a "seasonal bounty" RFC to fight fork fragmentation ([Discussion #1447](https://github.com/ClassicOldSong/Apollo/discussions/1447)). **moonlight-qt has not cut a release since v6.1.0 (Sept 2024)**, although master is busy ([issue #1711](https://github.com/moonlight-stream/moonlight-qt/issues/1711) asks for one).

---

## 2. Hosts

### 2.1 Sunshine (LizardByte)

- **What:** The reference open-source GameStream host. It runs on Windows, Linux, macOS and FreeBSD. A Vue web UI on `:47990` handles config and pairing.
- **Status:** Date-based versioning `vYYYY.MDD.HHMMSS`. Latest stable is **v2026.914.233613 (2026-09-15)**, which includes a high-severity Linux security fix (GHSA-fp6g-27w5-489j). Prereleases ship almost daily ([release](https://github.com/LizardByte/Sunshine/releases/tag/v2026.914.233613)). 2026 highlights:
  - **Vulkan video encode on Linux** with zero-copy DMA-BUF ([Phoronix](https://www.phoronix.com/news/Sunshine-v2026.413.143228)).
  - **FFmpeg 9**.
  - NVENC on Linux without CUDA.
  - XDG-portal capture fallback and KWin capture (`kwingrab.cpp`).
  - PipeWire pts passthrough.
  - macOS gamepads via `libvirtualhid`.
  - A Windows Virtual HID driver.
  - A `gamepad_driver` setting.
  - Connector-name display selection.
- **License:** GPL-3.0.
- **Architecture:** A single daemon (`src/main.cpp`) with:
  - `nvhttp.cpp` for the GameStream HTTP/HTTPS control plane;
  - `rtsp.cpp` for the RTSP handshake;
  - `stream.cpp` for the UDP video/audio/control planes;
  - `video.cpp` for capture → encode (NVENC/AMF/QSV/VAAPI/Vulkan/VideoToolbox/software);
  - `audio.cpp` for Opus;
  - `input.cpp`, which injects input via inputtino/ViGEm/virtual HID;
  - `confighttp.cpp` for the web UI and REST API (Basic auth + CSRF; `/api/apps`, `/api/clients/*`, `/api/config`, `/api/pin`, `/api/apps/close`, …).
  - It vendors moonlight-common-c headers for packet definitions.
- **Upstream lacks:** a virtual display (no VDD code in `src/`), multi-client concurrent sessions, scoped API tokens and a REST "launch app" endpoint. Launching happens only via the GameStream `/launch`.
- **Standout ideas:**
  - Configurable FEC% (default **20**).
  - Encryption policy split into `lan_encryption_mode` (default **never**) and `wan_encryption_mode` (default **opportunistic**).
  - Paced UDP send that uses about 80% of 1 Gbps per ms ([stream.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/stream.cpp)).
  - Recent protocol extensions: `0x5504` set-player-LEDs control message, `x-ss-video[0].intraRefresh`, and the `continuousAudio` launch param.
- **Pain points:** Display/virtual-display handling on Windows is left to third-party tools. That gap is why the Apollo, Vibepollo and Foundation forks exist. There is one active stream session per host.
- **Steal for Cha Portal:**
  - The FEC block-split math and send pacing.
  - The LAN/WAN encryption policy split.
  - Feature-flag negotiation via SDP (`x-ss-general.featureFlags`).
  - Vulkan zero-copy encode path on Linux (reference for Nodes).
  - Docker base images `lizardbyte/sunshine:<ver>-<os>`.
- **Verdict:** **Protocol-compatible.** Treat it as an external endpoint and support pairing automation through `POST /api/pin` with stored admin creds.

### 2.2 Apollo (ClassicOldSong)

- **What:** A Sunshine fork focused on "stream at the client's native resolution". It auto-creates a **SudoVDA virtual display** per client, matched to the client's requested mode, with a stable per-client display identity so Windows remembers its layout. It also adds a **client permission system**, **clipboard sync**, **server commands**, **OTP pairing**, an **input-only mode** and multi-instance support ([README](https://github.com/ClassicOldSong/Apollo)).
- **Status:** v0.4.6 (2025-07-13) and v0.4.7-alpha.1 (2025-08-12). Merges have been sporadic since (last commit 2026-05). It has 339 open issues. The maintainer's 2026-04-02 RFC proposes a voting/bounty process because "different forks implement similar ideas in incompatible ways" ([#1447](https://github.com/ClassicOldSong/Apollo/discussions/1447)). The virtual display is **Windows-only**.
- **License:** GPL-3.0.
- **Protocol extensions (verified in source):**
  - Control-stream messages `0x3000` exec server command (payload = command index), `0x3001` set clipboard and `0x3002` file-transfer nonce request ([stream.cpp](https://github.com/ClassicOldSong/Apollo/blob/master/src/stream.cpp)).
  - HTTPS `GET/POST /actions/clipboard?type=text`.
  - Launch args `virtualDisplay`, `scaleFactor`, `appuuid`.
  - `serverinfo` fields `Permission`, `VirtualDisplayCapable`, `VirtualDisplayDriverReady`.
  - **OTP pairing:** the host UI calls `request_otp(passphrase, deviceName)`. The client sends `otpauth = hex(SHA256(pin + salt + passphrase))` in pair phase 1, so the host needs no PIN entry.
- **Permission bitmask** (`crypto.h`):
  - input: controller / touch / pen / mouse / keyboard;
  - operation: clipboard set/read, file upload/download, server cmd;
  - action: list / view / launch.
  - The first paired client gets everything. Later clients get `view|list` only.
- **Steal:**
  - The **per-client permission bitmask** maps neatly onto Cha Portal roles (viewer/gamer/full).
  - **OTP pairing**, so the dashboard can pair without anyone at the host.
  - A **stable virtual-display identity per client**.
  - Clipboard via side-channel HTTPS.
- **Verdict:** Protocol-compatible plus inspiration. Superseded feature-wise by Vibepollo; treat Apollo-family extensions as one "dialect".

### 2.3 Vibepollo (Nonary) — deep dive

**What it is.** In its own words, an "AI-enhanced version of Apollo" (README). The maintainer says **about 99% of the code is AI-generated**, mostly with Codex/GPT-5.x. The same author maintains **Vibeshine**, a Sunshine-based twin with the same feature set: Vibeshine's README says it differs from upstream by "roughly 99,800 changed lines". Both ship as 2.0.0 (2026-09-30). The repo is a GitHub fork of Apollo with 1,218 stars and 96 open issues. Default branch `master`; the Linux install script references a `vibe-test` branch. Windows releases are code-signed via the SignPath Foundation.

**Status/stability.**

- Releases are very frequent: 2.0.0-beta.1 → beta.4 in Sept 2026, then 2.0.0 (notes dated 2026-09-28, tag 2026-09-30). In parallel there is a 1.18.x "stable" line (1.18.4-stable.4, 2026-09-10).
- Open issues on 2026-10-03 ([issues](https://github.com/Nonary/Vibepollo/issues)) include crashes (#557, #532, #535 lockup on start), "CRITICAL v2.0.0-beta.3 encoding failure" (#518), many Linux virtual-display/KWin issues (#552, #553, #526, #544), PyroWave shimmering (#536), and 10 GbE NIC black screen (#543).
- Antivirus false positives are a known issue ([vibeshine#59](https://github.com/Nonary/vibeshine/issues/59)).
- There is a single maintainer, plus a few contributors on the Playnite plugin and Linux.
- **Expect regressions. Do not build a hard dependency on its internals.**

**Platform support.**

- **Windows 10/11** is primary.
- **Linux beta**: Arch/CachyOS only. Requires x86_64, kernel ≥ 6.16 with headers, a **DKMS kernel module `vibeshine_drm`** for virtual displays, and **KDE Plasma 6 Wayland via SDDM/Plasma Login Manager**. Streaming from the login screen works only on NVIDIA. There are **no AppImage, Flatpak, Debian, Fedora or Docker** builds ([README](https://github.com/Nonary/Vibepollo), [docs/linux/install.md](https://github.com/Nonary/Vibepollo/blob/master/docs/linux/install.md)).
- **SteamOS**: an experimental "user bundle" that captures Gamescope (SDR, plus an HDR path with patched gamescope).
- macOS: inherited code, not advertised **(unverified)**.

**What it adds over Apollo/Sunshine** (README + 2.0.0 notes):

- **Display automation.** Display-layout restore after crashes (Win11 24H2 fixes), a **bundled virtual display driver**, SudoVDA kept as fallback, hybrid-GPU awareness, and automatic headless mode.
- **Capture.** **WGC (Windows Graphics Capture) in service mode**, with an auto-fallback to DDA for UAC/login. Frame-generated capture fixes for DLSS/FSR frame-gen: the virtual display guarantees composed flip and targets 4× refresh.
- **VRR** (with Nonary's client). The client sends `clientVrrRequested`, and the Windows virtual display switches to a **fixed 1000 Hz mode** so newly presented frames are captured promptly. Linux uses presentation-driven KMS capture.
- **PyroWave** on Windows and Linux. See the wire contract below.
- **Free Windows virtual gamepad driver** (`vhf_*` profiles). Emulates Xbox Series/One, DS4, DualSense and Switch Pro with rumble, impulse triggers, touchpad, motion, battery, lightbar and adaptive triggers, without ViGEmBus. On Linux it provides DS4 emulation, plus wireless DualSense haptics and adaptive triggers when both ends run Linux.
- **Remote Monitor / Remote Input.** A second Moonlight device can launch "Remote Monitor" to become an extra, independently streamed display. **Up to 4 clients** (`max_client_vdds = 4`). "Remote Input" attaches a client with no video. These are implemented as **synthetic app tiles with fixed magic IDs** (e.g. `monitor_id = 2147483505`, `terminate_id = 2147483504`) so **stock Moonlight clients can trigger host actions from the app grid** ([remote_session.h](https://github.com/Nonary/Vibepollo/blob/master/src/remote_session.h)).
- **Game library sync.** Playnite (C# plugin), Steam and Lutris, with artwork. **RTSS / NVCP** frame limiting matched to client FPS. Lossless Scaling and NVIDIA Smooth Motion automation. Per-app 10-bit SDR.
- **Web UI v2** (Vue, "dependency-light"). Includes session history, host stats, crash bundles and update notifications.
- **Security/auth.** Session-cookie login with "remember me" (`__Host-` cookie + refresh rotation). **API tokens scoped to path regex + HTTP methods.** Only the token hash is stored. CSRF protection for browser origins (`csrf_allowed_origins`) ([docs/api.md](https://github.com/Nonary/Vibepollo/blob/master/docs/api.md)).
- **Runtime bitrate.** HTTPS `GET /bitrate?bitrate=<kbps>` changes encoder bitrate mid-stream; it is clamped to `max_bitrate` and 500 Mbps. `GET /api/abr/capabilities` returns `{"supported":false,"version":1,"features":["runtime_bitrate"]}`. That tells "Foundation-compatible clients (e.g. Moonlight V+)" to run their own client-side ABR ([nvhttp.cpp](https://github.com/Nonary/Vibepollo/blob/master/src/nvhttp.cpp)).

**Web UI / REST API (`confighttp`, HTTPS :47990)** — routes enumerated from source:

- **Apps:** `/api/apps` (GET/POST), `/api/apps/{id}`, `/api/apps/launch`, `/api/apps/close`, `/api/apps/reorder`, `/api/apps/{uuid}/cover|icon`, `/api/covers/*`.
- **Clients:** `/api/clients/list|update|unpair|unpair-all|disconnect|display-layout|hdr-profiles`.
- **Session/host:** `/api/session/status`, `/api/rtsp/sessions`, `/api/host/info`, `/api/host/stats`, `/api/history/sessions[/active|/{id}]`.
- **Display:** `/api/display-devices`, `/api/display/golden*`, `/api/display/terminate_virtual`, `/api/framegen/edid-refresh`.
- **Providers:** `/api/steam/*`, `/api/lutris/*`, `/api/playnite/*` (games, launch, force_sync, status).
- **Auth/pairing:** `/api/auth/login|logout|refresh|status|sessions`, `/api/token[s]`, `/api/token/routes`, `/api/otp`, `/api/pin`, `/api/password`, `/api/csrf-token`.
- **WebRTC:** `/api/webrtc/capabilities`, `/api/webrtc/cert`, `/api/webrtc/sessions` (POST create), `/api/webrtc/sessions/{id}`, `/offer`, `/answer`, `/ice`, `/ice/stream` (SSE).
- **Health/logs:** `/api/health/*`, `/api/logs[/export]`, `/api/restart`, `/api/quit`.
- **Readiness metadata:** `/api/metadata` exposes `encoder_status` (h264/hevc/av1) and Linux virtual-display/capture status.

→ This is **enough for Cha Portal to manage a Vibepollo host remotely**: list and launch apps, see sessions, kick clients, and pair via OTP. Use a **scoped bearer token** rather than admin credentials.

**GameStream HTTP additions** (`nvhttp`):

- Routes: `/bitrate`, `/api/abr/capabilities`, `/pyrowave-bandwidth-probe`, and Apollo's `/actions/clipboard`.
- `serverinfo` adds `FrameLimiter*`, `VirtualDisplayHDRCapable`, `PyroWaveHostLinkMbps` and `PyroWaveBandwidthProbeBytes`.
- Launch args add `clientVrrRequested`, `psmap`, `bitrate` and `clientName`.

**Built-in WebRTC (browser) streaming:**

- Implementation: `src/webrtc_stream.cpp`, 6.5k lines.
- It links a **libwebrtc C wrapper** (`third-party/libwebrtc`). Build flag `SUNSHINE_ENABLE_WEBRTC` is **"Windows only"** and default OFF, but **ON in Windows CI** (`ci-windows.yml`).
- Session options: codec `h264|hevc|av1`, HDR, 4:4:4, audio `opus|aac`, pacing modes `latency|balanced|smoothness`, and max frame age.
- The default is `encoded=true`: **encoder output is fed straight into WebRTC (passthrough)**, plus a raw-frame path.
- Input arrives as JSON on a DataChannel labeled `input` and is translated to moonlight-common-c `Input.h` structures. Viewers' held keys are released on WebRTC loss.
- Signaling is plain REST plus an SSE stream for ICE. I found **no STUN/TURN config in source**, so it is effectively LAN-oriented **(unverified)**.
- The repo's `architecture.md` has a full "WebRTC deep dive".
- Verdict: this proves a *host-native* WebRTC path is viable. Cha Nodes could do the same in their own encoder process.

**PyroWave wire contract** ([docs/pyrowave-protocol.md](https://github.com/Nonary/Vibepollo/blob/master/docs/pyrowave-protocol.md)). This is the most important artifact for Cha Portal's PyroWave goal. Summary:

- **Bitstream pinning.** Both ends vendor PyroWave at commit **`186f0393`**. The bitstream has no version field, so the host advertises `a=x-ss-pyrowave.bitstream:186f0393` in RTSP DESCRIBE and the client warns on mismatch.
- **Capability bits** in `ServerCodecModeSupport`:

  | Bit | Name | Format |
  |---|---|---|
  | `0x00800000` | `SCM_PYROWAVE` | 8-bit 4:2:0 |
  | `0x01000000` | `SCM_PYROWAVE_444` | 8-bit 4:4:4 |
  | `0x02000000` | `SCM_PYROWAVE_HDR10` | 10-bit 4:2:0 |
  | `0x04000000` | `SCM_PYROWAVE_HDR10_444` | 10-bit 4:4:4 |

  These are **not upstream-registered**, so they could collide with future upstream bits.
- **Bandwidth probe.** `serverinfo` (paired HTTPS) adds `PyroWaveHostLinkMbps` and `PyroWaveBandwidthProbeBytes=33554432`. `GET /pyrowave-bandwidth-probe` returns 32 MiB to time. The client takes the slowest of 3 runs and reserves 20%.
- **RTSP DESCRIBE.** Includes `a=rtpmap:99 PYROWAVE/90000` as a capability marker; payload type 99 is never actually sent.
- **RTSP ANNOUNCE.** Sets `x-nv-vqos[0].bitStreamFormat=3`; 0/1/2 are H.264/HEVC/AV1. Other attributes: `x-ss-video[0].pyrowaveFeatures` (bit 0x1 = record framing), `pyrowaveAdaptiveFec`, and `pyrowaveAdaptiveBitrate`. The host replies `400` if unavailable.
- **Transport.** Reuses the normal GameStream video RTP (`NV_VIDEO_PACKET`, ≤4 FEC blocks, optional AES-GCM). `frameType` is always IDR. **IDR/RFI requests are ignored** because every frame is intra.
- **Record framing.** The frame is a sequence of 32-bit LE records:
  - a `BitstreamSequenceHeader`;
  - block records;
  - padding records `0xFFFFFFFF, N, N×0`.
- **Packing for resilience.** The **coarsest wavelet level** is packed first into "critical" packets, which get Reed-Solomon parity (`pyrowave_critical_fec_percentage`). Finer detail is packed first-fit with no parity, except for **adaptive extra parity** when cadence drops below the negotiated FPS: `min(50, 100*(fps/observed−1))`.
- **Lossy decode.** Lost finer packets are replaced with zeros and **the frame still decodes, slightly blurred**. That needs `BUFFER_TYPE_LOST` / `BUFFER_TYPE_RECORD_START` support in a patched moonlight-common-c.
- **Budgets and pacing.** The per-frame byte budget is capped at bitrate/negotiated FPS, with ≤4000 packets per frame. Pacing follows routed link speed. A newer frame replaces a pending unsent frame.
- **Guidance.** About **1.6 bits/pixel** for clean 4:2:0 SDR (≈200 Mbps @ 1080p60). 4:4:4 costs ×1.6 and 10-bit ×1.15. **Wired LAN only.**
- **Compatible clients:**
  - Nonary's moonlight-qt fork (D3D11 on Windows, libplacebo Vulkan on Linux).
  - "Aurora" client **(unverified)**.
  - The azafrob / andygrundman / dimizago "length-prefixed framing" clients, including [Moonlight PyroWave for Xbox](https://github.com/dimizago/moonlight-xbox/releases/tag/v1.18.1-pyrowave.1).
- **Linux encoder.** Imports DMA-BUF into Vulkan and does scale + CSC + cursor + wavelet encode on the GPU. NvFBC capture is unsupported ([docs/linux/pyrowave.md](https://github.com/Nonary/Vibepollo/blob/master/docs/linux/pyrowave.md)).

**Codec support overall:** H.264, HEVC and AV1 via NVENC/AMF/QSV/VAAPI/Vulkan (inherited), plus PyroWave.

**Steal for Cha Portal:**

1. **The PyroWave-over-GameStream wire contract, verbatim**, so Cha Nodes interoperate with existing PyroWave Moonlight clients. Also its *ideas* for Cha's own protocol: FEC only on the critical band, partial-frame decode and replace-pending-frame.
2. Scoped API tokens (path + method) and the `__Host-` cookie with refresh rotation.
3. **Synthetic app tiles as a control surface for stock Moonlight clients.** Expose "Terminate env", "Snapshot", "Add monitor" and similar actions as fake apps.
4. Runtime `/bitrate` plus a client-driven ABR capability flag.
5. A bandwidth probe endpoint over the pinned HTTPS connection.
6. Fixed high-refresh virtual display for VRR capture.
7. The idea of a free virtual gamepad driver: DualSense haptics and adaptive triggers end-to-end.

**Verdict: protocol-compatible + adopt its PyroWave extension.** Integrate Vibepollo/Vibeshine as a first-class **external endpoint type** with REST-API management: OTP pairing, app list and launch, session status. Do not fork it: it is Windows-centric, monolithic and AI-churned.

### 2.4 Foundation Sunshine (AlkaidLab; formerly qiin2333/Sunshine-Foundation)

- **What:** A Chinese-community Sunshine fork, at 7k stars and very active. It adds HDR10/HDR Vivid, ZakoVDD virtual display management, **microphone redirection from client to host**, advanced audio (7.1.4), runtime bitrate (the "Foundation ABR" API that Vibepollo mirrors) and a modern control panel ([repo](https://github.com/AlkaidLab/foundation-sunshine), [config docs](https://github.com/AlkaidLab/foundation-sunshine/blob/master/docs/configuration.md)).
- **Companion client:** Moonlight V+ (Android), with mic, 7.1.4, live bitrate, QR pairing, multi-display, and "don't disconnect when switching apps".
- **License:** GPL-3.0. **Status:** dated prereleases; latest stable 2026-09-25.
- **Protocol extension:** a mic stream negotiated in RTSP and sent as UDP payloads. moonlight-common-rust implements it in `stream/proto/microphone/foundation/`, referencing [qiin2333/moonlight-common-c](https://github.com/qiin2333/moonlight-common-c) and moonlight-common-c PR #123.
- **Steal:** a mic-uplink design and client-driven ABR.
- **Verdict:** Protocol-compatible. Implement the Foundation mic dialect if browser mic is wanted for GameStream hosts.

### 2.5 Other GameStream-speaking hosts

- **Wolf** ([games-on-whales/wolf](https://github.com/games-on-whales/wolf), MIT, C++, GStreamer).
  - Per-session Docker containers with a headless Wayland compositor, multi-user "lobbies", and a REST API on `/api/v1` (pairing can be completed by posting the PIN via the API, as linckosz/moonlight-web does).
  - It also publishes the **best public protocol docs**: [HTTP pairing](https://games-on-whales.github.io/wolf/stable/protocols/http-pairing.html), RTSP, RTP video/audio, [control](https://games-on-whales.github.io/wolf/stable/protocols/control-specs.html) and input.
  - Detailed in another slice.
- **Moonshine** ([hgaiser/moonshine](https://github.com/hgaiser/moonshine), **BSD-2-Clause**, Rust, v0.16.1 on 2026-09-14). "Headless streaming server for Moonlight clients."
  - **Each stream runs in its own compositor**, built on smithay with xwayland and gamescope-swapchain support, so it needs no monitor or dummy plug.
  - Encode via **Vulkan Video** (`pixelforge`): H.264/HEVC/AV1 + 10-bit HDR.
  - Input via **inputtino**. Audio via an embedded Pulse server and Opus. `tokio-enet`, `fec-rs`, and `quinn-udp` with GSO for video send.
  - Steam, Lutris, Heroic and desktop app scanners. mDNS. Linux/systemd only. Needs Moonlight ≥ 6.0.
  - This is a clean, permissively-licensed **Rust GameStream host**, an ideal reference or donor for a Rust Cha Node.
- **Punktfunk** ([git.unom.io/unom/punktfunk](https://git.unom.io/unom/punktfunk), GitHub fork [rongrong666/punktfunk](https://github.com/rongrong666/punktfunk); **MIT OR Apache-2.0**, Rust; Cargo version 0.24.0).
  - Linux + Windows host. Per-client virtual display at the exact client mode: KWin, gamescope, Mutter, wlroots, and an **all-Rust Windows IddCx driver with push capture**.
  - GPU zero-copy NVENC; Vulkan Video on AMD/Intel.
  - A **GameStream host for stock Moonlight** (opt-in `--gamestream`, "trusted-LAN only — GameStream has inherent on-path weaknesses").
  - Its own **`punktfunk/1`**: QUIC control + **GF(2¹⁶) Leopard-RS** FEC + AES-GCM data plane, SPAKE2 PIN pairing, and **mid-stream mode renegotiation**.
  - Native clients for macOS/iOS/tvOS, Linux, Windows and Android. A web console over an OpenAPI management API.
  - The roadmap lists ICE/STUN/TURN with a self-hostable relay, QUIC migration and per-user sessions. It lists no browser client ([roadmap](https://docs.punktfunk.unom.io/docs/roadmap)).
  - **Very relevant to Cha's "thin native client" and Node design** (another slice may cover it in depth).
- **linckosz/moonlight-web native engine.** See §3.6. It acts as a host itself, with WebRTC to the browser only, not GameStream.
- **MultiSeat / Duo.** Windows multiseat orchestration: [MultiSeat](https://github.com/vibesoftwarecoder/MultiSeat) runs one Apollo instance per seat with a web dashboard; [Duo](https://github.com/DuoStream) is based on TermWrap + Sunshine. These are Windows-only and only relevant if Cha supports Windows nodes.

---

## 3. Clients

### 3.1 moonlight-common-c (core)

- **What:** The shared GameStream client core used by moonlight-qt, Android, iOS, embedded and others. It covers RTSP, the ENet control stream, RTP video/audio queues, Reed-Solomon FEC (`nanors`, now upstream with SIMD), AES (GCM/CBC), input packet builders and connection testing. **No HTTP or pairing** lives here; that is in each client (e.g. moonlight-embedded's `libgamestream`, moonlight-qt's `nvhttp.cpp`/`nvpairingmanager.cpp`).
- **Status:** Commits on 2026-09-26. 2026 work includes:
  - MbedTLS PSA rewrite;
  - **`LI_CTYPE_STEAM`** controller type;
  - **dual-touchpad controllers** (`LI_CCAP_DUAL_TOUCHPAD`, `touchpadIndex`);
  - `MODIFIER_EXTENDED` keyboard flag;
  - RFI fixes;
  - NXDK (original Xbox) and 3DS ports;
  - RTSP hardening.
- **License:** GPL-3.0. On **2026-09-26 an "App Store Distribution Exception" (GPL §7 additional permission)** was added ([commit 5a26329](https://github.com/moonlight-stream/moonlight-common-c/commit/5a2632990f5d77f6090cfb6f70797c1e16a630e0)). It allows app-store distribution if the source stays available under the GPL elsewhere.
- **Architecture gotcha:** Heavy use of **file-scope `static` globals** (sockets, ENet host, crypto contexts, queues). That means **one connection per process**. moonlight-web-stream v2 had to **spawn a "streamer" subprocess per stream** for this reason (v2.10.0 README). The README also warns it **requires its bundled, patched ENet**: it is ABI-incompatible with system libenet.
- **Steal:** The whole protocol knowledge base. Specifically speculative RFI, `SS_FRAME_FEC_STATUS` loss telemetry, and the connection tester / port-flag diagnostics (`LiTestClientConnectivity`, `LiStringifyPortFlags`). The latter are great for a dashboard "why can't I connect" view.
- **Verdict:** Reference implementation. If Cha embeds it (C, e.g. in a native thin client), use **one process per session**. For a multi-tenant gateway, prefer moonlight-common-rust.

### 3.2 moonlight-qt (PC) and desktop forks

- **Upstream:** Windows/macOS/Linux/Steam Link. Last tagged release **v6.1.0 (2024-09-17)**; master is active daily (Qt 6.12, SDL3, libplacebo, Enter vs Numpad-Enter, …). Supports H.264/HEVC/AV1, HDR, 4:4:4 (master), 5.1/7.1 audio, and gamepad motion/touchpad/haptics. GPL-3.0.
- **[Nonary/moonlight-qt](https://github.com/Nonary/moonlight-qt) "VRR Moonlight Client"** (v6.1.0-vrr18, 2026-09-30):
  - **PyroWave decode**: D3D11 on Windows, libplacebo Vulkan on Linux, async compute.
  - Bitrate/hardware calibration sweeps and a throughput probe.
  - **VRR presentation profiles** (Low latency / Balanced / Smooth, reduce judder). Steam Deck present-to-flip dropped from 3.9 to 2.0 ms at 4K 4:4:4.
  - Wireless DualSense haptics on Linux.
  - Needs a Vibeshine/Vibepollo host ([release](https://github.com/Nonary/moonlight-qt/releases/tag/v6.1.0-vrr18)).
- **Artemis desktop** is fragmented:
  - wjbeckett/artemis (Qt; the GitHub parent of the NextdoorPsycho fork; status **(unverified)**);
  - [NextdoorPsycho/Artemis-Multiplatform-Mac-Win-Linux](https://github.com/NextdoorPsycho/Artemis-Multiplatform-Mac-Win-Linux) (macOS-first; Apollo permissions, OTP, clipboard, server cmds);
  - [Unitron07/Artemis-Windows](https://github.com/Unitron07/Artemis-Windows);
  - [Lavagnou/LavArtemis-Qt](https://github.com/Lavagnou/LavArtemis-Qt).
- **[FoggyBytes/StreamLight](https://github.com/FoggyBytes/StreamLight)**: a gamepad-first moonlight fork (GPL-3.0, 135 stars).
- **Steal:** VRR pacing profiles, PyroWave calibration UX, and the connection diagnostics UX.

### 3.3 Android: moonlight-android, Artemis, Moonlight V+

- **Upstream** v12.2 (2026-09-12): Android 16 target, keyboard capture on 16.1+, F13–F24, native USB Xbox driver.
- **Artemis** (Apollo companion, GPL-3.0):
  - custom resolutions and bitrates;
  - multiple mouse modes, including **local cursor mode**;
  - touchpad/trackpad modes;
  - external-monitor mode;
  - **virtual display / server-command / clipboard integration with Apollo** ([README](https://github.com/ClassicOldSong/moonlight-android)).
  - Releases have stalled since Aug 2025, although pushes continue.
- **Moonlight V+** (Foundation companion): up to 800 Mbps, 144/165 Hz, HLG, mic redirection, 7.1.4, live bitrate, QR pairing, audio-driven haptics.
- **Steal (UX for Cha's browser client on touch devices):** trackpad mode vs direct touch, local cursor mode, custom on-screen controls, QR pairing.

### 3.4 iOS/tvOS, embedded, Chrome, others

- **moonlight-ios:** active (Xcode 27 update and YUV444 strings on 2026-09-26). Distributed via the App Store, hence the new §7 exception.
- **moonlight-embedded** v2.7.1 (2025-11-30). Pi/Linux framebuffer client. Its `libgamestream` is a compact **C reference for HTTP + pairing**.
- **moonlight-chrome:** **archived 2024-02**. It was a NaCl/PNaCl app; NaCl is dead. It is the only historical "in-browser native protocol" Moonlight, and it relied on Chrome App socket APIs. A modern equivalent would be **Isolated Web Apps + Direct Sockets** (Chrome; availability/platforms **unverified**) or WebTransport via a relay.
- **Xbox:** TheElixZammuto/moonlight-xbox, plus the [dimizago PyroWave build](https://github.com/dimizago/moonlight-xbox/releases/tag/v1.18.1-pyrowave.1) (D3D12 PyroWave decode).

### 3.5 Browser: MrCreativ3001/moonlight-web-stream — deep dive

- **What:** A self-hosted web server, the "Moonlight Web" client. It pairs with Sunshine-family hosts and forwards the stream to a browser over **WebRTC**, with a **WebSocket** fallback ([README](https://github.com/MrCreativ3001/moonlight-web-stream)).
- **Status:**
  - 710 stars, GPL-3.0-or-later, pushed 2026-10-03. v3.0.0-prerelease.7 (2026-09-16); docs for stable are on the `v2.10.0` branch. 29 open issues.
  - Commits in Sept 2026 added **FlexFEC**, PLI/FIR fixes, the webrtc-rs RC upgrade, touch improvements and an on-screen keyboard.
  - Single main author plus contributors (e.g. Tom1tk: "stop host stream when web socket closes").
- **Architecture (v3, read from source):**
  - **Server:** Rust 2024, `actix-web` + `rustls`, **`webrtc`/`rtc` 0.21 (webrtc-rs, the new sans-IO stack)**, and **`moonlight-common` = moonlight-common-rust** (features `tokio`, `tokio-hyper`, `rustcrypto`, `stream-proto`).
    - v3 is a **single async process**. v2 spawned a per-stream `streamer` subprocess using moonlight-common-c bindings (`stream-c`, OpenSSL).
    - Users, roles and permissions live in JSON storage. It supports reverse-proxy auth via a forwarded username header, a URL path prefix, an ICE server list/script (TURN credentials), a WebRTC UDP port range, and NAT 1:1 IPs.
  - **Video path, passthrough with no transcode:**
    1. moonlight-common-rust receives Sunshine RTP, applies RS FEC recovery and reassembles the **frame** (Annex-B access unit or AV1 OBUs).
    2. `VideoChannel` re-packetizes with webrtc-rs `H264Payloader`, a custom `H265Payloader` (RFC 7798) or `Av1Payloader` into a `TrackLocalStaticRTP`.
    3. It adds the **playout-delay header extension (min=max=0)** and a color-space extension for HDR, and registers **NACK, goog-remb and FlexFEC-03**.
    4. **Browser PLI/FIR → host IDR request.**
    - Codec choice is driven by the browser's offered RTP codecs, intersected with host `ServerCodecModeSupport`.
  - **Audio:** Opus RTP passthrough, also with playout-delay 0.
  - **Control/input:** WebRTC DataChannel `moonlight.control` (reliable/ordered) plus optional `moonlight.control.*` unreliable channels. The **browser builds the raw (unencrypted) GameStream control packets itself**, using **moonlight-common-rust compiled to WASM** via uniffi-bindgen-react-native (`ubrn build web`). The server forwards them onto the ENet control stream.
  - **Browser receive tuning:** `receiver.jitterBufferTarget = 0` and `playoutDelayHint = 0` (web/stream/transport/webrtc.ts).
  - **Fallback transport: WebSocket.** Frames go to **WebCodecs `VideoDecoder`**, which needs a secure context, and are rendered to canvas or OffscreenCanvas in a worker pipeline. There is a MediaSource fallback, an optional openh264-WASM software decoder, and libopus WASM for audio.
  - **Signaling spec:** [`src/webrtc/protocol.md`](https://github.com/MrCreativ3001/moonlight-common-rust/blob/master/src/webrtc/protocol.md) "Moonlight over WebRTC [WIP]", which is **WHIP/WHEP-inspired**:
    - `OPTIONS` returns ICE servers as `Link` headers.
    - `POST application/sdp` returns `201` + `Location`.
    - `PATCH application/trickle-ice-sdpfrag` and `DELETE` work as in WHIP.
    - Custom SDP attrs: `a=x-moonlight-app-id`, `x-moonlight-mode:WxHxFPS`, `x-moonlight-bitrate`, `x-moonlight-hdr`, `x-moonlight-preferred-codec`, `x-moonlight-host-id`, `x-moonlight-microphone`.
    - Control payloads are the unencrypted Wolf-documented control packets.
    - Mic is a `sendonly` audio transceiver.
- **Latency:** No published glass-to-glass numbers found **(unverified)**. Structurally it adds one reassemble/repacketize hop plus the WebRTC receive pipeline. With playout-delay 0 that should be roughly native Moonlight + a few ms when the gateway is on the host's LAN.
- **Pain points** ([issues](https://github.com/MrCreativ3001/moonlight-web-stream/issues)):
  - audio desync/disconnects (#171);
  - **HEVC cropping on Edge** and H.265 resolution cut-off (#168, #77);
  - "stream becomes unstable over time" (#150);
  - **macOS host encoder not producing IDRs over WebRTC** (#145);
  - ICE failures with STUN IP literals (#101);
  - **ABR requested** (#169);
  - Ctrl+Alt+Del capture (#146);
  - mobile IME (#133);
  - Defender false positive (#103).
- **v3 regressions** from leaving moonlight-common-c ([moonlight-common-rust README](https://github.com/MrCreativ3001/moonlight-common-rust)): no **video encryption**, no **RFI/LTR**, AV1 not parsed (passed through), and potentially no old GFE/Sunshine support. **OpenSSL was swapped for rustls, so old self-signed certs break.**
- **Steal:**
  1. The **whole Rust gateway design**: moonlight-common-rust + webrtc-rs passthrough, PLI→IDR, playout-delay=0, FlexFEC, codec intersection.
  2. **The WHIP-like signaling spec**. Make Cha's browser↔Node signaling a superset of it, so Moonlight-Web and Cha clients can interoperate.
  3. **Running the protocol core in WASM in the browser**, so the server just forwards control bytes.
  4. The transport abstraction (WebRTC | WebSocket), with WebTransport as the obvious third.
  5. Reverse-proxy auth headers and an ICE-server script for short-lived TURN creds.
- **Verdict: depend-on (moonlight-common-rust) and fork or vendor the gateway parts.** The licenses are compatible with copyleft Cha. Upstream improvements (AV1 parsing, video decryption, RFI) are worth contributing back.

### 3.6 Other browser attempts

- **[linckosz/moonlight-web](https://github.com/linckosz/moonlight-web)** (GPL-3.0, C++17/Qt 6.11 server, created 2026-06-22, 96 stars).
  - Its own **native capture/encode engine**: "capture → encode (zero-copy) → SCTP/DTLS → browser". That means video over **DataChannels**, "no RTSP, RTP or FEC" on that path.
  - The browser uses **WebCodecs + WebGPU/canvas** and an **AudioWorklet**.
  - For other hosts it **embeds moonlight-common-c** and re-packetizes onto WebRTC.
  - It pairs with Sunshine/Apollo by PIN, **Wolf automatically via `/api/v1`**, and MultiSeat seats via API.
  - Claims "< 20 ms glass-to-glass over Wi-Fi on LAN, ~25 ms over the Internet" **(unverified)**. An opt-in internet relay at `stream.moonlightweb.top/<id>` (third-party).
  - **Session sharing**: up to 3 guests with Viewer/Gamer/Desktop/Full permission levels, link + 6-digit PIN, lifetimes, and mid-game permission changes.
  - Steal: **session-sharing UX and permission tiers**, the Wolf API pairing flow, and DataChannel video as an alternative to RTP tracks. Verdict: inspiration.
- **[spacedouut/moonlight-web-stream-simplified](https://github.com/spacedouut/moonlight-web-stream-simplified)**: a 0-star fork with WebTransport "still in the works". Ignore.
- **Vibepollo built-in WebRTC** (see §2.3): host-native, Windows-only.

---

## 4. Protocol deep dive (GameStream as implemented by Sunshine + moonlight-common-c, 2026)

Primary sources:

- [moonlight-common-c/src](https://github.com/moonlight-stream/moonlight-common-c/tree/master/src): `Limelight.h`, `Input.h`, `Video.h`, `RtspConnection.c`, `SdpGenerator.c`, `ControlStream.c`, `RtpVideoQueue.c`, `RtpAudioQueue.c`, `AudioStream.c`, `ConnectionTester.c`.
- [Sunshine/src](https://github.com/LizardByte/Sunshine/tree/master/src): `nvhttp.cpp`, `rtsp.cpp`, `stream.cpp`, `crypto.cpp`, `config.cpp`.
- [Wolf protocol docs](https://games-on-whales.github.io/wolf/stable/protocols/index.html).
- [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust) (well-commented Rust re-implementation with links to the C sources).

### 4.1 Ports (Sunshine base port 47989; offsets in source)

| Port | Proto | Purpose | Sunshine offset |
|---|---|---|---|
| 47984 | TCP | HTTPS (paired: applist, launch, resume, cancel, pair phase 5, appasset) | base −5 |
| 47989 | TCP | HTTP (serverinfo, pair phases 1–4) | base +0 |
| 47990 | TCP | Web UI / REST API (Sunshine-specific) | base +1 |
| 47998 | UDP | Video RTP | base +9 |
| 47999 | UDP | Control (ENet) | base +10 |
| 48000 | UDP | Audio RTP | base +11 |
| 48002 | UDP | Mic (forks) **(unverified exact port)** | base +13 |
| 48010 | TCP (UDP for GFE `rtspru://`) | RTSP | base +21 |

Clients learn the actual UDP ports from RTSP SETUP responses, so hosts can move them dynamically (Wolf does). Discovery uses mDNS `_nvstream._tcp`. Moonlight also has a "connection tester" that probes these ports.

### 4.2 HTTP(S) control plane

All responses are XML `<root status_code=...>`. Requests carry `uniqueid` and `uuid` query params. After pairing, HTTPS uses **mutual TLS with the pinned client cert**.

- `GET /serverinfo`. Fields include:
  - `hostname`, `appversion` (e.g. `7.1.431.-1`), `GfeVersion`, `uniqueid`, `mac`, `LocalIP`, `ExternalPort`, `HttpsPort`;
  - `PairStatus` (HTTPS only), `currentgame`, `state` (`SUNSHINE_SERVER_FREE|BUSY`);
  - **`ServerCodecModeSupport`** bitmask (`SCM_H264 0x1`, `SCM_HEVC 0x100`, `SCM_HEVC_MAIN10 0x200`, `SCM_AV1_MAIN8 0x10000`, `SCM_AV1_MAIN10 0x20000`, 4:4:4 bits `0x40000–0x400000`; Vibepollo PyroWave `0x800000–0x4000000`);
  - `MaxLumaPixelsHEVC`.
  - Forks add `Permission`, `VirtualDisplay*`, `FrameLimiter*` and `PyroWave*`.
- `GET /applist` (HTTPS) returns apps with `ID`, `AppTitle`, `IsHdrSupported`; Apollo adds `UUID`. `GET /appasset?appid=&AssetType=2&AssetIdx=0` returns box art (PNG).
- `GET /launch` (HTTPS) parameters (moonlight-common-rust `http/launch.rs`):
  - `appid`, `mode=WxHxFPS`, `additionalStates=1`, `sops` (optimize game settings);
  - **`rikey` (hex AES-128 key) + `rikeyid` (int IV seed)**, which are the session keys for input/audio/(video) encryption;
  - `localAudioPlayMode`, `surroundAudioInfo` (channel count/mask packed), `remoteControllersBitmap`/`gcmap`, `gcpersist`;
  - `hdrMode=1` + `clientHdrCap*`;
  - `corever` (1 = RTSP encryption supported).
  - The response has `sessionUrl0` (an `rtsp://`, `rtspenc://` or `rtspru://` URL) and `gamesession`.
  - Sunshine adds `continuousAudio`. Apollo adds `virtualDisplay`, `scaleFactor`, `appuuid`. Vibepollo adds `clientVrrRequested`, `psmap`, `bitrate`, `clientName`.
- `GET /resume` (HTTPS): same keys, reattaches to the running app. `GET /cancel` quits the running app.
- `GET /unpair`.
- Forks: `/actions/clipboard` (Apollo), `/bitrate`, `/api/abr/capabilities` and `/pyrowave-bandwidth-probe` (Vibepollo/Foundation).

### 4.3 Pairing (PIN, cert exchange)

Five phases on `/pair` ([Wolf doc](https://games-on-whales.github.io/wolf/stable/protocols/http-pairing.html), `http/pair/phase1..5.rs`). The client has a self-signed RSA-2048 X.509 cert and the user types a **4-digit PIN** shown by the client into the host UI.

1. **Phase 1:** `phrase=getservercert&salt=<16B hex>&clientcert=<PEM hex>`. The host waits for the PIN to be entered (async), then returns `plaincert` (the server cert). **AES key = SHA-256(salt‖PIN)[0..16]** (Gen 7; older GFE used SHA-1).
2. **Phase 2:** `clientchallenge=<AES-128-ECB(random16)>` returns `challengeresponse` = ECB(SHA256(challenge‖serverCertSignature‖serverSecret) ‖ serverChallenge).
3. **Phase 3:** `serverchallengeresp=<ECB(SHA256(serverChallenge‖clientCertSignature‖clientSecret))>` returns `pairingsecret` = serverSecret ‖ RSA-SHA256 signature (verified against the server cert).
4. **Phase 4:** `clientpairingsecret` = clientSecret ‖ signature by the client key.
5. **Phase 5 (HTTPS, mTLS):** `phrase=pairchallenge`, which confirms the client cert is now trusted.

Notes:

- The PIN is the only shared secret: ~13 bits, and an offline brute-force of a captured exchange is feasible. **Pairing must happen on a trusted network.**
- Apollo **OTP**: `otpauth=hex(SHA256(pin‖salt‖passphrase))` in phase 1 lets a pre-provisioned PIN+passphrase pair without anyone typing at the host.
- Sunshine's REST `POST /api/pin` lets an admin-authenticated tool submit the PIN, which enables **dashboard-driven pairing**.
- Wolf exposes pending pair requests via its API.

### 4.4 RTSP handshake

Sequence (`performRtspHandshake`):

1. **`OPTIONS`**.
2. **`DESCRIBE`**: the host SDP lists codecs, surround params, RFI support, `x-ss-general.featureFlags` and encryption supported/requested flags.
3. **`SETUP streamid=audio/0/0`**: the response has the `Session` id, the port in `Transport`, and **`X-SS-Ping-Payload`**.
4. **`SETUP streamid=video/0/0`**.
5. **`SETUP streamid=control/13/0`**: the response has **`X-SS-Connect-Data`**.
6. **`ANNOUNCE`**: the client SDP (below).
7. **`PLAY`**.

Header `X-GS-ClientVersion`. TCP for Sunshine.

- **RTSP encryption** (`rtspenc://`, `corever=1`): each message is framed as `BE32(0x80000000 | len)` + `BE32(seq)` + 16-byte GCM tag + ciphertext. **AES-128-GCM** keyed with `rikey`; the IV is derived from the sequence number.
- **Ping payload / connect data** are Sunshine extensions (`ML_FF_SESSION_ID_V1`). The client's first UDP packets on each port carry the 16-byte payload, so the host binds UDP flows to the session. This makes multiple clients behind one NAT and port changes work.

**Client ANNOUNCE attributes** (from `SdpGenerator.c`):

- `x-nv-video[0].clientViewportWd/Ht`, `maxFPS`, `clientRefreshRateX100`, **`packetSize`** (1392 LAN / 1024 remote by default), `rateControlMode`, `timeoutLengthMs`, `framesWithInvalidRefThreshold`, `initialBitrateKbps`, `initialPeakBitrateKbps`, `averageBitrate`, `peakBitrate`, `videoEncoderSlicesPerFrame`, `maxNumReferenceFrames`, **`dynamicRangeMode`** (HDR), `encoderCscMode` (colorspace and range);
- `x-nv-vqos[0].bw.maximumBitrateKbps/minimumBitrateKbps`, `fec.enable`, `fec.repairPercent`, `fec.minRequiredFecPackets`, **`bitStreamFormat`** (0 H.264, 1 HEVC, 2 AV1, 3 PyroWave in Vibepollo), `drc.*`, `qosTrafficType`;
- `x-nv-audio.surround.numChannels/channelMask/enable/AudioQuality`, `x-nv-aqos.packetDuration`;
- `x-nv-general.useReliableUdp`, `featureFlags`, `x-nv-ri.useControlChannel`;
- **`x-ml-video.configuredBitrateKbps`**, `x-ml-general.featureFlags`, **`x-ss-general.encryptionEnabled`**, **`x-ss-video[0].chromaSamplingType`** (4:4:4);
- Sunshine also parses `x-ss-video[0].intraRefresh`.

### 4.5 Video: RTP + NV header + Reed-Solomon FEC

- **RTP header** (12 B, or 16 with extension) + **`NV_VIDEO_PACKET`** (16 B): `streamPacketIndex`, `frameIndex`, `flags` (PIC_DATA 0x1, EOF 0x2, SOF 0x4), `extraFlags` (LTR 0x1), `multiFecFlags`, `multiFecBlocks`, `fecInfo`.
- **`fecInfo` bitfield:** data shards `(fecInfo & 0xFFC00000) >> 22`, FEC index `(… & 0x3FF000) >> 12`, FEC percent `(… & 0xFF0) >> 4`.
- **`multiFecBlocks`:** current block `(>>4)&3`, last block `(>>6)&3`. That gives **max 4 FEC blocks per frame**.
- The first payload carries an 8-byte **frame header** (frame type, `lastPayloadLen` for AV1 trimming).
- **RS over GF(2⁸): ≤255 shards per block.** Sunshine computes `maxDataShards = 255·100/(100+FEC%)` (212 at 20%) and splits frames into ≤4 blocks. **If more are needed, FEC is disabled for that frame** ("over 800 packets at 20%"), and frames beyond 4096 packets (10-bit index) are unrecoverable (`stream.cpp`). At 1392-byte packets that caps FEC-protected frames at about 1.15 MB, i.e. **≈1.1 Gbps at 120 fps or ≈0.55 Gbps at 240 fps**. This is the "FEC wall" Punktfunk escapes with GF(2¹⁶).
- **Video encryption** (optional `SS_ENC_VIDEO`): a per-packet `ENC_VIDEO_HEADER` {12-byte IV, frameNumber, 16-byte tag} with **AES-GCM**, and the header padded to 16-byte multiples for FEC alignment.
- **Client loss telemetry:** `SS_FRAME_FEC_STATUS` (ptype 0x5502 on ENet, when `ML_FF_FEC_STATUS` is set) reports per-frame received/missing data and parity counts, so a host can adapt FEC.
- **Pacing:** Sunshine batches sends, paced to about 80% of 1 Gbps per ms by default. GSO is used where available.

### 4.6 Audio

- **Opus** in RTP payload type **97**, with **RS FEC payload type 127: 4 data + 2 parity shards** (`RTPA_DATA_SHARDS 4`, `RTPA_FEC_SHARDS 2`). Packet duration 5 ms (default) or 10/20 ms.
- Stereo, 5.1 or 7.1 via **Opus multistream** (surround params in SDP).
- Encrypted with **AES-128-CBC**, IV = `BE32(rikeyid + rtp.seq)` padded to 16 bytes. Audio encryption is mandatory in classic GFE mode and optional in Sunshine (`SS_ENC_AUDIO`).

### 4.7 Control stream (ENet over UDP 47999)

- **Reliable ENet** channel with optional **encrypted framing**: `{type=0x0001 LE, length, seq}` followed by AES-GCM (IV derived from seq; 16-byte tag) wrapping `{type, payloadLength, payload}` (`NVCTL_ENCRYPTED_PACKET_HEADER`).
- Packet types (Gen7 encrypted table in moonlight-common-c, plus host tables):

| Type | Direction | Meaning |
|---|---|---|
| 0x0305 / 0x0307 | C→H | Start A / Start B |
| 0x0302 | C→H | Request IDR |
| 0x0301 | C→H | Invalidate reference frames (RFI: first/last frame index) |
| 0x0201 | C→H | Loss stats (periodic) |
| 0x0204 | C→H | Frame stats (unused) |
| 0x0206 | C→H | **Input data** (wraps input packets) |
| 0x0200 | C→H | Periodic ping (Sunshine) |
| 0x010b | H→C | Rumble |
| 0x0109 / 0x0100 | H→C | Termination (extended / legacy) with reason code |
| 0x010e | H→C | **HDR mode + static metadata** |
| 0x5500 | H→C | Trigger rumble (Sunshine ext) |
| 0x5501 | H→C | Set motion-sensor event rate (gyro/accel) |
| 0x5502 | H→C (and C→H FEC status) | Set RGB LED / `SS_FRAME_FEC_STATUS` |
| 0x5503 | H→C | DualSense adaptive triggers (opaque effect payloads) |
| 0x5504 | H→C | Player indicator LEDs (Sunshine, 2026; not yet in moonlight-common-c) |
| 0x0350 | C→H | LTR frame ACK |
| 0x3000 / 0x3001 / 0x3002 | C→H | Apollo: exec server cmd / set clipboard / file-transfer nonce |

### 4.8 Input packets (`Input.h`)

The header is `{BE32 size, LE32 magic}`. Magics:

- **Keyboard:** down 0x03 / up 0x04 (`flags`, `keyCode` = Windows VK, modifiers). `UTF8_TEXT` 0x17 carries up to 32 bytes.
- **Mouse:**
  - rel move 0x06/0x07, `short dx, dy`;
  - **abs move 0x05**, `x, y, refWidth, refHeight`, so the host scales;
  - buttons 0x08/0x09 (5 buttons);
  - scroll 0x09/0x0A (high-res 120 units);
  - `SS_HSCROLL` 0x55000001.
- **Gamepad:** `MULTI_CONTROLLER` 0x0C/0x0D carries controllerNumber, `activeGamepadMask`, `buttonFlags` + **`buttonFlags2`** (Sunshine extra buttons), 8-bit triggers and 16-bit sticks.
- **Sunshine extensions:**
  - `SS_TOUCH` 0x55000002 (pointerId, normalized float x/y, pressure, contact area, rotation; hover/down/up/move/cancel);
  - `SS_PEN` 0x55000003 (tool, buttons, tilt, rotation);
  - **`SS_CONTROLLER_ARRIVAL` 0x55000004** (type Xbox/PS/Nintendo/**Steam**, capability bits: analog triggers, rumble, trigger rumble, touchpad, accel, gyro, battery, RGB LED, **dual touchpad**; supported button mask);
  - `SS_CONTROLLER_TOUCH` 0x55000005 (with `touchpadIndex`);
  - `SS_CONTROLLER_MOTION` 0x55000006 (accel/gyro floats);
  - `SS_CONTROLLER_BATTERY` 0x55000007.
- `ENABLE_HAPTICS` 0x0D.
- Floats are little-endian ("netfloat").

The browser Gamepad API exposes buttons/axes but **not gyro, touchpad or adaptive triggers**. WebHID could fill gaps on Chromium **(unverified per device)**.

### 4.9 HDR

- The client requests HDR at launch (`hdrMode=1`) and in SDP (`dynamicRangeMode=1`, 10-bit codec).
- The host sends **0x010e HDR mode** packets with `SS_HDR_METADATA`: display primaries, white point, max/min luminance, MaxCLL, MaxFALL. It re-sends them when host HDR state toggles.
- The colorspace is chosen via `encoderCscMode` (Rec.601/709/2020 × limited/full).
- moonlight-web-stream maps this metadata into a WebRTC color-space RTP extension.

### 4.10 RFI, IDR, loss stats, bitrate

- **RFI:** on frame loss the client sends `INVALIDATE_REF_FRAMES(first,last)`. A capable encoder (NVENC; some AMF/VAAPI) re-references an older good frame instead of sending a costly IDR. The client reports decoder RFI capability per codec (`CAPABILITY_REFERENCE_FRAME_INVALIDATION_AVC/HEVC/AV1`). moonlight-common-c adds **speculative RFI** when it predicts unrecoverable loss early.
- **IDR request (0x0302)** is the fallback, and is what a WebRTC PLI maps to.
- **Bitrate** is fixed at ANNOUNCE (client-configured, with the host subtracting FEC/audio overhead). **There is no in-protocol ABR.** Forks add a runtime `GET /bitrate` (Foundation/Vibepollo). Loss stats (0x0201) and FEC status give a host the data it would need.
- **Connection quality:** the client computes interval loss% (poor ≥15–30%) and calls `ConnListenerConnectionStatusUpdate`.

### 4.11 Resolution, virtual display, sessions

- **The stream mode is fixed for the session.** A client resize means `/cancel` + `/launch`, or `/resume` with a new mode. Whether every host reconfigures capture on resume is **(unverified)**.
- Host-side virtual displays at the client's mode are fork features:
  - Apollo: SudoVDA, per-client identity;
  - Vibepollo: own IDD + Linux DRM module;
  - Foundation: ZakoVDD;
  - Moonshine and Punktfunk: per-session compositor or IDD;
  - Wolf: per-container compositor.
- Punktfunk/1 adds **mid-stream mode renegotiation**, which GameStream lacks.
- **Concurrency:** stock Sunshine allows one stream; Apollo uses multi-instance; Vibepollo allows ≤4 Remote Monitor clients plus a game owner; Wolf, Moonshine and Punktfunk do multi-session.

### 4.12 Extension dialect matrix

| Feature | Sunshine | Apollo | Vibepollo/Vibeshine | Foundation | Wolf | moonlight-web-stream WebRTC |
|---|---|---|---|---|---|---|
| 4:4:4, AV1, HDR | ✅ | ✅ | ✅ | ✅ (+HDR Vivid) | ✅/partial (unverified) | passthrough |
| Virtual display at client mode | ❌ (3rd-party) | ✅ Win | ✅ Win + Linux beta | ✅ Win | ✅ (headless compositor) | n/a |
| Client permissions | ❌ | ✅ bitmask | ✅ (Apollo) + scoped API tokens | (unverified) | profiles | roles/users |
| OTP pairing | ❌ | ✅ | ✅ | (unverified) | API pairing | n/a |
| Clipboard | ❌ | ✅ text | ✅ text | (unverified) | ❌ (unverified) | ❌ |
| Server commands | ❌ | ✅ 0x3000 | ✅ | (unverified) | ❌ | ❌ |
| Mic uplink | ❌ | partial (`microphone/apollo` stub in moonlight-common-rust) | (unverified) | ✅ | ❌ (unverified) | ✅ (WebRTC sendonly track) |
| Runtime bitrate | ❌ | ❌ | ✅ `/bitrate` | ✅ | ❌ | ❌ (requested #169) |
| PyroWave | ❌ | ❌ | ✅ (bitStreamFormat 3) | ❌ | ❌ | ❌ |
| VRR pacing | ❌ | ❌ | ✅ (`clientVrrRequested`) | ❌ | ❌ | ❌ |
| Built-in browser streaming | ❌ | ❌ | ✅ WebRTC (Windows build) | ❌ | ❌ (unverified) | ✅ (gateway) |

### 4.13 Protocol limitations relevant to Cha Portal

1. **WAN:** no NAT traversal (only UPnP and a simple STUN for external-IP discovery). Fixed UDP ports, with Sunshine encryption off on LAN and "opportunistic" on WAN. Punktfunk calls GameStream "trusted-LAN only". → For **WAN = first-class**, the browser gateway with ICE/TURN, or Cha's own QUIC-based native protocol, must carry WAN traffic. GameStream-to-Node over WAN needs WireGuard or Tailscale.
2. **FEC ceiling ≈1 Gbps** (GF(2⁸), 4 blocks). This matters for PyroWave at 4K (hundreds of Mbps to over 1 Gbps).
3. **Cursor is baked into the video**, so pointer latency equals video latency. Remote-desktop products usually send cursor shape and position separately; Artemis "local cursor" is a client hack. **Gap for desktop use.**
4. **Mode fixed per session**: no seamless window-resize.
5. **No standardized clipboard, file transfer, mic, multi-monitor or permissions**. Three incompatible dialects exist (Apollo, Foundation, Vibepollo).
6. **Weak pairing** (4-digit PIN, AES-ECB) and per-host trust: each Node is its own "PC" in Moonlight.

---

## 5. Practical answers for Cha Portal

### 5.1 (a) Cha Portal as a Moonlight *client* from the browser (external Sunshine/Vibepollo/Wolf hosts)

**Options:**

| | Design | Pros | Cons |
|---|---|---|---|
| **A. Gateway → WebRTC passthrough** (moonlight-web-stream model) | Gateway speaks GameStream to the host, reassembles frames (RS FEC), re-packetizes H.264/HEVC/AV1 into WebRTC RTP; Opus passthrough; control over DataChannel; PLI→IDR | Works in every browser; **ICE/STUN/TURN = WAN**; browser HW decode; NACK + FlexFEC/RED on WAN leg; existing GPL Rust code | Browser jitter buffer (mitigate: playout-delay 0, `jitterBufferTarget=0`); **HEVC WebRTC only Chrome ≥136 / Safari, HW-only; Firefox none**; 4:4:4 generally not available in WebRTC; HDR in WebRTC limited; gateway sees plaintext |
| **B. Gateway → WebTransport (or WSS) + WebCodecs** | Gateway sends whole access units (one QUIC stream per frame or datagrams); browser `VideoDecoder({optimizeForLatency:true})` → WebGPU canvas; AudioWorklet | No jitter buffer; full control over pacing/dropping; HEVC/AV1/(4:4:4 where decoder allows); can render HDR via WebGPU (browser-dependent) | Must build own loss handling/congestion control; WebTransport needs HTTP/3 + certs (`serverCertificateHashes` for self-signed homelab, max 14-day cert); Safari WebTransport **(unverified)** |
| **C. Moonlight-in-WASM + dumb relay** (novel) | Run **moonlight-common-rust (Sans-IO, WASM)** in the browser; gateway only relays host UDP ↔ WebTransport datagrams and TCP ↔ streams | End-to-end Moonlight semantics (RS FEC, RFI, encryption → gateway can't see content once video decryption lands); gateway nearly stateless | Moonlight packets (1392+16+RTP) exceed typical QUIC datagram payload (~1200 B) → negotiate `packetSize` ≈1024–1100 (supported via ANNOUNCE); WASM RS/FEC CPU cost at high bitrate; experimental |
| **D. Host-native WebRTC** (Vibepollo `/api/webrtc`) | Browser talks to the host directly | No gateway | Vibepollo-on-Windows only; no TURN; not portable |

**Recommendation:**

- **A as the default** and **B as the "edge/LAN-max" transport**, both behind one transport abstraction (moonlight-web-stream already has `Transport = webrtc | websocket`).
- **Colocate the gateway with the host.** Run it as a container on a Cha Node in the host's LAN, or as a sidecar on the host itself. The GameStream leg then stays on a lossless LAN/loopback and the WAN leg uses ICE/TURN.
- Use **moonlight-common-rust** (multi-session, async, GPL-3-or-later) rather than moonlight-common-c (one process per stream).
- **No transcoding**: pass NALUs/OBUs through. Transcode only when the browser lacks the codec, e.g. HEVC on Firefox. Better: negotiate H.264/AV1 from the host using the browser's `RTCRtpReceiver.getCapabilities()` / `VideoDecoder.isConfigSupported()` intersected with `ServerCodecModeSupport`.
- Prototype **C** for a later "WASM Moonlight" mode. The same Rust core could power the thin native client.

### 5.2 (b) Should Cha Portal Nodes *also* speak the Moonlight protocol?

**Yes, as a compatibility front door, not as Cha's only native protocol.**

Why it is worth having:

- Free, mature clients on Android, iOS/tvOS, Apple TV, Android TV, Xbox, Switch, webOS/Tizen, Steam Link and Raspberry Pi.
- Artemis and Moonlight V+ power users.
- Nonary's moonlight-qt fork as a **ready-made native PyroWave client**.

How to do it:

- **Engine:** reuse Wolf (MIT, Docker-native; per the other slice), **Moonshine** (BSD-2, Rust, per-session compositor) or **Punktfunk**'s GameStream host (MIT/Apache). Do not fork Sunshine/Vibepollo for Nodes: they are desktop-session oriented, Windows-first and not container-native.
- **App mapping:** map **Cha environments → `/applist` entries** (Wolf does this). Map **session controls → synthetic apps**, following the Vibepollo trick: "Stop env", "Snapshot", "Persist", "Attach as second monitor", "Input only".
- **Pairing:** pair from the Cha dashboard. Show the client PIN entry in Cha's UI, or implement Apollo-style **OTP** so a QR code or link pairs a device. Make one Cha user identity map to many client certs.
- **PyroWave:** implement **Vibepollo's PyroWave wire contract** exactly: the bits, RTSP attributes, record framing and critical-band FEC. Pin the PyroWave commit and advertise `x-ss-pyrowave.bitstream`. Coordinate with Nonary on registering the SCM bits.
- **Extensions:** pick dialects deliberately:
  - Apollo: permissions, clipboard, server-cmd;
  - Foundation: mic, `/bitrate`;
  - Sunshine: player LEDs, intraRefresh.
- **Network scope:** **gate GameStream to LAN/VPN by default** (e.g. bind to a WireGuard/Tailscale interface) and require encryption (`SS_ENC_VIDEO|AUDIO|CONTROL_V2`, `corever=1`).
- **Cha's own native protocol:** for the thin native client and WAN, use a QUIC-based design. Ideas to borrow: Punktfunk/1 (GF(2¹⁶) FEC, QUIC control, mid-stream renegotiation, SPAKE2 pairing) and Vibepollo's PyroWave partial-frame recovery.

### 5.3 Licensing (Cha = AGPL/GPL)

- **GPL-3.0 code** can be combined with an **AGPL-3.0** Cha Portal: both licenses' §13 explicitly permit the combination. GPL-3.0 code: moonlight-common-c, moonlight-common-rust (GPL-3.0-or-later), Sunshine, Apollo, Vibepollo, moonlight-web-stream (GPL-3.0-or-later) and linckosz/moonlight-web. Each part keeps its license, and the AGPL network-use clause applies to Cha's AGPL parts. If Cha chooses **GPL-3.0** instead, everything combines trivially.
- **Wolf (MIT), Moonshine (BSD-2), Punktfunk (MIT/Apache-2.0) and PyroWave (MIT)** are permissive and compatible either way. Keep notices.
- **App stores:** moonlight-common-c now has a §7 App Store exception (2026-09-26). If Cha ships an iOS or tvOS thin client, the **Cha code itself also needs an equivalent exception** (Cha controls its own license). Other GPL deps without the exception (e.g. moonlight-common-rust) would block App Store distribution unless their authors add it.
- **Reimplementing in Rust** to avoid GPL is unnecessary given the copyleft decision. Clean-room reimplementation only matters if Cha later wants permissive licensing. In that case, base on **Moonshine (BSD-2)** or **Punktfunk (MIT/Apache)** for host-side GameStream, and Wolf's docs for the protocol.

### 5.4 Existing non-C implementations of the protocol

| Impl | Side | Lang | License | Notes |
|---|---|---|---|---|
| [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust) | Client (pairing, HTTP, RTSP, ENet, video/audio depayload, control, Foundation mic; WebRTC SDP helpers) | Rust, Sans-IO, WASM-capable | GPL-3.0-or-later | No video decryption, RFI or AV1 parsing yet; also offers `stream-c` bindings to moonlight-common-c; forks exist (hachimi-cat, Greaper88, kayhanb) |
| [Moonshine](https://github.com/hgaiser/moonshine) | Host | Rust | BSD-2 | Linux; smithay compositor per session; Vulkan encode |
| [Punktfunk](https://git.unom.io/unom/punktfunk) | Host (+ own protocol, native clients) | Rust | MIT/Apache-2.0 | GameStream compat + punktfunk/1 (QUIC, GF(2¹⁶) FEC) |
| [Wolf](https://github.com/games-on-whales/wolf) | Host | C++ | MIT | Docker-native, multi-session, best protocol docs |
| moonlight-web-stream v2 `streamer` | Client | Rust over C | GPL-3.0 | Historical: one subprocess per stream |
| Go | — | — | — | **None found** |

---

## 6. Consolidated "Steal for Cha Portal" list

1. **Gateway design** from moonlight-web-stream v3: moonlight-common-rust + webrtc-rs; NALU/OBU passthrough into RTP; playout-delay=0; `jitterBufferTarget=0`; NACK + FlexFEC; PLI→IDR; codec intersection with browser capabilities; WebSocket + WebCodecs fallback; Opus/openh264 WASM fallbacks.
2. **WHIP/WHEP-style signaling** with `x-moonlight-*` SDP attributes. Adopt it as a subset of Cha's browser↔Node API so stock Moonlight-Web clients can talk to Cha Nodes and vice versa.
3. **The protocol core in WASM in the browser** (control packet building, later full client).
4. **PyroWave wire contract** from Vibepollo, for native-client interop. Also borrow its partial-frame recovery and "FEC only on the critical band" for Cha's own transport.
5. **Synthetic apps as remote controls** for stock Moonlight clients.
6. **Scoped API tokens** (path regex + methods, hash-only storage) and `__Host-` session cookies with refresh rotation (Vibepollo).
7. **Per-client permission bitmask and OTP pairing** (Apollo). **Session sharing with Viewer/Gamer/Desktop/Full tiers, link + PIN and expiry** (linckosz/moonlight-web).
8. **Runtime bitrate endpoint + client-driven ABR capability flag** (Foundation/Vibepollo). A **bandwidth probe** over the pinned HTTPS connection.
9. **Session-bound UDP via ping payload / connect data**, which lets a Node multiplex many sessions behind one port set.
10. **Connection tester / port-flag diagnostics** in the dashboard.
11. **Stable virtual-display identity per client** and a **fixed high-refresh virtual display for VRR capture**.
12. **FEC block math, send pacing and GSO** (Sunshine, Moonshine `gso_socket.rs`).

---

## 7. Risks / gotchas

- **Vibepollo churn:** about 99% AI-generated, a single maintainer, daily releases, and open crash/encoding-failure issues. Its Linux build is Arch-only with a DKMS kernel module. **Integrate via the protocol and REST API only.** Pin tested versions and expect breaking UI/API changes. Its PyroWave SCM bits are unregistered upstream.
- **Fork dialect fragmentation** (Apollo vs Foundation vs Vibepollo) for clipboard, mic, bitrate and permissions. Apollo's own maintainer flags this. Feature-detect via `serverinfo` fields and HTTP 404s, and never assume.
- **moonlight-common-c global state** means one stream per process, so a multi-tenant gateway must use subprocesses or moonlight-common-rust.
- **moonlight-common-rust gaps:** no video decryption, so you **must not request `SS_ENC_VIDEO`** (fine on LAN, weak on WAN). No RFI, so every loss means an IDR, which costs bandwidth spikes. Pre-1.0 API churn. New rustls certs are incompatible with v2 OpenSSL-generated ones.
- **Browser codec reality:** HEVC over WebRTC needs Chrome ≥136 or Safari with hardware decode (none on Firefox). Edge has open HEVC-cropping bugs (#168). There is generally no 4:4:4 over WebRTC. HDR is inconsistent. Gamepad API lacks gyro, touchpads and adaptive triggers. Keyboard Lock and Pointer Lock need a secure context and fullscreen.
- **PyroWave in the browser** is not decodable via WebRTC or WebCodecs. It needs a WebGPU port of the decoder plus WebTransport (or DataChannel) at **hundreds of Mbps**. That is LAN-only and outside the GameStream→WebRTC gateway path (pyrowave slice).
- **GameStream on WAN:** no ICE, the 4-digit-PIN pairing and default-off LAN encryption make it **unsafe to expose publicly**. Keep it LAN/VPN-only.
- **The FEC wall (~1 Gbps)** collides with high-bitrate PyroWave or 4K240 goals on GameStream.
- **Single-session hosts** (stock Sunshine): a Cha user launching on an external Sunshine host can kick another user's session. Show `state=BUSY` / `currentgame` in the dashboard.
- **App-store distribution** of any Cha native client needs Cha's own §7 exception, and every GPL dependency must carry one too.

---

## 8. Open questions

1. Gateway placement for *external* hosts with no Cha Node on their LAN. Is a "host sidecar" container or binary acceptable, or must the WAN leg be raw GameStream (needs a VPN)?
2. Do we standardize on moonlight-common-rust and **contribute** video decryption, RFI and AV1 parsing upstream? Or keep a moonlight-common-c subprocess fallback for old hosts?
3. Is WebTransport + WebCodecs (option B) worth building in v1, or is WebRTC with playout-delay 0 "Moonlight-class" enough? This needs a glass-to-glass benchmark against native moonlight-qt on the same host.
4. Which GameStream engine for Nodes: Wolf vs Moonshine vs Punktfunk? This depends on the container/Steam slice findings.
5. Should Cha propose a **registered extension namespace** (e.g. `x-cha-*` SDP attrs, SCM bits) with LizardByte, Nonary and Foundation, to avoid adding a fourth incompatible dialect?
6. Does `/resume` with a different `mode` reliably reconfigure capture on Sunshine, Vibepollo and Wolf? Needed for browser window-resize UX. **(unverified)**
7. Cursor: does Cha's own protocol send cursor shape and position separately for desktop environments, which GameStream cannot do?
8. Availability of **Direct Sockets in Isolated Web Apps** on desktop Chrome in 2026. That could make a no-relay, browser-installed Moonlight client possible. **(unverified)**

---

## Sources (primary unless noted)

- Repos and metadata (GitHub REST API, 2026-10-03): [Sunshine](https://github.com/LizardByte/Sunshine), [Apollo](https://github.com/ClassicOldSong/Apollo), [Vibepollo](https://github.com/Nonary/Vibepollo), [Vibeshine](https://github.com/Nonary/vibeshine), [Foundation Sunshine](https://github.com/AlkaidLab/foundation-sunshine), [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c), [moonlight-qt](https://github.com/moonlight-stream/moonlight-qt), [moonlight-android](https://github.com/moonlight-stream/moonlight-android), [moonlight-ios](https://github.com/moonlight-stream/moonlight-ios), [moonlight-embedded](https://github.com/moonlight-stream/moonlight-embedded), [moonlight-chrome](https://github.com/moonlight-stream/moonlight-chrome), [Artemis](https://github.com/ClassicOldSong/moonlight-android), [Moonlight V+](https://github.com/qiin2333/moonlight-vplus), [Nonary/moonlight-qt](https://github.com/Nonary/moonlight-qt), [moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream), [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust), [linckosz/moonlight-web](https://github.com/linckosz/moonlight-web), [Moonshine](https://github.com/hgaiser/moonshine), [Punktfunk mirror](https://github.com/rongrong666/punktfunk), [Wolf](https://github.com/games-on-whales/wolf), [PyroWave](https://github.com/Themaister/pyrowave).
- Source files read from shallow clones: Vibepollo `README.md`, `docs/pyrowave-protocol.md`, `docs/api.md`, `architecture.md`, `release_notes/2.0.0*.md`, `src/nvhttp.cpp`, `src/confighttp.cpp`, `src/webrtc_stream.{h,cpp}`, `src/remote_session.h`, `cmake/dependencies/webrtc.cmake`, `third-party/pyrowave/VENDOR.txt`, `docs/linux/*.md`; Apollo `README.md`, `src/nvhttp.cpp`, `src/stream.cpp`, `src/crypto.h`; Sunshine `src/stream.{h,cpp}`, `src/rtsp.{h,cpp}`, `src/nvhttp.{h,cpp}`, `src/config.cpp`, `src/platform/common.h`; moonlight-common-c `LICENSE.txt`, `src/*.{c,h}`; moonlight-web-stream `README.md`, `Cargo.toml`, `src/api/stream/webrtc/*`, `web/stream/transport/webrtc.ts`; moonlight-common-rust `README.md`, `src/http/*`, `src/webrtc/protocol.md`, `src/stream/proto/*`; moonshine `README.md`, `moonshine-core/Cargo.toml`.
- Release pages: [Sunshine v2026.914.233613](https://github.com/LizardByte/Sunshine/releases/tag/v2026.914.233613), [Vibepollo releases](https://github.com/Nonary/Vibepollo/releases), [Nonary moonlight-qt vrr18](https://github.com/Nonary/moonlight-qt/releases/tag/v6.1.0-vrr18), [moonlight-android v12.2](https://github.com/moonlight-stream/moonlight-android/releases/tag/v12.2), [Moonlight PyroWave for Xbox](https://github.com/dimizago/moonlight-xbox/releases/tag/v1.18.1-pyrowave.1).
- Discussions/issues: [Apollo #1447 bounty RFC](https://github.com/ClassicOldSong/Apollo/discussions/1447), [moonlight-qt #1711](https://github.com/moonlight-stream/moonlight-qt/issues/1711), [moonlight-web-stream issues](https://github.com/MrCreativ3001/moonlight-web-stream/issues), [Vibepollo issues](https://github.com/Nonary/Vibepollo/issues).
- Docs: [Wolf protocol docs](https://games-on-whales.github.io/wolf/stable/protocols/index.html), [Punktfunk roadmap](https://docs.punktfunk.unom.io/docs/roadmap), [Phoronix: Sunshine Vulkan encode](https://www.phoronix.com/news/Sunshine-v2026.413.143228) (secondary), Chrome WebRTC HEVC ([blink-dev intent to ship](https://groups.google.com/a/chromium.org/g/blink-dev/c/3h8lL8a377c); secondary summaries claim Chrome 136).
