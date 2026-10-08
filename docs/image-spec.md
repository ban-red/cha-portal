# Environment image specification

What an image must do to run as a Cha environment, and how to describe it in a catalog. It is written for someone who has never seen this repository: you need this document, Docker and a Cha node.

The rules come from what the node agent (`cha-node`) and the streamer (`cha-streamer`) actually do, and each says what breaks without it. Where our own images do more than the rules require, [`images/README.md`](../images/README.md) says so. [`images/example/`](../images/example) is a complete minimal image and catalog entry. The decision behind the on-demand pulls is [ADR 0017](adr/0017-images-pulled-on-demand.md).

Words in capitals (MUST, SHOULD, MAY) are used as in RFC 2119. Everything else is description.

## 1. Scope and terms

- **Environment**: one running app for one user, on one node. It is two containers sharing a volume: the **streamer** (ours; the compositor, the encoder, sound, input, the network side) and the **app container**, which runs your image.
- **Image**: what the app container runs. You build it. This document is the contract between it and the node.
- **Template**: a catalog entry that names an image and says how to run it (section 6). One image can back several templates.
- **Catalog**: a JSON document, `{"version": 1, "templates": [...]}`. Our own is [`images/catalog.json`](../images/catalog.json), built into the portal. An admin can load others; they follow stricter rules (section 6.3).
- **Node**: a Linux machine running `cha-node` and Docker. It is the only place images run. It has an NVIDIA GPU (driver brought in by CDI), an Intel or AMD GPU (VA-API) or no GPU.
- **Home**: `/home/cha` in the app container.
- **Base**: our `ghcr.io/ban-red/cha-env-base`, which does most of this section's work for you (section 7).

The app is a **Wayland client** of the streamer's compositor. It draws into a window the compositor shows, makes sound through a PulseAudio socket and reads input from the compositor's seat. Nothing is shown on a real display and no display server runs in the container unless the image starts one.

## 2. Runtime contract

### 2.1 MUST

- **The image runs as user 1000:1000.** The node starts the app container with `User: 1000:1000`; the entrypoint and everything it starts run as that uid. The streamer chowns the runtime directory and the Wayland socket to uid 1000, so any other user cannot connect. The image does not need a name for the user, but tools that look up `$HOME` or the passwd entry (GTK, Qt, shells) want one, so give uid 1000 a passwd entry with home `/home/cha`.
- **Nothing in the image needs root, capabilities or privilege gain.** The container runs with every capability dropped (`CapDrop: ALL`) and `no-new-privileges`. `sudo`, setuid helpers, `chown` of files you don't own, binding ports below 1024 and anything that mounts or uses `ptrace` on other processes fail. Do the root work at build time.
- **The image waits for the Wayland socket before it starts the app.** The container starts at the same time as the streamer, and the compositor creates `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` a moment later (a second or two; a first-time pull is not part of this). An app that connects at once exits with "no Wayland display" and the environment fails. Wait for the socket as a socket (`[ -S ... ]`), poll every 100 ms, give up after about 30 s with a message on stderr. The base's `cha-run` does exactly this (section 7).
- **The image sets, or tolerates, the environment in section 3.** The node sets the first group; the image sets the second group itself, because the node doesn't:
  - `HOME=/home/cha`: the node's bind mount and its seeding are at that path.
  - `PULSE_SERVER=unix:/run/cha/pulse/native`: the streamer's sound server. Without it, libpulse looks for a daemon that doesn't exist, and the app has no sound (or, for some apps, no start).
  - `SDL_JOYSTICK_DISABLE_UDEV=1`: gamepads appear in `/dev/input` after the app starts, and hotplug events don't reach the container. With SDL's udev backend the later pads are never seen.
