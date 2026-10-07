# 01: Games on Whales / Wolf and Steam-in-Docker

> Background research from planning ([index](README.md)). What Cha Portal actually ported is in [PROVENANCE.md](../PROVENANCE.md).

Research slice 01 for **Cha Portal** (portal.cha.sh). Snapshot date: **2026-10-03**.
Method: shallow clones of the repos (source read, nothing built or run), GitHub pages, author blogs and docs.
Anything not confirmed from a primary source is tagged **(unverified)**.

Constraints from the user (received mid-research): Cha Portal will be **copyleft OSS (AGPL/GPL)**, so GPL code is acceptable. Target is **homelab / small group first**, containers are fine and microVM isolation is not required. **LAN and WAN are equally first-class.**

---

## TL;DR

- **Wolf** ([games-on-whales/wolf](https://github.com/games-on-whales/wolf), **MIT**, about 2.2k stars, 152 commits in 2026, last push 2026-09-29) is the most complete open "one GPU, many isolated containerised sessions" streaming server. It is built from separable parts:
  - **gst-wayland-display**: a Rust/Smithay micro-compositor shipped as a GStreamer source, MIT.
  - **inputtino**: a uinput/uhid virtual-input library, MIT, with C/C++/Rust/Python bindings.
  - **fake-udev**: hotplug of input devices into running containers.
  - A **Docker runner** that talks to the Docker socket.
  - **GoW app images** (MIT).
  - The **Moonlight protocol stack**, which is the part Cha Portal cares least about.
- Wolf only speaks **Moonlight** (RTP/UDP, ENet, RTSP). There is no WebRTC or WebTransport output, and the stock GStreamer is built with `-Drs=disabled -Dtls=disabled`. Codecs on `stable` are **H.264, HEVC Main8 and AV1 Main8, NV12 only**: no HDR, no 4:4:4, no RFI and no dynamic bitrate. Vulkan encode, HDR and **PyroWave** exist only as **open, unmerged PRs** (#450, #469, #517, the last opened 2026-09-29).
- There is **no formal release since `v2024.07`**. Users run the rolling `ghcr.io/games-on-whales/wolf:stable` image.
- **Steam in Docker is solved in GoW** without `--privileged`. The recipe is:
  - Steam runs under nested Sway (or nested gamescope) as a Wayland client of Wolf's compositor.
  - A **patched bubblewrap** skips the capability check so pressure-vessel works.
  - The container gets `CAP_SYS_ADMIN, SYS_NICE, SYS_PTRACE, NET_ADMIN, NET_RAW, MKNOD`, `seccomp=unconfined, apparmor=unconfined` and `IpcMode=host`.
  - Device cgroup rules cover the input and hidraw majors.
  - Decky Loader is baked into the image.
- The browser bridges built on Wolf were **HelixML** and **moonlight-web(-stream)**. Helix went from WebRTC to raw H.264 NALs over WebSocket with WebCodecs. Its 2026 docs no longer mention Wolf at all and describe their own PipeWire → GStreamer → WebSocket pipeline. That they replaced Wolf is an inference from the docs.
- **Kubernetes**: `fenrir` (the official operator) says "NOT IN A USEABLE STATE".
- **Steam-Headless v2** (Sept 2026, `josh5/steam-headless:2`) moved to KDE Plasma 6 Wayland with a patched KWin, systemd in the container, Sunshine, and its own browser UI (WebRTC and WebSocket). It is one session per container and multi-session needs macvlan. That is a second independent team building its own stack instead of adopting Wolf.
- **Recommendation: (b) embed and reuse Wolf components in our own Rust node streamer, with (a) "Wolf unmodified + bridge" as the bootstrap and Moonlight-compatibility path. Do not hard-fork Wolf (c).** Instead, upstream small generic hooks (Vulkan producer, PyroWave, API-driven sessions).
  - Reuse `wayland-display-core` (a Rust crate), `inputtino` (Rust bindings), the fake-udev technique and the **GoW container contract**, which lets the GoW images run unmodified.
  - Own the transport (WebTransport/QUIC plus WebRTC fallback), rate control and PyroWave.

---

## 1. Project snapshot (as of 2026-10-03)

| Project | What | License | Status / activity | Verdict for Cha |
|---|---|---|---|---|
| [games-on-whales/wolf](https://github.com/games-on-whales/wolf) | Moonlight server, multi-session, containerised apps | **MIT** | 2,213★, 114 open issues, last push 2026-09-29. Last tagged release `v2024.07` (2024-07-10). Rolling `:stable` image. 152 commits since 2026-01-01 (ABeltramo 53, JBailes 38, camelcx 36). | **Protocol-compatible + inspiration; run unmodified as the Moonlight-compat runtime** |
| [games-on-whales/gst-wayland-display](https://github.com/games-on-whales/gst-wayland-display) | Smithay micro-compositor → GStreamer `waylanddisplaysrc` + C API + Rust crate | **MIT** | 74★, last push 2026-09-07. No releases. Active Vulkan-encode branches. | **Depend-on (embed the Rust crate)** |
| [games-on-whales/inputtino](https://github.com/games-on-whales/inputtino) | Virtual input (uinput/uhid): kbd, mouse, touch, pen, Xbox/PS5/Switch pads | **MIT** | 97★, last push 2026-07-13. Used by Wolf, Sunshine, Moonshine. | **Depend-on** |
| [games-on-whales/gow](https://github.com/games-on-whales/gow) | App images (Steam, XFCE, Firefox, RetroArch, Lutris, Heroic, Pegasus, ES-DE, Kodi, Prism, …) | **MIT** | 738★, last push 2026-06-21. 98 commits in 2026. Ubuntu 25.04 default + Fedora 43 variants (Apr 2026). amd64 only. | **Depend-on (run images unmodified via the container contract)** |
| [games-on-whales/fenrir](https://github.com/games-on-whales/fenrir) | K8s operator ("direwolf" CRDs, moonlight-proxy, wolf-agent) | MIT | Last commit 2026-07-14. README: "NOT IN A USEABLE STATE!" | Inspiration-only |
| [games-on-whales/wolf-den](https://github.com/games-on-whales/wolf-den) | Web UI for Wolf (.NET 10 "WolfLeash") over the unix socket | MIT | Last commit 2026-08-17 | Inspiration-only |
| [games-on-whales/wolf-ui](https://github.com/games-on-whales/wolf-ui) | In-stream launcher (Godot/C#) run as a Wolf app | `src/LICENSE` is MIT but credits "Martin Fuchs and Tweaklab AG". No repo-root license, so the license is effectively unclear. | Last commit 2026-06-23 | Inspiration-only |
| [Steam-Headless/docker-steam-headless](https://github.com/Steam-Headless/docker-steam-headless) | v1: Xorg + noVNC/Neko + Sunshine + Steam. v2: Plasma 6 Wayland + Sunshine + SHUI | **GPL-2.0** (v1 repo). Frontend GPL-3.0. | 4,834★. v1 repo last push 2026-09-25 with a banner telling users to move to v2. v2 image `josh5/steam-headless:2`, tags `2.44.20260915.1` … `2.44.20260926.1`. | Inspiration-only. Compare UX. |
| [MrCreativ3001/moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream) | Moonlight client server → browser (WebRTC + WebSocket/WebCodecs) | **GPL-3.0-or-later** | Commit 2026-10-03. Tags up to `v3.0.0-prerelease.7`. | Fork/borrow for the Moonlight bridge |
| [MrCreativ3001/moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust) | **Sans-IO** Rust Moonlight client protocol; compiles to WASM | **GPL-3.0** | Commit 2026-10-03. Lists **Wolf ✅**. | **Depend-on** for "external Moonlight endpoint" support |
| [ValveSoftware/gamescope](https://github.com/ValveSoftware/gamescope) | Micro-compositor; `--backend headless`, PipeWire export | BSD-2-Clause | Latest tag 3.16.31 | Depend-on (inside images) |

---

## 2. Wolf architecture deep-dive (from source, `stable` @ `facb8e0`, 2026-09-29)

The repo ships its own architecture brief: [AGENTS.md / CLAUDE.md](https://github.com/games-on-whales/wolf/blob/stable/AGENTS.md). The `src/` tree is about **21k lines of C++20**.

### 2.1 Code map

- `src/moonlight-protocol/`: stateless Moonlight library. It covers HTTPS pairing XML, control packets, Reed-Solomon FEC (on [nanors](https://github.com/sleepybishop/nanors)) and a PEG-grammar RTSP parser ([cpp-peglib](https://github.com/yhirose/cpp-peglib)).
- `src/core/` (`wolf::core`): reusable platform layer.
  - `docker.hpp`: Docker/Podman REST over libcurl.
  - `input.hpp`: inputtino wrapper.
  - `virtual-display.hpp`: gst-wayland-display wrapper.
  - `audio.hpp`: libpulse wrapper.
  - `batched_send`: `sendmmsg` UDP batching.
- `src/fake-udev/`: CLI that forges udev netlink events inside a container.
- `src/moonlight-server/`: the `wolf` binary.
  - `rest/`, `rtsp/`, `control/` (ENet), `rtp/`: the Moonlight protocol stack.
  - `streaming/`, `gst-plugin/` (`rtpmoonlightpay_video`/`_audio`): GStreamer pipelines and payloaders.
  - `runners/docker.cpp` and `runners/process.cpp`.
  - `sessions/moonlight.cpp`, `sessions/lobbies.cpp`.
  - `audio/pulse_router.cpp`.
  - `api/`: unix-socket REST API with OpenAPI and SSE.
- **Style:** functional, with [immer](https://github.com/arximboldi/immer) persistent data structures in `immer::atom`. Components talk over an event bus (`dp::event_bus`) using events such as `StreamSession`, `VideoSession`, `PlugDeviceEvent`, `CreateLobbyEvent` and `SwitchStreamProducerEvents`. Serialisation uses reflect-cpp.

### 2.2 Session lifecycle (what actually happens)

`src/moonlight-server/sessions/moonlight.cpp`:

1. A Moonlight `/launch` (or `POST /api/v1/sessions/add`) fires a **`StreamSession`** event. Wolf immediately:
   - starts a **video producer** pipeline in a thread: `waylanddisplaysrc render_node=… ! <caps> ! interpipesink name={session_id}_video max-buffers=1` (`streaming.cpp:start_video_producer`);
   - waits for the compositor to report its `WAYLAND_DISPLAY`;
   - then **starts the runner** (the Docker container) with the socket bind-mounted.
2. RTSP SETUP/ANNOUNCE fires **`VideoSession`** and **`AudioSession`** events. Each handler blocks until an **RTP ping** arrives on the shared UDP port. The ping is matched by a 16-byte `rtp_secret_payload`, or by legacy IP+port. Only then does Wolf start the **consumer** pipeline: `interpipesrc listen-to={session}_video ! <convert/scale> ! <encoder> ! rtpmoonlightpay_video ! appsink name=wolf_udp_sink`.
3. Wolf configures the `appsink` with a custom callback that pushes `GstBufferList`s through **`sendmmsg`**:
   - batches are at most `min(16, 65536/packet_size)` packets;
   - **pacing** is fixed at 80% of 1 Gbit/s unless `WOLF_ENABLE_VIDEO_PACING=FALSE`. PR #517 makes it dynamic.
4. **Pause** (client disconnect) sends EOS to the *consumer* pipeline. The code comment says the client IP:port, AES key, resolution and codec can change on resume, so "the only solution is to kill the pipeline and re-create it". The container and compositor keep running until a stop event, and `WOLF_STOP_CONTAINER_ON_EXIT=TRUE` then removes the container.
5. **IDR requests** from the control channel become `GstForceKeyUnit` upstream events. There is **no reference-frame invalidation** (see [moonlight-common-rust's table](https://github.com/MrCreativ3001/moonlight-common-rust) pointing at [wolf#5](https://github.com/games-on-whales/wolf/issues/5)). Intra-refresh is only an open PR (#495, nvh265enc).

**Key architectural property:** the **producer (compositor → interpipesink)** is decoupled from **N consumers (interpipesrc → scale → encode → transport)**. That is what makes lobbies, co-op and per-client resolution scaling cheap. It also means a new transport is mostly a new consumer pipeline tail.

### 2.3 The Docker runner (`runners/docker.cpp`, `sessions/common.cpp`)

For each session Wolf builds a Docker `ContainerCreate` JSON from three inputs: the app's `base_create_json` (raw Docker API JSON, merged), its `env`, `mounts`, `devices` and `ports`, and what Wolf injects.

**The GoW container contract.** If we replicate this, GoW images run unmodified under *our* streamer:

| Injected by Wolf | Value |
|---|---|
| `XDG_RUNTIME_DIR` | runtime dir (default `/tmp/sockets`) |
| `WAYLAND_DISPLAY` + bind mount | `host_xdg_runtime_dir/wayland-N` → same path in container |
| `PULSE_SERVER`, `PULSE_SINK=virtual_sink_<id>`, `PULSE_SOURCE=<sink>.monitor` + socket bind mount | per-session null-sink on a shared PulseAudio |
| `GAMESCOPE_WIDTH/HEIGHT/REFRESH` | session display mode (consumed by GoW `launch-comp.sh`) |
| `WOLF_SESSION_ID`, `WOLF_VIDEO_BUFFER_CAPS` | identifiers / caps |
| `PUID`/`PGID` | per paired client (`WOLF_DEFAULT_RUN_UID/GID`, default 1000) |
| `/home/retro` bind | `HOST_APPS_STATE_FOLDER/profile_data/<profile_id>/<app_title>` (persistent home) |
| `/usr/bin/fake-udev`, `/run/udev/` | fake-udev binary + private per-session udev dir |
| GPU | render node + linked `/dev/dri/card*`; NVIDIA: driver volume at `/usr/nvidia` **or** `DeviceRequests: [{DeviceIDs:["all"],Capabilities:[["gpu"]]}]` + `Runtime: nvidia` + `NVIDIA_VISIBLE_DEVICES/DRIVER_CAPABILITIES=all` |
| Intel | `INTEL_DEBUG=norbc` ([wolf#50](https://github.com/games-on-whales/wolf/issues/50)) |
| `DeviceCgroupRules` | `c <hidraw major>:* rwm`, `c <input major>:* rwm`, read from `/proc/devices` at runtime because the hidraw major is dynamic |

**Lifecycle.** The runner **polls container status every 500 ms**. Hotplugged devices go in with `docker exec` (as root):

```
mkdir -p /dev/input && mknod <DEVNAME> c <MAJOR> <MINOR> && chmod 777 <DEVNAME> && fake-udev -m <base64 uevent>
```

It also writes hwdb files into the session's private `/run/udev/data`. Unplug reverses these steps. On exit the runner logs the container output, then stops and removes the container.

### 2.4 Virtual compositor: gst-wayland-display

[Repo](https://github.com/games-on-whales/gst-wayland-display) and [design doc](https://games-on-whales.github.io/wolf/stable/dev/wayland.html).

- **What it is:** Rust, about 9k LOC, on a pinned **Smithay fork** (`games-on-whales/smithay@a166cf4`). It has three faces:
  - the `waylanddisplaysrc` GStreamer element;
  - a C API (`display_init`, `display_get_frame`, `display_keyboard_input`, `display_pointer_motion[_absolute]`, `display_pointer_button/axis`, `display_touch_*`, `display_add_input_device`, `display_set_video_info`);
  - the `wayland-display-core` crate, built as `cdylib`, `staticlib` **and `rlib`**, so it embeds directly in a Rust node agent.
- **Protocols implemented:** compositor, xdg-shell, shm, linux-dmabuf, **drm-syncobj** (explicit sync), wl_drm, data_device (in-container clipboard only), pointer-constraints, relative-pointer, presentation-time, viewporter, single-pixel-buffer.
  - **No XWayland.** X apps use nested Sway or gamescope inside the app container.
  - No layer-shell, xdg-decoration, fractional-scale or color-management on `stable`.
- **Output:** the compositor renders with GLES. Output memory types:
  - `video/x-raw` (CPU);
  - `memory:DMABuf` with explicit DRM modifiers;
  - `memory:CUDAMemory`, behind the `cuda` cargo feature, via EGLImage → CUDA, with `cuda-device-id` for multi-GPU;
  - `render_node=software` for no GPU.
  - Branch [`feat/vulkan-encode`](https://github.com/games-on-whales/gst-wayland-display/tree/feat/vulkan-encode) adds `vulkan=true`. It emits **NV12/P010 as `memory:VulkanImage`** on the *encoder's* `GstVulkanDevice`, obtained through a context query. A **Vulkan compute RGB→NV12/P010 converter** (`vulkan_nv12.rs`, about 90 KB) feeds `vulkanh264enc` and a patched `vulkanh265enc` with zero copies.
  - The repo's [benchmark/README](https://github.com/games-on-whales/gst-wayland-display/blob/stable/benchmark/README.md) reports per-frame producer convert cost falling from 1265 µs to 59 µs (Intel UHD 770, 720p) and from 8471 µs to 193 µs (4K) after pipelining.
  - Open PRs: #62 Vulkan AV1 encoder patches, #66 vulkanh265enc reference/IDR fixes, #52 "write directly into compatible encode images", #64 tablet (pen) protocol.
- **Input:** mouse, keyboard and touch arrive as **GStreamer custom upstream events** (`MouseMoveRelative`, `MouseMoveAbsolute`, `MouseButton`, `MouseAxis`, `KeyboardKey`, `Touch*`). Alternatively the element reads real evdev nodes (`mouse=`/`keyboard=` properties). This is why **Wolf does not create uinput mice or keyboards** for compositor sessions.
- **Resolution** is fixed by caps at session start. Branch `codex/output-resize-updates` suggests live resize is WIP **(unverified)**.

### 2.5 Encoding: what's pluggable and what's not

The default config is embedded in `src/moonlight-server/state/default/config.include.toml` (`config_version = 7`).

- **Encoder selection:** ordered lists `[[gstreamer.video.{hevc,h264,av1}_encoders]]`, each with `check_elements`. Wolf picks the first whose elements instantiate, then filters by GPU vendor in `configTOML.cpp`. Defaults:
  - **HEVC:** `nvh265enc` (p1, ultra-low-latency, CBR, two-pass-quarter) → `vah265enc` → `qsvh265enc` → `vah265lpenc` → `x265enc`.
  - **H.264:** nvh264enc → vah264enc → vah264lpenc → qsvh264enc → x264enc.
  - **AV1:** nvav1enc → vaav1enc → vaav1lpenc → qsvav1enc → aom `av1enc`.
  - **No Vulkan encoders** on `stable`.
- **Zero-copy:** `video_params_zero_copy` per vendor. NVIDIA uses `cudaupload ! cudaconvertscale add-borders=true ! video/x-raw(memory:CUDAMemory),format=NV12`. VA/QSV uses `vapostproc ! video/x-raw(memory:VAMemory),format=NV12`. It is on by default; `WOLF_USE_ZERO_COPY=FALSE` disables it. See the [blog, 2025-06-12](https://abeltra.me/blog/road-to-zero-copy-in-wolf/).
- **Format limits:** everything is **NV12 / 8-bit / profile main**. `configTOML.cpp` literally says *"TODO: HDR isn't supported by Wolf yet (so we remove P010 and AR30 format)"*. `serverinfo` advertises only `VIDEO_FORMAT_H264 | H265 | AV1_MAIN8` (`moonlight.cpp`). So there is **no HDR, no 10-bit and no YUV444** on `stable`.
- **Pipeline strings are user-editable** via `{width}`, `{height}`, `{fps}`, `{bitrate}`, `{vbv_buffer_size}`, `{slices_per_frame}`, `{color_space}` and similar substitutions. **Adding a GStreamer encoder needs no code.** Adding a **new codec** does, because it touches the Moonlight negotiation. PyroWave PR #517 changed **32 files**, adding `SCM_PYROWAVE = 0x00800000`, `pyrowave_gst_pipeline` and the `support_pyrowave` serverinfo flag.
- **GStreamer build:** 1.26.7 from source with `-Dgpl=enabled` (x264/x265), `-Drs=disabled` (no `webrtcsink`/`whipsink`) and `-Dtls=disabled` (no DTLS, so effectively **no `webrtcbin`**). It also adds RidgeRun **gst-interpipe** (GoW fork, LGPL-2.1). See [`docker/gstreamer.Dockerfile`](https://github.com/games-on-whales/wolf/blob/stable/docker/gstreamer.Dockerfile).

### 2.6 Transport

Moonlight only: RTP video with Reed-Solomon FEC, optional AES-GCM encryption and Opus audio RTP, plus an ENet control channel.

- **Ports:** 47984/tcp HTTPS, 47989/tcp HTTP, 48010/tcp RTSP, 47999/udp control, 48100/udp video, 48200/udp audio. Ports are **shared by all sessions**; sessions are demultiplexed by secret payloads.
- **Session matching:**
  - RTSP matches on a per-session **fake IP** that Wolf hands out in `/launch` (Moonlight parrots it back).
  - RTP matches on the 16-byte ping payload.
  - ENet matches on `enet_secret_payload`.
  - Each falls back to the client IP.
  - [wolf#265](https://github.com/games-on-whales/wolf/issues/265) "Multiple users from a single IP" is still open. PR [#513](https://github.com/games-on-whales/wolf/pull/513) (2026-09-26) rejects ambiguous IP matches and notes that "Moonlight can discard the RTSP identifier at low bitrates".
- **One running session per paired client certificate** (`get_session_by_client`). A bridge therefore needs **one client cert per user/session**; linckosz/moonlight-web documents exactly this.
- **FEC** is skipped for frames larger than 255 packets (pre-existing limit noted in PR #517).
- **No mid-stream bitrate or resolution change**, because the Moonlight protocol has no ABR. That is acceptable on LAN and weak on WAN.

### 2.7 Input

- **Mouse, keyboard, touch** go straight into the compositor via GStreamer events. No host device is created, so they are naturally isolated.
- **Gamepads and pen** are real kernel devices created by **inputtino**:
  - Xbox, Switch and the PS/Nintendo variants use **uinput**.
  - **DualSense** uses **uhid**, emulating the real HID report descriptor. That gives gyro, accelerometer, touchpad, adaptive triggers, LED and battery support, and it works without Steam Input. See the blog series [1](https://abeltra.me/blog/inputtino-uhid-1/), [2](https://abeltra.me/blog/inputtino-uhid-2/), [3](https://abeltra.me/blog/inputtino-uhid-3/).
- **Host isolation** comes from [`85-wolf.rules`](https://github.com/games-on-whales/wolf/blob/stable/85-wolf.rules). Devices are named `Wolf … (virtual) …`; the rules move them to phantom `seat9` and strip `uaccess`, so the host desktop doesn't react to streamed pads ([wolf#451](https://github.com/games-on-whales/wolf/issues/451)). The **rules must be installed on the host**.
- **Hotplug into containers** uses fake-udev ([design doc](https://games-on-whales.github.io/wolf/stable/dev/fake-udev.html), [blog 2023-11-06](https://abeltra.me/blog/docker-hotplug/)), in four steps:
  1. `--device-cgroup-rule` allows the major numbers.
  2. `mknod` the node via `docker exec`.
  3. Write `/run/udev/data/c<maj>:<min>`.
  4. Send a forged `NETLINK_KOBJECT_UEVENT` on `GROUP_UDEV` **inside the container's netns**, so non-host-network containers stay isolated.
- **Steam Input inside unprivileged containers is still broken.** [wolf#81](https://github.com/games-on-whales/wolf/issues/81) is open since 2024-06: Steam creates uinput devices, but no udev events reach the container. Draft PR [#474](https://github.com/games-on-whales/wolf/pull/474) adds an **LD_PRELOAD fake-uinput shim**. It intercepts `UI_DEV_CREATE`/`UI_DEV_DESTROY` and POSTs to a new Wolf API, which then mknods the device and fakes udev. It is x86-only (Steam 32-bit, `-m32`).
- **Controller type mapping:** `controllers_override`, `motion_controller_override`, and auto-promotion of "UNKNOWN + motion" clients to DualSense.
- Mouse acceleration and scroll multipliers are configurable per client.

### 2.8 Audio

- Since June 2026, **PulseAudio runs inside the Wolf container** under supervisord (`WOLF_EMBED_PULSE=true`, `auth-anonymous=1`). Before that it was a `WolfPulseAudio` sidecar container, which is now only the fallback. An external `PULSE_SERVER`, such as host PipeWire-pulse, is also supported.
- Each session gets a `module-null-sink` named `virtual_sink_<id>`. The producer is `pulsesrc device=<sink>.monitor ! … ! interpipesink`, and the consumer is `opusenc … ! rtpmoonlightpay_audio`.
- `audio/pulse_router.cpp` **re-routes sink-inputs by container hostname**, so apps that ignore `PULSE_SINK` still land in their own session sink.
- Isolation is soft: one shared PA daemon with anonymous auth.
- **No microphone path** (Moonlight has none).
- Open issues: [#481](https://github.com/games-on-whales/wolf/issues/481) audio about 500 ms behind on Apple clients; [#523](https://github.com/games-on-whales/wolf/issues/523) PA/GStreamer threads at 300% CPU after a lobby reconnect (2026-10-03).

### 2.9 Multi-user, lobbies, persistence

- **Multi-user:** every session gets its own compositor, encoder and container on a shared GPU. There are no GPU quotas; [wolf#204](https://github.com/games-on-whales/wolf/issues/204) asks how to limit CPU/RAM and the answer is "use `base_create_json`". NVENC session caps on consumer NVIDIA cards apply **(unverified current limit)**.
- **Multi-GPU:**
  - `WOLF_RENDER_NODE` and `WOLF_ENCODER_NODE` are per host, and per-app `render_node` exists.
  - Docs: "keep Wayland rendering and encoding on the same GPU".
  - [#298](https://github.com/games-on-whales/wolf/issues/298): GStreamer NVENC always uses the first NVIDIA card.
- **Lobbies** (`sessions/lobbies.cpp`) are the key to Kasm-like persistence:
  - A lobby is a **runner + compositor + audio sink that exists independently of any Moonlight session**, with fixed resolution and fps.
  - Sessions join by firing `SwitchStreamProducerEvents`, which flips the consumer's `interpipesrc listen-to` to the lobby. Mouse and keyboard are re-pointed at the lobby compositor, and joypads are hot-unplugged and replugged into the lobby container.
  - Options: `multi_user`, `pin`, `stop_when_everyone_leaves`.
  - Bugs: [#364](https://github.com/games-on-whales/wolf/issues/364) crash when joining; [#518](https://github.com/games-on-whales/wolf/issues/518) "Leaving a lobby can leak every frame until Wolf is OOM-killed" (2026-09-30).
- **Persistence today:**
  - The per-profile/app home folder is bind-mounted as `/home/retro`, so `profile_data/<profile>/<app>` persists.
  - The container itself is ephemeral by default.
  - Draft PR [#261](https://github.com/games-on-whales/wolf/pull/261) "background sessions" (HelixML, 2025-09) auto-starts sessions with virtual clients.
  - Draft PR [#305](https://github.com/games-on-whales/wolf/pull/305) pauses and resumes containers.
  - Draft PR [#475](https://github.com/games-on-whales/wolf/pull/475) adds `sessions/resume`.
- **Party mode:** PR [#396](https://github.com/games-on-whales/wolf/pull/396) splits rendering for local co-op. GoW's base-app already builds a **patched XWayland** that removes `xwl_output_fake_modes` so per-player tile sizes work.

### 2.10 `config.toml` schema (v7)

Top-level keys:
- `hostname`, `uuid`, `config_version`, `support_hevc`.
- `paired_clients[]`: `client_cert`, `app_state_folder`, and `settings{run_uid, run_gid, controllers_override[], motion_controller_override, mouse_acceleration, v/h_scroll_acceleration}`.
- `profiles[]`: `id`, `name`, `icon_png_path`, `pin[]`, and `apps[]`.
- `apps[]` entries: `title`, `icon_png_path`, `start_virtual_compositor`, `start_audio_server`, `app_state_folder`, `render_node`.
  - `runner{type=docker|process, name, image, env[], mounts[], devices[], ports[], base_create_json | run_cmd}`.
  - `video{source, …overrides}` and `audio{source}`.
- `[gstreamer.video]`: `default_source`, `default_sink`, `defaults.{nvcodec,qsv,va}.video_params[_zero_copy]`, and `{hevc,h264,av1}_encoders[]{plugin_name, check_elements, encoder_pipeline, video_params…}`.
- `[gstreamer.audio]`: `default_source`, `default_audio_params`, `default_opus_encoder`, `default_sink`.
- The special profile `moonlight-profile-id` is what plain Moonlight sees. By default it holds only Wolf UI and the test ball; real apps live under user profiles shown inside **Wolf UI**.
- Reference: [configuration.adoc](https://github.com/games-on-whales/wolf/blob/stable/docs/modules/user/pages/configuration.adoc).

Env vars: `WOLF_CFG_FILE`, `WOLF_LOG_LEVEL`, `WOLF_DOCKER_SOCKET` (unix only), `WOLF_RENDER_NODE`, `WOLF_ENCODER_NODE`, `WOLF_SOCKET_PATH`, `WOLF_STOP_CONTAINER_ON_EXIT`, `NVIDIA_DRIVER_VOLUME_NAME`, `HOST_APPS_STATE_FOLDER`, `WOLF_DOCKER_FAKE_UDEV_PATH`, `WOLF_USE_ZERO_COPY`, `WOLF_ENABLE_VIDEO_PACING`, `WOLF_USE_RTSP_FAKE_IP`, `WOLF_WAYLAND_SOCKET_WAIT_TIMEOUT_MS`, `WOLF_DEFAULT_RUN_UID/GID`, `PULSE_SERVER`.

### 2.11 Wolf API and management UIs

- **Transport:** HTTP/1.0 over a **unix socket** (`WOLF_SOCKET_PATH`). TCP exposure goes through nginx, and the docs warn it is "highly dangerous". See [api.adoc](https://games-on-whales.github.io/wolf/stable/dev/api.html). `GET /api/v1/openapi-schema` returns OpenAPI JSON.
- **Endpoints** (`api/unix_socket_server.cpp`):
  - `GET /events` (**SSE** stream of event-bus events)
  - `GET /pair/pending`, `POST /pair/client`, `POST /unpair/client`
  - `GET /clients`, `PATCH /clients/settings`
  - `GET /apps`, `POST /apps/add`, `POST /apps/delete`
  - `GET /profiles`, `POST /profiles/add`, `POST /profiles/remove`
  - `GET /sessions`, `POST /sessions/{add,start,pause,stop,input}`
  - `POST /runners/start`
  - `GET /lobbies`, `POST /lobbies/{create,join,leave,stop}`
  - `GET /utils/get-icon`
  - `POST /docker/images/{inspect,pull}`
- **Notable for Cha.** `sessions/add` accepts a `StreamSession` with `client_ip`, `aes_key/iv`, `rtsp_fake_ip`, `video_width/height/refresh_rate` and `client_settings`, and **immediately starts the app and compositor**. `sessions/start` takes a full `VideoSession`/`AudioSession` **including an arbitrary `gst_pipeline` string**.
  - If the pipeline has no `appsink name=wolf_udp_sink`, Wolf runs it as-is. You could, for example, end in `udpsink host=127.0.0.1` or `shmsink` to a sidecar. The pipeline still won't start until something sends an **RTP ping with the session's secret payload** to Wolf's video/audio UDP ports.
  - `sessions/input` accepts **hex-encoded Moonlight input packets**.
  - Together this is an undocumented but source-verified way to drive Wolf from a non-Moonlight transport **without forking**. It is untested, and [PR #506](https://github.com/games-on-whales/wolf/pull/506) "Apps added over the API never get a display" shows the API path has rough edges.
- **wolf-ui** ([repo](https://github.com/games-on-whales/wolf-ui)) is a Godot/C# launcher that runs *as a streamed app*. It handles profiles, PINs, image pull and lobbies, and has a "return to Wolf UI" hotkey (Ctrl+Alt+Shift+W, or START+UP+RB). Issue [#522](https://github.com/games-on-whales/wolf/issues/522) reports Wolf UI eating GPU.
- **Wolf Den** ([repo](https://github.com/games-on-whales/wolf-den)) is an ASP.NET (.NET 10) web UI on the socket. Wolf has a branch `feat/embed-wolf-den` to ship it under supervisord.
- **fenrir** ([repo](https://github.com/games-on-whales/fenrir)) is a Go K8s operator with CRDs `App`, `User`, `Pairing` and `Session`:
  - `moonlight-proxy` terminates pairing/launch and maps client-cert fingerprints to `Pairing` CRs.
  - Each `Session` becomes a Pod with game, Wolf, wolf-agent and pulseaudio containers.
  - Port forwarding uses Cilium LB `lb-sharing-key`; Gateway API UDPRoute is listed as an alternative.
  - The README says "NOT IN A USEABLE STATE!", a single session at a time works, and the user is hard-coded to "alex".
  - The older [k8s-at-home `games-on-whales` Helm chart](https://artifacthub.io/packages/helm/angelnu/games-on-whales) predates Wolf.
  - [wolf#82](https://github.com/games-on-whales/wolf/issues/82) "Native Kubernetes Integration" is the top-commented open issue.

### 2.12 NVIDIA handling

There are two modes, auto-detected in `runners/docker.cpp`:
1. **Container Toolkit (≥ 1.16):** Wolf injects `DeviceRequests` + `Runtime: nvidia` + `NVIDIA_*` env. GoW `30-nvidia.sh` then synthesises the missing ICD/EGL/GBM JSON files and the `nvidia-drm_gbm.so` symlink, and prefers the host `libnvrtc`.
2. **Manual driver volume:** build `gow/nvidia-driver` with `NV_VERSION=$(cat /sys/module/nvidia/version)`, populate the docker volume `nvidia-driver-vol`, and set `NVIDIA_DRIVER_VOLUME_NAME`. The volume is mounted at `/usr/nvidia` in every app container and must be rebuilt on each driver update.

Both need `nvidia-drm.modeset=1` on the host. Driver ≥ 530.30.02 is required for the manual path.

Pain points:
- [#124](https://github.com/games-on-whales/wolf/issues/124) toolkit EGL exception, open since 2024.
- [#501](https://github.com/games-on-whales/wolf/issues/501) zero-copy crash on 2nd+ app launch.
- [#413](https://github.com/games-on-whales/wolf/issues/413) no audio on NVIDIA.
- [#298](https://github.com/games-on-whales/wolf/issues/298) multi-GPU.
- [#371](https://github.com/games-on-whales/wolf/issues/371) NixOS driver volume.
- A Blackwell black-screen fix landed in June 2026.
- Open PR [#467](https://github.com/games-on-whales/wolf/pull/467) documents **CDI**.

### 2.13 Roadmap signals: open PRs as of 2026-10-03

There is no published roadmap. These PRs are the de facto one:

- **#517 PyroWave** (bscubed, 2026-09-29).
  - Adds a `gstpyrowaveenc` element. It imports **DMA-BUF** compositor buffers into PyroWave's own Vulkan device, and Wolf deliberately doesn't link libvulkan.
  - Pins pyrowave `89f7e47`.
  - Adds the `SCM_PYROWAVE` capability bit and fails hard if the client asks for PyroWave without host support.
  - Introduces dynamic pacing (up to 3× send rate) and raises the 500 Mbit/s cap for streams above 1 Gbit/s.
  - Tested on 7900 XTX → Steam Deck over 2.5 GbE. The only client is **Aurora** (a Moonlight fork).
  - The issue ([#515](https://github.com/games-on-whales/wolf/issues/515)) also claims Steam Remote Play and the Sunshine fork **Solarflare** support PyroWave **(unverified here)**.
  - No maintainer review yet.
- **#450 `wolf:vulkan` image** (JBailes, draft). Fedora base, GStreamer 1.28.4 with `vulkanh264enc`/patched `vulkanh265enc`, zero-copy `VulkanImage` NV12/P010, `RADV_PERFTEST=lowlatencyenc`. Tested on 7900 XTX and RTX 5080.
- **#469 HDR end-to-end** (draft): P010, HEVC/AV1 Main10, BT.2020 PQ, Moonlight HDR_MODE control packets.
- **#495** nvh265enc intra-refresh patch. **#474** fake-uinput. **#513** shared-IP sessions. **#215** IPv6. **#261** background sessions. **#305** pause/resume containers.
- The maintainer merges slowly and carefully. Many PRs are drafts by contributors, and several, by their own description, were written with AI assistance (#513).

### 2.14 Privilege footprint (Cha node threat model)

**Wolf container:**
- `--network=host` (optional: fixed ports can be published instead).
- `/var/run/docker.sock` (root-equivalent).
- `-v /dev:/dev:rw`, `-v /run/udev:/run/udev:rw`.
- `--device /dev/dri /dev/uinput /dev/uhid`, `--device-cgroup-rule "c 13:* rmw"`.
- Runs as root under supervisord.
- Host prerequisites: the `85-wolf.rules` udev rules, plus `nvidia-drm.modeset=1` on NVIDIA.
- Proxmox LXC only works privileged. Rootless Podman has device-cgroup problems ([#477](https://github.com/games-on-whales/wolf/issues/477), closed).

**Steam app container:**
- `CapAdd SYS_ADMIN, SYS_NICE, SYS_PTRACE, NET_RAW, MKNOD, NET_ADMIN`.
- `SecurityOpt seccomp=unconfined, apparmor=unconfined`.
- `IpcMode host`.
- nofile 10240.
- `Privileged: false`.

This is fine for homelab and small groups (the user's stated scope). It is **not** a multi-tenant security boundary.

### 2.15 Notable pain points

From the [issues by comment count](https://github.com/games-on-whales/wolf/issues?q=is%3Aissue+is%3Aopen+sort%3Acomments-desc) and the recently updated list:
- K8s (#82).
- Shared game libraries (#83).
- Single-IP multi-user (#265).
- NVIDIA EGL (#124).
- Steam "Wayland events: Broken pipe" (#152).
- Black screen in DOOM: The Dark Ages (#248).
- Steam Input (#81).
- HDR (#222).
- Outdated Mesa in GoW for new hardware (#330).
- Lobby crash and leak (#364, #518).
- Steam UI stays on top of the game (#266).
- Steam overlay broken by global `MANGOHUD=1` (#520).
- Sway refresh reported as 0 / vsync max 60 (#519).

---

## 3. inputtino

[README](https://github.com/games-on-whales/inputtino/blob/stable/README.md)

- C++ core with C API, **Rust bindings** (`bindings/rust`, crate `inputtino`) and Python bindings. MIT.
- Devices: keyboard, mouse (relative and absolute), touchscreen, trackpad, pen tablet. Joypads: Xbox One (uinput), PS (uinput), **PS5 DualSense (uhid)** and Nintendo Switch Pro (uinput).
- Joypad features: rumble callbacks; DualSense gyro, accelerometer, touchpad, LED, battery and adaptive triggers.
- Each device can emit the udev events and hwdb entries needed for fake-udev injection (`get_udev_events()`, `get_udev_hw_db_entries()`).
- Recent fixes: touch MT slot reuse (2026-07), absolute-mouse buttons and scroll (2026-05).
- **Steal:** use directly from a Rust node agent. It's the best open implementation of DualSense-over-uhid.

---

## 4. GoW app images

### 4.1 Layering

([gow/images](https://github.com/games-on-whales/gow/tree/master/images))

- **`base`** (`ubuntu:25.04`; Fedora 43 variant in `build-fedora/`):
  - adds gosu;
  - `/entrypoint.sh` runs `/etc/cont-init.d/*.sh` as root:
    - `10-setup_user.sh` creates `retro` with `PUID`/`PGID`;
    - `15-setup_devices.sh` runs `ensure-groups $GOW_REQUIRED_DEVICES`, adding the user to the groups that own `/dev/input/*`, `/dev/dri/*` and `/dev/nvidia*`;
    - `30-nvidia.sh` sets up NVIDIA, either via the driver volume or the toolkit;
  - then `exec gosu retro /opt/gow/startup.sh`.
- **`base-app`**:
  - Sway, waybar, gamescope, MangoHud, sdl-jstest;
  - a **patched XWayland** built from the latest upstream tag with `xwl_output_fake_modes` neutralised, so arbitrary resolutions work;
  - `launch-comp.sh` provides `launcher()`:
    - `RUN_GAMESCOPE` → `gamescope -b -W/-H/-w/-h/-r -- app`, nested Wayland backend;
    - `RUN_SWAY` → generates `~/.config/sway/config` with `output * resolution WxH`, then `exec app && killall sway`, then `dbus-run-session -- sway --unsupported-gpu`;
    - otherwise runs the app directly.
- **`base-emu`**: launchers for Cemu, Citron, Dolphin, PCSX2, RetroArch, RPCS3, xemu and Xenia.
- **Apps:** steam, xfce, firefox, retroarch, lutris, heroic-games-launcher, pegasus, es-de, kodi, prismlauncher, plex, emby, youtube. Images are published as `ghcr.io/games-on-whales/<app>:edge`; tags beyond `edge` and `fedora-43` were **(unverified)**.
- **There is no KDE Plasma image.** The desktop is **XFCE**, which runs **rootful Xwayland `:0`** as a client of Wolf's compositor, then `startxfce4`, plus Flatpak/Flathub.

### 4.2 Steam image

[apps/steam/build/Dockerfile](https://github.com/games-on-whales/gow/blob/master/apps/steam/build/Dockerfile)

**Build:**
- Debian/Ubuntu `steam` package (i386 multiarch), with NVIDIA userland libraries explicitly *avoided* (`nvidia-driver-libs-`).
- dbus, NetworkManager and bluez for the Big Picture first-run flows.
- Fake `steamos-update`, `steamos-session-select` and `jupiter-biosupdate`, plus a `steamos-dbus-watchdog.sh` that stops Steam on Power → Off.
- **Decky Loader v3.2.3 baked in** (it used to be fetched at runtime and hit GitHub rate limits).

**Patched bubblewrap.** The build compiles upstream bubblewrap with [`ignore_capabilities.patch`](https://github.com/games-on-whales/gow/blob/master/apps/steam/build/ignore_capabilities.patch). The patch deletes the `die("Unexpected capabilities but not setuid…")` branch. As the Dockerfile puts it: "bwrap needs CAP_SYS_ADMIN. However if you explicitly give CAP_SYS_ADMIN, bwrap throws an error". This is what lets **pressure-vessel / Steam Linux Runtime** containers work without `--privileged`. It comes paired with `seccomp=unconfined` and `apparmor=unconfined`.

**Startup (`startup.sh`):**
- `STEAM_STARTUP_FLAGS` defaults to `-bigpicture`. Setting `steam://rungameid/<id>` direct-launches a game.
- Deck-style environment: `STEAM_USE_MANGOAPP`, `STEAM_USE_DYNAMIC_VRS` + `RADV_FORCE_VRS_CONFIG_FILE`, `STEAM_GAMESCOPE_FANCY_SCALING_SUPPORT`, `SRT_URLOPEN_PREFER_STEAM`, and the ibus Steam on-screen keyboard (`QT_IM_MODULE=steam`).
- **Gamescope mode** runs `gamescope -e -R <startup socket> -T <stats pipe> -W/-H -w/-h -r` in the background (embedded "Steam" mode, Deck-like). It reads `DISPLAY`/`GAMESCOPE_WAYLAND_DISPLAY` back from the socket, starts `mangoapp`, then `dbus-run-session -- steam`.
  - `STEAM_MULTIPLE_XWAYLANDS` is commented out because it "breaks without the steamdeck flags".
  - Docs warn gamescope can be unstable on some NVIDIA drivers ([#60](https://github.com/games-on-whales/wolf/issues/60)) and handles multi-window apps badly.
- **Sway mode** (the default) sets `MANGOHUD=1` globally, which is now known to break the Steam overlay ([#520](https://github.com/games-on-whales/wolf/issues/520)). It then launches Steam via `launcher`.

**Docs** ([steam.adoc](https://games-on-whales.github.io/wolf/stable/apps/steam.html)):
- The Steam overlay doesn't work headless; use MangoHud.
- Shared libraries are mounted at `.steam/debian-installation/steamapps` and must be owned by 1000:1000.
- A first-run permission fix-up script exists.
- ProtonGE goes into `compatibilitytools.d`.

**Fedora variant (April 2026):** migration bugs fixed in that window include login state being wiped every boot and the steamapps migration.

### 4.3 Persistence model

- Home is `HOST_APPS_STATE_FOLDER/profile_data/<profile_id>/<app_title>` bind-mounted to `/home/retro`, separate per profile and per app. Wolf resolves the host path dynamically even if `/etc/wolf` is mounted elsewhere.
- Everything outside home is ephemeral, so a container image update equals a "system" reset. This is the Kasm model (persistent profile, ephemeral system) and maps directly onto Cha's "persistent vs ephemeral environment" toggle.

---

## 5. Putting Wolf behind a browser, and Kubernetes

### 5.1 HelixML

HelixML ran a fork at [helixml/wolf](https://github.com/helixml/wolf) plus a fork of moonlight-web-stream.

- **[Technical deep dive](https://blog.helix.ml/p/technical-deep-dive-on-streaming)** (HN thread [45825121](https://news.ycombinator.com/item?id=45825121), about Nov 2025): Wolf + moonlight-web as a WebRTC adapter, plus work to let several clients share one session. Lobbies came out of the wolf-ui branch.
- **["We Killed WebRTC (And Nobody Noticed)"](https://blog.helix.ml/p/we-killed-webrtc-and-nobody-noticed)** (2025-12-11):
  - Enterprise firewalls blocked UDP 3478 and 49152–65535. TURN TCP fallback "unreliable and adds 50-100ms", and one customer saw an 80% connect rate.
  - New path: Wolf → Moonlight → **raw H.264 NAL units over WebSocket** → **WebCodecs** with no jitter buffer. Input uses Moonlight binary messages over the same WebSocket. Audio uses `AudioDecoder` and drops frames more than 100 ms late.
  - Claimed result: "20-30ms lower end-to-end latency" than their WebRTC.
  - Costs: no ABR and TCP head-of-line blocking.
- **["We Mass-Deployed 15-Year-Old Screen Sharing Technology"](https://blog.helix.ml/p/we-mass-deployed-15-year-old-screen)** (2025-12-18): adaptive fallback to **JPEG polling** at 2–10 fps when RTT exceeds 150 ms. Wolf was still in use at that point.
- **2026:** the [Helix sandbox runtime docs](https://helix.ml/docs/ref-sandbox-runtimes) describe "Desktop renders to Wayland compositor → PipeWire captures → GStreamer H.264 → WebSocket". The modes are `zerocopy` (DMA-BUF → CUDA → NVENC), `native` (VA) and `shm`, with **no mention of Wolf or Moonlight**. PRs in Oct 2026 reference `pipewirezerocopysrc`. **Inference (unverified): Helix has replaced Wolf with its own PipeWire-based capture.** Lesson for Cha: the Moonlight hop became a liability once the browser was the main client.

### 5.2 moonlight-web-stream and moonlight-common-rust

- **[moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream)** (Rust/actix + TS, GPL-3.0-or-later, `v3.0.0-prerelease.7`, commit 2026-10-03).
  - It acts as a **Moonlight client on the server side** and re-transports to the browser over **WebRTC** (`webrtc`/`rtc` 0.21 crates) or a **WebSocket + WebCodecs** fallback.
  - It needs HTTPS for the Gamepad, Keyboard Lock and `VideoDecoder` APIs. The README documents TURN, a fixed port range and NAT 1:1 for WAN.
- **v3 swaps moonlight-common-c for [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust)** (GPL-3.0).
  - It is a **Sans-IO** Moonlight protocol core that "compile[s] to WebAssembly and run[s] in the browser, where networking is provided externally (e.g. WebRTC, **WebTransport**, Direct Sockets in IWA's)".
  - Its feature table lists Sunshine, **Wolf**, Apollo and Foundation Sunshine as supported hosts.
  - Not yet supported: AV1, RFI/LTR and video encryption.
  - This is the best building block for Cha's feature 8 (external Moonlight/Sunshine/Vibepollo endpoints). It is GPL-3, which is acceptable under the copyleft decision.
- **[linckosz/moonlight-web](https://github.com/linckosz/moonlight-web)** (C++/Qt, GPL-3.0, about 96★):
  - embeds moonlight-common-c → WebRTC or WSS;
  - auto-pairs with Wolf by posting the PIN to Wolf's `/api/v1`;
  - supports Wolf profiles and lobbies;
  - needs **one client certificate per player**.

### 5.3 Wolf-native ways to get frames out without Moonlight

All verified in source:

1. **API custom pipeline:** `sessions/add` + `sessions/start` with a free-form `gst_pipeline` (§2.11), then send an RTP ping to trigger it. Input goes through `sessions/input`, one HTTP request per Moonlight input packet. This works without a fork but is hacky, and API-path bugs exist (#506).
2. **Config-level override:** per-app `[apps.video] source` and the `gstreamer.video.default_sink` strings can replace `rtpmoonlightpay_video ! appsink` with any GStreamer sink. Session setup still requires a Moonlight handshake.
3. **Fork** and add a transport-agnostic "external consumer": the encoded `GstBuffer` goes to an in-process QUIC/WebTransport server or a unix-socket ring for a sidecar.

---

## 6. Alternatives for Steam in Docker

| | **Wolf + GoW Steam** | **Steam-Headless v1** | **Steam-Headless v2** | **linuxserver/steam** | **gamescope headless + PipeWire (DIY)** |
|---|---|---|---|---|---|
| Display | Wolf compositor → nested Sway/gamescope (XWayland inside) | Xorg (dummy driver or host X) + XFCE | **KDE Plasma 6 Wayland, KWin patched for virtual headless outputs**, or "gaming" = nested gamescope + Gamepad UI | Selkies baseimage (pixelflux Wayland compositor + labwc, X11/Openbox fallback) | `gamescope --backend headless` (BSD-2), PipeWire node exporting **BGRx or NV12** as DmaBuf or MemFd (`src/pipewire.cpp`) |
| Streaming | Moonlight (Wolf) | Sunshine (Moonlight), noVNC/websockify, Neko (WebRTC) | Sunshine + **SHUI** browser streaming (WebRTC **and** WebSocket) on :8483 | Selkies: WebSocket default, WebRTC opt-in; Opus via pcmflux | Bring your own encoder/transport |
| Multi-session | **Yes**: N sessions per host/GPU, one Wolf | One per host (host network) | One per container. Multiple sessions via **macvlan** (one LAN IP each). Docs claim GPU encode time-slicing across containers. | One per container | One gamescope per session |
| Privileges | Wolf: docker.sock, /dev, udev. Steam: SYS_ADMIN/NICE/PTRACE/NET_ADMIN/MKNOD, seccomp/apparmor unconfined | NET_ADMIN, SYS_ADMIN, SYS_NICE, seccomp/apparmor unconfined, ipc host, /dev/uinput, /dev/fuse | SYS_ADMIN, SYS_PTRACE, NET_ADMIN, DAC_READ_SEARCH, SYS_NICE; `writable-cgroups`, `seccomp=unconfined`, `systempaths=unconfined`; /dev/dri, /dev/fuse, /dev/uinput, /dev/uhid; **systemd as init** | seccomp/apparmor unconfined (bwrap userns); NVIDIA driver ≥ 580 | Depends |
| Input | inputtino (uhid DualSense) + fake-udev hotplug, per-session isolation | uinput + **[dumb-udev](https://github.com/Steam-Headless/dumb-udev)** (Python udevd replacement, GPL-3) | `shinputd` "causal input seat isolation" via `pidfd_open`/`pidfd_getfd`. Physical passthrough regressed vs v1 ([#262](https://github.com/Steam-Headless/docker-steam-headless/issues/262)). | Userspace gamepad interposer (Proton) | Your choice (inputtino) |
| License | MIT | GPL-2.0 | Images public. v2 source not located on GitHub **(unverified)**. Frontend GPL-3. | GPL-3 (LSIO) **(unverified)** | BSD-2 |
| Sources | this doc | [repo](https://github.com/Steam-Headless/docker-steam-headless) | [steamheadless.com](https://steamheadless.com/), [docs](https://steamheadless.com/docs/), [Unraid template](https://ca.unraid.net/apps/steam-headless-1jp6pfc11mmiyk), Docker Hub tags | [docs](https://docs.linuxserver.io/images/docker-steam/) (initial release 2026-01-09), [selkies baseimage](https://docs.linuxserver.io/images/docker-baseimage-selkies/) | [gamescope](https://github.com/ValveSoftware/gamescope) |

Others, minor:
- [dlommm/steamos-headless](https://github.com/dlommm/steamos-headless): real SteamOS 3.9 packages + gamescope/Plasma + Sunshine. Privileged, host network, one session per container, MIT, 0★.
- linuxserver/steamos is **deprecated**.

**Takeaways:**
- Steam Big Picture plus gamescope inside a container is now routine. The hard parts are:
  - bwrap/pressure-vessel privileges (patched bwrap, or seccomp/apparmor unconfined plus userns);
  - Steam Input / uinput isolation;
  - NVIDIA userland injection;
  - the dbus/NetworkManager/bluez stubs Big Picture expects.
- Wolf/GoW is the only option that does **multi-session on one GPU with per-session input isolation** in a single daemon.
- Steam-Headless v2 bets on "one full Plasma desktop per container + macvlan".

---

## 7. Verdict: how Cha Portal should make Steam first-class

### Option (a): run Wolf unmodified on nodes, bridge Moonlight → browser

- **How:** the Cha node compose stack includes the `wolf:stable` container and a Cha "node agent". The agent:
  - drives Wolf's unix-socket API for pairing, apps, profiles, lobbies and SSE events;
  - runs a **Moonlight client** (moonlight-common-rust or moonlight-common-c) per viewer on loopback;
  - re-packetises to WebTransport/WebRTC/WebSocket for the browser.
- **Pros:**
  - Zero streaming-core work; everything in §2 works today, including DualSense, hotplug, lobbies, zero-copy on 3 vendors and the GoW catalogue.
  - **Native Moonlight/Artemis/Aurora clients work for free**, which covers feature 8 in reverse.
  - The MIT core can be swapped out later.
  - Proven by Helix and both moonlight-web projects.
- **Cons:**
  - An extra hop: depacketise RTP/FEC, decrypt, re-send. It costs little on loopback but adds complexity.
  - We inherit Moonlight's limits: no ABR or resolution change mid-stream, NV12/8-bit only on stable, FEC/packetisation tuned for UDP LAN, one session per client cert, single-IP edge cases (#265).
  - **PyroWave to the browser only works if Wolf merges #517** and our bridge passes the bitstream through; there is also no browser PyroWave decoder.
  - Wolf's stability issues (lobby leak #518, NVIDIA #124/#501) become ours.
- **Fit:** good for an MVP and for LAN. For WAN the browser leg is ours anyway; only the loopback leg is Moonlight.

### Option (b): embed Wolf components in our own node streamer (recommended end-state)

- **How:** a Rust `cha-streamer`/node agent that does the following.
  - **Compositor:** links **`wayland-display-core`** (MIT `rlib`), or runs `waylanddisplaysrc` in gstreamer-rs. Take the **`feat/vulkan-encode` VulkanImage NV12/P010 path** (which is what a Vulkan-compute codec like PyroWave wants), with DMA-BUF and CUDA paths for VA/NVENC.
  - **Input:** **inputtino** (Rust bindings) for pads and pen; compositor events for keyboard, mouse and touch.
  - **Hotplug:** ports **fake-udev** (about 300 LOC, MIT, or reuse the binary).
  - **Apps:** implements the **GoW container contract** (§2.3) so GoW images (Steam, XFCE, Firefox, …) run unmodified, using `bollard` (or a similar Rust Docker client) against the Docker socket.
  - **Audio:** per-session sinks on PipeWire-pulse or PulseAudio, plus Wolf's hostname-based sink-input routing trick.
  - **Encoders:** GStreamer (nvcodec/va/qsv/vulkan) and **PyroWave** behind one trait.
  - **Transport:** our own. WebTransport/QUIC datagrams to browsers and the thin native client, a WebRTC fallback, a WebSocket last resort, ABR and intra-refresh designed for WAN.
- **Pros:**
  - Full control of rate control, pacing, FEC, resolution changes and codec negotiation, which is the PyroWave-first requirement.
  - One language for the node agent.
  - The Cha control plane owns sessions, lobbies and persistence, so there are no Moonlight-shaped concepts (pairing certs, RTP ping, fake IPs).
  - The hardest-won pieces (compositor zero-copy caps negotiation, DualSense uhid, NVIDIA userland in containers, the Steam container recipe) are reused, not rewritten.
  - All MIT, compatible with AGPL.
- **Cons:**
  - We rewrite Wolf's generic glue: Docker runner, session state, lobbies, Pulse routing, device queues. At a guess that is roughly 5k of Wolf's 21k LOC; this split was not measured.
  - We track a Smithay fork pinned by gst-wayland-display.
  - Native Moonlight clients lose access unless Wolf also runs on the node.
- **Evidence this is the industry direction:** Helix moved from Wolf+Moonlight to its own capture → GStreamer → WebSocket stack (§5.1). Steam-Headless v2 built its own compositor and input daemon plus its own browser streaming, keeping Sunshine only for Moonlight. Both teams kept Moonlight as an *optional* compatibility path, not the core.

### Option (c): fork Wolf and add our transport and codec

- **How:** add a "Cha session" type: API-created, no RTP-ping gate, with a consumer sink handing encoded buffers to our transport. Merge #450, #469 and #517 into the fork.
- **Pros:**
  - The fastest way to get PyroWave, HDR and Vulkan encode plus multi-session together, since all three PRs exist.
  - The producer/consumer split (§2.2) makes the transport tail straightforward.
  - MIT permits it.
- **Cons:**
  - A C++20 immer/event-bus codebase.
  - Moonlight assumptions are spread across session state: client certs, AES keys, ping gating, ports, and "pause = kill pipeline".
  - Upstream is very active (152 commits in 2026, with big PRs in flight), so a fork rots quickly.
  - The open lobby and NVIDIA stability issues stay ours.
  - Mixing a second transport into Wolf duplicates what (b) gives us more cleanly.
- **Use instead:** **upstream** small, generic hooks that benefit both projects, for example "API session without RTP ping + external sink", the Vulkan producer, and PyroWave (#517). Then run Wolf unmodified.

### Recommendation

1. **Phase 0–1 (bootstrap, LAN-first demo):** (a). Ship Wolf unmodified in the Cha Node compose stack. Cha Portal manages it through the unix-socket API, exposed only to the node agent. The node agent bridges to the browser using **moonlight-common-rust** (GPL-3) and owns the WAN transport. Steam/GoW work on day one, and native Moonlight clients work too.
2. **Phase 2 (core):** build `cha-streamer` per (b), starting with `wayland-display-core` + inputtino + the GoW contract + GStreamer encoders + our transport. Add PyroWave on the VulkanImage path.
3. Keep Wolf as an optional per-node **"Moonlight compatibility runtime"**. Contribute upstream instead of forking.

---

## 8. Steal list for Cha Portal

1. **The GoW container contract** (§2.3): env, mounts and devices. Implement it exactly and the GoW catalogue (Steam, XFCE, Firefox, RetroArch, Lutris, Heroic, ES-DE, Pegasus, Kodi, Prism) runs unmodified under our streamer.
2. **Producer/consumer split:** one compositor/capture producer per environment, N encoder/transport consumers per viewer, hot-switchable sources (Wolf's interpipe + `SwitchStreamProducerEvents`). This covers co-op, spectators, and per-viewer resolution/codec (Moonlight on LAN, PyroWave on a LAN browser, AV1 over WAN) from the same environment.
3. **Lobbies as the persistence primitive:** an environment's lifetime is independent of viewer sessions, with `stop_when_everyone_leaves`, PINs and multi-user. That maps 1:1 onto Cha's ephemeral and persistent environments.
4. **gst-wayland-display's zero-copy matrix:** DMABuf with explicit modifiers for VA, EGLImage → CUDAMemory for NVENC, and the VulkanImage NV12/P010 compute converter for Vulkan Video and PyroWave. Also `render_node=software` for GPU-less nodes.
5. **Input split:** keyboard, mouse and touch go into the compositor and never become host devices. Only pads and pens become kernel devices (inputtino), using **uhid DualSense** for gyro and touchpad.
6. **`85-wolf.rules`** (seat9 + strip `uaccess`) and **fake-udev** for per-container hotplug without a host udev leak. The Cha Node installer must lay down the udev rules on the host.
7. **Patched bwrap + capability set** for Steam/pressure-vessel without `--privileged`. Also: Decky baked in, steamos-* stubs, and the `steam://rungameid/<id>` direct-launch env.
8. **NVIDIA dual-mode handling:** the toolkit/CDI path with ICD/EGL/GBM JSON synthesis (`30-nvidia.sh`) and the driver-volume fallback.
9. **PulseAudio routing by container hostname** so misbehaving apps can't escape their session sink.
10. **Encoder fallback lists with `check_elements`**, vendor filtering, and pipeline-string overrides in config, which gives power users a lot of hackability.
11. **`sendmmsg` batching + per-ms packet pacing** for UDP video (`streaming.cpp`), useful for our native-client QUIC/UDP path.
12. **Patched XWayland without fake modes**, so arbitrary client-native resolutions such as 2256×1504 work inside nested sessions.
13. **Wolf API shape**: REST over a unix socket, an OpenAPI schema, and an SSE event stream. It is a good model for the Cha node-agent ↔ portal control channel, with mTLS added for WAN.
14. **moonlight-common-rust** (Sans-IO, WASM-capable) for connecting to external Moonlight/Sunshine/Vibepollo/Wolf endpoints. It could even run in the browser over WebTransport via a relay.

---

## 9. Risks and gotchas

- **No releases:** Wolf ships via the rolling `:stable` image; the last tag is 2024-07. Pin image digests in the Cha Node compose.
- **Host prerequisites can't be fully containerised:** udev rules, `nvidia-drm.modeset=1`, an NVIDIA toolkit ≥ 1.16 or CDI, and `/dev/uinput`/`/dev/uhid` permissions. Proxmox LXC must be privileged.
- **Docker socket = root.** Any component holding it (Wolf or our agent) is node-root. That is acceptable for homelab scope, but document it.
- **Steam Input** in unprivileged containers is unsolved upstream (#81, draft #474).
- **Moonlight shape:** one session per client cert, plus single-IP ambiguity (#265, #513). Any bridge must mint per-viewer client certs.
- **Fixed resolution and bitrate per session.** Pause kills the encoder pipeline. Changing the resolution of a running lobby is unsupported.
- **NVIDIA:** EGL errors with the toolkit (#124), zero-copy crash on relaunch (#501), first-GPU-only NVENC (#298), and consumer NVENC session caps **(unverified number)**.
- **Lobby stability:** OOM leak on leave (#518), crash on join (#364), audio CPU spike after reconnect (#523).
- **The GoW Mesa version** lags new hardware (#330). We may need our own image builds or Fedora variants.
- **Gamescope nested** is flaky on some NVIDIA drivers, and Sway is the default for Steam.
- **The Vulkan encode path** (gst-wayland-display branch + patched `vulkanh265enc`) relies on **out-of-tree GStreamer patches**, which is a maintenance burden if we adopt it before upstream lands them.

## 10. Open questions

1. Will Wolf merge PyroWave (#517), Vulkan (#450) and HDR (#469)? Ask ABeltramo about timelines and whether an "external transport" hook would be accepted upstream.
2. Can `wayland-display-core` be used standalone, without GStreamer in our process? The crate depends on `gst`/`gst-video`, so the answer is probably no. GStreamer as a dependency is likely acceptable.
3. Can KDE Plasma run nested as a client of gst-wayland-display (`kwin_wayland` with the Wayland backend)? GoW has no Plasma image and Steam-Headless v2 had to patch KWin for headless outputs. Needs a spike.
4. Is the Steam-Headless v2 source (patched KWin, `shinputd`, SHUI WebRTC/WebSocket) public, and under what license?
5. Is Steam Remote Play's PyroWave support (claimed in wolf#515) real? Does it imply gamescope's PipeWire path plus a PyroWave encoder we could reuse?
6. Does moonlight-common-rust cover enough of Wolf's protocol extensions (secret payloads, RTSP fake IP) for multi-viewer bridging, given it lacks AV1 and video encryption?
7. How many concurrent sessions per consumer GPU do we target? We need an NVENC session-limit check and VRAM budgeting per gamescope or Plasma environment.
