# 08: Adjacent self-hosted and open cloud-gaming / streaming platforms (as of 2026-10-03)

> Background research from planning ([index](README.md)). What Cha Portal actually ported is in [PROVENANCE.md](../PROVENANCE.md).

Researcher slice 08: projects that overlap with Cha Portal as a whole, especially new entrants. Other slices cover Wolf/GoW internals, PyroWave and codecs, Moonlight/Sunshine/Apollo/Vibepollo, Kasm/Selkies/Neko/Guacamole, browser transport APIs, Linux container graphics and control-plane patterns. Those come up here only where an adjacent project depends on them.

How this was gathered: shallow clones (some blobless, for history) into `scratchpad/repos/08/`, reading the source and the in-repo design docs, plus GitHub API and search, WebSearch and WebFetch. The GitHub REST API was rate-limited for much of the session, and the session's WebSearch budget ran out partway through. Anything I could not confirm against a primary source is tagged **(unverified)**. Star counts come from the GitHub search API on 2026-10-03.

Decisions from the user that shape the verdicts: Cha Portal will be copyleft (AGPL or GPL), so forking GPL code is acceptable. It targets homelabs and small groups first, and treats LAN and WAN as equally important.

---

## TL;DR

1. **Nestri has rewritten itself and is no longer a drop-in reference.** As of 2026-08/09 it went from "containers + GStreamer + WebRTC + Go/libp2p relay + browser client (AGPL)" to "Firecracker-derived micro-VMs (nesbox) + virtio-gpu native context / virtio-nvgpu + an in-process Vulkan capture layer + iroh/QUIC + a native client (Apache-2.0)". The control plane is open. The client, the edge and the box lifecycle are not open yet, and some will stay closed (open-core). Its written transport and capture lessons are the most valuable public material I found for Cha Portal. It is also porting PyroWave over QUIC datagrams right now (design specs dated 2026-09-27 to 2026-10-01).
2. **The biggest new entrant is Punktfunk** (Rust, MIT/Apache, primary repo on git.unom.io, mirrored to GitHub). It has a native `punktfunk/1` protocol (QUIC control, then UDP media with GF(2^16) Leopard-RS FEC and AES-GCM), PyroWave, a GameStream/Moonlight compatibility plane in the same process, one virtual display per client, and a browser client over WebTransport + WebCodecs built from the same Rust core compiled to wasm. It does not run PyroWave in the browser: "a browser has no device to run it on."
3. **Polaris + Nova** (GPL-3.0, Sunshine/Apollo lineage, Linux-only host) has shipped PyroWave (v1.4.13, 2026-09-26). It also has "Spaces" (a per-user Steam, Heroic or Lutris runtime in Docker), an experimental WebTransport + WebCodecs browser stream for the LAN, and a "Doctor" and "Mission Control" observability UX.
4. **Valve shipped PyroWave in the Steam Remote Play beta on 2026-09-21** (100 to 500 Mbit/s, wired gigabit, 4:4:4 and HDR; a Linux host needs the SteamRT3 client), after AV1 and HDR streaming earlier in 2026. The Steam Frame (released 2026-09-18) does encoder-side foveated streaming. PyroWave is now a mainstream LAN codec, so "first-class PyroWave" is table stakes for the LAN tier, not a moat.
5. **The browser PyroWave decode gap is real and has a lead.** Nobody ships PyroWave decode in a browser: Punktfunk explicitly disables it and Nestri, Polaris and Valve are all native-only. But a bitstream-compatible **WebGPU (WGSL) port** of the PyroWave encoder and decoder exists in `imbcmdth/pyrowave` on the `webgpu` branch, dated 2026-09-26. It needs WebGPU `subgroups`, decodes 1080p in about 0.5 ms wall-clock on wgpu-native, and has notes for a WASM build. That is the clearest unique wedge for Cha Portal.
6. **The Moonlight protocol now exists as a Sans-IO Rust crate** (`MrCreativ3001/moonlight-common-rust`, GPL-3.0, wasm-capable). With the moonlight-web projects, this makes "external Sunshine/Wolf/Vibepollo endpoints in the browser" a gateway problem that is largely solved. Reuse it rather than rebuild it.
7. **Positioning:** no one combines (a) a multi-node, Kasm-style environment orchestrator, (b) first-class Steam containers, (c) browser-first delivery with WebTransport/WebCodecs plus PyroWave over WebGPU, and (d) federation of external Moonlight-protocol hosts behind one dashboard. Each piece exists somewhere. The integration and the browser PyroWave path are the gap.

---

## Overview table

| Project | What | Transport / codec | License | Activity (2026-10-03) | Verdict for Cha Portal |
|---|---|---|---|---|---|
| **Nestri** (nestrilabs/nestri) | Open-core cloud gaming. Micro-VM boxes, native client | iroh 1.x QUIC (noq quinn fork): datagrams + streams. Vulkan Video H.264/H.265/AV1, PyroWave in progress | Apache-2.0 since 2026-08-26. Pre-2026-08 tree was AGPL-3.0 + commercial `cloud/` | 1,752★, pushed today, mid-rewrite | **Inspiration + selective reuse** (nesprotocol PyroWave framing, nescope, nescapture ideas). Watch nesbox/virtio-nvgpu |
| **nesbox / virtio-nvgpu** | Micro-VM with a shared GPU (NVIDIA driver-ABI forwarding; DRM native context for AMD/Intel) | n/a | Apache-2.0 (guest kmod GPL-2.0) | 46★ / 402★, pushed today | **Future option** for strong isolation. Not for v1 |
| **Punktfunk** | Native low-latency host + clients, Moonlight compat, web client | QUIC control + UDP FEC/AES-GCM. WebTransport browser plane. HEVC/AV1/H.264/PyroWave | MIT OR Apache-2.0 | GitHub mirror 12★ (primary on git.unom.io). v0.42.0, very active | **Protocol-compatible / depend-on candidate** (punktfunk-core as an optional transport). Heavy inspiration |
| **Polaris + Nova** (papi-ux) | Linux host (Sunshine/Apollo lineage) + Android/Deck client | GameStream. Experimental WebTransport+WebCodecs browser stream. PyroWave | GPL-3.0 | 395★ / 95★, v1.4.13 (2026-09-26) | **External endpoint (protocol-compatible)**. Inspiration for Spaces/Doctor UX |
| **moonlight-web-stream** (MrCreativ3001) | Web gateway: Sunshine to browser | WebRTC or WebSocket+WebCodecs. H.264/H.265 | GPL-3.0 | 710★, active (v3 prerelease) | **Fork / depend-on** for the external-endpoint gateway |
| **moonlight-common-rust** | Sans-IO Moonlight protocol in Rust, wasm-capable | GameStream | GPL-3.0-or-later | 10★, pushed today | **Depend-on** (GPL is fine) |
| **linckosz/moonlight-web** | C++/Qt host + GameStream relay to browser | WebRTC (DataChannels + RTP), WSS fallback | GPL-3.0 | 96★, pushed today | Inspiration (session sharing, Wolf auto-pairing) |
| **CloudyPad** | CLI + app that provisions Wolf/Sunshine on clouds or SSH hosts | Moonlight | AGPL-3.0 (README says GPLv3; LICENSE.txt is AGPLv3) | 979★, last release v0.45.2 2026-05-10 | Inspiration (provisioning, autostop, pairing automation) |
| **Unreal Pixel Streaming 2** | UE plugin + signalling/SFU/frontend | WebRTC (EpicRtc layer), mediasoup SFU, simulcast | MIT (infra) | 546★ (EpicGames mirror), UE5.6–5.8 branches | Inspiration (input protocol, SFU, quality ownership) |
| **Valve Steam Remote Play / Link / Frame** | Proprietary | Proprietary. H.264/HEVC/AV1, PyroWave beta, foveated | Proprietary | PyroWave beta 2026-09-21 | Inspiration. Possibly "let Steam stream" as an env mode |
| **Parsec** | Proprietary remote desktop | BUD (UDP+DTLS, custom CC). Web client via WebRTC | Proprietary | v150-105a 2026-09-28 | Inspiration (web-client limits) |
| **Shadow** | Proprietary full cloud Windows PC | Proprietary **(unverified details)** | Proprietary | New Neo Lite tier 2026-07 | Business lesson only |
| **Rainway** | Defunct | WebRTC-ish browser streaming **(unverified)** | Proprietary | Consumer shutdown 2022-10-31 | Cautionary tale |
| **Steam-Headless** (Josh5) | Steam in Docker, noVNC + Sunshine | VNC / GameStream | GPL-2.0 (only vs or-later unverified) | 4,834★, active | Inspiration (Steam container pitfalls) |
| **linuxserver/docker-steam** | Steam in Docker via Selkies | Selkies (WebRTC/WebSocket) | GPL-3.0 | 35★, active | Inspiration (bwrap/seccomp/gamepad pitfalls) |
| **cloud-game (CloudRetro)** | Retro cloud gaming | pion WebRTC, VP8/H.264 | Apache-2.0 | 2,480★, active | Inspiration (coordinator/worker + browser latency-ping placement) |
| **CrossDesk** | RustDesk-like remote desktop with a web client | MiniRTC (WebRTC), H.264/AV1 | GPL-3.0 | 4,365★ | Low relevance |
| **browserpane / beam / gawk** | Tiny WebTransport/WebCodecs remote-desktop experiments | WebTransport + WebCodecs | AGPL / MIT / Apache | 11★ / 4★ / 1★ | Signal only |
| **imbcmdth/pyrowave `webgpu`** | WGSL port of the PyroWave encoder/decoder | n/a | MIT | fork, 2026-09-26 | **Depend-on / fork** for browser PyroWave decode |