- **The command is the image's `ENTRYPOINT` and `CMD`.** The node gives the app container no command of its own. A shell wrapper is fine; the app MUST be the process that ends the container, since the container's exit ends the environment.
- **The app handles SIGTERM.** Stopping an environment sends the app container SIGTERM (through Docker's init, which forwards it) and removes it after 5 seconds. An app that ignores SIGTERM loses whatever it hadn't saved.
- **The image contains `/home/cha`, owned by uid 1000, and a `cp` that takes `-a`.** When a user keeps their data for the first time, the node starts a short-lived container from your image as root, with the entrypoint replaced by `cp -a /home/cha/. /to/`, no network and a read-only root filesystem, to copy the image's home into the user's directory. If that fails, the environment starts with an empty home and a warning in the node's log. Any Debian or Ubuntu image has `cp`; a `scratch` image or a distroless one has not.
- **The app speaks Wayland.** The compositor implements `wl_compositor`, `xdg-shell`, `xdg-decoration`, `wl_shm`, `linux-dmabuf` (v5, with feedback naming the render node), `wl_seat` (keyboard and pointer), `relative-pointer`, `pointer-constraints`, `data-device`, `primary-selection`, `viewporter`, `presentation-time`, `single-pixel-buffer`, `xdg-output` and `cursor-shape`. Nothing else is advertised (no `layer-shell`, `xdg-activation`, `text-input` or `idle-inhibit`), so an app that needs one of those may start but miss the feature. Windows are kiosk style: every toplevel is maximized to fill the output, or fullscreen when it asks to be; there are no server-side decorations.
- **The image is linux/amd64.** Nodes are x86-64 only; the published base is `linux/amd64`.

### 2.2 MUST, for GPU use

An app that draws with OpenGL, EGL or Vulkan, or decodes video on the GPU, needs these. They apply to every image that isn't a pure software renderer, which is nearly all of them.

- **Install the glvnd dispatchers, not a vendor's libraries.** `libglvnd0`, `libegl1`, `libgl1`, `libgles2`, `libvulkan1` and `libgbm1` on Debian or Ubuntu. For NVIDIA, the node's CDI spec injects the driver's own libraries and the vendor entries (`10_nvidia.json` and the like) into the container at run time; the dispatchers then find them. An image that bundles its own libGL or `libEGL` and puts it first in the library path hides them, and the app falls back to software or fails to create a context.
- **Install Mesa's drivers for Intel, AMD and CPU nodes** (`libgl1-mesa-dri`, `libegl-mesa0`, `libglx-mesa0`, `mesa-libgallium` and Vulkan's `mesa-vulkan-drivers` if the app uses Vulkan). The node gives the app the render node (VA-API devices) or nothing (CPU), and Mesa finds the rest. Without Mesa the app cannot render on those nodes. On the base these arrive as dependencies of the dispatchers.
- **The app finds the GPU through the render node, not the display server.** Use EGL with the Wayland platform (or GBM), not GLX over X11. The node adds the render node's group to the app's groups, so uid 1000 can open it.

### 2.3 SHOULD

- **Run the app on Wayland natively**, not through Xwayland. Toolkits: GTK 3 and 4, Qt (`QT_QPA_PLATFORM=wayland`), SDL 2 and 3 (`SDL_VIDEODRIVER=wayland`), Electron (`--ozone-platform=wayland`), Chrome and Firefox (`MOZ_ENABLE_WAYLAND=1`). An app that needs X11 MUST bring its own Xwayland (section 9).
- **Take the display size from `CHA_WIDTH`, `CHA_HEIGHT` and `CHA_REFRESH`** only when the app has a display of fixed size (set `fixedSize` in the catalog then). Otherwise follow the compositor: the page sizes the picture to its element and the streamer resizes the output to match, so the app receives ordinary configure events.
- **Keep the filesystem quiet while running.** The root filesystem is writable but belongs to the container and is gone when the environment ends. State that matters goes in the home (section 5).
- **Include fonts, a cursor theme and a locale.** The compositor draws no decorations, fonts or cursors for you. The base has `fonts-dejavu-core`, Noto, Noto Color Emoji, the DMZ cursor theme (`XCURSOR_THEME=DMZ-White`) and `en_US.UTF-8`.
- **Treat a missing GPU as a case.** The same image runs on NVIDIA, VA-API and CPU nodes. Detect software rendering and turn heavy effects off, rather than failing.
- **Keep the image small.** A node pulls it on first launch, and the user watches a progress bar. Our Chrome image is 369 MB.
- **Pin versions** (section 7) and label the image: `org.opencontainers.image.licenses`, `.source`. The node doesn't read labels (our own images carry `sh.cha.template` for humans only).

### 2.4 MAY

- Start more than one process (a window manager inside a nested compositor, a clipboard helper, a status writer). The container's init reaps orphans.
- Set `shmMb` as large as the app needs, within the node's memory.
- Use the integrations in section 4.
- Ignore sound, gamepads and the clipboard. Nothing requires an app to use them.

## 3. What the node gives the container

The app container is made by `app_config` in `crates/cha-node/src/environments.rs`. The streamer container is separate (host network, the GPU, the input devices) and isn't the image's concern.

