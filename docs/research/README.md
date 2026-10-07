# Cha Portal: research synthesis (snapshot 2026-10-03)

> [!NOTE]
> Background research from planning: surveys of the field, kept for the reasoning behind the plan. Naming a project here doesn't mean any of its code is in Cha Portal. [`docs/PROVENANCE.md`](../PROVENANCE.md) records what was ported and from where.

Eight parallel research slices, each with sources inline and claims we could not confirm tagged **(unverified)**. This page holds the cross-cutting conclusions. The numbered notes have the evidence.

| # | Slice | File |
|---|---|---|
| 01 | Wolf / Games on Whales, Steam-in-Docker | [01-wolf-games-on-whales.md](01-wolf-games-on-whales.md) |
| 02 | PyroWave and the low-latency codec landscape | [02-pyrowave-and-codecs.md](02-pyrowave-and-codecs.md) |
| 03 | Moonlight / Sunshine / Apollo / Vibepollo and the GameStream protocol | [03-moonlight-sunshine-ecosystem.md](03-moonlight-sunshine-ecosystem.md) |
| 04 | Kasm-like browser desktop streaming (Kasm, KasmVNC, Selkies, Neko, Guacamole, …) | [04-browser-desktop-streaming.md](04-browser-desktop-streaming.md) |
| 05 | Browser transport / decode / input APIs, commercial cloud gaming | [05-web-transport-and-input.md](05-web-transport-and-input.md) |
| 06 | Node-side Linux graphics, capture, input, audio, GPU-in-container | [06-linux-graphics-container-stack.md](06-linux-graphics-container-stack.md) |
| 07 | Control plane, node agent, networking, persistence, native client | [07-control-plane-and-native-client.md](07-control-plane-and-native-client.md) |
| 08 | Adjacent platforms and 2025–26 entrants (Nestri, Punktfunk, Polaris, …) | [08-adjacent-platforms-2026.md](08-adjacent-platforms-2026.md) |
| 09 | Technology for the native client, October 2026 (Game Mode, AWDL, MetalFX, iroh 1.0, L4S, MoQ) | [09-native-client-tech-2026-10.md](09-native-client-tech-2026-10.md) |

---

## 1. What changed in 2026 (and why it matters)

