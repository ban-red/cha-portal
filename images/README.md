# Environment images

Our own images for the catalog (`catalog.json`, which the portal embeds). Every environment runs one of these as its **app container**, beside a `cha-streamer` container that the agent starts (plan §4.2).

| Image | What runs | Profile |
|---|---|---|
| `base` | Ubuntu 26.04, user `cha` (uid 1000), fonts, cursor theme, the GPU libraries' dispatchers | — |
| `test-pattern` | `cha-testpattern`, our own Wayland client: a sweeping bar, a frame counter, a frame-ID strip, a flash on input | `standard` |
| `chrome` | Google Chrome on Wayland, GPU-rendered, sandbox on | `browser` |
| `firefox` | Firefox (Mozilla's .deb) on Wayland, GPU-rendered, sandbox on; first-run pages off by policy | `browser` |
| `xfce` | XFCE inside one rootful, fullscreen Xwayland, so the compositor needs no X11 window manager | `standard` |

Build them on a node, from the repository root:

```bash
docker compose -f images/compose.yaml build
```

## The contract

- **Wayland.** The streamer and the app share a volume at `/run/cha` (`XDG_RUNTIME_DIR`). The streamer creates `wayland-0` there and hands the directory and socket to uid 1000. `cha-run`, the images' entrypoint, waits for the socket and then starts the app.
- **User.** The app runs as uid 1000, plus the render node's group (the agent reads it from the device), so EGL, GL and Vulkan reach the GPU. The NVIDIA driver comes from CDI at run time.
- **Confinement.** No capabilities (`--cap-drop ALL`), no privilege gain (`no-new-privileges`), and a seccomp profile:
  - `standard`: Docker's default profile.
  - `browser`: Docker's default plus `clone`, `unshare`, `setns`, `chroot`, `mount`, `umount2` and `pivot_root` ([`crates/cha-node/profiles/seccomp-browser.json`](../crates/cha-node/profiles/seccomp-browser.json)). Chrome's and Firefox's sandboxes create user, PID and network namespaces. The kernel still requires capabilities inside a new namespace, and the container has none in its own.
- **`/dev/shm`** is sized per template; browsers need more than Docker's default 64 MB.

## Known gaps

- **bubblewrap.** GTK loads images through glycin, which sandboxes each loader with bubblewrap. bubblewrap can't build its sandbox here: Docker's AppArmor profile denies its mounts, and the host restricts unprivileged user namespaces (Ubuntu's `apparmor_restrict_unprivileged_userns`). The base image therefore puts a stand-in `bwrap` first in `PATH` ([`base/bwrap-shim`](base/bwrap-shim)). It runs the loaders directly, so the container is their sandbox. A `cha-browser` AppArmor profile installed by `cha doctor` (P1.7, with the owner's sudo) will remove the shim. Steam's pressure-vessel needs the same profile.
- **XFCE** has no wallpaper yet.
