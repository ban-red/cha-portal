# 06: Node-side Linux graphics, capture, input, audio and GPU-in-container stack

> Background research from planning ([index](README.md)). What Cha Portal actually ported is in [PROVENANCE.md](../PROVENANCE.md).

Research slice 06 for **Cha Portal**. Written 2026-10-03.

Sources are linked inline. Wherever a claim comes from reading source code, the repo and commit are given. Shallow clones live in the session scratchpad under `repos/06/`. Anything I could not verify is marked **(unverified)**.

Constraints from the user decisions (2026-10-03):
- The project is copyleft OSS (AGPL/GPL).
- Homelab or small-group scale, with mostly trusted users. Containers are acceptable isolation. MicroVMs are optional and can come later.
- LAN and WAN are equally first-class.

---

## TL;DR

1. **Run the compositor yourself. Do not scrape someone else's.** Every serious 2026 project builds a headless Smithay compositor and hands GPU-resident DMA-BUFs straight to the encoder:
   - Wolf uses `gst-wayland-display`.
   - linuxserver/Selkies use `pixelflux`.
   - Nestri uses `nescope`.

   Cha Portal should do the same, either with its own Rust/Smithay "session compositor" or by reusing gst-wayland-display (MIT) or pixelflux (MPL-2.0). Inside that compositor, nest:
   - **gamescope** for Steam and games.
   - **KWin** for a full KDE Plasma desktop.
   - **Chrome** directly, using `--ozone-platform=wayland`.

2. **Full KDE Plasma in an unprivileged container is solved.** linuxserver webtop (`ubuntu-kde`, 2026) runs `kwin_wayland --xwayland` *nested* as a Wayland client of pixelflux's compositor.
   - It runs under `dbus-run-session` with PipeWire and WirePlumber.
   - It needs no systemd, no logind and no Sysbox.
   - It needs only `--shm-size=1gb` and `--device /dev/dri`. On NVIDIA, add `--gpus all --device /dev/nvidia-modeset`.

   A more native alternative also exists, but nobody ships it in a container yet:
   - `kwin_wayland --virtual` renders with GBM/EGL on a render node.
   - Frames come out through KWin's own screencast. It exports PipeWire DMA-BUF with modifiers, cursor metadata, damage metadata and explicit sync.
   - Input goes in through KWin's EIS (libei) D-Bus endpoint.

3. **Zero-copy capture to encode works on every vendor.** The import path differs:
   - **AMD and Intel:** import the DMA-BUF into VA-API, or into Vulkan for Pyrowave.
   - **NVIDIA, Sunshine's path:** EGLImage → GL → `cuGraphicsGLRegisterImage` → NVENC.
   - **NVIDIA, Polaris's path:** LINEAR DMA-BUF → Vulkan copy → CUDA → NVENC.
   - **Everyone:** Vulkan Video encode.

   **Pyrowave** imports DMA-BUFs with DRM modifiers directly. It has a dedicated NV12 path for PipeWire interop and an RGB→YCbCr scaled-encode path, so it fits this pipeline naturally.

4. **NVENC cap: NVIDIA's matrix now lists 12 concurrent sessions on every GeForce** (fetched 2026-10-03). The cap was 8 from Jan 2024. AMD and Intel VA-API have no artificial cap. **Pyrowave runs on compute, so it uses no NVENC sessions**, which matters for multi-session consumer nodes.

5. **Gamepads require `/dev/uinput` and `/dev/uhid` on the host kernel.**
   - inputtino (MIT) emulates DualSense fully over uhid: gyro, touchpad, adaptive triggers, LEDs.
   - Wolf hot-plugs devices into app containers with `--device-cgroup-rule` plus `mknod` plus a **fake-udev** netlink broadcast.
   - Host udev rules keep the virtual pads away from the host seat.
   - Keyboard and mouse should skip uinput and go straight into the compositor. Use Wayland seat injection, or gamescope's EIS socket `<display>-ei`.

6. **Steam is the privilege outlier.** Its pressure-vessel/bwrap sandbox needs user namespaces.
   - Docker's default seccomp allows `unshare`, `mount` and `clone` with namespace flags only together with `CAP_SYS_ADMIN`. `clone3` returns ENOSYS. I verified this against `moby/profiles`.
   - Ubuntu 24.04+ AppArmor also blocks unprivileged userns.
   - Wolf's answer is `SYS_ADMIN` + `seccomp=unconfined` + `apparmor=unconfined`, plus a **patched bwrap**.

   Better options:
   - A custom seccomp/AppArmor profile.
   - Podman. Its default seccomp allows `unshare` and `clone`.
   - `proot-bwrap` (Selkies). It needs no namespaces but makes syscalls slow.

7. **Isolation for the homelab target: plain containers, no `--privileged`.** The node agent is the only root-equivalent piece. It holds the docker socket, uinput/uhid and the host udev rules.

   Future microVM options:
   - virtio-gpu **DRM native context** (AMD/Intel, upstream).
   - Nestri's **virtio-nvgpu** (NVIDIA, announced 2026-09-26, experimental, Linux guests only). By its own description, it gives "roughly the same trust boundary as Docker".

   Ruled out:
   - gVisor: nvproxy has no DRM, which kills compositors.
   - Kata: whole-GPU VFIO only.
   - Sysbox: no release since v0.7.1 in July 2024.

8. **Windows guests cannot share a consumer GPU.** The options are:
   - Whole-GPU VFIO passthrough.
   - Intel Arc Pro B50/B60 SR-IOV VFs.
   - Licensed NVIDIA vGPU.

   Run **Vibepollo/Sunshine inside the guest** and treat it as an external Moonlight endpoint. Later, a Cha Windows agent could use Pyrowave's D3D11/12 interop.

---

## 1. Reference implementations: how others run desktops in containers (2026)