---

## 1. Nestri (nestrilabs/nestri): deep dive

Sources: repo at `dev` `ef32d5f` (2026-10-03) and history clone. https://github.com/nestrilabs/nestri, https://github.com/nestrilabs/nesbox, https://github.com/nestrilabs/virtio-nvgpu, https://linuxiac.com/virtio-nvgpu-brings-shared-nvidia-gpu-access-to-linux-kvm-guests/ (2026-09-26).

### 1.1 Status, licence, governance (2026-10)

- GitHub description: "[Experimental] Run multiple gaming sessions on a single GPU". 1,752★, branches `dev`, `prod`, `feat/nvgpu-vklayer`, `feat/team-and-box-reads`. There are no streaming releases. The only released artifact is `nesdoctor` (v0.1.0 to v0.3.0, 2026-09).
- README (2026-10): "This repository is mid-rewrite, and the documentation is behind the code." The only thing a stranger can run is `nesdoctor`, a host-readiness checker that measures latency under load (bufferbloat) and reads EDID and decode capabilities. The box lifecycle, storage, the edge and the client are "not here yet". "Some of that will open as it is written; some is deliberately closed": open if it handles user data, closed if "it decides our capacity, which is the part we sell."
- **Licence history:** until 2026-08-06 the repo was AGPLv3, except a commercial `cloud/` directory (LICENSE header in the history clone). On 2026-08-06 a "Sync to OSS repo" commit deleted about 79k lines (the old relay, server, web and MoQ stack). The repo had no licence until **2026-08-26, when Apache-2.0 was added** for "this tier". The old AGPL code is still in git history, and its licence is compatible with an AGPL Cha Portal.
- Guest components (nescope, nescapture, neswire, neshub, nesprotocol) were "opened" on 2026-08-26 as tree imports without history. The PyroWave crate `nespyro` is MIT, the same as upstream.
- The repo has a `CLAUDE.md` with strict "public repo" rules. The team develops with AI agents (`docs/superpowers/plans|specs`).

### 1.2 Transport history (what they tried, what they settled on)

Reconstructed from commit history (blobless clone):