**Environment variables the node sets**

| Variable | Value | Notes |
|---|---|---|
| `XDG_RUNTIME_DIR` | `/run/cha` | Also set by the base. The shared volume. |
| `WAYLAND_DISPLAY` | `wayland-0` | The socket is `/run/cha/wayland-0`. |
| `CHA_WIDTH`, `CHA_HEIGHT` | the output size in pixels | The size the environment starts with. |
| `CHA_REFRESH` | 60, 90 or 120 | The frame rate the user chose (`fps` in the catalog is the default). |
| `CHA_SHARED_DIR` | the shared directory's path | Only when a shared directory is mounted (section 5.4). |
| `CHA_PER_USER_DIRS` | colon-separated paths | Only when there are per-user directories laid over the shared one. |

Anything else you set with `ENV` in the image stays. The image's own `ENV` is read as usual; a variable the node sets wins over the same name in the image.

**Mounts**

| Where | What | Notes |
|---|---|---|
| `/run/cha` | Docker volume shared with the streamer | Holds `wayland-0`, `pulse/native`, and the files in section 4. Owned by uid 1000. New for every environment; gone when it ends. |
| `/dev/input` | The streamer's virtual gamepads | Read-only volume; only these devices exist there. Present when the node can make gamepads. |
| `/run/udev` | udev's entries for those gamepads | Read-only volume. Chrome, Firefox and Wine look here. |
| `/dev/hidraw<n>` | Hidraw nodes of DualSense and Steam Controller pads | Read-only subpath mounts, one per node; the device cgroup allows exactly those. |
| `/home/cha` | The user's directory for this app | Only when the user keeps their data (section 5). Otherwise `/home/cha` is the image's. |
| `/srv/cha-portal/shared/<template id>` | The app's shared directory | Only when its access isn't `none`; read-only for `read` (section 5.4). A node whose owner keeps it elsewhere mounts it at that path instead. |
| per-user paths inside the shared directory | The user's own directories, from their home | Writable even when the rest is read-only (section 5.4). |
| the host's NVIDIA Wine directory | At its own path, read-only | Only on NVIDIA nodes where it exists (it has `nvngx.dll`, which Proton needs for DLSS). |

**Devices and limits**

