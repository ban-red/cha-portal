# 04 — Browser-based remote desktop / app streaming platforms (Kasm-like)

> Background research from planning ([index](README.md)). What Cha Portal actually ported is in [PROVENANCE.md](../PROVENANCE.md).

Research slice for **Cha Portal** (portal.cha.sh). Snapshot as of **2026-10-03**. Sources are inline. I verified repo facts (license, last push, latest release) against the GitHub REST API or shallow clones made on 2026-10-03. I read source for Selkies, pixelflux, pcmflux, SealSkin, Neko, neko-rooms, KasmVNC, Kasm's noVNC fork, Kasm workspaces images, xpra-html5, cloud-game and BrowserPane. Anything I could not confirm is tagged **(unverified)**.

User decisions received mid-research and applied to the verdicts: Cha Portal will be **copyleft OSS (AGPL/GPL)**, it targets **homelab / small groups first** (simple RBAC, no heavy DLP or multi-tenancy), and **LAN and WAN are equally first-class**.

---

## TL;DR

- **Selkies 2.0.0 (2026-09-23) is now the strongest open-source engine in this category.** It replaced GStreamer with two Rust/PyO3 extensions: `pixelflux` (capture and encode) and `pcmflux` (Opus). It uses a WebCodecs client over **plain WebSockets by default**, with WebRTC as an opt-in transport that can be switched mid-session.
  - Codecs: H.264, H.265, VP8, VP9 and AV1 on NVENC, VA-API, V4L2-M2M or Tegra, with software fallbacks.
  - Zero-copy capture: Wayland dmabuf, X11 NvFBC and X11 DRI3.
  - It ships a token-based "secure mode" control plane that is built for an orchestrator like Cha Portal.
  - Everything is MPL-2.0, which fits an AGPL project.
  - **Verdict: depend-on for Linux environments in v1, and consider forking `pixelflux` as the Rust capture/encode core later.**
- **Kasm Workspaces is the product reference, not a code source.** The platform (API, manager, agent) is proprietary. Community Edition is free for **≤5 concurrent sessions, non-commercial only**.
  - Its *ideas* are the gold standard to copy: zones, agent check-in, a **workspace registry JSON format**, an **image contract**, **persistent profiles** (volume or S3 with `{username}/{image_id}` templating), **session staging** (pre-warmed pools), **casting links**, and DLP.
  - Kasm images are MIT-licensed but download **closed-source sidecar binaries** (gamepad, webcam, mic, upload, printer, recorder) from S3. Do not redistribute them.
- **KasmVNC 1.5.0 (2026-07-29) is no longer "just VNC".** It added a video streaming mode (H.264, H.265 and AV1 via NVENC/VA-API/software, decoded with WebCodecs), relative mouse, and a WebRTC *data-channel* "UDP" path.
  - It is still RFB-shaped, Xvnc-based and pull-oriented. Fine for office use, but not the Moonlight-class path.
  - **Verdict: protocol-compatible (accept KasmVNC/Kasm images as a "compat" environment type).**