| Period | Media transport | Signalling / relay | Browser client | Evidence |
|---|---|---|---|---|
| 2024-03 → ~2024-11 | **Media over QUIC** via kixelated's moq-rs (`moq-server`, "warp" `moq-pub`), WebTransport | moq relay (Hetzner relay from 2024-09) | `packages/moq` (moq-js) + MoQ checker page | commits `7d5d8be` (relay docker), `4897a20` (warp), `c0f5735` (input over MoQ, 2024-05), moq-server bumps to 2024-06 |
| 2024-11/12 | **WebRTC** via GStreamer `webrtcsink` with a custom signaller | WebSocket signalling | Browser WebRTC client. Cross-origin isolation (needed for moq-js `SharedArrayBuffer`) removed 2024-11-27 | `339779f`/`5eb21ee` "Remove cross-origin isolation", `379db1c` "Add streaming support", `b6196b1` "Custom gst webrtc signaller" (2024-12-08) |
| 2025-03 → 2025-12 | WebRTC (pion in a Go relay, an SFU-like "room" fan-out) | **libp2p** (QUIC, TCP, WebTransport transports; gossipsub room-state sync; mDNS) replaced WebSocket signalling 2025-06-06. Relay "Deprecate websocket in favor of quic-v1" 2025-12-02 | `packages/play-standalone` (Astro) + `packages/input` (TS) | `4985380` port muxing/TLS, `6e82eff` libp2p, `549a98b`. Playout-delay experiments (`f9b4269` "try high playout delay", reverted `0f71b02`) |
| 2026-08 → now | **iroh 1.x QUIC** (`noq`/`noq-proto`, a quinn fork): delta video + audio as **datagrams**, keyframes as short-lived uni streams, cursor/stats/input on their own streams. One connection out of the box ("ticket" = endpoint addr + stream name) | iroh relay/hole-punching (iroh's own relays) | **Native client** (`nesvideo`/`nesrecon`, in a non-public repo "nescore", per design docs). No browser client in the open tree | `apps/neshub`, `docs/media-transport.md`, `docs/superpowers/specs/2026-10-01-*` |

Why they moved: the commits do not document why MoQ was abandoned. Visible facts: moq-js required `SharedArrayBuffer` and therefore COOP/COEP cross-origin isolation, which was removed when WebRTC landed. WebRTC then needed a separate signalling/relay system that they rebuilt twice (WebSocket, then libp2p). The final rewrite drops the browser and WebRTC entirely in favour of one QUIC connection that they fully control, with a native Vulkan client. **My inference (unverified):** they wanted transport-level control (datagram vs stream choice, queue measurement, custom codecs like PyroWave) that WebRTC does not expose, and browser decode limits blocked their codec plans.

### 1.3 Current architecture (guest side, open)

```
micro-VM ("box", nesbox)                                 client (native, closed)
┌───────────────────────────────────────────────────┐
│ nesinit (PID 1)  ──vsock JSON lifecycle──► host agent (closed)
│ nescope  (headless Smithay Wayland compositor +    │
│           XWayland, gamescope WSI protocol, HDR)   │
│ game ── Vulkan ──► nescapture (implicit Vulkan     │
│                     layer: capture at vkQueuePresent,
│                     GPU blit → ring → compute CSC → │
│                     Vulkan Video encode, same VkDevice)
│ neswire (audio/Opus)   nesgamepad (uhid DS4/DualSense)
│ neshub: Unix sockets in ──► one iroh QUIC endpoint ──────────► nesvideo / nesrecon
└───────────────────────────────────────────────────┘
```

- **nescapture** (Vulkan implicit layer): captures inside the game process at `vkQueuePresentKHR`, blits into a ring of images on the game's own device, converts colour with a compute shader (or `VK_VALVE_video_encode_rgb_conversion` where available), and encodes with Vulkan Video (H.264/H.265/AV1, 4:2:0/4:4:4, 8/10-bit) via `pixelforge`. Runtime reconfiguration (bitrate, codec, IDR) goes over a command socket. Intra-refresh is supported with a QP delta for the refresh band. HUD detection uses SPIR-V shader hashes, for HUD-aware encoding. It falls back to its own device plus CPU readback when the game's device cannot host the encoder. Source: `apps/nescapture/README.md`.
- **nescope**: a headless compositor that never allocates GBM buffers (the DMA-BUF global exists only so XWayland can init DRI3), uses timer-driven frame callbacks, acts as a process subreaper, and works in two shapes (wrap one command, or plain compositor). It is a "lighter answer to the same problem gamescope solves". Source: `apps/nescope/README.md`.
- **neshub**: the listener on every Unix socket, so producers dial in and "it removes the startup ordering problem entirely". It knows nothing about the payload ("The same neshub binary serves a game, a desktop, or anything else"). It muxes everything into one iroh endpoint and serves the ticket over a socket to nesinit.
- **nesinit**: PID 1 in the VM. It reaps, does ordered shutdown, starts a fixed service table, and holds a vsock control channel (newline JSON, protocol versioned, `ready` and `initialized` are distinct states, every launch has an id). "A box outlives what runs in it": one box can launch several times.
- **nesgamepad**: rebuilds real controllers through `/dev/uhid` (DS4 rebuilt from the real report descriptor, so Proton exposes hidraw). No controller devices exist until one connects, because "some games stop listening to the keyboard and mouse as soon as one exists."

### 1.4 GPU handling

- **Old (2024–2025):** containers (Arch-based "runner" images), runtime installation of an NVIDIA userspace driver matching the host (it downloads `NVIDIA-Linux-x86_64-<hostver>.run` and installs it with `--no-kernel-module`-style flags), VA-API/QSV/NVENC GStreamer pipelines, gamescope nested on `waylanddisplaysrc` (GoW's gst-wayland-display), and supervisord for dbus/pipewire/wireplumber/vimputti.
- **New (2026):** **nesbox**, a micro-VM VMM that "started as a fork of Firecracker", was rewritten on rust-vmm with virtio over PCIe because "Firecracker's virtio-MMIO devices can't give a GPU the large memory windows it needs", and borrows GPU work from libkrun. NVIDIA uses **virtio-nvgpu**: the guest runs NVIDIA's unmodified userspace (Vulkan, NVENC, CUDA), and a guest kernel module forwards `/dev/nvidia*` ioctls and mmaps over a virtqueue to a per-VM, seccomp+Landlock-sandboxed backend. Memory maps are shared, so "the backend handled one message per 59 frames". Their claim is "within 2% of bare metal", with 12 VMs on an RTX 3060 each encoding 720p60 H.264. NVENC's GeForce 12-session cap is sidestepped by encoding with Vulkan Video (16 at once worked). AMD/Intel use DRM native context (patched virglrenderer, Mesa `-Dintel-virtio-experimental`, `-Damdgpu-virtio`). **Trust boundary:** the host NVIDIA driver, "roughly the same trust boundary Docker containers have". Linux guests only, driver ≥ 535.129.03, "tested on only two cards". Sources: nesbox README, virtio-nvgpu README/ARCHITECTURE.md.
- **"Why a VM":** "A container shares the host kernel, which makes strong isolation hard and a GPU harder." It does not run in Docker, because it needs `/dev/kvm` on the host.

### 1.5 Steam in Nestri

- Old runner: `pacman -S steam`, a pre-seeded `config.vdf` (Proton Experimental), launched as `steam -tenfoot -cef-force-gpu` under `gamescope --backend wayland -g -f --rt ... -e` (Steam integration), with MangoHud configs. Variant images existed for Heroic and Minecraft.
- New: guest components are payload-agnostic by policy ("no code here may branch on which one it is"). nescope comments name Steam and Proton as the motivating cases for workarounds. The control plane links a Steam account (`apps/api` "record which host holds a Steam…", Steam login in OpenAuth). The real Steam box image is not public **(unverified beyond commit titles)**.

### 1.6 PyroWave in Nestri (live work, 2026-09-27 → 2026-10-01)

Four specs, each with rationale:
1. `nespyro`: PyroWave ported to a Rust crate on `ash`, with shaders ported to **Slang** and compiled at build time. The bitstream is frozen at upstream `89f7e47` (2026-09-25), with a format version carried in the protocol "since the bitstream itself has none and is marked draft". Desktop GPUs only, Vulkan 1.3. Notes on a mobile path exist.
2. Transport: opt-in only (`best()` never picks it). **No rate controller**: a static rate, default 300 Mbit/s. **Critical packets are sent twice (first and last) as datagrams, with no reliable stream.** Packets are cut on PyroWave packet boundaries to fit `max_datagram_size()`. The client collector releases a frame when it is complete, or 2 ms after a newer frame starts, or 20 ms after its last datagram: "Not a deadline from the first datagram … the end-to-end burst test found this." A hub-side skip guard keeps the queue at about 2 frames.
3. nescapture: shared-device only ("a CPU round trip of a frame defeats the codec's reason to exist"). It is decoupled from Vulkan Video, so "a GPU with no video encode can still stream PyroWave".
4. Client decode (in closed nescore): nespyro decodes on the client's shared device. Planes are always R16. Deblocking is off. On Intel ANV's single queue they had to share the render queue under a lock. Capabilities are sent twice (once at connect, once after the decode device exists).

Relevance: this is a worked, reviewed design for **PyroWave over QUIC datagrams with partial-frame delivery**. Cha Portal can mirror it on WebTransport datagrams. The PyroWave researcher covers the codec itself.

### 1.7 Transport lessons (`docs/media-transport.md`), the most quotable material for Cha Portal

Read against iroh 1.1 / noq-proto 1.3. Paraphrased rules:
- "**A path is the unit of congestion, not a stream and not a connection.**" Moving keyframes to streams changes reliability, not isolation. A second connection "does not create bandwidth".
- quinn-family `populate_packet` writes **DATAGRAM frames before STREAM frames**. Delta frames cannot be starved by keyframes, but keyframes *can* be starved by deltas. That produced field logs of "26 keyframe fallbacks, 19 IDR requests". Their fix is a `ResyncGate`: do not send frames that depend on a keyframe the receiver doesn't have, bound that suppression in time, and reuse the sequence number of a frame you skipped on purpose so the receiver doesn't count it as loss.
- `send_datagram` **evicts the oldest queued datagram and returns Ok**, so it gives no backpressure signal. "encoder perfectly healthy, zero frames dropped" appeared beside a client receiving almost nothing.
- QUIC already paces, so **do not add app-level pacing**. Produce less instead.
- **Loss is a lagging indicator, and the queue is the leading one.** On a 1000-mile link there was zero loss and a flat 180 ms RTT, yet the picture ran up to 8 s behind, because a 4 MiB datagram buffer at a few Mbit/s is seconds of queue. Compute `backlog_ms = (BUF - datagram_send_buffer_space()) * 8 / drain_kbps`. A loss-driven controller sawtooths.
- "**Trust the receiver over the sender.**"
- Read dependency internals and cite paths, because docs describe intent. The same applies to Vulkan Video: rate control is session state, `gopFrameCount=0` means "implementation chooses" (not infinite), and a hard-coded QP ceiling silently made low bitrates unreachable.

### 1.8 Capture-path lessons (`docs/superpowers/plans/2026-09-15-nescapture-cpu-cost.md`)

Triggered by Cyberpunk 2077 on 4 vCPU holding about 45 fps against a 60 fps target:
- "**The present hook must never block.**" A frame that cannot be captured is dropped, never waited for. The slot ring is the backpressure.
- "**No dropped encoded frames.** … Backpressure belongs before the encoder, never after it", because a dropped encoded frame breaks the reference chain.
- The capture ring was in host-coherent, linear memory, so every frame crossed the bus twice. They moved it to device-local tiled memory with DRM modifiers. They record the blit once rather than per frame, and merge the capture and encode threads.
- Every task is gated by a once-a-second stats line (`present/admitted/encoded/starved/dropped/capture ms/encode ms`). "A task that moves none of these numbers should be reverted."

### 1.9 Control plane (open, TypeScript)

- Hono + zod-openapi API on Bun or Workers (`apps/api`) and a self-hosted OpenAuth issuer (`apps/auth`), with Postgres as state (the API also lists a `redis` dependency). Both run on Cloudflare Workers *or* as plain containers ("no infrastructure-as-code"; the IaC layer was dropped 2026-09-05).
- Auth: email-code only, plus the **RFC 8628 device authorization grant** "for programs with no browser". Issuer state lives in Postgres because codes and grants must transition exactly once.
- **Host registration** (`packages/core/src/machine`): "A box trades an owner-supplied token for an assigned id and a secret". They deliberately rejected deriving an id from `/etc/machine-id` because "self-hosted boxes mean the operator is not automatically trusted". The heartbeat cadence is returned by the server (30 s placeholder). A host is offline after **3** missed beats ("a single missed beat is a lost packet … would make placement flap"). The heartbeat also carries the host's current iroh endpoint id.
- Domain modules: `organisation`, `team`, `machine`, `box`, `session`, `game`, `steam`, `pairing-code`, `billing` (burn windows). "A box is a row, a session is …", "a box gets one run" (commit titles).

### 1.10 Pitfalls seen in issues and history

- Most-commented issues (https://github.com/nestrilabs/nestri/issues): "no command specified to run the game", "Future compatibility with non-CUDA graphics cards", Podman support, "How to run nestri?", controller support, "Cannot find .X11-unix directory", pointer lock and non-Chromium browser problems. These match the generic failure modes of a container-plus-browser stack.
- Three transport rewrites in about two years (MoQ, then WebRTC + WebSocket, then WebRTC + libp2p, then iroh QUIC native). Each one reset the client.
- Open-core drift: the useful-to-users parts (client, box lifecycle) are not open. Self-hosters cannot run Nestri end to end today.

### 1.11 Steal for Cha Portal

- The **media-transport rules** in 1.7: build them into the Cha transport from day one, especially queue-based congestion signals, ResyncGate and receiver-side truth.
- The **PyroWave-over-datagrams wire design** (critical packets duplicated, partial-frame collector with GRACE/IDLE release, a skip guard on backlog, a format version in negotiation, opt-in only).
- The **nescapture approach**: an in-process Vulkan layer encoding on the game's own device. It is an alternative to compositor capture when we control the game container. Gate it behind an env var, because implicit layers load into every process.
- **neshub's "listener on every socket"** pattern for in-environment sidecars, plus a payload-agnostic in-environment agent.
- **nesinit's** lifecycle protocol (versioned handshake first; `ready` ≠ `initialized`; launch ids; "A box outlives what runs in it"). It maps directly to Cha environment agents.
- **Machine registration and heartbeat semantics** in 1.9, and RFC 8628 device grant for headless node enrolment.
- **nesdoctor**: a "can this machine host / what does my link really do under load" tool. Cha should ship the same for nodes and clients: bufferbloat under load, EDID/HDR/refresh, decode capabilities.
- **nesgamepad** uhid DS4/DualSense reconstruction, and no pad device until a pad connects.

### 1.12 Verdict

**Inspiration-only for the overall architecture. Selectively fork or borrow open Apache-2.0 pieces** (nesprotocol PyroWave framing, nescope, nescapture, nesgamepad), all GPL/AGPL-compatible. Do **not** depend on Nestri as a platform: the client and lifecycle are closed or unbuilt, and interfaces "will change". Track **nesbox + virtio-nvgpu** as a v2+ "isolated environment" backend (micro-VM per environment, many per GPU), which no other self-hosted project has. It is early, NVIDIA-ABI-fragile, and has a single-host-driver trust boundary.

---

## 2. Punktfunk (punktfunk/punktfunk, primary at git.unom.io/unom/punktfunk): new entrant

Sources: shallow clone at `21:11 2026-10-03`, `README.md`, `docs-site/content/docs/(reference)/how-it-works.md`, `support-matrix.md`, `developers/architecture.md`, `crates/punktfunk-host/src/webtransport.rs`, and https://github.com/punktfunk/client-web.

- **What:** "Low-latency desktop and game streaming with first-class Linux and Windows hosts." Every client gets its **own virtual display at its native mode** (KWin, Mutter, gamescope, Hyprland, sway virtual outputs; its own signed IddCx driver on Windows that also shows UAC and lock screens). It has a game library with plugins (Bun runner, bwrap-sandboxed plugins, generated OpenAPI SDK), a web console (TanStack Start/Bun), and native clients for Apple, Android, Linux, Windows, Steam Deck (Decky) and LG webOS, plus a **browser client**.
- **Protocols:** `punktfunk/1` has a QUIC control channel (SPAKE2 PIN pairing, then pinned identity, mode changes, clock sync, adaptive bitrate, clipboard). The **data plane is UDP with GF(2^16) Leopard-RS FEC + AES-GCM** (ChaCha20-Poly1305 for clients without AES hardware), which "breaks the ~1 Gbps FEC wall". Input goes as QUIC datagrams. The **GameStream** plane is opt-in, "trusted-LAN only" (weak pairing crypto), and lives in the same process. The **WebTransport** plane is opt-in (`--webtransport`): "Datagrams carry media, one bidirectional stream carries control — the same split the native plane uses".
- **Codecs/encode:** zero-copy dmabuf into CUDA/Vulkan, then NVENC. Vulkan Video for HEVC/AV1 and VAAPI for H.264 on AMD/Intel. **PyroWave runs in a separate `punktfunk-encode-worker` process at raised GPU priority.** Decode is a ladder: Vulkan Video, then VAAPI/DXVA, then V4L2, then openh264/rav1d. PyroWave decodes in Vulkan compute (Metal on Apple).
- **Browser client** (`punktfunk/client-web`, MIT/Apache, 2★): the Rust `punktfunk-core` (handshake, FEC, decrypt, reassembly, SPAKE2) is **compiled to wasm**, so "The protocol is not reimplemented anywhere here". It decodes with WebCodecs into WebGL2, or WebGPU ("the only route that carries HDR"). "A decoded frame goes straight into a texture and never enters the wasm heap." Audio is Opus with RED copies, played from an AudioWorklet ring. It was verified on Safari 27 and Firefox 156. Known gaps (README): no mic uplink, no latency/loss stats, codec pinned, **no automatic bitrate in the browser**, an 8.2 MB wasm payload (3.5 MB gzipped), and **PyroWave disabled** (`opts.pyrowave_ok = false; // A Vulkan compute codec: a browser has no device to run it on.`).
- **WebTransport certificate handling (worth copying verbatim as a design):** the plane mints its own ECDSA P-256 certificate, valid **13 days** (the `serverCertificateHashes` ceiling is 14), keeps it only in memory, and rotates it a day early. The hash is published on an unauthenticated route. **The long-lived host identity signs the throwaway certificate hash**, so a paired browser can verify the chain. PAKE pairing over the control stream is what actually authenticates the peer.
- **Status:** v0.42.0 (wire protocol 2, C ABI 42). The CHANGELOG is 530 KB. The GitHub mirror has only 12★, so community size is unclear (Discord and r/Punktfunk exist). Licence MIT OR Apache-2.0.
- **Standout ideas:** two protocols in one process. One virtual display per client. A management API with two credential lanes (paired-client cert for read-only LAN, a loopback-only bearer token for admin). The web console proxies so the token never reaches the browser. PyroWave runs in a priority-boosted worker process. Explicit version tables per release (wire, ABI, driver protocol).
- **Pitfalls they document:** browsers can't do PyroWave. `serverCertificateHashes` forces 13-day rotation. An emscripten default stack of 64 KB "overflows just by being constructed", and the symptom looks like a corrupt module. Switch Pro over Bluetooth is "not read correctly by browsers" (also seen in moonlight-web).
- **Steal:** the WebTransport cert-rotation-plus-signed-hash scheme; the protocol core shared between native and wasm; the decode ladder; the FEC design for high-bitrate LAN; one virtual display per client; the PyroWave worker at raised priority; the two-lane management-API auth.
- **Verdict:** **protocol-compatible / optional depend-on.** Cha Portal could embed `punktfunk-core` (MIT/Apache, compatible with AGPL) as a high-performance native and wasm transport, and treat Punktfunk hosts as an external endpoint type. It is also the closest architectural peer for the "thin native client" half of feature 6. Risk: a single-vendor project (unom) with a young GitHub presence, and an evolving wire protocol (v2).

---

## 3. Polaris + Nova (papi-ux): new entrant

Sources: https://github.com/papi-ux/polaris (clone at 2026-10-03), `docs/pyrowave.md`, `docs/spaces.md`, `docs/configuration.md`, `browser_stream_helper/`.

- **What:** "Linux game streaming that answers to you." A Linux-only host in the Apollo/Sunshine lineage ("stays protocol-compatible with the wider Moonlight ecosystem") with its own client **Nova** (Android, plus a Steam Deck alpha). 395★ / 95★, GPL-3.0, v1.4.13 (2026-09-26), very active.
- **Session modes:** Private Stream (a session-owned compositor), Gamescope Stream, Host Virtual Display, Headless Dongle, Mirror Desktop. Unavailable modes "fail closed". It streams straight from Steam Game Mode on a Steam Deck.
- **Spaces (preview):** a per-player Steam, Heroic or Lutris runtime **in Docker**, chosen per device. The runtime image is published by digest to `ghcr.io/papi-ux/polaris-worker-steam`. "The NVIDIA runtime borrows this PC's own driver files", so driver updates don't strand it. Host Setup runs 8 checks, and privileged fixes require approval at the physical PC. Limit: **one active Space at a time**. AMD/Intel runtime untested.
- **PyroWave:** shipped in the v1.4.13 release, with clients Nova for Android beta and Nova Linux Flatpak (SDR 4:2:0). "Wired gigabit ethernet on every link". A KDE host in HDR with KMS capture cannot stream PyroWave.
- **Browser stream:** `browser_streaming` is "Experimental zero-install LAN browser access over WebTransport and WebCodecs", implemented as a Go helper (`quic-go/webtransport-go`). Their own recommendation is to use it for touch, desktop and slower games, and Nova or Moonlight for controller-first play.
- **UX ideas:** "Mission Control" shows the resolved capture path, encoder, viewers, latency and loss. "Doctor … can make one reversible same-stream bitrate change, verify … and restore the previous target when verification fails". "Refusals say why" (error codes with a fix). Deterministic launch envelopes ("Auto, Quality, High FPS, Stability resolve into one app- and topology-bound envelope"). "Anyone can watch": a second device watches at its own resolution.
- **Steal:** Doctor's bounded, reversible auto-tuning; "refusals say why"; per-device Spaces bound to a container runtime pinned by digest; reusing the host's NVIDIA driver files inside containers; fail-closed mode availability.
- **Verdict:** **external endpoint (protocol-compatible via GameStream)**, plus inspiration. GPL-3.0 code is forkable under Cha's copyleft, but it is a C++ host app, not a library. Integrate as a "Moonlight-protocol host" endpoint type.

---

## 4. The Moonlight-in-the-browser family (gateway to external endpoints)

The Moonlight/Sunshine researcher owns the protocol. These projects matter here because they *are* the external-endpoint gateway Cha Portal needs.

### 4.1 MrCreativ3001/moonlight-web-stream (GPL-3.0, 710★, `3.0.0-prerelease.7`)
- A Rust (actix-web) server that is a full Moonlight client and **re-emits the stream to browsers** over WebRTC (webrtc-rs 0.21), or a **WebSocket + WebCodecs** fallback for restrictive networks. That fallback needs a secure context, and without one it falls back to an older, higher-latency playback API. It has users/roles/admin, TURN and port-range configuration, and NAT 1:1 configuration.
- The v3 change log replaces openssl with rustls and **moonlight-common-c with moonlight-common-rust**.
- The browser pipeline (`web/stream/`) is a pluggable chain of pipes: WebRTC track, WebCodecs `VideoDecoder`, MSE (`media_source_decoder.ts`), **openh264 wasm**, worker pipes, canvas/OffscreenCanvas, AudioWorklet/Opus decoder. It sets `receiver.jitterBufferTarget = 0` and `playoutDelayHint = 0`.
- Forks: `spacedouut/moonlight-web-stream-simplified` (GPL-3.0, "now supporting WebTransport", WIP).

### 4.2 MrCreativ3001/moonlight-common-rust (GPL-3.0-or-later, 10★, pushed 2026-10-03)
- "A Rust implementation of the Moonlight game streaming protocol built around a **Sans-IO** architecture". It can "Compile to WebAssembly and run in the browser, where networking is provided externally (e.g. WebRTC, WebTransport, **Direct Sockets in IWA's**)".
- Supported hosts: Sunshine, **Wolf**, Apollo, Foundation Sunshine (not Nvidia GameStream). Codecs H.264/H.265 (**no AV1 yet**). No video encryption, no RFI/LTR. It also has bindings to moonlight-common-c for interop.

### 4.3 linckosz/moonlight-web (GPL-3.0, 96★, very active)
- A C++/Qt server. It has its own native capture and encode (NVENC/AMF/QSV/VA-API/VideoToolbox), which it ships "capture → encode (zero-copy) → SCTP/DTLS → browser" (DataChannels, "No loopback hop, RTSP, RTP or FEC"). It is also a GameStream client relaying other hosts (embedded moonlight-common-c). WebCodecs + WebGPU/canvas, AudioWorklet.
- **It auto-pairs Wolf by posting the PIN through Wolf's `/api/v1`.** It provisions MultiSeat seats via API.
- **Session sharing:** up to 3 guests, each with its own stream, resolution and bitrate, with levels Viewer / Gamer (pad) / Desktop (KB+M) / Full. Link and PIN are sent separately.
- An opt-in internet entry relay at `stream.moonlightweb.top/<id>` (a hosted rendezvous).

### Steal / verdict (4.x)
- **Depend on moonlight-common-rust** (or fork moonlight-web-stream's server) for Cha Portal's "external Moonlight host" endpoint type. GPL-3.0-or-later is compatible with Cha's AGPL/GPL. Two deployment shapes are possible: (a) the node runs the gateway beside the external host (LAN-side, re-emits over Cha's WebRTC/WebTransport), or (b) **wasm in the browser with an IWA + Direct Sockets** for a zero-gateway LAN client (Chromium-only, future; the browser-transport researcher should confirm IWA availability).
- Steal linckosz's permissioned session sharing and Wolf auto-pairing via API, and moonlight-web-stream's pluggable decode pipeline with capability probing (WebCodecs, then MSE, then wasm openh264).

---

## 5. CloudyPad (PierreBeucher/cloudypad)

Sources: clone (`v0.45.2`, 2026-05-10), `docs/src/contributing/architecture.md`, `docs/src/usage/*`, `ansible/roles/wolf/*`, https://github.com/PierreBeucher/cloudypad/issues.

- **What:** a TypeScript CLI (and since 2025-07 a hosted "Cloudy Pad App") that **provisions** a GPU VM on AWS, Azure, GCP, Scaleway, Paperspace or Linode, or any machine over **SSH**, then **configures** it with Ansible (NVIDIA driver, container toolkit, then Sunshine *or* Wolf in Docker), then **pairs** Moonlight. 979★. Licence: GitHub and LICENSE.txt say **AGPL-3.0**, README says GPL-3.0, and a commercial licence is available.
- **Architecture:** `InstanceInitializer`, then `InstanceManager` = Provisioner (**Pulumi** per provider) + Configurator (Ansible) + `MoonlightPairer` (Sunshine or Wolf) + Runner (start/stop). State is a Zod-validated YAML at `~/.cloudypad/instance/<name>/state.yml` with provision and configuration inputs and outputs.
- **Lessons visible in code and docs:**
  - **Pairing automation:** it sends the Moonlight PIN to the Sunshine API over SSH + curl, with 60 retries every 2 s. Wolf has its own pairer. Since v0.45.0 it uses the **hostname rather than the IP for pairing**, so Moonlight config survives reboots with DNS.
  - **Auto-stop:** inactivity means no traffic on the Moonlight control port 47999, no significant download traffic, and no Ansible running. This avoids shutting down mid-download or mid-configure.
  - **Cost features:** spot instances, cost alerts, egress throttling, and the roadmap items "snapshot OS disk on stop" and "move data disk to S3 on stop".
  - The Wolf deployment pins `wolf:stable` and app images by **digest**, uses an NVIDIA **driver volume** (`NVIDIA_DRIVER_VOLUME_NAME`), `network_mode: host`, `/dev/uinput`, and the cgroup rule `c 13:* rmw`, and has an optional "preheat" compose to pull all app images in parallel.
  - The Sunshine image is Ubuntu Noble with **Xorg + dummy driver**, Steam, Lutris and Heroic baked in, plus a healthcheck and screenshotter.
- **Pitfalls (issues):** NVIDIA driver installs failing per provider (#61), provisioning failures (#44, 25 comments), "Sunshine maxxed out at 2560x1600" (#144), "DX12 / Vulkan WSI broken in container" (#381, GPU device permissions; v0.45.2 fixed NVIDIA device permissions), "Steam performance issue: resources may not be fully utilized" (#335). Activity has slowed: the last commit and release were 2026-05-10.
- **Steal:** a provider abstraction (provisioner/runner/state); idempotent "configure" over SSH for bring-your-own nodes; activity-based autostop signals; PIN-push pairing automation; digest-pinned images plus a preheat pull; disk snapshot and object-storage offload for persistent environments.
- **Verdict:** **inspiration-only** (AGPL code is forkable, but Pulumi+Ansible per-cloud provisioning is outside Cha's homelab-first scope). Possibly a later "provision a cloud node" add-on modelled on it.

---

## 6. Unreal Pixel Streaming 2 (EpicGamesExt/PixelStreamingInfrastructure)

Sources: clone (`master`, 2026-10-02; branches UE5.6/5.7/5.8), `Common/docs/Protocol.md`, `SFU/README.md`, `Docs/pixel-streaming-2-migration-guide.md`, `Frontend/library/src/**`.

- **Architecture:** the UE app (streamer) connects to the **Signalling server** ("Wilbur", Node/TS). Players connect to the same server. JSON messages (`config`, `identify`, `endpointId`, `listStreamers`, `subscribe`, `offer`/`answer`, `iceCandidate`, `playerConnected`…). The **SFU** (Node + **mediasoup**) connects as a special player on port 8889, subscribes to a streamer, and re-forwards **simulcast** layers per player based on connection quality. A matchmaker spreads players across instances. Streamer port 8888. The SFU Docker image needs host networking.
- **PS2 changes (UE 5.5+):** WebRTC moved behind Epic's internal **EpicRtc** layer, and WebRTC types are no longer public. **"Stream sharing"** (one encode, many peers, a single "quality controlling" peer) was **removed** as "a hack with many issues around stuttering and freezing". The replacement is per-peer encode (default) or SFU + simulcast. `DegradationPreference` is hard-coded to `MAINTAIN_FRAMERATE`. **Keyframe interval defaults to −1** (keyframes only on demand). Rate control is effectively CBR only. A quality scale of 0–100 replaces codec-specific QP. `UseMediaCapture` is the default (safer than fences, about 10% more CPU).
- **Input protocol over data channel:** binary messages with a 1-byte type. Mouse coordinates are **normalized uint16** (0–65535) with int16 deltas. There are keys (`KeyDown` uint8 key + repeat), touch (id, x, y, force), gamepad (controller idx, button, analog as double), `UIInteraction`/`Command` JSON strings, `IFrameRequest`, `LatencyTest`, and more. **The streamer sends a `Protocol` message (id 255) declaring the message IDs and structures**, so the protocol is negotiated and extensible (`IPixelStreaming2DataProtocol::Add()`). From the streamer come `QualityControlOwnership` and **`InputControlOwnership`** (who may drive input), FreezeFrame (a JPEG while the stream pauses), and file transfer. Data channels are `ordered: true`, negotiated ids via SFU.
- **WebRTC tuning in the frontend:** SDP munging `x-google-start-bitrate=10000; x-google-max-bitrate=100000`, Opus `maxaveragebitrate=510000; stereo=1; useinbandfec=1`, optional **`abs-capture-time`** header extension (Chromium only; it breaks Firefox), TURN-only `iceTransportPolicy: 'relay'` mode, AFK timeouts, and a data-channel latency tester.
- **Steal:** a self-describing input protocol (a schema message at connect, so host and client can evolve independently); normalized absolute coordinates; explicit input-control and quality-control ownership for multi-viewer sessions; "per-peer encode by default, SFU + simulcast for spectators"; abs-capture-time for glass-to-glass measurement; FreezeFrame-style placeholders during renegotiation.
- **Verdict:** **inspiration-only** (MIT infra, but UE-specific). The SFU-with-simulcast idea fits Cha's spectator and party mode.

---

## 7. Valve: Steam Remote Play, Steam Link, Steam Frame

Sources: https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/ (2026-09-22), https://www.gamingonlinux.com/2026/09/a-big-steam-stable-update-is-out-with-improvements-for-remote-play-steam-input-and-bug-fixes/ (2026-09-02), https://steamdeckhq.com/news/steam-deck-beta-client-hdr-streaming-remote-play/ (2026-07-29), https://www.club386.com/steam-remote-play-update-streaming/ (2026-09-29), https://en.wikipedia.org/wiki/Steam_Frame, https://www.uploadvr.com/valve-steam-frame-official-announcement-features-details/, https://www.tomshardware.com/virtual-reality/valve-steam-frame-review.

- **2026 Remote Play timeline:**
  - 2026-07-29 beta: HDR streaming on Steam Deck OLED, **AV1 streaming with the experimental SteamRT3 Steam client**, improved colour range.
  - 2026-09-02 stable: colour range, Deck OLED HDR, a black-window fix, a fix for PipeWire capture with Wayland desktop scaling, and a VA-API "unlimited bandwidth" framerate fix.
  - **2026-09-21 beta: PyroWave** ("Added an experimental video codec, Pyrowave, which allows high bandwidth, low latency video streaming."). Host and client on Windows, macOS and Linux (Linux needs SteamRT3). Bitrate 100–500 Mbit/s. **Direct gigabit Ethernet** is expected. 4:4:4 and HDR are auto-selected. Valve says it uses "5-10 times" the bandwidth of other codecs. A Club386 test measured about 192 Mbit/s vs 11.5 Mbit/s for NVENC H.264. **Steam Link mobile apps get it later.** It was apparently added, removed, then re-enabled for testing (GoL).
- **Steam Frame** (announced 2025-11-12, released **2026-09-18**, $1,059/$1,299): "streaming-first". A **Wi-Fi 6E USB dongle** gives a dedicated 6 GHz PC-to-headset link, while the headset's Wi-Fi 7 radios keep 5 GHz for the home network and **pick between links in real time** for lowest loss. **Foveated streaming**: eye tracking raises the bitrate where you look; it "is applied at the encoder level and does not require the game developer to support it", and Valve claims "10x" quality in the foveated region. Flat (non-VR) games stream via Steam Link. Codec details are **(unverified)**.
- **Steam Remote Play in containers:** Steam-Headless notes the container needs its own LAN IP (custom macvlan network), otherwise "Steam thinks you are on a different network" and routes via the internet.
- **Steal / implications:**
  - **PyroWave is now validated by Valve as the wired-LAN codec**, with a 100–500 Mbit/s UX and auto 4:4:4/HDR. Copy the UX: an explicit "wired LAN only" warning, a bitrate slider in that range, and auto 4:4:4/HDR.
  - **Gaze/ROI-weighted encoding** generalises to desktop: a **cursor- or focus-region QP map** (NVENC/VAAPI/Vulkan Video ROI) for WAN desktop streams is a cheap, differentiating idea **(speculative)**.
  - A **dual-path link selection** idea, as in the Frame's per-packet choice between 5 and 6 GHz, maps to QUIC multipath / iroh's direct-plus-relay paths.
  - An environment mode in which **Steam itself is the streamer** (a Steam container exposing Remote Play to Steam Link clients, including PyroWave) costs little to support as a "native Steam client" option.
- **Verdict:** **inspiration**, with Steam Remote Play optionally exposed as a passthrough protocol for Steam environments.

---

## 8. Parsec (Unity)

Sources: https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007, https://parsec.app/technology, https://parsec.app/blog/game-streaming-tech-in-the-browser-with-parsec-5b70d0f359bc, https://parsec.app/changelog. (support.parsec.app returned 403. Feature-matrix details below come from search snippets and are **partly unverified**.)

- **BUD ("Better User Datagrams"):** UDP + DTLS 1.2 (OpenSSL), with "basic reliability semantics like TCP" plus a custom congestion control that tries to "detect a congestion event before it starts" and adapts the *encoder bitrate* rather than retransmitting heavily. They claim a 97% NAT-traversal success rate. Symmetric NATs and firewalls are still a problem. They claim about 7 ms added latency on LAN.
- **Pipeline:** "zero-copy GPU pipeline to the encoder", hardware decode everywhere, "built in cross platform C … no wrappers".
- **Browser client history:** the original (about 2018) shipped **video over an RTCDataChannel, not media tracks**, into **MSE with Chrome's low-delay mode**, and was Chrome-only. Pointer lock needed `webkitRequestFullscreen`. Today the web client is Chromium-only and uses WebRTC. Per the support-page snippets, it lacks low-level hardware optimisations, 4:4:4, extra screens, mic, camera, DS4 touchpad and full DualSense features, and has "less control over networking" (**unverified detail**). 2026-09-28 (v150-105a): "Re-enable our web-based UI for Free and Warp users", "Always Relative" mouse mode, and up to 5 screens including virtual monitors.
- **Steal:** a delay-based, proactive CC that adapts encoder bitrate (consistent with Nestri's queue-first lesson); an "Always Relative" mouse mode toggle; honest web-vs-native feature parity tables.
- **Verdict:** **inspiration-only.** Its web client is a reminder that the browser is the lower-capability tier unless you own transport and decode. That is why Cha Portal should own both (WebTransport + WebCodecs/WebGPU) rather than live inside WebRTC media tracks.

## 9. Shadow, Rainway, Playtron, GameLift Streams (brief)

- **Shadow:** a full dedicated Windows VM per user. Blade went bankrupt in 2021 and was bought by Octave Klaba (OVH founder). The Neo Lite tier launched 2026-07-23 (Wikipedia, plus a search snippet for 2026 pricing). The protocol is proprietary and public technical detail is **unverified**. Lesson: persistent full-PC VMs are expensive to run, and the business only works at high utilisation. For homelab Cha, "persistent environment" should mean disk persistence with on-demand compute, not reserved hardware.
- **Rainway:** a browser-first game streaming service (C#/JS/React/Flutter/C++ stack per Wikipedia). It partnered with Microsoft (xCloud) in 2021, the consumer service shut down 2022-10-31, and then it ceased operations (Wikipedia, GeekWire). Lesson: a browser-only consumer product with no hardware moat and no monetisation died, while the SDK and tech had value to a platform owner. For an OSS self-hosted project, the risk is maintainer burnout, not revenue.
- **Playtron:** pivoted to a Sui-blockchain handheld OS and "Game Dollar" stablecoin (2025–26). **Not relevant.**
- **Amazon GameLift Streams** (2025-03): a managed WebRTC browser streaming service with Windows, Linux and **Proton** runtimes (Proton 9 added 2025-08), 1080p60, and "stream groups" with multi-application pools across regions. Its concepts (an application, a stream group, a capacity pool) are a useful vocabulary for Cha's environment/template/node-pool model.

---

## 10. Steam-in-Docker images (non-Wolf)

- **Steam-Headless** (https://github.com/Steam-Headless/docker-steam-headless, 4,834★, GPL-2.0): Debian Trixie, Xfce + **noVNC with audio** in the browser, a bundled Sunshine (Moonlight), Flatpak installs of Heroic/Lutris/EmuDeck, NVIDIA/AMD/Intel, and a "secondary" mode reusing the host X server. Pitfall: Steam Remote Play needs its own LAN IP (macvlan), otherwise traffic goes via the internet.
- **linuxserver/docker-steam** (https://github.com/linuxserver/docker-steam, GPL-3.0, on `baseimage-selkies:debiantrixie`): "web accessible Steam" through Selkies. It says itself: "if you want a fully flushed out Moonlight couch solution please consider Wolf". Pitfalls it documents:
  - Steam runs its browser helper and every game in **bubblewrap, which needs user namespaces**. Docker's default seccomp and AppArmor block them, so it recommends `seccomp=unconfined, apparmor=unconfined`. Otherwise it falls back to slow proot/ptrace.
  - Gamepads work through a **userspace interposer** (no uinput/HID), so you must "always force Proton … even for Linux native games" and "**cannot use the Steam Input feature**".
  - Relative mouse needs a "Gaming Mode" (pointer lock). NVIDIA needs host driver ≥ 580.
- **slooock-dev/podstage** (18★, MIT): headless Steam Big Picture in **rootless Podman**, streamed to Moonlight. A small signal that rootless Steam containers are viable.
- **Steal:** treat the bwrap/userns, uinput/uhid and pointer-lock constraints as first-class node capability checks, and surface them in the dashboard (Polaris-style "refusals say why").

## 11. Other adjacent projects (signals and ideas)

- **cloud-game / CloudRetro** (https://github.com/giongto35/cloud-game, Apache-2.0, 2,480★): a coordinator plus geographically spread workers that dial *into* the coordinator. **The browser pings candidate workers over HTTP, sends the latency list to the coordinator, and the coordinator picks the worker.** Uses pion WebRTC VP8/H.264 and Opus (https://webrtchacks.com/open-source-cloud-gaming-with-webrtc/, 2020-04-15). Pitfalls they wrote up: Go GC pauses and slow channels in the real-time path, and CGO obscuring crashes. **Steal:** client-measured latency drives node placement, and workers dial into the control plane.
- **cloud-morph** (MIT, 1,182★): Wine + WebRTC "decentralized" app streaming. An older, inspiration-only example of collaborative single-app streaming.
- **CrossDesk** (GPL-3.0, 4,365★): a RustDesk-like remote desktop on MiniRTC (WebRTC) with a web client, H.264/AV1, self-hosted signalling + TURN, and Linux headless with Xvfb. Low relevance (an IT remote-support product).
- **ITmedes/browserpane** (AGPL-3.0, 11★): a "web-native remote desktop protocol built on WebTransport, WebCodecs, and WebGL 2", with **crisp tiles for UI and H.264 for video regions**. A hybrid tile + video idea worth remembering for desktop (text) environments over WAN. **frecar/beam** (MIT, 4★): WebCodecs + VA-API Linux remote desktop. **Tuhis/gawk** (Apache-2.0, 1★): "sub-500 ms" WebTransport + WebCodecs streaming. All signal that WebTransport + WebCodecs is the 2026 default for new browser streamers.
- **PiterWeb/LibreRemotePlay** (GPL-3.0, 196★): a WebRTC "Steam Remote Play alternative". **OpenNOW** (MIT, 2,474★): a Rust GeForce NOW client, useful for seeing how a commercial WebRTC cloud-gaming protocol behaves **(not inspected)**.
- **Neko** (Apache-2.0, 22,446★) and **Selkies** (MPL-2.0, 2,267★): covered by slice 04. Listed here only to note that Neko is the most-starred adjacent project overall. "Virtual browser in Docker" demand is large, so Cha's "Chrome environment" use case has a big audience.
- **The PyroWave ecosystem** (GitHub search "pyrowave", 2026-10): ALVR/WiVRN forks for Quest 3, Vision Pro and Galaxy XR, `lutyjj/pyrowave-rs` bindings, Vibepollo + Moonlight notes, and **`imbcmdth/pyrowave` `webgpu` branch** (https://github.com/imbcmdth/pyrowave/tree/webgpu, MIT):
  - A WGSL hand-port of the encoder and decoder against standard `webgpu.h` (Dawn or wgpu-native). **Its streams interoperate both ways with the Vulkan encoder and decoder.**
  - It needs the WebGPU **`subgroups`** feature, and does not assume a subgroup size: encoder 16–64 lanes, decoder 4–128.
  - On wgpu-native at 1080p and 250 kB/frame: decode 0.48 ms wall-clock (GPU dequant 0.055 ms + iDWT 0.034 ms), encode 0.62 ms.
  - It includes "Notes for a WASM build" and is used by `imbcmdth/ffrwd-package-pyrowave` (a wasi:webgpu component).
  - It is the **only browser-capable PyroWave path I found**. The PyroWave and browser-transport researchers should validate subgroups availability in shipping browsers.

---

## 12. Competitive positioning

### 12.1 What Cha Portal uniquely fills

| Capability | Who has it (2026-10) | Gap |
|---|---|---|
| Multi-node fleet: nodes register, environments spin up on demand | Nestri (closed lifecycle), Wolf/Fenrir (k8s, slice 01), Kasm (slice 04), CloudyPad (one instance per deploy) | **No open, homelab-first, multi-node dashboard that spans games + desktops + browsers** |
| Ephemeral and persistent environments (Kasm-like) | Kasm, Selkies/LSIO images, Polaris Spaces (one at a time), Nestri boxes (closed) | Persistent Steam libraries + ephemeral sessions across nodes is unsolved in OSS |
| First-class Steam in containers | Wolf/GoW (Moonlight), LSIO Steam (Selkies, crippled input), Steam-Headless, Polaris Spaces | **Steam in containers *in the browser* with real gamepad/uhid support** is weak everywhere |
| Browser client at native-class latency | Punktfunk web (WebTransport+WebCodecs, no PyroWave), Polaris browser stream (experimental), moonlight-web (WebRTC) | Nobody ships a production browser client that matches native |
| **PyroWave in the browser** | **Nobody** (Punktfunk disables it; Polaris, Nestri and Valve are native-only). A WGSL port exists | **Clear wedge** |
| One dashboard for external hosts (Sunshine, Apollo, Vibepollo, Wolf, Polaris, Punktfunk) | moonlight-web (GameStream only), Polaris/Nova (own ecosystem) | **Federation across protocol families** behind one auth and UI |
| Thin native client | Moonlight, Nova, Punktfunk clients, Nestri (closed), Parsec | Not unique. Reuse a core rather than build from scratch |

**One-line position:** *"Kasm's environment orchestration + Wolf's container gaming + Punktfunk-grade transport, delivered first to the browser (WebTransport/WebCodecs plus PyroWave on WebGPU for the LAN), with any Moonlight-protocol host pluggable as an external endpoint."*

### 12.2 Solved already: reuse, don't rebuild

- **Moonlight protocol client:** `moonlight-common-rust` (Sans-IO, wasm), with moonlight-web-stream as the gateway reference.
- **Container gaming runtime:** Wolf/GoW images and gst-wayland-display (slice 01). Polaris shows how to reuse host NVIDIA driver files in containers. CloudyPad shows the driver-volume pattern.
- **Virtual display per client, capture, zero-copy encode:** Punktfunk (pf-vdisplay, pf-capture, pf-zerocopy, MIT/Apache), Wolf, and Nestri's nescope/nescapture (Apache).
- **PyroWave over datagrams:** Nestri's spec plus nespyro (MIT). PyroWave on WebGPU: the imbcmdth `webgpu` branch (MIT).
- **WebTransport browser plane with self-signed rotation:** Punktfunk's scheme.
- **High-bitrate FEC:** Punktfunk's Leopard-RS GF(2^16).
- **Desktop/browser environments:** Selkies/LSIO base images and Neko (slice 04).
- **Micro-VM GPU sharing** (later): nesbox + virtio-nvgpu.

### 12.3 Recurring pitfalls across these projects

1. **Transport churn.** Nestri rewrote its transport 3–4 times. moonlight-web-stream needed WebSocket fallbacks. Parsec's web client stayed second-class. Decide early on **WebTransport (QUIC) as the primary browser transport, WebRTC as the NAT/TURN fallback, and WebSocket as the last resort**, all behind one internal media abstraction.
2. **Sender-side metrics lie.** Nestri's "zero frames dropped" while the client got nothing, and an 8 s queue at zero loss. Instrument receiver truth and send-queue duration from day one.
3. **Keyframe storms and IDR loops** (Nestri's 26 fallbacks / 19 IDRs; Pixel Streaming's periodic keyframes now off by default). Use intra-refresh, on-demand IDR, gate dependent frames, and use PyroWave (all-intra) on LAN.
4. **GPU driver coupling in containers** (CloudyPad #61/#381, Nestri's runtime `.run` installs, LSIO's ≥580 requirement, Polaris pinning runtimes per driver version). Mount host driver files rather than baking drivers into images, and check versions at node enrolment.
5. **Steam sandboxing and input in containers:** bwrap needs user namespaces, no uinput/HID breaks Steam Input, Remote Play needs its own LAN IP, and pointer lock is required for relative mouse. Make these explicit node capabilities.
6. **Browser input gaps:** Switch Pro over Bluetooth misread by browsers (linckosz), Keyboard Lock and Gamepad APIs need a secure context (moonlight-web-stream), pointer lock quirks (Parsec, Nestri issues), non-Chromium gaps.
7. **TLS on the LAN:** WebCodecs and WebTransport need secure contexts, and self-signed certs hurt UX. Use Punktfunk's `serverCertificateHashes` plus a signed-hash scheme, or ACME through the portal domain.
8. **Single-tenant resource assumptions:** NVENC session caps on GeForce (12, sidestepped by Vulkan Video), "one active Space at a time" (Polaris), and stream sharing removed from Pixel Streaming. Design per-viewer encode or SFU explicitly.
9. **Open-core drift and bus factor:** Nestri closed its client and lifecycle, Punktfunk is single-vendor, CloudyPad has slowed since 2026-05. Prefer depending on small, well-bounded crates (moonlight-common-rust, punktfunk-core, nespyro, the pyrowave WGSL port) over whole platforms.

---

## 13. Consolidated "steal" list (prioritised)

1. **Transport rules** (Nestri media-transport.md): queue-duration congestion signal, receiver-truth stats, ResyncGate, no app-level pacing on QUIC, sequence-number reuse for intentional skips.
2. **PyroWave LAN profile:** Nestri's datagram framing (duplicated critical packets, GRACE/IDLE collector, skip guard, format version), Valve's UX (100–500 Mbit/s, wired-only warning, auto 4:4:4/HDR), and **WebGPU decode via the imbcmdth WGSL port**.
3. **WebTransport browser plane:** Punktfunk's 13-day ECDSA cert + `serverCertificateHashes` + long-lived-identity-signed hash + PAKE pairing. Media on datagrams, control on one bidi stream. Decoded `VideoFrame` goes straight to a WebGPU/WebGL texture and never enters the wasm heap.
4. **Shared protocol core, native + wasm** (Punktfunk, moonlight-common-rust): write Cha's protocol core once in Rust, Sans-IO, and compile it to wasm for the browser.
5. **Self-describing input protocol** (Pixel Streaming `Protocol` message), with normalized coordinates and input and quality ownership for multi-viewer sessions.
6. **Session sharing with permission levels** (linckosz: Viewer / Gamer / Desktop / Full; link and PIN sent separately), plus spectators via per-viewer encode or SFU.
7. **Node enrolment and liveness:** owner token exchanged for an id+secret (not machine-id), RFC 8628 device grant for headless nodes, server-dictated heartbeat interval, offline after 3 missed beats, current endpoint id carried on the heartbeat.
8. **In-environment agent** like nesinit plus a socket hub like neshub: a versioned handshake first, `ready` ≠ `initialized`, launch ids, the hub listens and producers dial.
9. **Doctor tooling:** nesdoctor (bufferbloat under load, EDID/HDR, decode caps) for clients and nodes; Polaris Doctor (one bounded, reversible auto-tune with verify and rollback); "refusals say why".
10. **Placement by client-measured latency** (CloudRetro) across nodes, and dual-path or multipath link selection (Steam Frame, iroh direct + relay).
11. **Autostop and persistence economics** (CloudyPad): activity signals (control-port traffic, downloads, admin ops), snapshot or offload on stop.
12. **Gamepad fidelity:** nesgamepad uhid DualShock 4/DualSense rebuilt from real descriptors, and no pad until one connects.
13. **ROI/foveated-style encoding:** cursor or focus-region QP boost for desktop over WAN **(speculative)**.

---

## 14. Risks and open questions

- **Browser PyroWave feasibility:** does the WGSL port run in Chrome, Firefox and Safari with `subgroups`, at what decode cost including `VideoFrame`/texture interop, and can WebTransport datagrams sustain 150–500 Mbit/s in a browser (per-datagram JS overhead, ~1.2 kB MTU means about 50k datagrams/s)? Needs a spike, and a handoff to the PyroWave and browser-transport slices.
- **Protocol strategy:** build a Cha-native protocol, adopt `punktfunk/1` (MIT/Apache, wire v2, single vendor), or adopt Moonlight/GameStream (ubiquitous, weak crypto, no PyroWave in Moonlight)? A plausible path: Cha-native over QUIC/WebTransport internally, with GameStream and Punktfunk as external-endpoint adapters.
- **Isolation model:** containers (Wolf-style, fastest path) now, micro-VMs (nesbox/virtio-nvgpu) later? virtio-nvgpu's trust boundary equals the host NVIDIA driver's and it is tested on two cards.
- **Nestri's direction:** will the client and lifecycle open, and could nescope/nescapture/neshub become shared building blocks? Watch `nestrilabs` monthly.
- **Licence hygiene:** GPL-2.0-only components (possibly Steam-Headless) are incompatible with GPLv3/AGPLv3 if code is combined. Apache-2.0, MIT and MPL-2.0 are fine. The virtio-nvgpu guest driver is GPL-2.0 (a kernel module, separate work).
- **Unverified items** to re-check when web search is available: Parsec web-client feature matrix specifics, Steam Frame codecs, Shadow protocol, Punktfunk community size off GitHub, Rainway final status, WebTransport Baseline date (an MDN snippet said "newly available since March 2026").