| What | Value |
|---|---|
| User | `1000:1000`, plus the render node's group (and, for the `steam` profile, the primary node's group) |
| GPU, NVIDIA | CDI device request (the driver's libraries, device nodes and vendor files) |
| GPU, Intel or AMD | One render node (`/dev/dri/renderD*`) with `rw` |
| GPU, none | Nothing |
| Device cgroup | `c 13:* rw` (input devices) when the node can make gamepads, plus exactly each hidraw node as `rwm` |
| `/dev/shm` | `shmMb` MiB |
| Open files | `nofile` 524288 for `steam`; Docker's default otherwise |
| Capabilities | none (`CapDrop: ALL`) |
| Privilege gain | blocked (`no-new-privileges`) |
| Init | Docker's `tini` is PID 1 (`Init: true`) |
| Restart | `no` |
| Network | Docker's default bridge: outbound internet, no route to the streamer except through `/run/cha`. Not the host network |
| Root filesystem | Writable, the container's own |

**Confinement by `security` profile**

| Profile | seccomp | AppArmor | Use it for |
|---|---|---|---|
| `standard` | Docker's default | Docker's default | Everything that doesn't create namespaces |
| `browser` | Docker's default plus `clone`, `unshare`, `setns`, `chroot`, `mount`, `umount2`, `pivot_root` ([`seccomp-browser.json`](../crates/cha-node/profiles/seccomp-browser.json)) | Docker's default | Apps with their own namespace sandbox (Chrome, Firefox, Electron with the sandbox on) |
| `steam` | as `browser` | `cha-sandbox` ([`deploy/node/host/apparmor/cha-sandbox`](../deploy/node/host/apparmor/cha-sandbox)), which allows mounts and `pivot_root` inside user namespaces the app creates | Apps that build a bubblewrap container (Steam's pressure-vessel). The owner must load the profile on the node |

The `browser` profile widens seccomp only. The kernel still demands capabilities inside a new namespace, and the container has none in its own, so a namespace the app creates can't reach beyond the container. The `steam` profile is the widest we offer and the portal keeps it behind an admin's approval for third-party catalogs (section 6.3).

Pick the narrowest profile that works: try `standard` first. The sign that an app needs more is an error like `bwrap: Failed to make / slave: Permission denied`, `clone failed: Operation not permitted` or Chrome's `No usable sandbox`.

## 4. Optional integrations

Each is a file or socket in `/run/cha` that the streamer reads. An app that doesn't use them runs without them.

**Setup status: `/run/cha/status`.** For a first run that is slow and shows nothing (a download, an install). Write one JSON object, replacing the file whole:

```sh
printf '{"label":"Downloading","done":123,"total":496,"unit":"MB"}' > /run/cha/status.tmp \
  && mv /run/cha/status.tmp /run/cha/status
```

Only `label` is required. With no `total` the page shows an indeterminate bar; `unit` names `done` and `total`. The streamer reads the file every 250 ms and shows the label and bar over the picture to everyone watching. A file that doesn't parse, or is over 4 KiB, is ignored and the last status stands. Remove the file, or write `{}`, when the app's UI is up: nothing else clears it.

**Performance overlay: `/run/cha/mangohud.conf`.** For an app that can draw MangoHud's overlay. Create the file before the app starts, with the line `no_display`; its existence tells the streamer the app has an overlay. The streamer turns the overlay on and off by replacing the file atomically (`mangoapp_steam` and `preset=N`, N 1 to 4, for on). The app must re-read the file when it changes; `mangoapp` does within about 100 ms. Our Steam image is the only user ([`images/README.md`](../images/README.md#the-contract)).

**Clipboard.** Wayland clients share the compositor's clipboard (`data-device`) and need nothing. An X11 app keeps its clipboard inside its X server, which only the app's container can reach, so an image with its own Xwayland or X server must bridge it: run `cha-x11-clipboard` (source: [`crates/cha-x11-clipboard`](../crates/cha-x11-clipboard), x11rb, AGPL-3.0-or-later) as the user of the X session once the X server is up. It connects to `/run/cha/clipboard`, the streamer's Unix socket, and passes text both ways (CLIPBOARD only, up to 1 MiB). Without it, copy and paste between the page and the X app does nothing.

**Gamepads.** The streamer makes virtual controllers before the app starts: one Xbox 360 pad over `uinput`, or a DualSense or Steam Controller over `uhid`, according to the user's choice for the app. Their device nodes are in `/dev/input` (`event*` nodes), udev entries in `/run/udev`, and for the uhid kinds also `/dev/hidraw<n>` (used by SDL's HIDAPI driver, Proton and Steam). Anything that reads `/dev/input` or the Gamepad-like APIs on top of it works: SDL (set `SDL_JOYSTICK_DISABLE_UDEV=1`), evdev, libinput, Wine, browsers. See [`controllers.md`](controllers.md). An app can ask for a default kind with the catalog's `gamepad`.

**Steam's `uinput`: `/run/cha/uinput.sock`.** Steam Input makes its own virtual pad by opening `/dev/uinput`, which no app container has. With the `steam` kind of pad, the streamer listens on this `SOCK_SEQPACKET` socket and makes the device on the app's behalf after validating the request. Our Steam image preloads a shim (`images/steam/uinput-shim/`) that turns Steam's `open("/dev/uinput")` into a connection to it. No other app needs this; the protocol is in `crates/cha-streamer/src/uinput_proto.rs`.

## 5. App data

### 5.1 The home

On the node, each (user, app) pair has a directory the user may keep:

- **Keeping is on or off per user and app.** The user chooses; an admin sets the default per app, which starts as the catalog's `persistent`. When on, the node mounts `users/<user id>/<template id>` from its data root (`CHA_DATA_ROOT`, `/srv/cha-portal` unless the owner changes it) at `/home/cha`. The directory is `1000:1000`, mode `0700`, made by the node's agent. When off, `/home/cha` is the container's own and goes with it.
- **The first time, the directory is filled from the image's `/home/cha`.** The node copies it as described in section 2.1 (`cp -a`: owners, modes, links). After that the image's home is ignored. **Don't put anything in `/home/cha` that an image update must replace.** A new version of your image does not touch the files of users who already have a directory. Put program files, defaults the app re-reads, and anything update-critical elsewhere (`/opt`, `/usr/local`, `/etc`), and let the app fall back to them. An app that keeps its config in the home must cope with an older config written by an older image.
- **The directory holds the user's files across stops and node reboots.** Stopping an environment never removes it. The user can reset it (the portal deletes the directory and the next launch seeds again), and may not while an environment of that app is live.
- **One live environment per (user, app) when the home is kept**, since two would share the home. A second launch is refused with a message.

### 5.2 Coping with an unclean stop

A stopped container leaves its state in the home: lock files, stale pid files, sockets, caches that point into a container that no longer exists. The next launch starts in a new container (new hostname, new `/tmp`, new `/run/cha`) over the same home. Start-up must cope:

- **Lock files that record a host name or pid are stale after every stop.** Qt's `QLockFile` records the host name, which is new in every container, so a lock never looks stale and the app waits out its stale time on each file. Remove such locks before the app starts (our `kde-clean-stale` does, for files under `~/.config`, `~/.local/share`, `~/.local/state` and `~/.cache`, under 512 bytes, that begin with a pid and a host name).
- **Pid files and named pipes of the previous run** (Steam's `steam.pid`, `steam.pipe`), the D-Bus session bus address and ICE authority files, and the service cache (`ksycoca*`). Delete them, or make the app ignore them.
- **Nothing in the home may assume the same hostname, IP, uid-to-name mapping or display number as last time.**
- **Check the home is writable by uid 1000** and say so on the page if not (`/run/cha/status`) or on stderr. A directory created by something other than the node's agent may not be.
- **Sockets and buses belong in `/tmp` or `/run/cha`**, which are new on each launch, not in the home.

### 5.3 Moving from older volumes

Earlier nodes kept homes in Docker volumes named `cha-home-<user id>-<template id>`. The node copies such a volume once into the new directory, in a short-lived container of your image, and leaves the volume in place. This does not change what an image does. It is why the image's `cp` has to work as section 2.1 says.

### 5.4 Shared data

An app can share a directory across all its users. The template's `shared.access` is the default (`none`, `read`, `write`); an admin can change it.

- **Mounted unless `none`**, at `/srv/cha-portal/shared/<template id>` (read-only for `read`). If the node's owner keeps this template's shared directory elsewhere (`CHA_SHARED_DIRS`, a NAS share), it is mounted at its own path and the agent never creates, changes or removes anything in it.
- **The app finds it in `CHA_SHARED_DIR`.** The variable is unset when no shared directory is mounted (access `none`, or an external directory that isn't usable, which the agent logs). The app MUST start without it.
- **`shared.perUser`** lists relative paths inside the shared directory that each user keeps their own copy of, because sharing them breaks the app (Steam's Proton prefixes and shader caches). The user's own directory for each, `.cha-shared/<path>` in their home, is mounted over it, writable even when the rest is read-only. They live in the home, so an app with such parts gets its shared directory only for users who keep their data. `CHA_PER_USER_DIRS` lists the targets, colon separated. The node watches that they stay mounted (a NAS can drop them) and warns the user if not; an image whose app would misbehave without them SHOULD check `/proc/self/mountinfo` at start and refuse to run (Steam's `steam-per-user-dirs` exits 1).
- **Two users write at once.** Anything the app puts in a shared directory is shared with every user of the app. Design for concurrent writers or use `read`.

## 6. Catalog entry reference

A catalog is JSON. The JSON Schema is [`images/catalog.schema.json`](../images/catalog.schema.json) (draft 2020-12). Check a catalog with any validator, for example:

```bash
bunx ajv-cli@5 validate --spec=draft2020 -s images/catalog.schema.json -d my-catalog.json
```

The portal does not use unknown fields and does not reject them: serde ignores keys it doesn't know, so the schema allows extra properties too (a typo in a field name is silently ignored, which the schema won't catch; read the field names carefully). The portal does reject a document that isn't valid JSON, lacks a required field or has a wrong type.

### 6.1 The document

| Field | Type | Required | Notes |
|---|---|---|---|
| `version` | integer | no | `1`. Absent means 1. Any other value is refused. |
| `id` | string | no | External catalogs: a suggested namespace slug, `^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$`. The admin chooses the real one when loading the catalog. |
| `name` | string | no | External catalogs: a display name for the catalog. |
| `templates` | array of template | yes | At least one is useful; the portal accepts an empty list. |

The built-in catalog is the same document without `id` and `name`.

### 6.2 A template

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `id` | string | yes | | Unique in the document, not empty. Use `^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$`, and not `migrated` or `migrating` (the node's own files beside a home): the portal refuses an external catalog otherwise. The id appears in URL path segments, in the node's directory names (`users/<user>/<template>`) and in Docker labels. Local to the document: the portal namespaces an external catalog's ids as `<catalog>.<app>` (6.3). |
| `name` | string | yes | | The card's title. |
| `description` | string | yes | | One or two sentences under the title. May be empty, but a card with none is poor. |
| `image` | string | yes | | The image reference to run. Not blank. `{version}` is replaced by the node agent's release when the node resolves it. An external catalog's must name a registry (6.3). |
| `localImage` | string | no | none | A dev build the node runs when it already has it, before pulling `image`. Built-in catalog only. |
| `class` | string | yes | | Groups the card. `browser`, `desktop`, `gaming` and `test` get their own icon, and `desktop` templates are listed as desktops; any other value is accepted and gets a generic icon. |
| `icon` | string | no | none | The app's logo (6.4). Absent: a generic icon for the class. |
| `security` | `standard` \| `browser` \| `steam` | yes | | The confinement profile (section 3). |
| `shmMb` | integer | yes | | `/dev/shm` size in MiB. Docker's default is 64. Browsers need 512 to 1024; Steam uses 2048. |
| `persistent` | boolean | no | `false` | The default for whether users keep their home. Users and admins override it. |
| `fixedSize` | boolean | no | `false` | The app's display has a fixed size (`CHA_WIDTH`×`CHA_HEIGHT`) and the app letterboxes or misplaces input in any other. The page then never asks for a resize and the browser letterboxes the picture. |
| `shared` | object | no | none | `{"access": "none" \| "read" \| "write", "perUser": ["path", ...]}`. `access` is required in it; `perUser` defaults to empty. Section 5.4. |
| `gamepad` | `xbox360` \| `dualsense` \| `steam` | no | `xbox360` | The virtual controller the app gets unless the user picks another. The kind is fixed for the environment's life. |
| `fps` | `60` \| `90` \| `120` | no | `60` | The frame rate the app gets unless the user picks another. Any other number is ignored at run time and 60 used. The picture only shows that fast on a display that refreshes that fast. |
| `needsGpu` | boolean | no | `false` | The app needs real 3D and never runs on the CPU device. A CPU-only node is never chosen for it. Without it, the portal still prefers a GPU, but a CPU node is allowed. |

### 6.3 Built-in and external catalogs

| Rule | Built-in (`images/catalog.json`) | External (an admin loads it) |
|---|---|---|
| `image` | Any reference; a bare dev name (`cha/env-chrome:dev`) is allowed | Must name a registry: the first path part has a `.` or `:` or is `localhost` (`ghcr.io/you/name:tag`, `localhost:5000/name`). A bare name or `library/name` is refused |
| `localImage` | Allowed | Refused |
| `security` | Any of the three | Any of the three, but see trust below |
| `icon` | A file in the template's directory under `images/`, listed in `ICONS` | An `https://` URL, or a path relative to the catalog's URL (6.4) |
| Template ids | Bare (`chrome`) | Namespaced by the portal as `<catalog>.<app>` |
| Duplicate ids | Refused | Refused |

**Namespacing.** An external catalog is loaded under a slug the admin chooses (`^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$`; the catalog's own `id` is only a suggestion). Its template `foot` becomes `<slug>.foot` everywhere the portal uses an id. The separator is a dot because ids appear in URL path segments, in node directory names and in Docker labels. Don't put a dot in your own ids; the schema's id pattern forbids it. The built-in catalog keeps bare ids, so no external template can take the place of a built-in one.

**Trust.** Adding a catalog trusts its `standard` templates. A template asking for `browser` or `steam` stays unavailable until an admin approves that template's profile explicitly. If a refresh of the catalog changes a template's profile or the registry host of its image, the approval is cleared and the admin approves again. A catalog cannot give itself more confinement than the three profiles above; it can't name a seccomp profile, a capability, a device or a mount of its own. Only an admin's custom environment can ask a node for those, and only from a node whose owner allows them ([ADR 0021](adr/0021-custom-environments.md)).

Loading catalogs is the admin's feature and is being built; this section is the contract it will enforce. Until it ships, test an image by adding a template to `images/catalog.json` in your own checkout of this repository (with `localImage` for a local build).

### 6.4 Icons

- **Format**: SVG only, at most 256 KiB, with a `viewBox` (without one it doesn't scale). No scripts.
- **Built-in**: `"icon": "icon.svg"`, a file next to the image's `Dockerfile` under `images/<id>/`. The portal embeds it at build time and a test checks the list.
- **External**: an `https://` URL, or a path relative to the catalog's URL (`"icon": "icons/foot.svg"` next to `https://example.com/cha/catalog.json`). The portal fetches it when it loads the catalog, so the icon is not fetched by users' browsers from your server.
- **Serving**: the portal serves it at `/api/catalog/<id>/icon` as an inert image: `Content-Security-Policy: default-src 'none'`, `X-Content-Type-Options: nosniff`, no scripts. Anything your SVG fetches or runs is dropped.
- **Without an icon** the card shows a generic one for the `class`.
- A logo is shown only to say which app a card launches. If it is a trademark, say so where you keep the file ([`images/README.md`](../images/README.md#logos) does for ours).

### 6.5 What gets pulled

The portal gives the node a list of candidates, in order: `localImage` if set, then `image`, each with `{version}` filled. The node takes the first one it already has. If it has none, it pulls the first that names a registry; it never pulls a bare name. A pull shows a progress bar on the card and the "Starting…" page. A failed pull names every candidate tried.

Consequences you should plan for:

- **A node never re-pulls a tag it already has.** `:latest` or any fixed tag that you move is not picked up by nodes that hold the old one. To ship a change, use a new tag. `{version}` makes this automatic: the tag follows the node agent's release.
- **`{version}` is the node agent's version** (for example `0.2.1`), so a node pulls the image built for its own contract. Publish a tag for every agent release you support, or leave `{version}` out and use a tag you control.
- **The registry must be reachable from the node, without credentials the node doesn't have.** The node pulls without credentials, so the image must be public.

## 7. Building on our base

`ghcr.io/ban-red/cha-env-base` is Ubuntu 26.04 plus what sections 2 and 3 ask for. Starting `FROM` it is the shortest route to an image that works.

What it provides:

- User `cha`, uid and gid 1000, home `/home/cha` (the stock `ubuntu` user, renamed), `WORKDIR /home/cha`, and `USER cha` (so `USER root` for package installs and back to `USER cha` after).
- `ENTRYPOINT ["cha-run"]`. `cha-run` waits for `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` for up to 30 s (polling every 100 ms), then `exec`s its arguments. Set `CMD` to your app; keep the entrypoint.
- `ENV`: `HOME=/home/cha`, `XDG_RUNTIME_DIR=/run/cha`, `WAYLAND_DISPLAY=wayland-0`, `PULSE_SERVER=unix:/run/cha/pulse/native`, `SDL_JOYSTICK_DISABLE_UDEV=1`, `XCURSOR_THEME=DMZ-White`, `NVIDIA_DRIVER_CAPABILITIES=all`, `LANG=en_US.UTF-8`.
- Packages: `ca-certificates`, `locales`, `tzdata`; the GPU dispatchers `libglvnd0`, `libegl1`, `libgl1`, `libgles2`, `libvulkan1`, `libgbm1` (and through them Mesa's drivers); `libwayland-client0`, `libwayland-cursor0`, `libwayland-egl1`, `libxkbcommon0`; `libpulse0`; `fonts-dejavu-core`, `fonts-noto-core`, `fonts-noto-color-emoji`; `dmz-cursor-theme`.
- `/tmp/.X11-unix` and `/tmp/.ICE-unix` (mode 1777), so an unprivileged X session can make its sockets. The base keeps no apt package lists: run `apt-get update` in the same `RUN` as your installs.
- A stand-in `bwrap` first in `PATH` (`images/base/bwrap-shim`). GTK loads images through glycin, which runs each loader under bubblewrap. bubblewrap can't build its sandbox in an unprivileged container whose Docker AppArmor profile denies its mounts, so GTK aborts. The stand-in runs the loader directly: the container is the sandbox. Remove it (`rm /usr/local/bin/bwrap`) in an image that needs the real one, such as one run under the `steam` profile.

A minimal image:

```dockerfile
ARG BASE=ghcr.io/ban-red/cha-env-base:0.2.1
FROM ${BASE}
USER root
RUN apt-get update && apt-get install -y --no-install-recommends foot \
 && rm -rf /var/lib/apt/lists/*
USER cha
CMD ["foot"]
```

**Versioning.**

- **`{version}` is the node agent's release**, not your image's. Tag your image the same way (`ghcr.io/you/cha-env-foot:0.2.1`) and put `{version}` in `image`.
- **Pin the base by version, never `latest`.** The base's tags are the release version (`0.2.1`) and a source hash (`inputs-…`, `sha-…`); there is no `latest`. Build your image `FROM` the base of the release you target. A later base can change the packages, the user or the entrypoint the way this document describes, and the contract is per release.
- **Re-build and re-tag on each agent release you support.** A node at 0.3.0 asks for `…:0.3.0`; if you haven't published it, the pull fails and the user sees which image the node tried.
- **amd64 only.** Build with `--platform linux/amd64` (the base is linux/amd64 only).
- **To build against a local base**, use a build argument for it: `docker build --build-arg BASE=cha/env-base:dev -t cha/env-example:dev images/example`. Our base is built with `docker compose -f images/compose.yaml build`, which tags `cha/env-base:dev`.

An image not built on our base MUST provide all of section 2 itself: the socket wait; `HOME`, `PULSE_SERVER` and `SDL_JOYSTICK_DISABLE_UDEV` (and `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` if it relies on them); the glvnd dispatchers, so that CDI's NVIDIA libraries load; Mesa for Intel, AMD and CPU nodes; a uid 1000 user with home `/home/cha`; `cp`; fonts and a cursor theme. A distribution other than Ubuntu works if its glibc and libraries are recent enough for Mesa.

## 8. Checking an image

```bash
cha-node --check-image <image-ref> [--profile standard|browser|steam]
```

Run it on the node. An image the engine doesn't have is pulled if its reference names a registry; a bare name never is. Exit status is 0 when every MUST check passes, 1 otherwise. It prints one line per check, `ok`, `info`, `warn` or `FAIL`, then the check's name and a short reason. A `FAIL` is a MUST that isn't met; `info` lines (built on our base, size) judge nothing. The default profile is `standard`; pass the profile the template will ask for.

What it does:

- **Reads the image's config:** linux/amd64, a start command, `HOME`, `PULSE_SERVER`, `SDL_JOYSTICK_DISABLE_UDEV`.
- **Runs it as a launch would**, with the profile's confinement (uid 1000, no capabilities, no privilege gain, its seccomp and AppArmor profile, `/dev/shm`) and no GPU:
  - a probe (the one check that overrides the entrypoint, with `sh -c`) checks that it runs as 1000 and that `$HOME` is writable;
  - started with no Wayland socket in `/run/cha`, it must still be running after 3 s;
  - beside a real `cha-streamer` on the CPU, it must connect to the compositor within 30 s.
- **Leaves nothing behind.** Its containers and volumes carry `sh.cha.image-check` and are removed when it ends, also on Ctrl-C.

An app that needs a GPU (Steam's gamescope) can't connect to a CPU compositor; the check reports that as a `warn`, not a `FAIL`.

Run it before you write the catalog entry and again whenever you change the image. Passing doesn't prove the app works: launch it from the portal and use it (section 6.3 says how to get a test template in).

## 9. What doesn't work

- **Images that need root, `s6`, `systemd` or an init system of their own.** The container runs as uid 1000 with no capabilities. The linuxserver.io images (s6-overlay, `PUID`/`PGID`), Kasm workspaces and anything with `supervisord` as root fail at start. Rebuild them to run as uid 1000 with a plain command.
- **Added capabilities.** There is no way to ask for `SYS_ADMIN`, `NET_ADMIN`, `SYS_PTRACE` or any other, and no `--privileged`. The profiles in section 3 are the whole menu.
- **X11-only apps without their own Xwayland.** The compositor has no X server and no `DISPLAY`. The image must run Xwayland itself (rootful, as XFCE's does; or inside gamescope, as Steam's does) and bridge the clipboard (section 4). An image that starts only an X client gets "cannot open display".
- **VNC, noVNC, RDP and web desktops** (the linuxserver.io and Kasm desktop images): they bring their own capture and transport, and a framebuffer that nothing here can encode. They also bring root-owned supervisors. Use the application itself on Wayland, or a desktop running as a Wayland client (KDE's KWin and Steam's gamescope run as clients of our compositor).
- **Its own display server or login manager** (GDM, SDDM, a Wayland session that wants DRM master, `seatd` or `logind`). There is no seat, no VT and no DRM master; the output is the compositor's.
- **Non-amd64 images.** Nodes and the base are linux/amd64.
- **Hosting its own network service** for the user. The app is on a bridge network with no published ports; users reach the environment only through the streamer.
- **Apps that need a persistent hostname, MAC address or machine id** across launches. Each launch is a new container.
- **A different `ENTRYPOINT` that doesn't wait for the socket**, without something that does: the app starts before the compositor listens and dies.
- **Writing in the shared directory when access is `read`**, or counting on `CHA_SHARED_DIR` being set.