1. **PyroWave went mainstream in September 2026.**
   - Valve shipped it in the **Steam Remote Play beta** (2026-09-21).
   - **Vibepollo 2.0.0** (2026-09-30) and **Polaris 1.4.13** (2026-09-26) ship it over the Moonlight transport.
   - A **Wolf PR (#517)** adds a GStreamer element.
   - **Nestri** and **Punktfunk** carry it over QUIC.

   PyroWave on wired LAN is now *table stakes*. Nobody decodes it **in a browser**: Punktfunk disables it ("a browser has no device to run it on"). A bit-exact **WebGPU/WGSL port** (`imbcmdth/pyrowave@webgpu`, MIT, 2026-09-26) appeared a week ago. → [02], [08]
2. **WebTransport is "Baseline" on paper but not usable on Safari/iOS.**
   - It shipped in Chrome, Firefox and Safari 26.4, and `serverCertificateHashes` works in all three engines, so self-signed ≤14-day certs can be pinned without a CA.
   - **But WebKit bug 319818** (flow control never refills) deadlocks sessions after about 16 MiB, and Safari's handshake needs newer-draft capsules that `wtransport`/`h3` don't send. moq.dev disables WebTransport on WebKit.
   - WebTransport has no NAT traversal, and Chromium has no HTTP/2 fallback.

   → **WebRTC remains the universal baseline**, and every commercial cloud gaming service uses it in the browser. WebTransport is the Chromium/Firefox ≥153 fast path. → [05], [07]
3. **Chrome Local Network Access** now gates public-origin → private-IP WebSocket and WebTransport (Chrome 147+). **WebRTC is exempt.** The enterprise opt-out goes away in Chrome 156. Serving the portal from a LAN-resolvable name avoids the prompt. → [07], [05]
4. **Chrome's WebRTC "render ASAP" mode**: the sender stamps `playout-delay` 0/0 and Blink switches to its low-latency renderer. H.264/HEVC must carry VUI `max_num_reorder_frames=0`, or hardware decoders buffer about 4 frames. → [05]
5. **WebGPU reach**: Chrome Win/macOS/ChromeOS and Linux on Intel Gen12+ or NVIDIA on Wayland (AMD needs a flag); Safari 26; Firefox on Windows/macOS but **not Linux**. **Subgroups are Chrome-only.** This bounds who can decode PyroWave in a browser. → [05], [02]
6. **The Moonlight protocol is now a Sans-IO Rust crate** (`moonlight-common-rust`, GPL-3, wasm-capable). `moonlight-web-stream` v3 passes host H.264/HEVC/AV1 into WebRTC with no transcoding. "External Sunshine/Vibepollo/Wolf host in a browser" is a gateway problem that is mostly solved. → [03]
7. **Everyone builds their own headless Smithay compositor** and hands GPU-resident DMA-BUFs straight to the encoder: Wolf (`gst-wayland-display`, MIT), Selkies 2.0 (`pixelflux`, MPL-2.0), Nestri (`nescope`, Apache-2.0). Apps and desktops (gamescope, KWin, Chrome) run *nested* as Wayland clients. Full **KDE Plasma in an unprivileged container is solved** (linuxserver webtop: nested `kwin_wayland`, no systemd). → [06], [01]
8. **Selkies 2.0.0** (2026-09-23) dropped GStreamer for Rust `pixelflux`/`pcmflux`. It defaults to **WebSocket + WebCodecs**, with WebRTC opt-in. **No mature open project ships WebTransport + WebCodecs + modern congestion control as its default path.** → [04]
9. **New entrants worth tracking**:
   - **Punktfunk** (Rust, MIT/Apache): its own QUIC+FEC protocol, a GameStream façade, a wasm-compiled core for its browser client, and one virtual display per client.
   - **Polaris** (GPL-3): per-user Steam "Spaces" and an experimental WebTransport browser stream.
   - **Nestri**, rewritten in Aug–Sep 2026: microVMs (nesbox / virtio-nvgpu), iroh QUIC, native client, open-core.

   → [08]
10. **NVENC on GeForce now allows 12 concurrent sessions** (8 in 2024). PyroWave runs on compute and uses **no** NVENC sessions. → [06]
11. **Steam in containers works without `--privileged`.** It still needs user namespaces for pressure-vessel. Wolf's recipe is `SYS_ADMIN` + seccomp/apparmor unconfined + a patched bwrap. A custom seccomp/AppArmor profile is the cleaner route. → [01], [06]
12. **The control-plane shape is settled across the industry** (Kasm, Coder, Portainer Edge, Nestri, Fenrir): a stateful API plus a node agent that **dials out**, reports inventory and heartbeats, and reconciles desired state. Never expose the Docker API remotely. → [07]

## 2. Build vs. borrow (consolidated)

| Need | Borrow | License | How |
|---|---|---|---|
| Headless compositor + zero-copy capture | `gst-wayland-display` / `wayland-display-core` (Wolf). Alt: `pixelflux` (Selkies) | MIT / MPL-2.0 | **Depend** (Rust crate). Spike both |
| Virtual gamepads (DualSense over uhid, gyro/touchpad) | `inputtino` | MIT | **Depend** (Rust bindings) |
| Hotplug devices into running containers | Wolf `fake-udev` + device-cgroup-rule + `mknod` recipe | MIT | **Port / reuse binary** |
| Game & desktop app images | GoW images (Steam, XFCE, Firefox, RetroArch, Lutris, Heroic, ES-DE, …) via the **GoW container contract** | MIT | **Run unmodified** |
| KDE Plasma in a container | linuxserver webtop `ubuntu-kde` recipe (nested `kwin_wayland`) | GPL-3.0 | **Adapt** into a Cha image |
| Browser images + policies | Neko Chromium/Firefox images and enterprise policies | Apache-2.0 | **Adapt** |
| PyroWave codec | `Themaister/pyrowave` (C API v0.6) / `nespyro` (Rust port) / `pyrowave-rs` | MIT | **Depend** (pin commit) |
| PyroWave in browser | `imbcmdth/pyrowave@webgpu` (WGSL encoder+decoder) | MIT | **Fork** → `@cha/pyrowave-webgpu` |
| PyroWave over Moonlight interop | Vibepollo `docs/pyrowave-protocol.md` | GPL-3.0 (spec) | **Implement exactly** in the gateway |
| Moonlight client protocol | `moonlight-common-rust`, `moonlight-web-stream` | GPL-3.0 | **Depend / fork** for `cha-gateway` |
| Moonlight host façade (stock Moonlight clients) | Wolf (run unmodified). Later: Punktfunk/Moonshine-style plane | MIT / BSD-2 | **Run alongside**, then decide |
| WebTransport cert scheme | Punktfunk: 13-day ECDSA cert, hash signed by long-lived node key | MIT/Apache | **Copy design** |
| QUIC media-transport rules | Nestri `docs/media-transport.md` (queue-based CC, ResyncGate, receiver truth) | Apache-2.0 | **Copy design** |
| High-bitrate FEC | Leopard-RS GF(2^16) (as in Punktfunk); critical-band-only FEC for PyroWave (Vibepollo/Nestri) | MIT/Apache | **Depend** (Rust RS crate) + design |
| Control-plane/node channel | Coder: dRPC over yamux over one outbound WebSocket | AGPL-3.0 | **Copy design** (Rust equivalent) |
| Node enrollment | Nestri / k3s / Portainer: hashed one-time token, CA pin, node keypair | Apache-2.0 | **Copy design** |
| Workspace catalog format | Kasm registry `list.json` (schema 1.1) | MIT (format) | **Adapt** plus importers |
| Product model (zones, staging pools, persistent profiles, casting links) | Kasm Workspaces | Proprietary | **Inspiration only** |
| Shared-control UX | Neko (request/give/take), Selkies secure-mode roles, linckosz/moonlight-web share levels | Apache / MPL / GPL | **Copy UX** |
| RDP/VNC/SSH endpoints | Apache Guacamole `guacd` | Apache-2.0 | **Bundle** (later) |
| Native client reference | Magic Mirror `mm-client` (ash/winit/ffmpeg hwaccel), moonlight-qt (pacing/HDR) | MIT / GPL-3.0 | **Reference** |

**Do not ship**: Kasm's closed sidecar binaries; BUSL code (Magic Mirror server, Nomad); Steam-Headless code (GPL-2.0-*only* is unverified, and if so incompatible with GPLv3/AGPL).

## 3. Cross-cutting lessons (pitfalls others hit)

- **Transport churn kills projects.** Nestri rewrote its transport 3–4 times in two years, and each rewrite reset the client. → Put one transport abstraction in front from day one: WebTransport primary, WebRTC fallback, WebSocket last.
- **Sender metrics lie.** Nestri saw "zero frames dropped" while the client got nothing, and an 8 s queue at zero loss. → Make **receiver-side truth** and the **send-queue duration** the congestion signals. Never let QUIC `send_datagram` silently evict.
- **QUIC datagrams starve keyframes on streams.** quinn writes DATAGRAM frames before STREAM frames. → Use a ResyncGate: don't send deltas that depend on a keyframe the receiver lacks.
- **Moonlight-shaped limits**: fixed mode at launch, cursor composited into the video, RS FEC capped at about 1 Gbps, no NAT traversal, and incompatible fork extensions (clipboard, mic, permissions). → Keep GameStream as a *compatibility* plane, not our core protocol.
- **Wolf rough edges**: lobby OOM leak and crash, NVIDIA zero-copy crashes, encoding always on the first GPU, Steam Input broken in unprivileged containers, one session per client cert. → Wrap Wolf behind an adapter. Mint a Moonlight client cert per viewer.
- **PyroWave gotchas**:
  - Bitstream is a draft with no version field. → Pin the commit and negotiate a bitstream ID.
  - Only a 3-bit frame counter. → Carry our own frame ID.
  - Blocks are up to about 16 KB. → Fragment them.
  - It burns full bitrate on a static desktop. → Skip frames with no damage and send periodic heal frames.
  - At desk distance, 4:4:4 at 1440p/4K above 60 fps needs ≥2.5 GbE.
- **Browser reality**:
  - No HEVC decode on Linux Chrome/Firefox.
  - No `unadjustedMovement` raw mouse in Chrome on Linux.
  - Keyboard capture is split: `navigator.keyboard.lock()` on Chromium (Brave blocks it), `requestFullscreen({keyboardLock})` on Firefox 151 and Safari 26.4.
  - Subgroups are Chrome-only. → Write a subgroup-free PyroWave dequant path.
  - iPhone has no pointer lock, keyboard lock, element fullscreen or haptics.
  - The browser compositor likely adds about one frame of latency.
- **Containers on NVIDIA**:
  - Images must not bundle NVIDIA userland; use CDI or a driver volume.
  - Driver ≥580; the 595 series turns on the display mode setting Wayland needs by default.
  - Explicit GPU selection is needed (Wolf issue #298: encoding always lands on the first card).
- **Rootless containers can't hot-plug gamepads** (`mknod` fails in user namespaces). Gaming nodes need a rootful runtime. The node agent is root-equivalent: document it and keep it small.
