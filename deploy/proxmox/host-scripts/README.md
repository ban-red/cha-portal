# Host scripts for a Proxmox box running Cha containers

Four small scripts for the Proxmox host (run as root; they use `pct`). Copy them to `/root` and keep them executable:

```bash
scp cha-status update-lxcs update-images update-all root@proxmox-host:/root/
```

| Script | What it does |
|---|---|
| `cha-status` | Lists the LXCs, and in each the running containers with their images (`LOCAL` means built on this machine, not pulled), the checkout's tag and the pending apt upgrades. Also prints the newest version published on ghcr. |
| `update-lxcs [--host] [-n] [CTID...]` | `apt dist-upgrade` in the running LXCs (or the ones named). `--host` upgrades the Proxmox host too. It never reboots; it says when a container wants one. |
| `update-images [--version X.Y.Z\|latest] [-n] [--force] [CTID...]` | Moves Cha Portal in each container that has `/opt/cha-portal` to a release (see below). |
| `update-all [--host] [update-images options]` | `update-lxcs`, then `update-images`. |

`-n` prints what would change and changes nothing. Try it first.

## What `update-images` does

For each of `deploy/portal`, `deploy/node` and `deploy/quickstart` that has a `.env` and a running stack:

1. checks out the release tag in `/opt/cha-portal` (when it is a git clone; a copied checkout is left as it is);
2. points the `.env` at the release's images (`CHA_PORTAL_IMAGE`; or `CHA_NODE_IMAGE`, `CHA_STREAMER_IMAGE`, `CHA_GATEWAY_IMAGE`, `CHA_IMAGE_TAG`; or `CHA_VERSION`);
3. runs `docker compose pull` and `up -d` for that stack;
4. pulls every `cha-env-*` image already on the machine at the same version, and removes dangling images.

Notes:

- **"latest"** is the highest `X.Y.Z` tag of `cha-portal` on ghcr. There is no `:latest` tag on any Cha image, so the `.env` files are pinned to that version, and an update is always a visible change.
- **Local builds are protected.** A stack whose current image has no registry digest (built on the box, or a registry tag you replaced with your own) is skipped unless you pass `--force`, because the pull would overwrite it.
- **Stopped stacks stay stopped**, such as a quick start you replaced with a node alone.
- Back up the portal's database before a release that changes it ([Upgrading](../../../SETUP.md#upgrading-to-a-new-release)). A node's environments restart when its agent is recreated.

## A portal-only container

The container made for the portal (no GPU, no node) is an unprivileged Debian 13 LXC with nesting on, Docker and Tailscale installed, and the release cloned to `/opt/cha-portal`. In `deploy/portal/.env`:

```
CHA_PORTAL_IMAGE=ghcr.io/ban-red/cha-portal:0.5.0
CHA_BIND=0.0.0.0:7676
CHA_SECURE_COOKIES=false
```

That serves plain HTTP on the LAN, which gives no gamepads or keyboard lock in the browser. For HTTPS, run `tailscale up` and `tailscale serve --bg 7676` in the container, then set `CHA_BIND=127.0.0.1:7676` and drop `CHA_SECURE_COOKIES=false`. The first visitor creates the first admin, so claim it right after the first start.