| Project | Compositor / display | Capture → encode | Input | Container privileges | License |
|---|---|---|---|---|---|
| **Wolf** (games-on-whales, `stable` @ facb8e0, 2026-09-29) | `gst-wayland-display`: Smithay micro-compositor running inside Wolf. Apps connect to its Wayland socket. Sway or gamescope are nested for Xwayland. | GStreamer: `waylanddisplaysrc` DMA-BUF / CUDAMemory → `nvh26xenc`, `vah26xenc`, `qsv`. Interpipe fan-out to N clients. | Keyboard and mouse go into the compositor. Pads use inputtino (uinput/uhid) + fake-udev. | Wolf itself has the docker socket, `/dev/uinput`, `/dev/uhid`, `-v /dev:/dev`, `/run/udev` and `c 13:* rmw`. The Steam app container has `SYS_ADMIN, SYS_NICE, SYS_PTRACE, NET_RAW, MKNOD, NET_ADMIN`, `seccomp=unconfined`, `apparmor=unconfined`, `IpcMode=host`, nofile 10240, and device cgroup `c 13:*`, `c 244:*`. | MIT |
| **linuxserver webtop / baseimage-selkies** (2026-10-02) | `pixelflux`, a Smithay headless compositor, on `wayland-1`. labwc for single apps. KDE is `kwin_wayland --xwayland` nested. X11 fallback is a patched Xvfb with glamor. | pixelflux (Rust/PyO3). Wayland: compositor DMA-BUF → NVENC/VA-API in place. X11: DRI3 blit into GBM DMA-BUFs, or NvFBC on NVIDIA. JPEG/x264 fallback with damage-striped encoding. | Selkies injects into the compositor. Gamepads use an `LD_PRELOAD` interposer + **fake libudev**, so no uinput is needed. | `--shm-size=1gb`, `--device /dev/dri`. NVIDIA adds `--runtime nvidia --gpus all --device /dev/nvidia-modeset` and needs driver ≥580; ≥595.80 needs no kernel parameters. Steam runs "without additional permissions" through `proot-bwrap`. | GPL-3.0 (baseimage). MPL-2.0 (Selkies, pixelflux) |
| **Selkies** core (`main` @ 5927ca7) | Attaches to X11 by default. pixelflux Wayland behind a flag. | pixelflux + pcmflux (Opus from PulseAudio). WebSocket by default, WebRTC opt-in. | XTEST, or Wayland via pixelflux | "No dependencies that require access to special devices… not dependent on systemd." | MPL-2.0 |
| **Kasm / KasmVNC** | Xvnc with DRI3 (Intel/AMD). NVIDIA via EGL/VirtualGL. | KasmVNC added a video mode with VA-API/NVENC H.264/H.265/AV1 ([docs](https://kasmweb.com/kasmvnc/docs/master/gpu_acceleration.html)) | X11 | Standard containers | KasmVNC GPL-2.0 **(unverified)** |
| **Nestri** (`dev` @ ef32d5f, mid-rewrite) | **microVM** via nesbox (Firecracker-derived rust-vmm, virtio over PCIe). `nescope` is a Smithay headless compositor with XWayland and gamescope-WSI. | **nescapture**: a Vulkan *implicit layer* inside the game process. It blits on present → compute RGB→NV12/P010 → **Vulkan Video** encode on the game's own VkDevice. `nespyro` is a Rust port of Pyrowave. QUIC transport. | `nesgamepad` uses uhid replicas (DS4) and uinput, and writes its own udev DB entries and netlink broadcasts | `/dev/kvm`. Not runnable in Docker. | Apache-2.0 (nespyro is MIT, matching upstream Pyrowave) |
| **steamos-containerized** ([repo](https://github.com/jackhric/steamos-containerized)) | gamescope session | Sunshine/Wolf-style | uinput, USB/IP passthrough | `nvidia.com/gpu=all` (CDI), uinput, `c 13:*`, `seccomp/apparmor=unconfined`, `SYS_ADMIN, SYS_NICE, SYS_PTRACE, MKNOD, NET_*`, `ipc: host` | (unverified) |

Primary-source notes:
- **Wolf** architecture: [how-it-works.adoc](https://github.com/games-on-whales/wolf/blob/stable/docs/modules/dev/pages/how-it-works.adoc). Default app configs: [`config.include.toml`](https://github.com/games-on-whales/wolf/blob/stable/src/moonlight-server/state/default/config.include.toml). Quickstart flags: [quickstart.adoc](https://github.com/games-on-whales/wolf/blob/stable/docs/modules/user/pages/quickstart.adoc).
  - Wolf can run "very unprivileged… without uinput/uhid" if you accept mouse and keyboard only. In that mode, input goes straight into the compositor.
  - On NVIDIA, Wolf still calls the Container Toolkit path "not as stable as the manual method" ([issue #152](https://github.com/games-on-whales/wolf/issues/152)). Its manual method mounts a driver volume built to match `/sys/module/nvidia/version`. Toolkit ≥1.16 and driver ≥530.30.02 are required, along with `nvidia-drm modeset=1`.
- **webtop KDE** ([`ubuntu-kde` branch](https://github.com/linuxserver/docker-webtop/tree/ubuntu-kde)):
  - `root/defaults/startwm_wayland.sh` sets `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`, `QT_QPA_PLATFORM=wayland` and `XDG_SESSION_TYPE=wayland`.
  - It starts `pipewire`, `wireplumber`, `kded6` and `plasmashell` under `dbus-run-session`.
  - `kwin-xwayland.py` pre-binds `/tmp/.X11-unix/X1` and execs `kwin_wayland --no-lockscreen --xwayland --xwayland-fd=…` with `WAYLAND_DISPLAY=wayland-1`, which is the pixelflux compositor.
  - Chromium runs with **`--no-sandbox`** under Docker's default seccomp.
- **linuxserver GPU guide** ([docs](https://docs.linuxserver.io/selkies/user-guide/gpu/)):
  - `DRINODE` selects the render GPU. `DRI_NODE` selects the encode GPU.
  - Zero-copy needs both on the same device. Otherwise it falls back to readback.
  - Intel/AMD VA-API has no 4:4:4 H.264, so FullColor forces x264.
- **baseimage-selkies NVIDIA fixups** (`root/etc/s6-overlay/s6-rc.d/init-video/run`). The toolkit "may not place files correctly", so the init script writes these if they are missing:
  - `/etc/vulkan/icd.d/nvidia_icd.json` pointing at `libGLX_nvidia.so.0`.
  - `/etc/glvnd/egl_vendor.d/10_nvidia.json`.
  - The OpenCL ICD.
  - A fix for `nvidia-drm_gbm.so` linkage.

  Cha Portal environment images will need the same defensive init.

---

## 2. Headless and virtual compositors usable in containers

### 2.1 Comparison

| Compositor | Headless mode | GPU path | Full KDE? | Chrome? | Dynamic resize | Capture interface | Input injection | Requirements |
|---|---|---|---|---|---|---|---|---|
| **gamescope** (master @ 0e590c7, 2026-10-01) | `--backend headless` ("no window, no DRM output"). `--backend wayland` to nest. | Vulkan compositor. Renders on the render node. `--prefer-vk-device`. | No | Possible but pointless | Fixed at launch via `-W/-H` and `-w/-h`. The PipeWire consumer can request a scaled size (`SPA_FORMAT_VIDEO_requested_size`). Runtime change of the nested mode is **(unverified)**. | **PipeWire node**: BGRx or NV12. DMA-BUF only with **`DRM_FORMAT_MOD_LINEAR`**, else MemFd. Meta: Header and `requested_size_scale` only, so **no cursor or damage meta**. Also `gamescopestream` client. | **EIS server** at `$WAYLAND_DISPLAY-ei` (`LIBEI_SOCKET` exported, "XTEST emulation"). libinput when real devices exist. | `CAP_SYS_NICE` for `--rt`. Xwayland. `--steam` integration. HDR via `--hdr-enabled` + Gamescope WSI layer. |
| **gst-wayland-display** (stable @ 016b4fc, MIT) | Always headless. Built as a GStreamer source plus a C API. | Smithay GLES on a render node, or `render_node=software`. | No (no Xwayland, no layer-shell) | Yes (xdg-shell) | **Yes**. `apply_video_info` resizes at runtime with tests. | DMA-BUF caps, or **CUDAMemory** (`cuda-device-id=` for multi-GPU) | Raw mouse, keyboard and touch events sent as GStreamer messages, or evdev paths | Protocols: xdg-shell, linux-dmabuf, **drm-syncobj** (explicit sync), presentation-time, pointer-constraints, relative-pointer, viewporter, wl_drm, data-device. **The cursor is composited into the frame.** |
| **pixelflux** (main @ 20d3681, MPL-2.0) | Built-in Smithay compositor, outputs created on demand | GBM/EGL. Zero-copy into NVENC/VA-API. | Yes, nested KWin (webtop) | Yes | Yes (`create_output`, resize) | Internal. It also **serves `ext-image-copy-capture-v1`**. It can capture *external* compositors via ext-image-copy-capture (wlroots 0.19+, KWin 6.2+, COSMIC), wlr-screencopy, or the xdg portal with a restore token. | Internal. For external hosts: libei (`reis` 0.7) → virtual-kbd/ptr protocols → uinput → portal `Notify*` | Input "pull-forward": a frame is composited early on fresh input. |
| **nescope** (Nestri, Apache-2.0) | Headless. **Never allocates GBM buffers.** DMA-BUF v4 exists only so Xwayland DRI3 works. | None. Capture happens in the game process via the nescapture Vulkan layer. | No | No | Configurable output | Vulkan layer, not a compositor | calloop channel from neshub | Smithay 0.7. Subreaper. HDR (`wp_color_management`). gamescope WSI. |
| **KWin** (master @ ee272a4, Plasma 6.x) | **`kwin_wayland --virtual --width --height`**. Also nested via the `wayland` backend. | The virtual backend uses **GBM + EGL on a render node** (`virtual_egl_backend.cpp`). VsyncSource timer. | **Yes** | Yes | The virtual output has the `CustomModes` capability. Virtual outputs can be created via `zkde_screencast_unstable_v1.stream_virtual_output(name,w,h,scale,pointer)`. | **Screencast plugin → PipeWire**: DMA-BUF with modifier negotiation, **`SPA_META_Cursor`, `SPA_META_VideoDamage`, `SPA_META_SyncTimeline`**. Also `ext-image-copy-capture-v1` (6.2+, per pixelflux docs). | **EIS plugin**: D-Bus `org.kde.KWin.EIS.RemoteDesktop` at `/org/kde/KWin/EIS/RemoteDesktop`, method `connectToEIS(capabilities)` → fd. Also `fakeinput`. | dbus session. `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` for restricted protocols. No systemd. |
| **Mutter / GNOME Shell** | `--headless` + `--virtual-monitor WxH[@R]` ([meta-context-main.c](https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/core/meta-context-main.c)) | GBM/EGL | n/a (GNOME) | Yes | Virtual monitors via the ScreenCast `RecordVirtual` API. Stream-driven resize as used by g-r-d RDP **(mechanism not re-verified)**. | `org.gnome.Mutter.ScreenCast` + PipeWire | `org.gnome.Mutter.RemoteDesktop` + libei | gnome-remote-desktop headless multi-user via `grdctl --system` ([SUSE](https://www.suse.com/c/headless-remote-sessions-in-gnome-part-1/)). Containerised use: "no logind session… polkit prompts do not reach it." |
| **wlroots: sway / labwc / cage** (sway @ 1652c54 needs wlroots 0.21) | `WLR_BACKENDS=headless`. `swaymsg create_output` adds 1920x1080 headless outputs (`sway/commands/create_output.c`). | GLES2/Vulkan on a render node. `sway --unsupported-gpu` on NVIDIA. | No | Yes | `output HEADLESS-1 mode WxH` **(custom headless modes believed to work, unverified)** | `ext-image-copy-capture-v1` (0.19+, with **cursor sessions**), wlr-screencopy, xdg-desktop-portal-wlr | `zwlr_virtual_pointer`, `zwp_virtual_keyboard`. No libei **(unverified)**. | GOW nests sway inside Wolf's compositor to get Xwayland (`launch-comp.sh`). |
| **Weston** | headless, RDP and PipeWire backends | GL | No | Yes | — | PipeWire backend | RDP | Not used by any surveyed project. Low priority. |
| **X11: Xvfb / Xorg dummy** | Xvfb (glamor fork in XLibre), Xorg + dummy, or NVIDIA with fake EDID | DRI3 + glamor on GBM (Intel/AMD, and NVIDIA via GBM per pixelflux's GTX 1080 numbers). NvFBC on NVIDIA. | KDE X11 (`startplasma-x11`) | Yes | xrandr | DRI3 `PixmapFromBuffers` blit into DMA-BUF (pixelflux), XShm, NvFBC | XTEST | Legacy fallback only. NvFBC availability on GeForce is **(unverified)**. |

### 2.2 Which one runs what

- **Chrome/app:** our own Smithay compositor, the same idea as gst-wayland-display or pixelflux. Chrome runs with `--ozone-platform=wayland`. labwc or sway can be nested when window management or Xwayland is needed.
- **KDE Plasma full desktop:** two workable designs.
  - **(a) Proven:** KWin nested as a Wayland client of our compositor (the webtop approach). This means double composition, but KWin's nested output follows our output size.
  - **(b) Native:** `kwin_wayland --virtual` + zkde screencast + EIS. This is a single composition and gives proper cursor and damage metadata. Nobody ships it in containers yet, so it is a prototyping target.
- **Steam/games:** gamescope. It can be nested in our compositor (the Wolf/GOW approach, `gamescope -b -W -H -r -e`) or run headless with PipeWire capture. The second is the Steam Remote Play style. Polaris issue #152 (2026-06-29) documents a working 4K60 10-bit HDR path: "gamescope headless → portal ScreenCast → PipeWire LINEAR DMA-BUF → Vulkan copy → CUDA/NVENC" ([issue](https://github.com/papi-ux/polaris/issues/152)). Steam itself: `gamescope --backend headless --steam -- steam -tenfoot -pipewire-dmabuf` ([bwc9876 blog](https://bwc9876.dev/blog/headless_steam_remote_play/)).

### 2.3 Session plumbing without systemd or logind

- None of the surveyed container desktops use systemd or logind. They use `dbus-run-session` (or `dbus-launch`) and start `pipewire`, `wireplumber`, `pipewire-pulse` and `xdg-desktop-portal(-kde)` by hand.
- In a headless compositor there is no DRM master, so neither seatd nor logind is needed. A **render node** (`/dev/dri/renderD*`) is enough. The card node is only needed for Xorg or KMS.
- Systemd-based desktop images:
  - **Podman `--systemd=always`** runs systemd as PID 1 in unprivileged containers, which is a native Podman feature.
  - **Sysbox** looks stagnant. The last release is v0.7.1 from 2024-07-31 ([releases](https://github.com/nestybox/sysbox/releases)). Docker "sponsors" it but does not support it.
  - **Recommendation:** do not depend on systemd in environment images.

---

## 3. Zero-copy capture → encode

### 3.1 DMA-BUF / PipeWire negotiation

From [PipeWire DMA-BUF docs](https://docs.pipewire.org/1.2/page_dma_buf.html):
- The producer offers one EnumFormat with `SPA_FORMAT_VIDEO_modifier` flagged `MANDATORY|DONT_FIXATE`, listing every modifier (plus `DRM_FORMAT_MOD_INVALID` for implicit), and a second EnumFormat for SHM fallback.
- **The producer fixates:**
  1. It test-allocates against the intersection of modifiers.
  2. It re-announces a single modifier without `DONT_FIXATE`.
  3. It sets `SPA_PARAM_BUFFERS_dataType = 1<<SPA_DATA_DmaBuf`.
- Explicit sync (`SPA_META_SyncTimeline`) is negotiated separately, after format fixation.

How the producers I checked behave:
- **KWin** implements the full dance, including dropping modifiers that failed import. It adds cursor, damage and sync-timeline metadata (`src/plugins/screencast/screencaststream.cpp`).
- **gamescope** offers **only LINEAR**. That is trivially importable by NVENC, VA-API and Vulkan, at the cost of one tiled→linear blit in gamescope.
- **The consumer side (our node)** must offer the modifiers that the encoder import path accepts. Query them via `EGL_EXT_image_dma_buf_import_modifiers` or `VkDrmFormatModifierPropertiesListEXT`. On rejection, renegotiate down to LINEAR.

### 3.2 Import paths per encoder

| Encoder | Import path | Who does it |
|---|---|---|
| **VA-API (AMD/Intel)** | `vaCreateSurfaces` with `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2` (DMA-BUF + modifier) → `vapostproc` CSC/scale → `vah26xenc` / `vaav1enc`. Low-power (`*lpenc`) variants exist on Intel. | Wolf GStreamer pipelines (`vapostproc add-borders=true ! video/x-raw(memory:VAMemory),format=NV12`), pixelflux, Sunshine `vaapi.cpp` |
| **NVENC: EGL route** | `eglCreateImage(EGL_LINUX_DMA_BUF_EXT)` → GL texture → shader CSC to NV12 → `cuGraphicsGLRegisterImage` → NVENC | Sunshine `src/platform/linux/{graphics,cuda}.cpp` (GPL-3.0). gst-wayland-display `glupload ! glcolorconvert ! nvh265enc`. |
| **NVENC: CUDA-native compositor** | Compositor renders, then exports CUDAMemory directly | gst-wayland-display `cuda` feature (`waylanddisplaysrc cuda-device-id=0 ! video/x-raw(memory:CUDAMemory) ! nvh265enc`) |
| **NVENC: Vulkan route** | `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier` import → copy into exportable `OPAQUE_FD` memory → `cuImportExternalMemory` → NVENC. Timeline semaphores are shared via `cuImportExternalSemaphore`. | Polaris #152. pixelflux uses a Vulkan semaphore imported into CUDA for X11 sync. |
| **Vulkan Video encode** (H.264/H.265/AV1) | DMA-BUF → Vulkan image → compute RGB→NV12/P010 → `VK_KHR_video_encode_*`. `VK_VALVE_video_encode_rgb_conversion` lets the encoder take RGB directly. Per nescapture, only one driver offers it and it writes limited range. That driver is presumably RADV **(unverified)**. | Sunshine `vulkan_encode.cpp` ("No EGL/GL dependency"). Nestri nescapture. Per-vendor driver versions for Vulkan Video encode **(unverified)**. |
| **Pyrowave** | `pyrowave_image_create` imports a DMA-BUF with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` + `VkImageDrmFormatModifierExplicitCreateInfoEXT`. NV12 needs `MUTABLE_FORMAT_BIT`. A "very special case for pipewire interop" lets a COLOR_BIT NV12 image go through the scaling path. `pyrowave_encoder_encode_gpu_scaled_synchronous` takes an RGB(A) UNORM input with sRGB, scRGB-linear or **HDR10 ST2084** colour space, scales and converts on GPU. External sync is `OPAQUE_FD` / `SYNC_FD` semaphores. Requires Vulkan 1.3, subgroup size control, `shaderInt16`, 8-bit storage. Async-compute queue with `VK_QUEUE_GLOBAL_PRIORITY_HIGH/REALTIME` **needs root or `CAP_SYS_NICE`**. | [pyrowave.h](https://github.com/Themaister/pyrowave/blob/master/pyrowave.h) API 0.6.0 @ 89f7e47. Rust port: Nestri `crates/nespyro` (ash, Slang shaders, bitstream "frozen at PyroWave 89f7e47", desktop GPUs only). |

**Implication for Cha Portal's capture module.** Treat the capture output as one abstraction: **a GPU-resident RGB (or NV12/P010) DMA-BUF + a damage region + cursor state + a sync fd**. Then fan out per client:
- Pyrowave on LAN.
- NVENC, VA-API or Vulkan Video H.264/HEVC/AV1 on WAN.

Wolf's interpipe already fans one compositor out to N encoders. Pyrowave's own README targets "~200+ mbit/s… local network game streaming over ethernet", and nespyro quotes about 170 Mbit/s at 1080p60. That makes a hardware-codec WAN path mandatory.

### 3.3 Cursor, damage and frame pacing

- **Cursor.** For desktop-class "feels local" WAN use, send the cursor as **separate metadata** (sprite + hotspot + position) and draw it on the client.
  - Available today: KWin PipeWire `SPA_META_Cursor`; zkde_screencast `pointer` enum (`hidden=1`, `embedded=2`, `metadata=4`); ext-image-copy-capture cursor sessions (wlroots 0.19+); pixelflux's cursor callback.
  - Not available: gst-wayland-display composites the cursor into the frame, and gamescope's PipeWire has no cursor meta. For games, embedding is fine and often preferable.
  - In our own compositor, we own the cursor plane, so expose it separately.
- **Damage.** Encode only when something changed.
  - KWin emits `SPA_META_VideoDamage`.
  - A Smithay compositor knows client damage exactly. pixelflux: "no hashing needed, the compositor knows exactly which rectangles clients damaged".
  - On a static desktop, pixelflux "paint-over" re-sends at higher quality after N static frames. Copy this idea. Pyrowave's intra-only design makes partial-region refinement natural (64x64 blocks).
- **Frame pacing in headless mode.** No vblank exists, so each compositor runs a timer:
  - KWin virtual: `VsyncSource`.
  - gamescope: its vblank manager at `-r`.
  - nescope: a calloop timer drives frame callbacks.
  - pixelflux: per-display `target_fps` + **input pull-forward**, which composites half a frame early when input arrives so pointer motion is captured on landing.

  Best practice: set the virtual refresh to the client's display refresh (Moonlight-style), encode on commit rather than on a fixed tick, and expose presentation-time feedback to games.
- **Explicit sync.** NVIDIA needs `linux-drm-syncobj-v1` for correct Wayland behaviour. gst-wayland-display implements it, and KWin passes `SyncTimeline` through PipeWire. Our compositor must support it.

---

## 4. GPU in containers

### 4.1 NVIDIA

- **Container Toolkit** (latest 1.20.1 per [release notes](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/release-notes.html)):
  - **1.18.0** made **JIT-CDI the default** mode and added a systemd unit that auto-generates CDI specs. It requires Docker ≥26.1, containerd ≥1.7.16 or Podman ≥5.1.
  - **1.19.1** adds egl-wayland2 to CDI specs.
  - **1.20.0** adds an "application-profile hook that limits EGL and Vulkan visibility to GPUs assigned to the container" and exact version matching for graphics libraries.
  - **1.20.1** injects `ucode_*.bin` firmware.
  - Use the CDI device names `nvidia.com/gpu=<idx|uuid|all>`.
- **`NVIDIA_DRIVER_CAPABILITIES`** ([docs](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/docker-specialized.html)):
  - `compute, compat32, graphics, utility, video, display, all`.
  - The value *replaces* the defaults rather than adding to them.
  - For our use: `graphics,video,compute,utility`, plus `display` for Wayland/X window systems and `compat32` for Steam. In practice use `all`.
- **Driver coupling.** User-space libraries must exactly match the host kernel module. The toolkit injects them from the host, so environment images must **not** bundle NVIDIA user-space. GOW installs Steam with `nvidia-driver-libs- nvidia-vulkan-icd-` to avoid exactly this. Wolf's non-toolkit path rebuilds a driver volume per host driver version.
- **Display stack prerequisites:**
  - `nvidia-drm modeset=1` is the default since the **595** series ([NVIDIA forum](https://forums.developer.nvidia.com/t/wayland-support-for-the-595-release-series/365749)).
  - linuxserver requires driver ≥580. On ≥595.80 the only host step is `nvidia-modprobe --modeset` plus passing **`/dev/nvidia-modeset`**. On 580–594 you need `nvidia-drm.modeset=1 nvidia_drm.fbdev=1` and possibly a dummy plug.
  - GBM in the container needs `nvidia-drm_gbm.so` resolvable, plus the EGL vendor JSON and Vulkan ICD JSON (see the Selkies fixups in §1).
- **NVENC limits.** The [NVIDIA support matrix](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new), fetched 2026-10-03, lists **"Max # of concurrent sessions = 12" for all GeForce**, from Pascal through Blackwell. Professional and datacenter parts are "Unrestricted".
  - NVENC engine counts on Blackwell: RTX 5050–5070 have 1, 5070 Ti/5080 have 2, 5090 has 3.
  - All Blackwell GeForce support AV1 and 4:2:2.
  - History: 3 (2020) → 5 (2023-03) → 8 (driver 551.23, 2024-01) → 12 (date not found).
  - The session cap and engine throughput are separate limits. Eight 1440p60 streams already saturate most single-engine cards ([StreamGuides](https://streamguides.gg/2024/01/nvenc-update-all-nvidia-geforce-cards-quietly-updated-to-8-encoding-sessions/)).
- **MIG / vGPU.**
  - The RTX PRO 6000 Blackwell supports **MIG with `+gfx` profiles** ("Universal MIG"). Up to 4 instances, each 1g.24gb with 1 NVENC and 1 NVDEC. MIG-backed vGPU with up to 48 vGPUs on the Server Edition ([MIG guide](https://docs.nvidia.com/datacenter/tesla/mig-user-guide/supported-mig-profiles.html), [vGPU 19 blog](https://developer.nvidia.com/blog/nvidia-vgpu-19-0-enables-graphics-and-ai-virtualization-on-nvidia-blackwell-gpus/)).
  - All of this needs licensed vGPU, which is not a homelab target. GeForce cards have no MIG and no vGPU.

### 4.2 AMD / Intel

- Pass `--device /dev/dri/renderD12X` and add the **host's** `render` GID with `--group-add <gid>`. The GID differs between host and image.
- `/dev/dri/card*` is only needed for Xorg or KMS.
- Mesa and VA-API drivers come from the **container image**, so the image Mesa must be new enough for the GPU (e.g. RDNA4, Battlemage). There is no host user-space coupling beyond kernel UAPI, which is easier than NVIDIA.
- No concurrent-session cap on AMD VA-API ([Jellyfin docs](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/amd/)). Higher RDNA4 parts have 2× VCN 5.0 with improved low-latency H.264/HEVC/AV1.
- Intel VA-API 4:4:4 H.264 is unavailable. HEVC 4:4:4 / VP9 profile 1 per linuxserver docs.
- **Intel SR-IOV (xe driver):**
  - Supported on Tiger Lake, Alder Lake (N/Twin Lake), Panther Lake (Wildcat Lake) iGPUs and on **Battlemage Pro dGPUs only** (Arc Pro B50/B60). Consumer Arc does not get SR-IOV ([Phoronix](https://www.phoronix.com/news/Intel-SR-IOV-Only-For-Arc-Pro)).
  - Host Proxmox 9.2 / kernel 7.0. Guest kernel 6.18+ with xe ([abysm, 2026-05-23](https://blog.abysm.org/2026/05/pve-intel-xe-sriov/)).
  - B60 VF count is firmware-dependent: 7 on some boards, 24 on ASRock. Windows guests work. Linux-guest VFs on B50 were reported problematic ([L1T](https://forum.level1techs.com/t/b60-sr-iov-support-emerges-in-latest-arc-pro-drivers-7-vfs/246129)).
  - This is the only practical "one consumer-ish GPU → many Windows VMs" option in 2026.

### 4.3 Sharing one GPU across many sessions

- Containers simply share the render node and the NVIDIA device. There is **no hardware partitioning** on consumer cards: no VRAM quotas and time-sliced scheduling.
- Mitigations:
  - Cap per-session resolution and fps.
  - Use Vulkan global queue priority for encoders. This needs `CAP_SYS_NICE`; Pyrowave asks for HIGH/REALTIME.
  - Use `gamescope --rt` (`SYS_NICE`).
  - Monitor VRAM via NVML or amdgpu sysfs.
  - Enforce admission control in the node agent: max sessions per GPU and an NVENC session budget ≤12 on GeForce.
- **Multi-GPU selection.** Do it by PCI path (`/dev/dri/by-path/pci-…-render`) and Vulkan `deviceUUID`, not by index.
  - NVIDIA: `NVIDIA_VISIBLE_DEVICES=<uuid>`. Toolkit 1.20 hides other GPUs from EGL/Vulkan.
  - Mesa: `MESA_VK_DEVICE_SELECT`, and pin `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` to one ICD JSON.
  - gst-wayland-display `cuda-device-id`; gamescope `--prefer-vk-device vid:pid`; Pyrowave `pyrowave_create_device_by_compat2(vid,pid,uuids,…)`.
  - Wolf explicitly supports "stream encoding on iGPU whilst gaming on GPU". Rendering and encoding on different GPUs costs a readback or PCIe copy.
- **Vulkan ICDs inside containers.** Mesa ICDs ship in the image (`/usr/share/vulkan/icd.d/*.json`). The NVIDIA ICD JSON is injected by the toolkit or written by init. For Steam/Proton, 32-bit ICDs and libraries are also needed (`compat32`).

---

## 5. Virtual input in containers

- **Keyboard, mouse, touch, pen:** inject directly into the compositor.
  - Our Smithay seat (what Wolf does with gst-wayland-display GStreamer messages).
  - **gamescope EIS socket** (`$WAYLAND_DISPLAY-ei`).
  - **KWin EIS** (`org.kde.KWin.EIS.RemoteDesktop.connectToEIS`).
  - Mutter RemoteDesktop + libei.
  - wlroots virtual-pointer/keyboard.

  Rust libei: `reis` 0.7, used by pixelflux. This avoids uinput, keeps devices invisible to the host, and works rootless. Latency ranking from pixelflux for host capture: socket write ≈4 µs, uinput ≈12 µs, portal D-Bus ≈2 ms.
- **Gamepads: kernel devices are required.** Games read evdev or hidraw via SDL, Wine/Proton or Steam.
  - **inputtino** (MIT; used by Wolf, Sunshine, Moonshine) creates uinput Xbox, Switch and PS pads, plus a **uhid DualSense**. The DualSense goes through the host kernel's `hid-playstation` driver, so you get real `event*` nodes for motion sensors and touchpad plus a `hidraw` node. That gives gyro, touchpad, adaptive triggers, LED and battery ([uhid README](https://github.com/games-on-whales/inputtino/blob/stable/src/uhid/README.adoc); [blog 3](https://abeltra.me/blog/inputtino-uhid-3/)).
  - The uhid devices are emulated as **Bluetooth** (`BUS_BLUETOOTH` + CRC32 + 16 ms keepalive). SDL's hidapi path rejects USB devices without a USB parent.
  - Speaker-based haptics are out of scope; only rumble and trigger HID reports are supported **(unverified for audio haptics)**.
- **Host leakage.** uinput and uhid devices live in the **host** kernel. Without udev rules, logind gives the host seat user ACLs and the host desktop consumes the pad. Wolf's [`85-wolf.rules`](https://github.com/games-on-whales/wolf/blob/stable/85-wolf.rules) handles this:
  - It matches `ATTR{name}=="Wolf *virtual*"` and the hidraw parent `HID_NAME`.
  - It sets `ENV{ID_SEAT}="seat9"` and `TAG-="uaccess"`, plus `0660 input`.

  The node installer must install equivalent host rules, for example matching `"Cha *virtual*"`.
- **Hot-plug into a running container** ([fake-udev.adoc](https://github.com/games-on-whales/wolf/blob/stable/docs/modules/dev/pages/fake-udev.adoc)):
  1. Create the container with `DeviceCgroupRules: ["c 13:* rmw", "c 244:* rmw"]`. 13 is input; 244 is hidraw on that host. That major is dynamic and Wolf computes it.
  2. Add `CAP_MKNOD`.
  3. On plug, `docker exec mknod /dev/input/eventN c 13 N`.
  4. Write `/run/udev/data/c13:N`, ensure `/run/udev/control` exists.
  5. Broadcast a `NETLINK_KOBJECT_UEVENT` on the `GROUP_UDEV` multicast group **inside the container's netns**. This needs root and is why `NET_ADMIN`/`NET_RAW` are in Wolf's cap list.

  Nestri's `nesgamepad` re-implements the same idea, with notes on libudev's silent checks (`udev.rs`). It also replicates a DS4 over uhid from a real report descriptor, plus a neutral-identity uinput pad for XInput-only games. **mknod is impossible in rootless/userns containers**, so this approach requires rootful Docker/Podman for environment containers. Alternatives: pre-create N pad slots at container start, or bind-mount `/dev/input`.
- **Unprivileged alternative (Selkies).** `LD_PRELOAD` `selkies_input_interposer.so` (64- and 32-bit) + `libudev.so.1.0.0-fake`. It is injected into Proton via `user_settings.py`. No `/dev/uinput` is needed. Coverage is limited to programs whose input calls are intercepted; it does not cover Steam Input's hidraw path **(unverified)**.
- **Other options:**
  - **USB/IP** (vhci-hcd on the host) passes real controllers through. steamos-containerized advertises it for the "2026 Steam Controller".
  - **libvirtualhid** (LizardByte, MIT on Linux; the Windows driver is source-available LB-SAL) covers uhid/uinput profiles for X360/XOne/DS4/DualSense/Switch ([docs](https://docs.lizardbyte.dev/projects/libvirtualhid/latest/)).
- **Steam Input inside the container.** Steam reads the pad's hidraw and creates its own virtual X360 pad through `/dev/uinput`. Wolf's Steam container gets `c 13:*` and hidraw but **not** `/dev/uinput` (10:223) in the default config, so it is unclear how Steam Input remaps there **(unverified; open question)**.

---

## 6. Audio

- **Wolf:** a dedicated **PulseAudio container** (`ghcr.io/games-on-whales/pulseaudio:master`), socket shared through `XDG_RUNTIME_DIR`. Wolf uses libpulse to create a virtual sink per session and routes it (`audio/pulse_router`).
- **Selkies:** PulseAudio inside the environment (`--exit-idle-time=-1`) with two `module-null-sink`s: `output` (captured by pcmflux → Opus) and `input`. Browser mic audio is written into `input`, and its monitor acts as the mic. KDE webtop *also* starts PipeWire/WirePlumber because the portals and screencast need them.
- **Recommendation:**
  - Each environment runs **PipeWire + WirePlumber + pipewire-pulse**. Plasma and Chrome need it anyway.
  - Share `$XDG_RUNTIME_DIR/pipewire-0` with the node's streamer.
  - The streamer creates an `Audio/Sink` null sink (default) and captures its monitor.
  - It publishes a virtual mic as a `pw_stream` with `media.class=Audio/Source`.
  - Low latency: 48 kHz, `default.clock.quantum` 128–256 (2.7–5.3 ms), `PIPEWIRE_LATENCY=128/48000` on the capture stream, Opus 5–10 ms frames.

  These are standard PipeWire settings that I have not benchmarked here.
- **Mic passthrough security.** Virtual sources stay inside the container namespace. Nothing touches host audio unless you share the host socket, so don't.

---

## 7. Steam in containers

- **Sandbox requirement.** Steam runs its webhelper and each game in pressure-vessel containers via bwrap. That needs unprivileged user namespaces, or `CAP_SYS_ADMIN`, plus mount inside the userns.
  - **Docker default seccomp.** I verified this from [`moby/profiles` default.json](https://github.com/moby/profiles/blob/main/seccomp/default.json): `unshare`, `mount`, `setns` and `clone` with namespace flags are allowed only with `CAP_SYS_ADMIN`; `clone3` returns `ENOSYS` (38) without it.
  - **Podman default seccomp** ([containers/container-libs](https://github.com/containers/container-libs/blob/main/common/pkg/seccomp/seccomp.json)) allows `clone`, `clone3`, `mount`, `unshare` and `pivot_root` unconditionally (`setns` only with `SYS_ADMIN`), so bwrap and Chrome's namespace sandbox can work under Podman without extra flags.
  - **Ubuntu 24.04+:** `kernel.apparmor_restrict_unprivileged_userns=1` blocks `CLONE_NEWUSER` even with `seccomp=unconfined`. The `docker-default` AppArmor profile also denies mounts inside the new userns ([quasar #76](https://github.com/accreleus/quasar/issues/76)).
- **Workarounds, from most to least common:**
  1. **Wolf/GOW:** `CAP_SYS_ADMIN` + `seccomp=unconfined` + `apparmor=unconfined`, plus a **patched bwrap**. Stock bwrap dies with "Unexpected capabilities but not setuid" when it sees `CAP_SYS_ADMIN`; see [`ignore_capabilities.patch`](https://github.com/games-on-whales/gow/blob/master/apps/steam/build/ignore_capabilities.patch).
  2. **Custom seccomp** that moves `unshare`/`clone`/`clone3`/`mount`/`umount2`/`setns`/`pivot_root` out of the SYS_ADMIN gate. Add a scoped AppArmor profile, or `apparmor=unconfined`, and the host sysctl `kernel.apparmor_restrict_unprivileged_userns=0`. **This is the recommended path for Cha Portal**: no `SYS_ADMIN`, and the stock bwrap works.
  3. **Podman** default profile (see above).
  4. **[proot-bwrap](https://github.com/selkies-project/proot-bwrap)** (MPL-2.0). A bwrap stand-in that runs pressure-vessel containers through PRoot (or fakechroot for the webhelper).
     - Pros: no namespaces needed, and it supports soldier/sniper, Proton and GE-Proton.
     - Cons: "Not a security boundary". PRoot's single-threaded path translation reaches about 40k syscalls/s vs 805k untraced.
- **Steam client changes.** The SteamRT3 64-bit client runs Steam itself inside the runtime container. It was opt-in beta from 2026-03-20 and was still beta in September 2026 ([Phoronix](https://www.phoronix.com/news/Steam-Linux-Beta-In-Container)). Expect the client itself to require userns once SteamRT3 ships as stable.
- **Proton / esync / ntsync.**
  - Wolf sets `nofile 10240` for esync.
  - fsync needs futex_waitv (kernel ≥5.16).
  - Pass `/dev/ntsync` for newer Proton when the host has the ntsync driver **(unverified which Proton version defaults to it)**.
  - Proton core-count bug: limit via launch options, not cgroups (Wolf docs).
- **Shader cache.**
  - Persist `steamapps/shadercache` with the library.
  - Persist per-user `~/.cache` (`MESA_SHADER_CACHE_DIR`, `__GL_SHADER_DISK_CACHE_PATH`, DXVK/VKD3D caches).
  - Caches are GPU- and driver-specific, so key a shared cache by (GPU, driver version) **(design suggestion)**.
- **Shared library across sessions.**
  - Wolf mounts a host `steamapps` into each user's container (`mounts=['/path/steamapps:/home/retro/.steam/debian-installation/steamapps:rw']`). It must be owned by 1000:1000, and first-run permission hacks are needed.
  - Two Steam clients writing one library concurrently is unsafe **(unverified, but Steam has no multi-client locking)**.
  - Suggested design: one read-only shared library as an overlayfs lowerdir, with a per-user upperdir, or a node-level "library owner" session that does updates.
- **Steam + gamescope integration.** Use `gamescope -e` (`--steam`), Big Picture (`-bigpicture`/`-tenfoot`), mangoapp stats via `-T`, and `-R` ready-fd to read `DISPLAY` and `GAMESCOPE_WAYLAND_DISPLAY` (GOW `apps/steam/build/scripts/startup.sh`). The Steam overlay "does not work in our headless container" (Wolf docs).
- **Anti-cheat.**
  - EAC and BattlEye work under Proton only when the developer opts in, the same as on Steam Deck ([GamingOnLinux list](https://www.gamingonlinux.com/anticheat/vendor/battleye/)).
  - No container-specific blocker is documented **(unverified)**.
  - Kernel-level anti-cheats (Vanguard etc.) never work on Linux.
  - Many Windows anti-cheats refuse to run in VMs, which matters for the Windows-VM class.

---

## 8. Isolation and security

- **Never `--privileged`.** It grants every device including block devices, every capability, no seccomp or AppArmor, and writable `/sys`. Escaping is trivial. linuxserver: it "dramatically widens the blast radius" ([security docs](https://docs.linuxserver.io/selkies/user-guide/security/)).
- **Trust split:**
  - The **node agent** is root-equivalent: docker or podman socket, `/dev/uinput`, `/dev/uhid`, host udev rules, `mknod`/`exec` into environments.
  - **Environment containers** are unprivileged with a minimal device set.
  - This matches Wolf's design.
- **Per-environment minimum flags:** see §10.
- **Rootless Docker/Podman and userns-remap.**
  - GPU via CDI works rootless, with caveats such as missing `/dev/dri/renderD129` in a spec ([toolkit #885](https://github.com/NVIDIA/nvidia-container-toolkit/issues/885)).
  - Device nodes stay owned by host root:input/render, so the host user needs group membership (`--group-add keep-groups` on Podman).
  - **mknod-based gamepad hotplug does not work in user namespaces.**
  - Recommendation: rootful runtime with userns-remap off for gaming environments. Rootless is fine for Chrome/KDE classes that use compositor-only input.
- **IPC:** Wolf uses `IpcMode: host`, which shares SysV IPC with the host. steamos-containerized says Steam/CEF use "shared-memory segments" for webhelper↔client. Use container-private IPC when everything lives in one container **(unverified that host IPC is unnecessary)**.
- **gVisor:** nvproxy supports CUDA, Vulkan and NVENC/NVDEC when `--nvproxy-allowed-driver-capabilities` includes graphics/video. But **`/dev/nvidia-drm` and DRM are unsupported**, as are AMD and Intel ([gVisor GPU](https://gvisor.dev/docs/user_guide/gpu/)). That breaks GBM, compositors and Xwayland DRI3. Not viable.
- **Kata / Cloud Hypervisor:** whole-GPU VFIO passthrough only, with all GPUs assigned to one Kata VM and no vGPU ([NVIDIA GPU Operator + Kata](https://docs.nvidia.com/datacenter/cloud-native/gpu-operator/latest/deploy-kata-containers.html)). One GPU per environment is not homelab-friendly.
- **Firecracker:** no PCI passthrough upstream. Nestri's **nesbox** forked it and rewrote it on rust-vmm with "virtio over PCIe" ([nesbox](https://github.com/nestrilabs/nesbox), Apache-2.0, experimental).
- **GPU-sharing microVMs (future option):**
  - **DRM native context.** Upstream for AMD: Mesa 25.0+, host and guest kernel 6.14+. QEMU patches were at v11 in March 2025; merge status **(unverified)**. Partial for Intel. Fast blob mapping on crosvm, slow on QEMU ([Phoronix](https://www.phoronix.com/news/AMDGPU-VirtIO-Native-Mesa-25.0), [qemu-devel](https://lists.gnu.org/archive/html/qemu-devel/2025-03/msg02676.html)).
  - **virtio-nvgpu** (Nestri, announced 2026-09-26, [repo](https://github.com/nestrilabs/virtio-nvgpu)):
    - Forwards NVIDIA kernel-driver ioctls, so unmodified NVIDIA user-space runs in the guest.
    - Performance: about 98–100% of bare metal for ≥2 ms frames, and 12 VMs on one RTX 3060 each encoding 720p60.
    - Requirements: Linux guests and hosts only, host driver ≥535.129.03.
    - Its own security description: "roughly the same trust boundary Docker containers have".
    - Licenses: GPL-2.0 guest module, Apache-2.0 device, BSD-3 protocol.

  Given the user decision, these are an optional later backend. Keep the node's environment abstraction runtime-agnostic (OCI container now, microVM later).

---

## 9. Windows environments

- **Hosting:**
  - QEMU/KVM directly or via libvirt.
  - A container wrapper like [dockur/windows](https://github.com/dockur/windows) (`/dev/kvm`, `/dev/net/tun`, `NET_ADMIN`). Its docs do not mention GPU passthrough.
- **GPU options:**
  1. Whole-GPU VFIO passthrough: one GPU per VM, IOMMU groups, and a reset-capable GPU.
  2. Intel Arc Pro B50/B60 SR-IOV VFs: 7–24, firmware-dependent. Windows guest drivers work.
  3. NVIDIA vGPU or MIG+gfx on RTX PRO 6000 Blackwell: licensed.
  4. AMD consumer: no SR-IOV.

  Native-context sharing (virtio-gpu, virtio-nvgpu) is **Linux-guest only**.
- **Streaming from the guest:**
  - **Vibepollo** ([repo](https://github.com/Nonary/Vibepollo)) is an Apollo/Sunshine fork. Windows support is full; Linux is beta (Arch/CachyOS, KDE Plasma 6 Wayland, kernel 6.16+). It ships its own virtual display driver with SudoVDA fallback, Playnite integration, and scoped API tokens. About 99% AI-generated per its README. License **(unverified, presumably GPL-3.0 as a Sunshine fork)**.
  - The node treats the guest as an **external Moonlight-protocol endpoint**, which covers desired feature #8.
  - A longer-term Cha Windows agent could use Pyrowave's Windows interop: D3D11/D3D12 fence import and NT handle import. Upstream ships a DXGI Desktop Duplication sample, `encode_desktop.cpp`.
- **Looking Glass** (B7 stable; dev builds through August 2026):
  - IVSHMEM + **KVMFR** shared memory. KVMFR "can export regions for direct GPU import" on the Linux host.
  - In principle, a node could import guest frames as DMA-BUF and encode them on the *host* GPU, including Pyrowave. That needs a second GPU, because the guest owns the passthrough GPU.
  - A Looking Glass IDD (indirect display driver) in dev builds removes the need for dummy plugs **(details unverified; source page 403)**.
  - License GPL-2.0 **(unverified)**.
  - Niche. Prefer Vibepollo-in-guest.
- **Input:** Vibepollo handles it in-guest via the Moonlight protocol. Otherwise use virtio-input, USB passthrough or USB/IP.

---

## 10. Recommended node-side stack per environment class

Common to all classes:
- **cha-node agent:** rootful Docker or Podman. It owns the docker/podman socket, `/dev/uinput`, `/dev/uhid` and the host udev rules, and creates environments.
- **Per-session streamer:** compositor + capture + encoders. It runs as the node process (Wolf model) or as a sidecar sharing a tmpfs `XDG_RUNTIME_DIR` with the environment. DMA-BUF fds pass over the Unix socket; both sides need the same render node.
- **GPU:** CDI `nvidia.com/gpu=<uuid>` or `/dev/dri/renderD*` + `--group-add <render gid>`.
- **Encoders:** Pyrowave (LAN), and NVENC, VA-API or Vulkan Video H.264/HEVC/AV1 (WAN), all fed from the same DMA-BUF.

### A. Chrome / single app (kiosk; ephemeral or persistent)

- **Display:** cha-compositor (Smithay; fork or reuse gst-wayland-display MIT or pixelflux MPL-2.0). Chrome with `--ozone-platform=wayland --enable-features=…VaapiVideoDecoder…`.
- **Capture → encode:** compositor GBM DMA-BUF (XRGB8888/XRGB2101010 + modifier) + exact damage + cursor-as-metadata. Then either Pyrowave (Vulkan import), or VA-API, or NVENC (EGL→CUDA, Vulkan→CUDA, or a CUDAMemory-rendering compositor). Idle means no frames; use paint-over refinement.
- **Input:** compositor seat. No uinput. Clipboard through our compositor's data-device.
- **Audio:** PipeWire in the environment, null sink monitor captured by the streamer.
- **Container flags:** no `--privileged`. `--shm-size=1g`. `--cap-drop=ALL` + minimal adds. GPU device as above. A **custom seccomp that allows userns** (or Podman) so Chrome's sandbox works instead of `--no-sandbox`. No host IPC, no host netns.

### B. Full desktop (KDE Plasma 6)

- **Display, phase 1:** `kwin_wayland --xwayland --xwayland-fd …` nested on the cha-compositor socket (the webtop recipe). Session: `dbus-run-session`, `pipewire`, `wireplumber`, `pipewire-pulse`, `xdg-desktop-portal-kde`, `kded6`, `plasmashell`, `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`.
- **Display, phase 2 (prototype):** `kwin_wayland --virtual` (GBM/EGL) with capture via `zkde_screencast_unstable_v1.stream_output`/`stream_virtual_output(pointer=metadata)`. That yields PipeWire DMA-BUF with cursor, damage and sync-timeline metadata. Input via `org.kde.KWin.EIS.RemoteDesktop.connectToEIS` + `reis`. Resize via virtual-output custom modes or a recreated virtual output **(unverified end-to-end)**.
- **Capture → encode:** same as A.
- **Requirements:** no systemd, logind or Sysbox. For "real distro with systemd" images use Podman `--systemd=always`. Flatpak inside needs userns (same seccomp as A). FUSE for AppImages needs `/dev/fuse` + `SYS_ADMIN` **(unverified minimal set)**.
- **Flags:** as A. Optionally `/dev/fuse`.

### C. Steam / gaming

- **Display:**
  - Default: gamescope nested in cha-compositor (`gamescope -b -e -W/-H -w/-h -r <client Hz> -R <fd> -T <stats>`). This gives Xwayland, Steam integration and FSR/NIS scaling.
  - Alternative, Remote-Play style: `gamescope --backend headless … -- steam -tenfoot -pipewire-dmabuf`, captured from the gamescope PipeWire node (LINEAR BGRx/NV12; no cursor or damage meta).
- **Capture → encode:** the nested path reuses the common pipeline. The headless path uses PipeWire LINEAR DMA-BUF into one of:
  - Pyrowave's NV12 PipeWire special case.
  - Vulkan → CUDA → NVENC (Polaris path, 4K60 10-bit proven).
  - VA-API import.

  Prefer Pyrowave on LAN. It uses no NVENC session, but give it an async compute queue at HIGH priority.
- **Input:**
  - Keyboard and mouse via the compositor seat, or the gamescope EIS socket.
  - Pads via inputtino (uinput Xbox/Switch, uhid DualSense/DS4) + host udev rules (`ID_SEAT=seat9`, `TAG-="uaccess"`) + Wolf-style hot-plug (device cgroup `c 13:* rmw` + hidraw major, `mknod`, fake-udev netlink).
- **Sandbox:** custom seccomp allowing userns syscalls + AppArmor profile, or unconfined. On Ubuntu hosts set `kernel.apparmor_restrict_unprivileged_userns=0`. Fallback: Wolf's `SYS_ADMIN` + patched bwrap, or proot-bwrap.
- **Flags (target):**
  - `--cap-add SYS_NICE,MKNOD`, plus `NET_ADMIN`/`NET_RAW` only if fake-udev runs in-container.
  - `--security-opt seccomp=cha-steam.json`, `apparmor=cha-steam` (or unconfined).
  - `--device-cgroup-rule 'c 13:* rmw' --device-cgroup-rule 'c <hidraw>:* rmw'`.
  - `--ulimit nofile=10240:10240`, `--shm-size=2g`.
  - `/dev/ntsync` if present.
  - NVIDIA: `NVIDIA_DRIVER_CAPABILITIES=all` (compat32).
  - Shared library volume and per-user home volume.
- **Wolf's proven set for reference:** `SYS_ADMIN, SYS_NICE, SYS_PTRACE, NET_RAW, MKNOD, NET_ADMIN`, `seccomp=unconfined`, `apparmor=unconfined`, `IpcMode=host`.

### D. Windows VM

- **Hosting:** QEMU/KVM, either as a node-managed libvirt domain or a container with `/dev/kvm`. GPU via VFIO whole-device passthrough, or an Intel Arc Pro SR-IOV VF.
- **Capture → encode:** in-guest Vibepollo/Sunshine (NVENC/AMF/QSV on the guest GPU), exposed as an external Moonlight endpoint that Cha Portal brokers. Future: a Cha Windows agent with Pyrowave via DXGI/WGC + D3D12 fence interop. Optional Looking Glass KVMFR → host encode on a second GPU.
- **Input:** via the streaming protocol (Vibepollo's virtual HID/ViGEm-class drivers), or USB/IP.
- **Flags:** `/dev/kvm`, `/dev/vfio/*`, hugepages, `NET_ADMIN`/`tun`. Host IOMMU enabled. The GPU is bound to `vfio-pci`, or the PF is configured for SR-IOV.

---

## 11. Licenses of reusable components

All of the following are compatible with an AGPL/GPLv3 project. GPLv2-only code would only matter if we linked it, and we would not link KWin.

| Component | License | Reuse idea |
|---|---|---|
| Wolf, gow images, inputtino, gst-wayland-display | MIT | Fake-udev, hot-plug flow, uhid DualSense, compositor base, GStreamer pipelines |
| Smithay | MIT | Compositor framework |
| gamescope | BSD-2-Clause | Nested game compositor; PipeWire and EIS reference |
| Pyrowave | MIT | LAN codec. Nestri's `nespyro` Rust port is also MIT. |
| Selkies, pixelflux, pcmflux, proot-bwrap | MPL-2.0 (file-level copyleft) | Capture library, damage-striped encode, DRI3 path, external-host capture, bwrap stand-in |
| linuxserver baseimage-selkies, docker-webtop | GPL-3.0 | Environment image recipes (KDE startwm, NVIDIA ICD fixups) |
| Sunshine | GPL-3.0 | NVENC EGL→CUDA interop, Vulkan encode, KMS/KWin/portal grabbers |
| Nestri, nesbox | Apache-2.0 | nescope, nescapture (Vulkan layer), nesgamepad |
| virtio-nvgpu | GPL-2.0 (guest) / Apache-2.0 / BSD-3 | Future microVM backend |
| libvirtualhid | MIT (Linux), LB-SAL (Windows driver) | Alternative to inputtino |
| plasma-wayland-protocols (zkde_screencast) | LGPL-2.1-or-later | Protocol XML |
| KWin | GPL-2.0-or-later / mixed (repo `LICENSES/`) | Run as a process, don't link |
| Looking Glass, Vibepollo, KasmVNC | **(unverified)** | External processes |

---

## 12. Hardest unknowns and open questions

1. **NVIDIA zero-copy inside containers, end to end.** Which NVENC import path is most robust across driver branches (580/595/615) and with GBM inside containers?
   - The candidate paths are EGL→GL→CUDA, Vulkan→OPAQUE_FD→CUDA and Vulkan Video.
   - Dependencies: `/dev/nvidia-modeset`, `nvidia-drm_gbm.so`, ICD JSONs.
   - Does Pyrowave import NVIDIA tiled-modifier DMA-BUFs from our compositor without a copy?
   - Needs a hardware test matrix.
2. **Steam without `SYS_ADMIN`/unconfined.** Write and test a minimal seccomp and AppArmor profile for pressure-vessel, the webhelper and SteamRT3. How does it interact with Ubuntu's userns restriction? Is `IpcMode=host` really needed?
3. **Controllers.** Steam Input inside a container: does it need `/dev/uinput`? DualSense uhid devices live in the host kernel; are there seat leaks and logind interactions? Fake-udev fidelity for SDL3, Wine and Steam. Rootless incompatibility of mknod hot-plug.
4. **KWin `--virtual` + EIS + screencast in a container.** Not demonstrated by anyone. Open points: resize semantics (custom modes vs new virtual output), `KWIN_WAYLAND_NO_PERMISSION_CHECKS` scope, and portal-free operation.
5. **GPU QoS across sessions on consumer cards.** No VRAM or compute isolation. Open points: Vulkan global priority needs `SYS_NICE`; the NVENC 12-session cap counts per process or session and multi-client fan-out multiplies it; admission control heuristics.
6. **gamescope as a capture source.** Open points: no cursor or damage metadata, LINEAR-only, HDR format negotiation (P010?), whether nested resolution can change at runtime, and the extest/libei crash reported with headless Steam.
7. **Frame pacing.** Aligning compositor timers with client refresh and network jitter. Moonlight-class pacing in headless mode is not solved generically; pixelflux's input pull-forward is the best idea seen.
8. **Shared Steam library and shader-cache semantics** across concurrent users (overlayfs design untested).
9. **Windows guests on shared consumer GPUs** remain unsolved outside Intel Arc Pro SR-IOV. NVIDIA consumer cards need whole-GPU passthrough.
10. **Sysbox** looks unmaintained (last release 2024-07). Avoid it. If systemd-in-container is a hard requirement, validate Podman `--systemd` with GPU and CDI.

---

## Appendix: verification log (source-read, commit-pinned)

| Claim | Where verified |
|---|---|
| GeForce NVENC max sessions = 12 | NVIDIA support matrix HTML, fetched 2026-10-03 |
| Wolf Steam container caps and security opts | `wolf@facb8e0:src/moonlight-server/state/default/config.include.toml` |
| Wolf fake-udev netlink design | `wolf@facb8e0:docs/modules/dev/pages/fake-udev.adoc`, `src/fake-udev/fake-udev/fake-udev.hpp` |
| Wolf host udev rules (seat9, uaccess strip) | `wolf@facb8e0:85-wolf.rules` |
| GOW patched bwrap | `gow@bc4bb67:apps/steam/build/{Dockerfile,ignore_capabilities.patch}` |
| gst-wayland-display protocols, CUDA output, runtime resize, embedded cursor | `gst-wayland-display@016b4fc` (README, `wayland/handlers/*`, `tests/test_resolution.rs`, `comp/rendering.rs`) |
| gamescope headless backend, PipeWire LINEAR BGRx/NV12, EIS socket | `gamescope@0e590c7:src/main.cpp`, `src/pipewire.cpp`, `src/wlserver.cpp`, `src/InputEmulation.cpp` |
| KWin virtual backend GBM/EGL, CustomModes, EIS D-Bus, screencast meta | `kwin@ee272a4:src/backends/virtual/*`, `src/plugins/eis/eisbackend.{h,cpp}`, `src/plugins/screencast/screencaststream.cpp` |
| zkde_screencast virtual output + pointer metadata | `plasma-wayland-protocols:src/protocols/zkde-screencast-unstable-v1.xml` (v6) |
| Mutter `--headless`, `--virtual-monitor` | `mutter main:src/core/meta-context-main.c` |
| sway `create_output` on headless; wlroots 0.21 | `sway@1652c54:sway/commands/create_output.c`, `meson.build` |
| webtop KDE nested KWin recipe; Chromium `--no-sandbox` | `docker-webtop@ubuntu-kde:root/defaults/startwm_wayland.sh`, `root/kwin-xwayland.py`, `root/usr/local/bin/wrapped-chromium` |
| Selkies proot-bwrap + input interposer + fake libudev for Steam | `docker-baseimage-selkies@d32a6e1:root/steam.sh`, `Dockerfile` |
| Selkies NVIDIA ICD/EGL/GBM fixups; PulseAudio null sinks | `docker-baseimage-selkies@d32a6e1:root/etc/s6-overlay/s6-rc.d/{init-video,svc-selkies,svc-pulseaudio}/run` |
| pixelflux external capture ladder, libei via reis, DRI3 path | `pixelflux@20d3681:README.md`, `pixelflux/Cargo.toml` |
| Pyrowave interop API (DMA-BUF modifiers, NV12 PipeWire case, HDR10, SYS_NICE priority) | `pyrowave@89f7e47:pyrowave.h` |
| Nestri nescope / nescapture / nesgamepad / nespyro | `nestri@ef32d5f:apps/*/README.md`, `crates/nespyro/src/lib.rs` |
| Sunshine NVENC EGL→CUDA, Vulkan encode, kwingrab | `Sunshine@b487700:src/platform/linux/{cuda,graphics,vulkan_encode,kwingrab}.cpp` |
| Docker vs Podman default seccomp on userns syscalls | `moby/profiles@main:seccomp/default.json`, `containers/container-libs@main:common/pkg/seccomp/seccomp.json` |
