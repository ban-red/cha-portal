# Environment images

Our own images for the catalog (`catalog.json`, which the portal embeds). Every environment runs one of these as its **app container**, beside a `cha-streamer` container that the agent starts (plan §4.2).

| Image | What runs | Profile |
|---|---|---|
| `base` | Ubuntu 26.04, user `cha` (uid 1000), fonts, cursor theme, the GPU libraries' dispatchers, libpulse | — |
| `test-pattern` | `cha-testpattern`, our own Wayland client: a sweeping bar, a frame counter, a frame-ID strip, a flash and a tone on input, the first gamepad's state | `standard` |
| `chrome` | Google Chrome on Wayland, GPU-rendered, sandbox on | `browser` |
| `firefox` | Firefox (Mozilla's .deb) on Wayland, GPU-rendered, sandbox on; first-run pages off by policy | `browser` |
| `xfce` | XFCE inside one rootful, fullscreen Xwayland, so the compositor needs no X11 window manager | `standard` |
| `kde` | KDE Plasma 6: KWin runs as a Wayland client of our compositor (one window, the whole picture) with its own Xwayland, and Plasma inside it; no systemd | `standard` |
| `steam` | Steam's Big Picture (gamepad UI) inside gamescope, which runs as a Wayland client of our compositor and gives Steam its own Xwayland. The home persists per user | `steam` |

Build them on a node, from the repository root:

```bash
docker compose -f images/compose.yaml build
```

## The contract

- **Wayland.** The streamer and the app share a volume at `/run/cha` (`XDG_RUNTIME_DIR`). The streamer creates `wayland-0` there and hands the directory and socket to uid 1000. `cha-run`, the images' entrypoint, waits for the socket and then starts the app.
- **Sound.** The streamer's own PulseAudio-protocol server listens at `/run/cha/pulse/native` (`PULSE_SERVER` in the base image). Anything that speaks PulseAudio plays through it: libpulse (installed), and PipeWire's, SDL's or Wine's PulseAudio backends. There is no PulseAudio or PipeWire daemon in the image.
- **Gamepads.** The streamer makes virtual Xbox 360 pads (uinput) and shares them through two read-only volumes: their device nodes at `/dev/input` (the only input devices the app has) and udev's entries for them at `/run/udev`, where Chrome, Firefox and Wine look. The app's device cgroup allows input devices (major 13). Hotplug events don't reach the container, so one pad exists from the start; SDL is told to skip udev (`SDL_JOYSTICK_DISABLE_UDEV=1`) and watches `/dev/input`, so it sees later ones.
- **User.** The app runs as uid 1000, plus the render node's group (the agent reads it from the device), so EGL, GL and Vulkan reach the GPU. The NVIDIA driver comes from CDI at run time.
- **Confinement.** No capabilities (`--cap-drop ALL`), no privilege gain (`no-new-privileges`), and a seccomp profile:
  - `standard`: Docker's default profile.
  - `browser`: Docker's default plus `clone`, `unshare`, `setns`, `chroot`, `mount`, `umount2` and `pivot_root` ([`crates/cha-node/profiles/seccomp-browser.json`](../crates/cha-node/profiles/seccomp-browser.json)). Chrome's and Firefox's sandboxes create user, PID and network namespaces. The kernel still requires capabilities inside a new namespace, and the container has none in its own.
  - `steam`: `browser`, plus the `cha-sandbox` AppArmor profile ([`deploy/node/host/apparmor/cha-sandbox`](../deploy/node/host/apparmor/cha-sandbox)), which the owner loads on the node. Steam's pressure-vessel runs every game, and Steam's web helper, in a bubblewrap container: it mounts inside its own user namespace, which Docker's AppArmor profile denies (`bwrap: Failed to make / slave: Permission denied`). Also a high open-files limit, for Proton's esync.
- **`/dev/shm`** is sized per template; browsers need more than Docker's default 64 MB.
- **Persistent homes.** A template marked `persistent` (Steam) gets a volume per user, `cha-home-<user>-<template>`, as `/home/cha`. It survives stopping, so the user has one such environment at a time. Volumes are node-local; Phase 3 makes placement follow them.

## Known gaps

- **bubblewrap.** GTK loads images through glycin, which sandboxes each loader with bubblewrap. bubblewrap can't build its sandbox here: Docker's AppArmor profile denies its mounts, and the host restricts unprivileged user namespaces (Ubuntu's `apparmor_restrict_unprivileged_userns`). The base image therefore puts a stand-in `bwrap` first in `PATH` ([`base/bwrap-shim`](base/bwrap-shim)). It runs the loaders directly, so the container is their sandbox. A `cha-browser` AppArmor profile installed by `cha doctor` (P1.7, with the owner's sudo) will remove the shim. Steam's pressure-vessel needs the same profile.
- **XFCE** has no wallpaper yet.