- **Neko v3.1.6 (2026-09-30) is the best multi-user / shared-control model and the best curated browser image set.**
  - It covers 10+ browsers, ships Chromium enterprise policies, has NVIDIA variants, and offers control request/give/take APIs and OIDC.
  - Its streaming is GStreamer `ximagesrc` → VP8 by default at 1280x720@30 over Pion WebRTC, with a fixed resolution list. It is starting to decouple transports (PR #699) to allow WebSocket/WebTransport later.
  - Apache-2.0. **Verdict: inspiration plus reuse its browser images and policies.**
- **Newcomers worth watching:**
  - **BrowserPane** (AGPL-3.0, Rust): WebTransport, crisp tiles for UI, and ROI H.264 over *datagrams*. This is exactly the hybrid we want for "Chrome instance" environments.
  - **SealSkin** (MPL-2.0): a Selkies-based Kasm-like orchestrator with an extension, mobile apps and rooms.
  - **Kernel's kernel-images**: a neko fork used for AI-agent browser live view.
- **No mature project in this category ships WebTransport + WebCodecs + modern congestion control as the default path.**
  - Selkies and Neko both have it as an open issue or epic (Selkies #48 since 2022; Neko #690).
  - xpra-html5 has an optional WebTransport client. BrowserPane uses WebTransport by default, but it is experimental and Chromium-only.
  - This is Cha Portal's opening, and a natural home for a Pyrowave decoder (WASM/WebGPU) next to WebCodecs.

---

## 0. Landscape at a glance

| Project | Kind | License | Default transport | Video codecs | Latest (date) | Activity | Verdict for Cha Portal |
|---|---|---|---|---|---|---|---|
| [Kasm Workspaces](https://kasm.com) | Full VDI/CBI platform | **Proprietary** (CE free ≤5 sessions, non-commercial) | KasmVNC over WSS; RDP via guacd | via KasmVNC | 1.19.0 (2026-06-15¹) | active | inspiration-only (platform), protocol-compatible (images) |
| [KasmVNC](https://github.com/kasmtech/KasmVNC) | Web-native VNC server + noVNC fork | GPL-2.0-or-later (client MPL-2.0) | WebSocket; optional WebRTC DataChannel "UDP" | Tight JPEG/WebP/QOI rects; **H.264/H.265/AV1 video mode** | v1.5.0 (2026-07-29) | active (push 2026-10-01) | protocol-compatible |
| [Selkies](https://github.com/selkies-project/selkies) (ex-selkies-gstreamer) | Streaming engine + web client | MPL-2.0 | **WebSocket + WebCodecs**; WebRTC opt-in | H.264/H.265/VP8/VP9/AV1, striped H.264, MJPEG | **2.0.0 (2026-09-23)** | very active (push 2026-10-03) | **depend-on / fork pixelflux** |
| [pixelflux](https://github.com/selkies-project/pixelflux) / [pcmflux](https://github.com/linuxserver/pcmflux) | Rust capture+encode / Opus | MPL-2.0 (default build links GPL x264/x265) | n/a | see above | 2.1.0 | very active | fork/vendor candidate |
| [LSIO baseimage-selkies](https://github.com/linuxserver/docker-baseimage-selkies) / [webtop](https://github.com/linuxserver/docker-webtop) | Desktop/app images | GPL-3.0 | Selkies | Selkies | rolling (2026-10-02/03) | very active | depend-on (images) |
| [SealSkin](https://github.com/selkies-project/sealskin) | Orchestrator on Selkies | MPL-2.0 | Selkies | Selkies | 0.3.2 (rolling pre-releases 2026-10-01) | active | inspiration (closest competitor) |
| [Neko](https://github.com/m1k1o/neko) | Shared virtual browser/desktop | Apache-2.0 | WebRTC (Pion) | VP8 (default), VP9, AV1, H.264, H.265 | v3.1.6 (2026-09-30) | active | inspiration + reuse images/policies |
| [neko-rooms](https://github.com/m1k1o/neko-rooms) | Room orchestrator for neko | Apache-2.0 | n/a | n/a | v1.6.5 (2026-03-29) | slow | inspiration-only |
| [Apache Guacamole](https://guacamole.apache.org) | Clientless RDP/VNC/SSH gateway | Apache-2.0 | WebSocket/HTTP tunnel, Guacamole protocol | PNG/JPEG/WebP draw ops | 1.6.0 (2025-06-22) | slow | protocol-compatible (RDP/VNC bridge) |
| [Xpra](https://github.com/Xpra-org/xpra) + [xpra-html5](https://github.com/Xpra-org/xpra-html5) | Seamless-window remoting | GPL-2.0+ / MPL-2.0 | WS(S); QUIC/**WebTransport** | H.264 (+jpeg/webp/png in HTML5) | 6.5.4 (2026-09-27) / html5 v21 tag (2026-05-11) | active | inspiration (seamless windows, WebTransport client) |
| [noVNC](https://github.com/novnc/noVNC) | VNC web client | MPL-2.0 (mixed; GitHub: NOASSERTION) | WebSocket (websockify) | RFB encodings incl. H.264 | v1.7.0 (2026-04-28) | slow | protocol-compatible |
| [Hyperbeam](https://hyperbeam.com) | Commercial multiplayer virtual browser API | Proprietary SaaS | WebRTC | n/a (unverified) | n/a | active | inspiration-only (API/UX) |
| [cloud-game](https://github.com/giongto35/cloud-game) | Retro cloud gaming | Apache-2.0 | WebRTC (Pion) | H.264, VP8 | tags to v3.1.1 (last GitHub "release" object 2021) | maintenance (push 2026-09-05) | inspiration (coordinator/worker, latency-probe selection) |
| [RustDesk](https://github.com/rustdesk/rustdesk) | TeamViewer-like | AGPL-3.0 | own; WS for web client | VP8/VP9/AV1/H.264/H.265 (native) | 1.5.0 (2026-09-30) | very active | out of scope (web client tied to Server Pro) |
| [BrowserPane](https://github.com/ITmedes/browserpane) | Remote browser for humans+agents | **AGPL-3.0** | **WebTransport** | tiles + ROI H.264 | experimental (commit 2026-08-23) | active | **inspiration / possible code reuse (same license family)** |

¹ The date comes from a search-engine snippet of Kasm's release notes. The [1.19.0 page](https://docs.kasm.com/docs/reference/release-notes/1.19.0) itself does not state a date.

---

## 1. Kasm Workspaces (platform)

**What:** A "Containerized Streaming Platform" (VDI plus remote browser isolation). It delivers Docker-container desktops and apps, VMs over RDP/VNC/SSH, and Kubernetes pods, all to the browser.
**Status:** Active. 1.18.1 shipped in Nov 2025 and **1.19.0** in June 2026, plus rolling image fixes through Aug 2026 ([release notes](https://docs.kasm.com/docs/reference/release-notes/1.18.1)).
**License:** The platform is **closed source**. KasmVNC is GPL. The image repos are MIT, with a disclaimer that the license does not cover external dependencies.

### 1.1 Architecture

From the [architecture roles](https://kasm.com/docs/latest/install/multi_server_install/multi_architecture_roles.html) and [multi-server docs](https://www.kasmweb.com/docs/latest/install/multi_server_install.html):

| Role | Services | Ports | Purpose |
|---|---|---|---|
| Web App | `kasm_api` (web app/API), `kasm_manager` (manager), `kasm_proxy` (nginx) | 443 | UI, REST API, scheduling and session lifecycle |
| Database | `kasm_db` (PostgreSQL), `kasm_redis` ("share database") | 5432, 6379 | State and shared/cached session state |
| Agent | `kasm_agent`, `kasm_proxy` | 443 | **Where sessions (containers) are created**; the Docker host |
| Connection Proxy | `kasm_guac` (custom guacd build plus a Kasm connection manager), `kasm_rdp_gateway` | 443, 3389 | Converts RDP/VNC/SSH into HTML5; RDP gateway for thick clients |
| Dedicated Proxy | `kasm_proxy` | 443 | Edge proxy |

**Zones** ([docs](https://kasm.com/docs/latest/guide/zones/deployment_zones.html)):
- A zone is a logical grouping of services, typically by region or network enclave. "Agents are assigned the Zone of whichever manager they are currently checked in to."
- Zone settings include: upstream auth address, allow-origin domain, proxy hostname/port (direct to agent or via proxy), **load-balancing strategy** (Least Load / Most Load / Least Kasms / Most Kasms), **search alternate zones**, and **prioritize static agents** (use them before autoscaled ones).
- Autoscaling exists (cloud VM providers). Kubernetes deployment went **GA in 1.19**.

**1.19 additions** ([notes](https://docs.kasm.com/docs/reference/release-notes/1.19.0)):
- H.264/H.265/AV1 in KasmVNC for container workspaces.
- Guacamole 1.6.0 with FreeRDP 3.
- Intel/AMD GPU passthrough.
- **NVIDIA MIG slice provisioning** (partitioning without vGPU licences).
- Linux VM delivery over RDP.
- Server expiration for pool rotation.
- Native RDP clients authenticating with Kasm credentials.
- "Storage Mapping Based Profiles", a system metrics dashboard, config export/import, and support bundles.
- AI dev images (Claude Code, Gemini CLI, Codex CLI, Cursor).
- Breaking change: byte-based API fields.

### 1.2 Workspace images and registry format

- **Registry** ([template repo](https://github.com/kasmtech/workspaces_registry_template)):
  - Layout: `workspaces/<Name>/workspace.json` plus an icon PNG.
  - CI builds a `list.json` and a Next.js site deployed to GitHub Pages. Kasm installs import a registry by URL.
  - Schema v1.1 fields:
    - Required: `friendly_name`, `description`, `image_src`, `architecture` (`amd64`/`arm64`), and `compatibility[]`. Each `compatibility` entry holds `version` (e.g. `"1.16.x"`), `image`, `uncompressed_size_mb` and optional `available_tags`.
    - Optional: `categories` (≤3), `docker_registry`, `run_config` (docker run overrides), `exec_config` (hooks), `notes`, `cores`, `memory`, `gpu_count`, `cpu_allocation_method`.
  - **Directly reusable idea**: a git-backed, static JSON catalog that third parties can host.
- **Image contract** (read from [workspaces-core-images](https://github.com/kasmtech/workspaces-core-images) `dockerfile-kasm-core`):
  - Runs as `USER 1000` (`kasm-user`).
  - Build-time home is `/home/kasm-default-profile`, copied into `/home/kasm-user` at start (`kasm_default_profile.sh`). That copy is what makes persistent-profile overlays and staging work.
  - `ENTRYPOINT ["/dockerstartup/kasm_default_profile.sh","/dockerstartup/vnc_startup.sh","/dockerstartup/kasm_startup.sh"]`.
  - Env: `VNC_PORT=5901`, `NO_VNC_PORT=6901` (HTTPS/WSS KasmVNC), `AUDIO_PORT=4901`, `UPLOAD_PORT`, `VNC_PW`, `VNC_RESOLUTION`, `MAX_FRAME_RATE=24`, `VNCOPTIONS="-PreferBandwidth -DynamicQualityMin=4 -DynamicQualityMax=7 ..."`, and an `SDL_GAMECONTROLLERCONFIG` mapping for the virtual XInput pad.
  - App images ([workspaces-images](https://github.com/kasmtech/workspaces-images); 79 Dockerfiles: browsers, desktops for ~15 distros, Steam, RetroArch, Redroid, DinD, VS Code, etc.):
    - Each app image `FROM kasmweb/core-*`, installs the app, and drops in a **`custom_startup.sh`**. That script supervises the app in a restart loop, gated on `/usr/bin/desktop_ready` and `filter_ready`.
    - It accepts `--go`, `--assign` and `--url` so that the platform can **exec into an already-running (staged) container to open a URL at assignment time**.
    - Env: `LAUNCH_URL` / `KASM_URL`.
  - Hardening helpers: `single_app_security.sh`, a restricted GTK file chooser (`KASM_RESTRICTED_FILE_CHOOSER=1`), removal of the Thunar breakout, and Chrome managed policies (`urlblocklist.json`).
  - **Gotcha:** the core image downloads **prebuilt binaries** from `kasmweb-build-artifacts.s3.amazonaws.com`: `kasm_audio_input_server`, `kasm_gamepad_server`, `kasm_printer_service`, `kasm_recorder_service`, `kasm_smartcard_bridge`, `kasm_squid_adapter`, `kasm_upload_service`, `kasm_webcam_server` and `kasm_websocket_relay`. Their source is not in public repos, so treat them as proprietary **(unverified licence terms)**. Cha Portal must not depend on or redistribute them.

### 1.3 Ephemeral vs persistent

- **Default: ephemeral.** The container is destroyed on logout or expiry.
- **Persistent profiles** ([docs](https://kasm.com/docs/latest/guide/persistent_data/persistent_profiles.html)):
  - Two backends: **volume mounts** (NFS/SMB/SSHFS/etc.) or **S3**. With S3 the container gets **presigned URLs, not credentials**.
  - Path templating: `{username}`, `{user_id}`, `{image_id}`, e.g. `/mnt/kasm_profiles/{image_id}/{user_id}`.
  - Quota: `KASM_PROFILE_SIZE_LIMIT` (KB). S3 refuses to save over the limit; volume profiles only warn.
  - Users can choose **"Reset"** at launch.
  - The docs stress that each workspace needs a unique profile path, because app configs collide across images.
- **Volume mappings and file mappings** (per image, group or user) inject shared folders and config files. 1.19 lets admins disable file mappings without deleting them.
- **"Storage Mapping Based Profiles"** (1.19) cut disk usage and add a recovery mode.

### 1.4 Session staging (pre-warmed pools)

From the [docs](https://kasm.com/docs/latest/guide/staging.html):
- Config per (zone, workspace): desired session count, expiration in hours, and permission toggles (audio, clipboard each way, upload, download, mic, gamepad, webcam).
- Staged sessions run with no user. At request time the assignment order is:
  1. staged in the current zone,
  2. staged in another zone,
  3. on-demand in the current zone,
  4. on-demand in another zone.
- **Incompatibilities force on-demand creation:** persistent profiles, username-dependent volume or file mappings, language/timezone mismatches, SSH key injection, and web-filter policies.
- **Lesson:** keep anything user-specific *out of container creation* and inject it at assignment (exec hooks, mounts that can be bound late, or env through a side channel). Otherwise pools are useless for persistent users.

### 1.5 Casting, sharing and other UX

- **Session Casting** ([docs](https://kasm.com/docs/latest/guide/casting.html)):
  - A URL key (`/#/cast/<key>`) launches a session without login.
  - Options: optional anonymous auto-created users in a group, `kasm_url` passthrough, `docker_network`, per-IP rate limit, reCAPTCHA, referrer restriction, session caps, and "Valid Until".
  - Great for homelab "send a friend a link to a throwaway browser".
- **Session sharing:** read-only or collaborative share links **(details unverified for 2026)**.

### 1.6 DLP and security

DLP is mostly in KasmVNC config ([README](https://github.com/kasmtech/KasmVNC)):
- Clipboard: per direction enable, size limits, allowed MIME types, delay between operations, and primary-selection control.
- Keyboard: enable flag and rate limit.
- **Visible-region** masking.
- Keystroke and clipboard logging.
- Brute-force IP blacklist.
- Watermarking: image or text, with timestamp.

Platform-level controls: upload/download toggles, web-category filtering through a Squid adapter (Starter and Enterprise), and **session recording (Enterprise only)**.

### 1.7 GPU

- NVIDIA through the container toolkit for app rendering, DRI3 GPU acceleration in KasmVNC (1.1+), and VirtualGL install scripts in core images.
- 1.19 adds Intel/AMD passthrough and NVIDIA MIG.
- KasmVNC 1.5 adds hardware **encode** (NVENC/VA-API).

### 1.8 Licensing

From the [licence page](https://docs.kasm.com/docs/reference/license) and [features matrix](https://kasm.com/resources/features-matrix):

| Edition | Price | Limits and notes |
|---|---|---|
| **Community** | Free | **5 concurrent sessions**; "testing, nonprofit, and eligible non-commercial use" |
| **Starter** | $10/user or $20/session | Commercial use |
| **Enterprise** | Quote | Commercial use |

Feature availability by edition:

| Feature | Community | Starter | Enterprise |
|---|---|---|---|
| Session staging, session casting, autoscaling, developer API, log forwarding | yes | no | yes |
| Web category filtering | no | yes | yes |
| Session recording, custom branding | no | no | yes |

### 1.9 Pain points

- Closed control plane.
- The 5-session cap blocks even small groups that want to run "always-on" sessions.
- Sidecar binaries are opaque.
- Heavy footprint (Postgres + Redis + several services).
- KasmVNC defaults (24 fps) target bandwidth, not latency.

### 1.10 Steal for Cha Portal

1. **Zone → node → session** model, with node check-in to the control plane and a **pluggable placement strategy** (least-load / least-sessions / pinned).
2. **Workspace registry**: a git-hostable static `list.json`, with an image entry carrying `compatibility[]`, arch, resources and `run_config`/`exec_config`. Third-party catalogs work by URL.
3. **Image contract:**
   - Build-time default profile copied into the runtime home.
   - App supervised by a startup script.
   - **Exec hooks** (`--go`/`--assign`/`--url`) so a pre-warmed container can be personalized at assignment.
4. **Staging pools** per (node, environment), with explicit "pool-compatible" rules.
5. **Persistent profiles** as volume-per-`{user}/{env}`, with an S3 option using presigned URLs, a size limit and a user-visible "reset profile" button.
6. **Casting links** for throwaway sessions (rate-limit and expiry), which matches the homelab "share a browser" use case.
7. DLP as **declarative per-environment policy** (clipboard direction/size/MIME, upload/download, watermark). Keep it light: homelab scope.

**Verdict:** inspiration-only for the platform. Protocol-compatible for images: Cha Portal could launch an unmodified `kasmweb/*` image as a "KasmVNC" environment and proxy `:6901`. Kasm's proprietary sidecars inside those images would still be present, so leave it to the user to pull them.

---

## 2. KasmVNC

**What:** A TigerVNC-derived Xvnc server that "has broken from the RFB specification". It ships its own web server and a forked noVNC client ([kasmtech/noVNC](https://github.com/kasmtech/noVNC), MPL-2.0) and is configured in YAML at server and user levels.
**Status:** v1.5.0 on 2026-07-29 and v1.4.0 on 2025-10-22 ([releases](https://github.com/kasmtech/KasmVNC/releases)). Push on 2026-10-01; ~5.3k stars.
**License:** GPL-2.0-**or-later** (file headers checked), so it is compatible with an AGPL-3.0 combination.

### 2.1 Encoding and transport

- **Rect mode** (classic): Tight with **JPEG / WebP / QOI** (QOI is lossless and meant for LAN).
  - Quality settings: `min_quality`/`max_quality`/`consider_lossless_quality`.
  - Multi-threaded rectangle compression (`rectangle_compress_threads: auto`).
  - **Dynamic quality** based on the screen change rate, with automatic WebP/JPEG mixing based on CPU budget (`webp_encoding_time`).
- **"Video encoding mode"**: when more than `area_threshold: 45%` of the screen changes for `time_threshold: 5` s, the server drops quality and caps the resolution (`max_resolution` 1920x1080) until changes stop for 3 s.
- **"Video streaming mode"** (1.5.0, new): `codec: auto`, `quality: 17`, `gop: 24`.
  - Encoder table in `common/rfb/encoders/KasmVideoEncoders.h`: `{av1,h265,h264}_{vaapi, ffmpeg_vaapi, nvenc, software}` (libsvtav1, etc.) through FFmpeg.
  - It is carried as a custom RFB rect type **"KasmVideo"**, decoded by **WebCodecs** in the client (`core/decoders/kasmvideo.js`).
- **Transport:**
  - WebSocket over HTTPS by default.
  - Optional "UDP": the client opens a `RTCPeerConnection` and a **DataChannel `"webudp"` with `ordered:false, maxRetransmits:0`** (`core/rfb.js`). The server side is the embedded WebUDP "Wu" library (`common/network/webudp/`).
  - STUN IP discovery is **off by default** since 1.5.
- Other features:
  - Multi-monitor (1.3+) via extra browser windows coordinated with `BroadcastChannel`.
  - IME mode toggle (`enable_ime`).
  - Keyboard Lock API (1.4).
  - Relative mouse (1.5).
  - Smart card bridge (1.4).
  - `get_sessions` API.
  - Downloads API.

### 2.2 Limits for gaming

- Xvnc is a software X server. 3D needs DRI3 plus copies, and there is no NvFBC/dmabuf zero-copy path like Selkies has **(inferred from architecture)**.
- The video mode is new and its pacing is unmeasured publicly. Kasm images default to `MAX_FRAME_RATE=24`.
- Audio and gamepads are separate side-services in Kasm images, not part of the protocol.
- "UDP" is unreliable SCTP over DTLS in a data channel, with no RTP/TWCC-style congestion control **(unverified whether KasmVNC implements its own)**.

### 2.3 Steal for Cha Portal

- The YAML config schema with **`allow_client_to_override_*` + `allow_override_list`**: the server decides which knobs a client may change.
- Dynamic quality based on change rate.
- **QOI lossless for LAN.**
- The unreliable DataChannel trick as a cheap UDP path where WebTransport is unavailable.
- `BroadcastChannel`-coordinated multi-window multi-monitor.

**Verdict:** protocol-compatible (a "compat/low-power" environment type). Not the core streaming path.

---

## 3. Selkies (selkies-project/selkies — formerly selkies-gstreamer)

**What:** Low-latency GPU/CPU-accelerated HTML5 streaming for Linux X11/Wayland. Its tagline: "Moonlight, Google Stadia, or GeForce NOW in noVNC form factor" ([README](https://github.com/selkies-project/selkies)).
**License:** MPL-2.0 (Selkies, pixelflux, pcmflux, SealSkin).

### 3.1 History

1. **2019–2021:** Google Cloud solution using GStreamer `webrtcbin` and NVENC ([archived article](https://web.archive.org/web/20210310083658/https://cloud.google.com/solutions/gpu-accelerated-streaming-using-webrtc)).
2. **Then:** open-sourced as **selkies-gstreamer** and community-maintained (academic researchers).
3. **2025:** LinuxServer.io built a WebSocket mode on `pixelflux`/`pcmflux` for **Webtop 3.0**, which replaced its KasmVNC base images ([LSIO blog, SealSkin post 2025-11-21](https://www.linuxserver.io/blog/webtop-3-0-part-3-putting-it-all-together-with-sealskin)).
4. **Webtop 4.0 (2026-01-07):** a native **Wayland** compositor built on Smithay, with zero-copy dmabuf encode. It credits Games-on-Whales' `gst-wayland-display` for the CUDA path ([blog](https://www.linuxserver.io/blog/webtop-4-0-wayland-is-here-engage-the-reality-engine)).
5. **Selkies 2.0.0 (2026-09-23; rc0 2026-09-12):**
   - **GStreamer removed entirely.**
   - WebSockets is the default and WebRTC is opt-in.
   - Five codecs.
   - X11 and Wayland backends.
   - The `selkies-session` launcher for Jupyter/Coder/Open OnDemand.
   - .deb/.rpm/.apk/Arch/AppImage/PyPI packages ([releases](https://github.com/selkies-project/selkies/releases)).
   - The GitHub API for `selkies-project/selkies-gstreamer` now resolves to `selkies-project/selkies`, i.e. the repo was renamed.

**Status:** very active (push 2026-10-03, ~2.3k stars). The README asks for maintainers. Open epics: [#422 full Rust migration](https://github.com/selkies-project/selkies/issues), #48 WebTransport (open since 2022), #424 Vulkan Video, #57 Windows/macOS hosts.

### 3.2 Architecture (from source, commit 5927ca7, 2026-10-03)

- **One Python process** (`aiohttp`, uvloop) serves the web client and every endpoint on **one port (8080)**. Key files: `src/selkies/websockets_mode.py` (374 KB), `webrtc_mode.py`, `input_handler.py` (449 KB) and `settings.py`. Every setting is available as a flag and as an env var `SELKIES_*`.
- **pixelflux** (Rust 2024 edition, PyO3 cdylib; `pixelflux/src/{lib,pipeline,pace}.rs`, `encoders/{nvenc,vaapi,software,svtav1,vpx,tegra,v4l2m2m}.rs`, `wayland/`, `x11/`, `recorder/`, `webcam/`, `computer_use.rs`):
  - **Capture:**
    - X11 XShm: one copy.
    - **X11 NvFBC → CUDA → NVENC, zero-copy:** 2.51 ms/frame vs 5.24 ms XShm for H.264 at 1080p on a V100.
    - **X11 DRI3:** GBM dmabuf pool, server-side `CopyArea`, imported by NVENC/VA-API.
    - **Wayland:** its own headless compositor built on Smithay. dmabuf → encoder with **zero copy**.
    - **External compositor capture** via `ext-image-copy-capture` / `wlr-screencopy` / desktop portal, with input via virtual-keyboard/pointer or the portal.
  - **Encoders:** NVENC (H.264/H.265/AV1 Ada+), VA-API (all five), Jetson/Tegra, V4L2-M2M. Software: x264 or OpenH264, x265 or kvazaar, libvpx, SVT-AV1. A `PIXELFLUX_ENABLE_GPL=0` build drops x264/x265.
  - **4:4:4** (H.264/H.265, VP9 profile 1) and **10-bit**.
  - **Screen-content tricks:** damage detection, **striped** parallel H.264/JPEG, **"paint-over"** (re-encode a still screen at higher quality after N static frames), and an infinite GOP with IDR on demand.
  - **Pacing:** X11 capture publishes `_FAKE_SCREEN_FPS` on the root window. Selkies' patched XLibre Xvfb runs its fake vblank at the stream rate. An optional `SELKIES-SEMAPHORE` GPU semaphore cuts grab→encoded from 28 ms to **10–12 ms** for a GPU-bound client (GTX 1080).
  - **Extras:** fragmented-MP4 recording tap, virtual webcam, uinput gamepads, and a **Computer-Use HTTP API** for agents.
- **pcmflux:**
  - PulseAudio/PipeWire-Pulse capture → **Opus**, with **RED (RFC 2198)** redundancy and surround (6/8 channels, `multiopus`).
  - Mic uplink back into a Pulse source.
  - Capture runs only when some client is consuming.
- **Transports:**
  - **WebSockets (default):**
    - Binary frames typed by the first byte (0x01 Opus, 0x03 JPEG, 0x04 video stripe, 0x02 mic PCM, webcam, 0x05 gzip control). Control verbs travel as text.
    - The client **ACKs the newest frame id every 50 ms**. That sizes a per-display backpressure window (`BACKPRESSURE_ALLOWED_DESYNC_MS=250`) and feeds RTT smoothing and **delay-based congestion control** (`--congestion-control`).
    - Per-client `_VideoRelay` does **drop-and-resync past a byte budget**, so a slow viewer never stalls the shared encoder.
    - File transfers are **paced against video**.
  - **WebRTC (opt-in, `--mode=webrtc`):**
    - **Vendored aiortc fork** with RTP packetizers for H.264, H.265, VP8, VP9 and AV1.
    - Transport-wide-cc feedback, goodput-based loss recovery, ICE-lite, port range and UDP/TCP mux.
    - coTURN plus a TURN-REST credential service are in `addons/`.
  - `--enable-dual-mode` lets the user switch transport at runtime.
  - **Codec ladder:** hardware codecs by efficiency (AV1 > H.265 > VP9 > H.264 > VP8), then software by encode time, then striped H.264, then JPEG. Codecs the browser cannot decode are greyed out using measured client capabilities.
- **Web client:**
  - [`selkies-web-core`](https://github.com/selkies-project/selkies/tree/main/addons/selkies-web-core) decodes with WebCodecs and paints without a copy where possible.
  - Without WebCodecs it falls back to striped JPEG; for Opus it falls back to libopus-wasm.
  - Two reference React dashboards; every sidebar section can be hidden with `--ui-*` flags.
  - Embeddable through `window` messaging.

### 3.3 Product features

- **Gaming mode** (Ctrl+Shift+X): fullscreen + **Keyboard Lock API** + Pointer Lock + **raw (unaccelerated) pointer motion** (Windows and macOS grant raw motion; Linux and Android do not). Escape must be held for 2 s to exit.
- **Gamepads:**
  - Gamepad API → **Input Interposer**: an LD_PRELOAD library that fakes `/dev/input/js*` and evdev through sockets, so it works unprivileged in containers.
  - **fake-udev**, or **uinput** where writable.
  - Player slots 2–4 via share links.
  - [Universal Touch Gamepad](https://github.com/selkies-project/selkies/tree/main/addons/universal-touch-gamepad) overlay for phones.
- **Touch:**
  - Direct-touch and **trackpad** modes, with acceleration and a locally drawn cursor.
  - Soft modifier keys and a palette of "More keys" and user chords.
- **Clipboard:** text, images and HTML, in both directions. Server policy is `true|in|out|false` and per-browser toggles exist; a "Seamless" switch can turn auto-sync off.
- **File transfer:** upload, plus a download browser on `~/Desktop`; can be narrowed to `upload|download|none`.
- **Printing:** a per-session CUPS queue → PDF → the browser's print dialog.
- **Mic and webcam:** the webcam goes out as H.264/VP8/VP9/AV1/H.265/MJPEG → V4L2 interposer, v4l2loopback or PipeWire. `demand` policies prompt for the device only when an app opens it.
- **Second display:** an "Add Screen" companion window (`#display2-<side>`). The Wayland backend creates outputs on demand.
- **Display scaling:** dynamic resize to the browser size, DPI scaling (96–288), and CSS-scaling mode.
- **IME:** composition-event handling, with a keyboard input-assist field for mobile.
- **Session sharing:**
  - Links: `#shared` (view-only), `#player2..4` (gamepad slot only).
  - A second tab joins as a co-controller. Roles are **enforced server-side**.
- **Secure mode** ([docs](https://github.com/selkies-project/selkies/blob/main/docs/secure-mode.md)):
  - A `--master-token` lets the orchestrator `POST /api/tokens` with a session token table: `{token: {role: controller|viewer, slot, mk_control}}`.
  - Clients open `https://host/#token=…` (fragment, so it never appears in logs). Changing the table live disconnects removed tokens and reassigns roles.
  - **This is the exact hook Cha Portal's control plane needs.**
- **Operator API:** `GET/DELETE /api/sessions`, `POST/DELETE /api/recording` (fMP4 H.264 plus Opus), `GET /api/screenshot`.
- **Audit webhook:** JSON for clipboard, file, print, connect, recording and capture-demand events.
- **Observability:** Prometheus metrics and `/api/health`.
- **Hooks:** `--run-after-connect` and `--run-after-disconnect` (useful for idle/suspend logic).

### 3.4 Images and ecosystem

- **Upstream images:**
  - `ghcr.io/selkies-project/selkies/{base,desktop}:latest-{ubuntu26.04|debiantrixie}` (multi-arch).
  - KDE `selkies-egl-desktop` / `selkies-glx-desktop`.
- **LSIO** [`baseimage-selkies`](https://github.com/linuxserver/docker-baseimage-selkies) (GPL-3.0; pins `SELKIES_RELEASE=2.0.0`, `PIXELFLUX_RELEASE=2.1.0`):
  - Distros: Alpine 3.24, Arch, Debian trixie, Fedora 44, Kali, Ubuntu "resolute".
  - labwc on Wayland, or Openbox on X11.
  - **proot-apps** installs portable apps into `$HOME`.
  - A **built-in Steam installer** (glibc images).
  - Docker-in-Docker.
  - `DEV_MODE` hot-reload for Selkies and pixelflux development.
- **[Webtop](https://github.com/linuxserver/docker-webtop)** (GPL-3.0, ~4.4k stars): KDE/XFCE/i3/MATE/LXQt on Alpine, Arch, Debian, Fedora and Ubuntu. Most tags support Wayland. NVIDIA needs host driver ≥580.
- **[SealSkin](https://github.com/selkies-project/sealskin)** (MPL-2.0, v0.3.2):
  - A self-hosted orchestrator: one server container drives Docker. Apps come from YAML "app stores" of LSIO images.
  - Cleanroom or persistent homes, file manager and share links.
  - **Collaboration rooms** with chat, voice and video, gamepad slots and control hand-over.
  - Per-session E2E-encrypted API and passwordless auth with a client-held key.
  - Chrome/Firefox extensions (open links and files remotely) and iOS/Android apps.
  - **The closest open-source analogue to Cha Portal** (single-server, Linux-only, browser-isolation-first).
- **[Pelorus](https://docs.linuxserver.io/selkies/components/pelorus/)**: an LLM "navigator" that drives the desktop through pixelflux's Computer-Use API using **accessibility trees and window lists as text**. It has no auth of its own.

### 3.5 Latency and claims

- The project's own floor claim is "at least 60 fps on Full HD", plus 1080p60 on software encode under 100% CPU.
- **Host-side measurements** from the docs:
  - NvFBC capture: 2.5 ms/frame.
  - Key-press → encoded frame: **13 ms without vsync vs 43 ms with vsync** for a light GLX client on an RTX 3090.
  - GPU-semaphore path: 10–12 ms grab→encoded.
- **No published glass-to-glass number (unverified).**

### 3.6 Pain points

- Python control plane. The project itself is planning a Rust migration (#422).
- **The WebSocket default is TCP**, so it suffers head-of-line blocking on lossy WAN. WebRTC mode fixes that but needs TURN/ports. **WebTransport is still an epic (#48).**
- Linux hosts only (#57).
- Recent bugs: macOS stuck keys (#427), gamepad slot collision (#431).
- 2.0 is ten days old, so expect churn.
- Docs are huge and occasionally contradictory.
- The default pixelflux build links GPL x264/x265, which is fine for us.

### 3.7 Steal for Cha Portal

1. **Secure-mode token table** as the node↔control-plane contract: role, gamepad slot, mk_control, live reconciliation.
2. **The app-level frame-ACK backpressure plus delay-based CC** pattern, plus per-viewer drop-and-resync relays. This applies to any reliable transport (WebSocket or WebTransport streams).
3. **Codec ladder with measured client capability probing**; disabled-not-hidden UI for codecs the browser cannot decode.
4. **Paint-over** (quality refinement of static content) and **infinite GOP + IDR on demand** for desktop content.
5. Gaming mode UX (Keyboard Lock + Pointer Lock + raw motion), trackpad mode, soft keys, touch gamepad.
6. Unprivileged gamepads (LD_PRELOAD interposer + fake-udev). Required for Steam in rootless containers.
7. Demand-driven mic and webcam; printing → PDF; paced file transfer; audit webhook.
8. Share links by role and slot (`viewer`, `player2..4`), which fits "couch co-op over the internet".

**Verdict:**
- **Depend-on** Selkies (its images and server) as the Linux "desktop/app" engine for v1, driven via secure mode.
- **Fork or vendor `pixelflux`** (MPL-2.0) as the capture/encode core of a Cha Portal native node agent. It needs its PyO3 surface turned into a Rust crate API. This is also where a **Pyrowave encoder** would plug in (Vulkan compute next to the existing NVENC/VA-API encoders; speculative).

---

## 4. Neko (m1k1o/neko v3) and neko-rooms

**What:** A self-hosted virtual browser (or any X11 app or desktop) in Docker, streamed over WebRTC to **many simultaneous users** for watch parties, co-browsing and support. It is the open-source "alternative to Hyperbeam".
**Status:** v3.1.6 (2026-09-30), v3.1.5 (2026-08-05), ~22.4k stars. The v3 server merged the archived demodesk/neko server; there is a V2-client compatibility layer.
**License:** Apache-2.0.

### 4.1 Architecture (from source, commit bdd0428, 2026-09-30)

- **Go server** (`go 1.25`) using **Pion WebRTC v4**, chi, gorilla/websocket, viper and Prometheus. Modules: `server/internal/{capture,webrtc,websocket,member,session,desktop,plugins}` and `server/pkg/{gst,xorg,xinput,xevent}`.
- **Capture:**
  - **GStreamer via cgo.** Default pipeline is `ximagesrc display-name=… show-pointer=true use-damage=false ! videoconvert ! vp8enc …` at **25 fps** (`capture_pipeline.go`), so every frame is a CPU readback.
  - Codecs: VP8 (default), VP9, AV1 (libaom), H.264 (x264 / `vah264enc` / `nvh264enc` or **`nvautogpuh264enc` for driver 590+**), and **H.265 since 3.1.5**.
  - Multiple named pipelines (`capture.video.ids`) plus a **bandwidth estimator** that switches between them (simulcast-like). Extended to legacy clients in 3.1.6.
  - **RTMP broadcast**, a JPEG **screencast** fallback, webcam (`v4l2sink`) and mic.
- **Display:** Xorg dummy driver, plus a custom **`xf86-input-neko`** driver for touch. Resolution comes from a list of modes (`desktop.screen` default **1280x720@30**).
- **GPU** (`runtime/Dockerfile.nvidia`): CUDA base + **VirtualGL** (`VGL_DISPLAY=egl`). Chromium runs with Vulkan/ANGLE and VA-API features; encode uses nvh264enc.
- **Client:** Vue 2 (EOL) with Guacamole's keyboard library (composition handling) and server-side keyboard layout selection. The roadmap calls for a Vue 3 client and then a **framework-free TS library**, plus pluggable connection, media and control layers (HLS/WebRTC/QUIC) ([roadmap](https://github.com/m1k1o/neko/blob/master/webpage/docs/roadmap.md)).
- **Transport refactor:** [PR #699](https://github.com/m1k1o/neko/pull/699), merged 2026-09-24, adds a capture subscription API so that **WebSocket/WebTransport** transports can be added. Issue #690 asks for "webcodec, websocket, and webtransport".

### 4.2 Multi-user model

- **Member providers:** `multiuser` (shared admin/user passwords), `file`, `object`, `noauth`, and `oauth` (with an email allowlist), plus **OIDC-only** auth (3.1.6).
- **Session controls:**
  - A **host/control** model: one controller at a time.
  - REST verbs `control/request|give|take|release|reset`.
  - `implicit_hosting`: whoever clicks takes control.
  - `locked_controls`, `control_protection` (control only while an admin is present), `locked_logins` and `private_mode`.
  - `inactive_cursors` shows everyone's cursor positions out-of-band.
- **Plugins:** chat, file transfer (multi-select and delete in 3.1.6), and "open in app".
- **API:** full REST API with docs (members CRUD, clipboard, keyboard map and modifiers, screen config, broadcast, batch).

### 4.3 Browser and app images

- **Images:** `firefox`, `tor-browser`, `waterfox`, `chromium`, `google-chrome` (now arm64 too), `ungoogled-chromium`, `microsoft-edge`, `brave`, `vivaldi`, `opera`, plus `xfce`, `kde`, `remmina`, `vlc`. GPU variants are `nvidia-*` and `intel`. Widevine is installed for amd64 and arm64.
- **Chromium managed policy** (`apps/chromium/policies.json`):
  - Locks down: autofill off, `BrowserSignin: 0`, `DeveloperToolsAvailability: 2`, `DownloadRestrictions: 3`, `IncognitoModeAvailability: 1`, `SyncDisabled`, `PasswordManagerEnabled: false`.
  - Blocks: guest mode and add-person, `file://*` and `chrome://policy`.
  - Extensions: all blocked except a force-installed allowlist.
  - Launch flags: `--no-sandbox --bwsi --start-maximized --disable-dev-shm-usage …`. NVIDIA variants add `--use-angle=vulkan --enable-features=Vulkan,VaapiVideoEncoder,…`.
- **neko-rooms** ([repo](https://github.com/m1k1o/neko-rooms), v1.6.5, 2026-03-29):
  - A Go service that creates one neko container per room through the Docker API, with Traefik labels or an NGINX path. It has a web UI and an OpenAPI spec.
  - Optional storage mounts and a per-room GPU checkbox.
  - Images must be pre-pulled. The roadmap is still open on auto-pull, an API bearer token, Docker over SSH/TCP, Swarm and k8s.
- **Downstream use:** [kernel/kernel-images](https://github.com/kernel/kernel-images) (Apache-2.0, "browsers-as-a-service for web agents") **forked neko for live view**, and also runs it on Unikraft unikernels.

### 4.4 Pain points

- Fixed resolutions with no fluid resize ([#595](https://github.com/m1k1o/neko/issues/595) explicitly points at Webtop/Selkies as better).
- CPU capture and 25–30 fps defaults; VP8 default.
- Vue 2 client.
- iOS Safari crash (#626), mobile trackpad (#640), HDR request (#703).
- The streaming stack is not gaming-grade (no NvFBC/dmabuf).

### 4.5 Steal for Cha Portal

1. **Control hand-off semantics** (request/give/take/release/reset, implicit hosting, admin-protected control, inactive cursors). This is the best-designed multi-user model in the category.
2. **Curated browser image set plus managed-policy JSONs** for Chromium-family and Firefox. Reuse them directly (Apache-2.0 is GPLv3/AGPLv3-compatible).
3. Multiple encoder pipelines with BWE switching between quality rungs (a simulcast ladder for multi-viewer rooms).
4. **RTMP broadcast** of a session (watch-party streaming) and the JPEG screencast fallback for thumbnails and previews.
5. neko-rooms' "one container per room + reverse-proxy labels" is the minimal orchestrator shape for Cha Portal nodes.

**Verdict:** inspiration plus reuse of images and policies. Possibly protocol-compatible ("Neko room" environment type) for watch-party use.

---

## 5. Apache Guacamole

**What:** A clientless gateway. A C proxy daemon **`guacd`** speaks RDP (FreeRDP), VNC, SSH, telnet and Kubernetes. A Java web app tunnels the text-based **Guacamole protocol** (draw instructions with PNG/JPEG/WebP images, audio, clipboard and file streams) to a JS client over WebSocket or HTTP.
**Status:** 1.6.0 on 2025-06-22, with no newer release as of today ([releases](https://guacamole.apache.org/releases/)).
**License:** Apache-2.0.

**1.6.0** ([notes](https://guacamole.apache.org/releases/1.6.0/)):
- A rewritten server-side display optimizer (`guac_display`): worker thread pools, real-time scroll detection, and a 2-D Rabin-Karp diff to find moved or duplicate regions.
- FreeRDP 3.x support.
- A recording player with activity histograms and key events.
- An `AUDIT` permission.
- Time- and host-based access restrictions, auth rate limiting, smart-card auth, batch connection import (CSV/JSON/YAML), and ARM Docker images.

**Features:**
- Auth: LDAP, DB (MySQL/Postgres/SQL Server), **OIDC, SAML, CAS, TOTP, Duo**, header auth and JSON auth.
- Connection groups, sharing profiles (read-only or collaborative share links), and session recording (convertible to video).
- SFTP and RDP-drive file transfer, RDP audio in/out, RDP printing to PDF, and clipboard.

**Limits:** It is image-diff based, so it is not video-codec streaming. It is fine for office RDP and poor for video or games.

**Steal:** sharing profiles; the recording player with an activity heatmap; the "connection" abstraction for external endpoints.

**Verdict:** protocol-compatible. Bundle `guacd` as Cha Portal's **bridge for external RDP/VNC/SSH endpoints** (feature 8 adjacent), as Kasm does.

## 6. Xpra and xpra-html5

**What:** "screen for X11". It offers persistent sessions with **seamless per-window** remoting (not just full desktops) and forwards audio in/out, clipboard, printing, file transfer, webcam, notifications, trays and DPI ([repo](https://github.com/Xpra-org/xpra)).
**Status:** Xpra 6.5.4 (2026-09-27). xpra-html5 v20 GitHub release (2026-03-18) and v21 tag (changelog 2026-05-11). The v20 changelog line is dated "2026-12-19", an obvious typo.
**License:** Xpra GPL-2.0+; xpra-html5 MPL-2.0.

**HTML5 client** (source read):
- `VideoDecoder.js` and `DecodeWorker.js` / `OffscreenDecodeWorker.js` give WebCodecs H.264 decode (high profile) in workers with OffscreenCanvas, plus JPEG/WebP/PNG paint.
- **`WebTransport.js`** provides a WebTransport connection (Xpra server QUIC).
- v21 removed MSE/jsmpeg legacy paths.

**Steal:**
- Seamless-window mode, i.e. "app" environments that render as individual browser-side windows (a power-user feature).
- The WebTransport client plus OffscreenCanvas decode-worker architecture.

**Verdict:** inspiration (and protocol-compatible as an optional "Xpra app" environment).

## 7. noVNC

**What:** The canonical HTML5 VNC client with websockify ([repo](https://github.com/novnc/noVNC)).
**Status:** v1.7.0 (2026-04-28, per the GitHub API): ES-module NPM bundle, better H.264 detection, memory fixes, and a close-tab warning. v1.6.0 added the H.264 encoding, Zlib, a GUI redesign, `defaults.json`/`mandatory.json`, and relative WS URLs (date about 2025-03, **unverified**).
**License:** MPL-2.0 core with some differently-licensed vendored files.

**Role:** It is the client inside KasmVNC (forked), [dockur/windows](https://github.com/dockur/windows) (VNC on port 8006) **(unverified current version)**, and countless homelab images.

**Verdict:** protocol-compatible. Use it as a generic VNC viewer for external VNC endpoints and VMs.

## 8. Hyperbeam (commercial)

**What:** An API for embedding **multiplayer virtual Chromium**, plus Android and console emulator sessions, in web apps over WebRTC ([docs](https://docs.hyperbeam.com/llms.txt)).
- REST to create, list and end sessions.
- Roles and per-user permissions, "persist session state", custom Chrome extension install, resize, bookmarks and usage.
- JS SDK (`@hyperbeam/web`), Unity WebGL SDK, and a tab event API.
- Claimed bandwidth ([FAQ](https://docs.hyperbeam.com/home/faq)): **720p24 ≈ 5 Mbps, 1080p30 ≈ 14.1 Mbps**, with "sharp mode" tripling it.

**License:** proprietary SaaS.

**Steal:**
- An **embeddable SDK** (`<iframe>`/JS component) for Cha Portal sessions.
- Persisted browser state as a first-class API object.
- Per-participant permissions.
- An extension-install API.

**Verdict:** inspiration-only.

## 9. cloud-game (CloudRetro)

**What:** Retro cloud gaming: libretro cores, streamed via **Pion WebRTC** (H.264/VP8 + Opus), with input over a DataChannel ([repo](https://github.com/giongto35/cloud-game), [DESIGNv2.md](https://github.com/giongto35/cloud-game/blob/master/DESIGNv2.md)).
**Status:** Last commit 2026-09-05 (Go 1.26, pion/webrtc v4.2.19). Tags go to v3.1.1; the last GitHub "release" object is from 2021.
**License:** Apache-2.0.

**Architecture worth copying:**
1. A **Coordinator** (web frontend + load balancer + signaling) receives worker registrations and health checks.
2. The user's browser gets a **worker list and probes latency to each**.
3. The coordinator picks the best worker and brokers WebRTC.
4. Multiplayer and crowdplay work by sharing a room deeplink. Save states go to cloud storage.

**Steal:**
- **Client-side latency probing to rank nodes** before placement. This is directly relevant to the "portal into many nodes" dashboard with LAN + WAN nodes.
- Room deeplinks.

**Verdict:** inspiration-only.

## 10. RustDesk web client

- RustDesk (AGPL-3.0, 1.5.0 on 2026-09-30) is TeamViewer-style support remoting.
- The **web client** connects via hbbs/hbbr WebSocket ports 21118/21119 behind a reverse proxy, and is bundled with **RustDesk Server Pro**. It is not a separately published OSS component **(unverified: source availability of the current web client)**.
- Sept 2026 added a **WebRTC data-channel transport raced against TCP/UDP/relay** for native clients ([PR #15684](https://github.com/rustdesk/rustdesk/pull/15684), merged 2026-09-05). It does not enable browser clients.

**Verdict:** out of scope, except as an external-endpoint type later (launch the native client).

## 11. 2025–2026 newcomers and adjacent projects

| Project | What | License | Notes |
|---|---|---|---|
| **[BrowserPane](https://github.com/ITmedes/browserpane)** | Remote Chromium for humans and agents. Rust `bpane-host` (capture/classify), `bpane-gateway` (WebTransport on :4433), `bpane-client` (TS) | **AGPL-3.0** | **Tile-first rendering for UI/text over reliable WebTransport streams**, plus **ROI H.264** for media regions. H.264 **delta frames go as raw WebTransport datagrams**; keyframes and audio are reliable. Also: shared sessions with owner/viewer modes, CDP/Playwright endpoints, recording worker, OIDC/Postgres/Vault, and a frozen v1 binary protocol spec. Experimental; Linux and Chromium only. **Same license family as Cha Portal, so code reuse is possible.** |
| **SealSkin** | See §3.4 | MPL-2.0 | Closest OSS analogue to Cha Portal |
| **Kernel kernel-images** | Agent browsers with a neko-based live view, Docker or Unikraft | Apache-2.0 | Shows the "Chrome environment" pattern for agents |
| **Pelorus** (LSIO) | LLM desktop navigator via the pixelflux Computer-Use API | (LSIO; unverified) | Text/a11y-tree desktop representation |
| **moonlight-web-stream** ([MrCreativ3001](https://github.com/MrCreativ3001/moonlight-web-stream)) | Web server bridging **Sunshine → browser over WebRTC**; forks add WebTransport | (unverified) | Relevant to feature 8 (external Sunshine/Vibepollo hosts in the browser); likely covered in the Moonlight slice |
| LWFA, Viewpoint, FreeRemoteDesk, Reminal, YourDesk | Search-result-only newcomers (Wayland per-window WebCodecs; WebRTC screen share; CF+Vercel P2P) | (unverified) | Not inspected. Low signal, listed for completeness |

---

## 12. Feature matrix

Legend:

| Mark | Meaning |
|---|---|
| ✅ | Built in |
| ◐ | Partial, add-on, or paid |
| ❌ | No |
| ? | Unverified |

"Kasm" means Workspaces + KasmVNC images. "Selkies" means Selkies 2.0 + LSIO images + SealSkin where noted.

| Feature | Kasm | Selkies 2.0 | Neko v3 (+rooms) | Guacamole | Xpra-html5 | noVNC | Hyperbeam | cloud-game | BrowserPane |
|---|---|---|---|---|---|---|---|---|---|
| Ephemeral sessions | ✅ default | ◐ (orchestrator: SealSkin "cleanroom") | ◐ (rooms) | n/a (gateway) | ◐ | n/a | ✅ | ✅ | ✅ |
| Persistent profiles | ✅ volume/S3, templated, reset | ◐ `/config` volume; SealSkin persistent homes | ◐ storage mounts | n/a | ✅ (persistent X session) | n/a | ✅ "persist session state" | ◐ save states | ✅ "persistent session resources" |
| Pre-warmed pools | ✅ session staging | ❌ (SealSkin ?) | ❌ | ❌ | ❌ | ❌ | ? | ❌ | ? |
| Image catalog | ✅ registry JSON + 79 images | ✅ LSIO images + SealSkin YAML stores | ✅ ~15 images | n/a | ❌ | ❌ | n/a | ✅ libretro cores | ❌ (Chromium only) |
| Multi-user / shared control | ✅ share links | ✅ viewer/player2-4/co-controller, token roles | ✅ **best** host hand-off model | ✅ sharing profiles | ✅ multi-client | ◐ shared VNC | ✅ roles | ✅ crowdplay | ✅ owner/viewer |
| Clipboard | ✅ text/HTML/image + DLP | ✅ text/HTML/image, seamless or manual | ✅ text (+image API) | ✅ | ✅ | ◐ text | ? | ❌ | ✅ |
| File transfer | ✅ (closed upload service) | ✅ paced, audited | ✅ plugin | ✅ SFTP/RDP drive | ✅ | ❌ | ? | ❌ | ✅ |
| Audio out / mic in | ✅ / ✅ (sidecars) | ✅ Opus+RED, surround / ✅ | ✅ / ✅ | ✅ / ✅ (RDP) | ✅ / ✅ | ❌ (QEMU audio only) | ✅ / ? | ✅ / ❌ | ✅ / ✅ |
| Webcam | ✅ (sidecar) | ✅ V4L2 interposer/loopback/PipeWire | ✅ v4l2sink | ❌ | ✅ | ❌ | ❌ | ❌ | ◐ (v4l2loopback) |
| Gamepad | ✅ (sidecar) | ✅ unprivileged interposer, 4 slots, touch pad | ❌ | ❌ | ❌ | ❌ | ? | ✅ | ❌ |
| Multi-monitor | ✅ multi-window | ✅ second display | ❌ | ❌ | ✅ (seamless windows) | ❌ | ❌ | ❌ | ❌ |
| Dynamic resize / DPI | ✅ / ✅ | ✅ / ✅ (96–288) | ◐ mode list | ✅ (RDP) | ✅ | ◐ (server-dependent) | ✅ API | ❌ | ✅ |
| IME / keyboard layouts | ✅ IME mode | ✅ composition, layout | ◐ layout select, Guac keyboard | ✅ | ✅ new keymap packet | ◐ | ? | n/a | ? |
| Touch / mobile UX | ◐ | ✅ direct/trackpad, soft keys, touch gamepad | ◐ (#640) | ◐ | ◐ | ◐ | ✅ | ✅ | ? |
| DLP | ✅ extensive (watermark, clipboard, keystroke log, web filter) | ◐ clipboard/file policy, watermark, audit webhook | ◐ policies, locks | ◐ | ◐ | ❌ | ◐ | ❌ | ✅ policy/egress profiles |
| Recording | ◐ Enterprise only | ✅ fMP4 via API | ◐ RTMP broadcast | ✅ + player | ◐ | ❌ | ? | ✅ recorder | ✅ worker |
| Admin / RBAC | ✅ groups, zones | ◐ token roles only (SealSkin admin) | ✅ admin/user + session locks | ✅ fine-grained | ❌ | ❌ | ✅ | ❌ | ✅ projects |
| OIDC / SSO | ✅ SAML/OIDC/LDAP/2FA | ◐ via proxy; SealSkin key-based | ✅ OAuth/OIDC (3.1.6) | ✅ OIDC/SAML/CAS/LDAP/TOTP/Duo | ❌ | ❌ | custom | ❌ | ✅ OIDC |
| Transport | WS (+DataChannel UDP) | **WS (default)** / WebRTC | WebRTC | WS/HTTP | WS / **WebTransport** | WS | WebRTC | WebRTC | **WebTransport** |
| Codecs | JPEG/WebP/QOI + H.264/H.265/AV1 | H.264/H.265/VP8/VP9/AV1, 4:4:4, 10-bit | VP8/VP9/AV1/H.264/H.265 | PNG/JPEG/WebP | H.264 + images | RFB (+H.264) | ? | H.264/VP8 | tiles + ROI H.264 |
| GPU encode | ✅ (1.5) | ✅ NVENC/VA-API/Tegra/V4L2 | ✅ NVENC/VA-API | ❌ | ✅ (server) | n/a | ? | ❌ | ? |
| Zero-copy capture | ❌ (DRI3 only) | ✅ dmabuf/NvFBC/DRI3 | ❌ | ❌ | ? | n/a | ? | n/a (emulator FB) | ? |

### Transport, codec and latency summary

| Project | Video path | Congestion handling | Latency figures (source) |
|---|---|---|---|
| Selkies 2.0 | pixelflux → WS binary frames → WebCodecs; or RTP via aiortc | WS: frame-ACK window + delay-based CC + drop-and-resync. WebRTC: TWCC + goodput recovery | Host side: NvFBC 2.5 ms/frame; key→encoded 13 ms (no vsync, RTX 3090). No glass-to-glass |
| KasmVNC 1.5 | RFB rects or KasmVideo (WebCodecs) over WS, or DataChannel unreliable | Dynamic quality on change rate | None published; Kasm image default 24 fps |
| Neko 3.1 | GStreamer ximagesrc → VP8/H.264 → Pion RTP | REMB/TWCC-style estimator switching pipelines | None published; default 720p@30 (25 fps pipeline) |
| Guacamole 1.6 | Image diff ops over WS | Server-side optimizer | None published |
| Hyperbeam | WebRTC | n/a | 720p24 ≈ 5 Mbps; 1080p30 ≈ 14.1 Mbps |
| BrowserPane | Tiles (reliable) + ROI H.264 deltas (datagrams) over WebTransport | n/a | None published (experimental) |

---

## 13. Recommendations for Cha Portal

These are scoped by the project's decisions: AGPL/GPL, homelab/small group first, LAN = WAN.

### 13.1 Product features and UX patterns to adopt

**Must-have (v1)**
1. **Dashboard of nodes and environments, with live latency badges per node.** The browser probes each node directly, as cloud-game does. Show "LAN" vs "WAN" path and the transport chosen.
2. **Environment templates from a catalog** (Kasm registry JSON-style): image, arch, resources, GPU, persistence mode, default policies. Support third-party catalog URLs.
3. **Ephemeral vs persistent as a per-launch choice:**
   - Persistent means a named volume `{user}/{env}` with a "reset" action and a size limit.
   - Ephemeral means a destroyed container, with an optional "keep for N minutes after disconnect".
4. **Pre-warmed pools** (Kasm staging) for ephemeral environments, especially Chrome. Personalize at assignment time with an exec hook (`--assign --url …`). Mark which templates are pool-compatible.
5. **Share links with roles:** viewer / co-controller / player-N gamepad slot (Selkies), with control hand-off (Neko request/give/take, implicit hosting toggle). The link carries a token in the URL **fragment**.
6. **Gaming mode:** Keyboard Lock + Pointer Lock + raw motion, with a long-press Esc to exit. Plus gamepads (4 slots), touch-gamepad overlay, and trackpad and direct-touch modes for phones and tablets.
7. **Clipboard** (text/HTML/image), **file drop/upload + download panel**, **audio out + mic**. Mic and webcam on demand.
8. **Dynamic resize and DPI** that follow the browser window; an optional second-screen window.
9. **Stats overlay** that answers "am I on GPU encode / zero-copy / HW decode / which path?" first (Selkies' first four rows), then bitrate, RTT and drops.
10. Simple **RBAC**: owner/admin, user, guest (link-only). Optional **OIDC**. **Casting links** for throwaway sessions.

**Nice-to-have (later)**
- Session recording (fMP4) and screenshot API; RTMP broadcast for watch parties; audit webhook.
- Light per-environment policy: clipboard direction, upload/download, watermark. No heavy DLP.
- Seamless-window "app" mode (Xpra-style).
- Embeddable SDK / iframe component (Hyperbeam-style).
- Agent hooks: Computer-Use HTTP API, CDP endpoint for Chrome environments (BrowserPane/Kernel-style).

### 13.2 Reusable implementation pieces and licence notes

| Piece | Licence | How to use | Compatibility with AGPL-3.0 |
|---|---|---|---|
| **Selkies server + web client + base images** | MPL-2.0 | Run as the in-container streaming engine for Linux environments. Drive via **secure mode** (`POST /api/tokens`, `#token=` URLs, `/api/sessions`, `/api/recording`) | ✅ (MPL file-level copyleft; combine freely) |
| **pixelflux** (Rust capture/encode, Smithay compositor, NVENC/VA-API/SW, recording, computer-use) | MPL-2.0 (+GPL x264/x265 in default build) | Vendor/fork as the core of a native **Cha node streamer**: wrap its Rust internals without PyO3. Add a **Pyrowave** encoder beside NVENC/VA-API | ✅ |
| **pcmflux** (Opus + RED + mic) | MPL-2.0 | Same | ✅ |
| **Selkies Input Interposer, fake-udev, V4L2 interposer, universal-touch-gamepad** | MPL-2.0 | Reuse as-is in environment images (unprivileged gamepads and webcam) | ✅ |
| **LSIO baseimage-selkies / webtop images** | GPL-3.0 (build scripts) | Use as base images and an environment catalog | ✅ (GPLv3 + AGPLv3 combinable) |
| **Neko browser images + Chromium/Firefox policies** | Apache-2.0 | Copy policy JSONs and launch flags into Cha "Chrome" environments | ✅ (Apache-2.0 → GPLv3/AGPLv3 OK) |
| **Kasm image conventions** (default-profile copy, `custom_startup.sh` with `--go/--assign/--url`, `LAUNCH_URL`) | MIT (repos) | Adopt the *convention*. Optionally run unmodified `kasmweb/*` images as a compat type | ✅ for repo code. ❌ **do not redistribute Kasm's S3 sidecar binaries** |
| **Kasm registry schema** | MIT (template repo) | Mirror the schema (plus Cha extensions: transport, codec capabilities, GPU class) | ✅ |
| **KasmVNC** | GPL-2.0-or-later | Optional compat environment type; reuse DLP config ideas | ✅ (v2+ → v3 OK) |
| **guacd** | Apache-2.0 | Bridge for external RDP/VNC/SSH endpoints | ✅ |
| **noVNC** | MPL-2.0 (mixed) | Generic VNC viewer for VMs/external VNC | ✅ |
| **xpra-html5 WebTransport/decode-worker code** | MPL-2.0 | Reference for a WebTransport + OffscreenCanvas worker client | ✅ |
| **BrowserPane** (tile + ROI H.264 over WebTransport, protocol spec) | AGPL-3.0 | Study, and possibly reuse `bpane-protocol`/host classification for "Chrome" environments | ✅ (same licence) |
| Kasm Workspaces platform, Hyperbeam | Proprietary | Ideas only | ❌ |

### 13.3 Architecture implications

- **Two-tier node design** (Kasm agent / cloud-game worker analog):
  - A **Cha node agent** (in the docker compose stack) registers with the portal, reports capacity, GPUs, codecs and pool state, and creates or destroys environment containers.
  - Each environment runs a **streamer** (Selkies first; Cha's own pixelflux-based or Pyrowave streamer later) that the agent provisions with per-session tokens. This is exactly Selkies' master-token model.
- **Transport ladder** to make LAN and WAN equally first-class:
  1. **WebTransport** (QUIC datagrams for video deltas, streams for control and keyframes), as BrowserPane and xpra-html5 do.
  2. **WebRTC** for WAN/NAT with TURN, and for Safari where WebTransport is missing **(verify Safari WebTransport status in the transport slice)**.
  3. **WebSocket + frame-ACK CC** as the universal fallback (Selkies).

  Keep the codec layer transport-agnostic (WebCodecs, or a WASM/WebGPU Pyrowave decoder).
- **LAN profile:** high bitrate, 4:4:4, possibly lossless or QOI-like for static UI, ~0 jitter buffer.
  **WAN profile:** CC-driven bitrate, Opus RED, keyframe-on-demand, IDR recovery.
  Auto-select from the latency probe and path type.
- **Pre-warm the expensive part.** For gaming environments, a warm pool is less useful than **fast resume of persistent containers**. Keep Steam libraries on a shared volume and pool only the generic Chrome/desktop images.

### 13.4 Risks and gotchas

- **Selkies 2.0 is ten days old** and moves fast (the Rust migration epic could reshape the API). Pin versions and wrap secure mode behind our own adapter.
- **Selkies' default WebSocket transport is TCP.** On lossy WAN it will stutter (HOL blocking). We must offer WebRTC/WebTransport for WAN.
- **Kasm images phone home** to Kasm's S3 at build time for proprietary sidecars. Running `kasmweb/*` images is the user's choice; we cannot ship them.
- **NVIDIA in containers:** Webtop requires host driver ≥580, with kernel params and a dummy plug on older drivers. NvFBC needs the `video` driver capability. Pyrowave/Vulkan paths will add their own requirements.
- **Unprivileged gamepads** depend on the LD_PRELOAD interposer. Steam's 32-bit and 64-bit processes both need it (the LSIO image builds `_32.so` too). uinput needs privileges.
- **Browser capability skew:** no HEVC decode in Chrome/Firefox on Linux; Firefox lacks raw pointer motion on Linux; Brave Shields blocks Keyboard Lock; iOS Safari has crash history in neko (#626).
- **Licence hygiene:** pixelflux's default build links GPL x264/x265, which is fine for AGPL but means binary distributions must ship sources.

### 13.5 Open questions

1. Embed **Selkies as-is** (Python process per environment) for v1, or invest early in a Rust node streamer built on forked pixelflux (+Pyrowave)?
2. Is **WebTransport** available in all target clients (Safari, iOS) as of Oct 2026? If not, is WebRTC the WAN default and WebTransport the LAN fast path?
3. Should persistent profiles be **node-local volumes only** (homelab-simple), or also S3-synced so they roam between nodes, Kasm-style?
4. Do we want Neko-style **watch-party rooms** (many viewers, simulcast ladder, RTMP out) in v1, or just "share link to a co-controller"?
5. Should Cha Portal accept **existing Selkies/Webtop/Neko/KasmVNC containers** as "external environments" (protocol-compatible adapters), or only Cha-managed images?
