# Environment images

Our own images for the catalog (`catalog.json`, which the portal embeds). Every environment runs one of these as its **app container**, beside a `cha-streamer` container that the agent starts (plan §4.2).

| Image | What runs | Profile |
|---|---|---|
| `base` | Ubuntu 26.04, user `cha` (uid 1000), fonts, cursor theme, the GPU libraries' dispatchers, libpulse | — |
| `test-pattern` | `cha-testpattern`, our own Wayland client: a sweeping bar, a frame counter, a frame-ID strip, a flash and a tone on input, the first gamepad's state | `standard` |
| `chrome` | Google Chrome on Wayland, GPU-rendered, sandbox on | `browser` |
| `firefox` | Firefox (Mozilla's .deb) on Wayland, GPU-rendered, sandbox on; first-run pages off by policy | `browser` |
| `xfce` | XFCE inside one rootful, fullscreen Xwayland, so the compositor needs no X11 window manager, and `cha-x11-clipboard`, which bridges the X clipboard to the streamer | `standard` |
| `kde` | KDE Plasma 6: KWin runs as a Wayland client of our compositor (one window, the whole picture) with its own Xwayland, and Plasma inside it; no systemd | `standard` |
| `steam` | Steam's Big Picture (gamepad UI) inside gamescope, which runs as a Wayland client of our compositor and gives Steam its own Xwayland. The home persists per user. `steam-touch-mode` keeps gamescope's touch click mode out of the "passthrough" Big Picture sets, which would drop the pointer's motion. gamescope doesn't pass the clipboard through, so `cha-x11-clipboard` bridges its X server's, as in XFCE. `steam-status` shows the first launch's download and install on the page (*Setup status*, below) | `steam` |

Build them on a node, from the repository root:

```bash
docker compose -f images/compose.yaml build
```

## The contract

- **Wayland.** The streamer and the app share a volume at `/run/cha` (`XDG_RUNTIME_DIR`). The streamer creates `wayland-0` there and hands the directory and socket to uid 1000. `cha-run`, the images' entrypoint, waits for the socket and then starts the app.
- **Sound.** The streamer's own PulseAudio-protocol server listens at `/run/cha/pulse/native` (`PULSE_SERVER` in the base image). Anything that speaks PulseAudio plays through it: libpulse (installed), and PipeWire's, SDL's or Wine's PulseAudio backends. There is no PulseAudio or PipeWire daemon in the image.
- **Clipboard.** Wayland apps share the compositor's clipboard directly. X11 apps keep theirs inside their X server, so the `xfce` and `steam` images run `cha-x11-clipboard` (built from [`crates/cha-x11-clipboard`](../crates/cha-x11-clipboard) in each image's first stage, started by `start-xfce` once the X server is up, or beside Steam inside gamescope), which talks to the streamer over `/run/cha/clipboard`: text both ways, CLIPBOARD only.
- **Setup status.** An app whose first run is long (Steam downloads ~500 MB before it has a window) tells the page what it is doing by writing one JSON object to `/run/cha/status`:

  ```json
  {"label": "Downloading Steam", "done": 123, "total": 496, "unit": "MB"}
  ```

  - **Fields.** Only `label` is required. `done`, `total` and `unit` are optional: with no `total` the page shows an indeterminate bar, and `unit` names `done` and `total`. Removing the file, or writing `{}` (or no label), clears it. The app clears it itself when its UI is up: nothing else will.
  - **Replace it whole.** Write a temp file in `/run/cha`, then rename it over `status`: `printf '{"label":"Installing"}' > /run/cha/status.tmp && mv /run/cha/status.tmp /run/cha/status`. The directory belongs to uid 1000, so the app can. A file that doesn't parse, or is over 4 KiB, is ignored and the last status stands.
  - **Where it goes.** The streamer reads the file every 250 ms and sends every session `{"t":"status",…}` on the control channel, the current one to a page that joins late. The portal shows the label and a progress bar over the picture, and the player's `onStatus` hands it to any other page.
  - **Steam.** `steam-status` (Python, started by `start-steam` beside Steam; its tests are `test_steam_status.py`) follows Steam's bootstrap log, `~/.local/share/Steam/logs/bootstrap_log.txt`, from the moment it starts (the home keeps earlier launches' lines). `Downloading update (493,420 of 496,367 KB)...` becomes "Downloading Steam" with `done` and `total` in MB; `Extracting package...` is "Unpacking Steam", `Installing update...` "Installing Steam", and "Starting Steam" is what it says before, between and after. It ends, clearing the status, when Steam's web helper logs its first window (`PopupHTMLWindow` in `steamui_html.txt`), or when a `steamwebhelper` process has run for 20 s, or when its parent does. A launch with nothing to download only says "Starting Steam", for the few seconds the UI takes.
- **Gamepads.** The streamer makes virtual Xbox 360 pads (uinput) and shares them through two read-only volumes: their device nodes at `/dev/input` (the only input devices the app has) and udev's entries for them at `/run/udev`, where Chrome, Firefox and Wine look. The app's device cgroup allows input devices (major 13). Hotplug events don't reach the container, so one pad exists from the start; SDL is told to skip udev (`SDL_JOYSTICK_DISABLE_UDEV=1`) and watches `/dev/input`, so it sees later ones.
- **User.** The app runs as uid 1000, plus the render node's group (the agent reads it from the device), so EGL, GL and Vulkan reach the GPU. The NVIDIA driver comes from CDI at run time.
- **Confinement.** No capabilities (`--cap-drop ALL`), no privilege gain (`no-new-privileges`), and a seccomp profile:
  - `standard`: Docker's default profile.
  - `browser`: Docker's default plus `clone`, `unshare`, `setns`, `chroot`, `mount`, `umount2` and `pivot_root` ([`crates/cha-node/profiles/seccomp-browser.json`](../crates/cha-node/profiles/seccomp-browser.json)). Chrome's and Firefox's sandboxes create user, PID and network namespaces. The kernel still requires capabilities inside a new namespace, and the container has none in its own.
  - `steam`: `browser`, plus the `cha-sandbox` AppArmor profile ([`deploy/node/host/apparmor/cha-sandbox`](../deploy/node/host/apparmor/cha-sandbox)), which the owner loads on the node. Steam's pressure-vessel runs every game, and Steam's web helper, in a bubblewrap container: it mounts inside its own user namespace, which Docker's AppArmor profile denies (`bwrap: Failed to make / slave: Permission denied`). Also a high open-files limit, for Proton's esync.
- **`/dev/shm`** is sized per template; browsers need more than Docker's default 64 MB.
- **Persistent homes.** A template marked `"persistent": true` in `catalog.json` (Steam) keeps the app's home between launches, in a Docker volume per user and template, `cha-home-<user id>-<template id>`, mounted at `/home/cha`.
  - Docker makes the volume on the first launch and fills it from the image's `/home/cha`, owner and mode included (`cha`, uid 1000). After that the image's copy is ignored: don't put anything in `/home/cha` that an image update must replace.
  - The volume is the user's, not the environment's. Stopping an environment removes its other volumes and leaves this one.
  - It carries the labels `sh.cha.home`, `sh.cha.owner` and `sh.cha.template` (`docker volume ls --filter label=sh.cha.home`). `cha-node --doctor` counts them.
  - The portal allows one such environment per user and template at a time, since two would share a home. Launching a second says so.
  - Volumes are node-local; Phase 3 makes placement follow them.
  - To reset one, stop the environment, then on the node: `docker volume rm cha-home-<user id>-<template id>`. For Steam that deletes the client, its login and the installed games.
  - Start-up scripts must cope with what a stopped container leaves in the home: `start-steam` clears Steam's stale pid file and pipe.

## Known gaps

- **bubblewrap.** GTK loads images through glycin, which sandboxes each loader with bubblewrap. bubblewrap can't build its sandbox here: Docker's AppArmor profile denies its mounts, and the host restricts unprivileged user namespaces (Ubuntu's `apparmor_restrict_unprivileged_userns`). The base image therefore puts a stand-in `bwrap` first in `PATH` ([`base/bwrap-shim`](base/bwrap-shim)). It runs the loaders directly, so the container is their sandbox. A `cha-browser` AppArmor profile installed by `cha doctor` (P1.7, with the owner's sudo) will remove the shim. Steam's pressure-vessel needs the same profile.
- **XFCE** has no wallpaper yet.
