# Deploying Cha Portal

| Path | What |
|---|---|
| [`portal/`](portal) | The portal's compose stack: `cha-control` (API, SPA, the nodes' WebSocket, SQLite), plus optional Caddy (HTTPS with public DNS) and coturn (TURN) profiles |
| [`node/`](node) | A node's compose stack: the agent, which starts each environment's streamer and app containers through the Docker socket |
| [`node/host/`](node/host) | Host files that need root, installed by the owner with `install.sh`: the `cha-sandbox` AppArmor profile (Steam environments need it), the udev rules that keep virtual gamepads (Xbox 360, DualSense, Steam Controller) out of a desktop host's own session, and the `uinput`/`uhid` module list |
| [`streamer/`](streamer) | The streamer's image (`--target runtime`) and its dev loop on a node |

## A portal and one node

1. **The portal**, on any small machine:

   ```bash
   docker compose -f deploy/portal/compose.yaml up -d --build
   ```

   It listens on `127.0.0.1:7676`. Serve it over HTTPS: browsers only give gamepads, keyboard lock and audio worklets to secure pages. On a tailnet, run `sudo tailscale serve --bg 7676` ([guide](../docs/guides/tailscale.md)). With public DNS, set `CHA_DOMAIN` and add `--profile tls` (Caddy). A fresh portal is open to claim: the first visitor creates the first admin, so open it right after the first start.
2. **The node**, on the GPU server (NVIDIA with the Container Toolkit's CDI spec; or an Intel or AMD GPU, or no GPU at all: *Devices*, below). Build the images it runs:

   ```bash
   docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
   ```

   ```bash
   docker compose -f images/compose.yaml build
   ```

   Then start the agent and claim it. On the portal's LAN, start it with nothing else: it shows a pairing code in its log and appears in the portal under **Admin → Nodes → Found on your network**, where **Claim** asks for that code ([Claiming a node](#claiming-a-node)):

   ```bash
   docker compose -f deploy/node/compose.yaml up -d --build
   ```

   ```bash
   docker compose -f deploy/node/compose.yaml logs agent
   ```

   Anywhere else (another subnet, a tailnet), enroll it with a join token from **Admin → Nodes → Add node** instead:

   ```bash
   CHA_PORTAL_URL=https://portal.example CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
   ```

3. **Install the host files** (once, and after an update that changes them), as the machine's owner:

   ```bash
   sudo deploy/node/host/install.sh
   ```

   It installs, only where they differ, the udev rules (`/etc/udev/rules.d`), the `cha-sandbox` AppArmor profile (`/etc/apparmor.d`, loaded; skipped on a host without AppArmor) and `/etc/modules-load.d/cha.conf` (`uinput` and `uhid` at boot, and loaded now), prints one line for each, and can be run again safely. `deploy/node/host/install.sh --check` only reports, without root. The agent never writes these: it only reads them, through read-only binds in the compose file.

4. **Check the node**:

   ```bash
   CHA_PORTAL_URL=https://portal.example docker compose -f deploy/node/compose.yaml run --rm agent --doctor
   ```

   It checks Docker, the images, the GPU through CDI, NVIDIA's Wine DLLs for DLSS, PyroWave's Vulkan device, `/dev/uinput` and `/dev/uhid`, the kernel modules for the DualSense and Steam Controller, the Steam sandbox, whether the host files above are installed and current, the data root and any shared directories kept outside it, the home volumes left from before app data moved, the render node, user namespaces, the clock against the portal's (media tokens last 60 s), and the streamers' ports. It says how to fix each problem and changes nothing itself. The agent also logs a warning when it starts if the host files are missing or old.

## Claiming a node

An agent with no identity and no join token is **unclaimed** ([ADR 0007](../docs/adr/0007-claim-nodes-found-on-the-lan.md)):

- It picks an 8-digit pairing code, logs it (`unclaimed: … claim "<name>" with code 4821-9375`), and keeps it in its state directory for `--doctor`, which shows it too.
- It advertises itself on the LAN over mDNS (`_cha-node._tcp`), with its name, GPU and key fingerprint, and listens for a claim on TCP 7679 (`CHA_CLAIM_PORT`). It does both only until it is claimed.
- The portal lists what it finds under **Admin → Nodes → Found on your network**. **Claim** asks for the code. The portal and the node then prove to each other that they know it, without sending it (SPAKE2), and the portal records the node's key as enrolled. No token or secret crosses the network.
- Five wrong codes make the node pick a new one, logged, and refuse claims for 30 s.
- After the claim, the node connects to its own `CHA_PORTAL_URL` if it has one, else to the URL the admin's browser used for the portal (or the portal's `CHA_PUBLIC_URL`). It refuses a plain-`http://` URL to another machine unless `CHA_ALLOW_INSECURE_PORTAL` is set, and the portal shows why.

mDNS stays on one LAN: it doesn't cross subnets or a tailnet, where the join token is the way. The portal needs to see multicast, so its compose files run it on the host network. `CHA_DISCOVERY=false` on a node, or `CHA_DISCOVER_NODES=false` on the portal, turns this off.

## Published images

A release (a `v*` tag) publishes every image to GitHub's container registry, built by [`.github/workflows/publish.yml`](../.github/workflows/publish.yml): `ghcr.io/ban-red/cha-portal`, `cha-node` (the agent) and `cha-streamer`, and the environments, `cha-env-test-pattern`, `-chrome`, `-firefox`, `-xfce`, `-kde` and `-steam` (with their base, `cha-env-base`). Each is tagged with the version (`0.1.0`) and the commit (`sha-1a2b3c4`). There is no `latest`: use one version for all of them, since they change together. Each image carries signed build provenance (`gh attestation verify oci://ghcr.io/ban-red/cha-streamer:0.1.0 --owner ban-red`). The Chrome and Steam images contain Google Chrome and Valve's Steam bootstrap, under their owners' terms.

For a portal and a node on one machine, [`quickstart/compose.yaml`](quickstart/compose.yaml) runs both from these images ([SETUP.md](../SETUP.md#quick-start)). With the separate stacks, set the image variables and pull instead of building:

```bash
CHA_PORTAL_IMAGE=ghcr.io/ban-red/cha-portal:0.1.0 docker compose -f deploy/portal/compose.yaml pull
```

```bash
CHA_PORTAL_IMAGE=ghcr.io/ban-red/cha-portal:0.1.0 docker compose -f deploy/portal/compose.yaml up -d
```

On a node, set `CHA_NODE_IMAGE` the same way, and `CHA_STREAMER_IMAGE=ghcr.io/ban-red/cha-streamer:0.1.0` (in `deploy/node/.env`, so every run gets them). The agent pulls the streamer image when it starts, if the node doesn't have it. It only ever pulls an image whose name includes its registry: a bare name like `cha/streamer:dev` is a local build, and pulling it would fetch whatever Docker Hub's `cha` namespace holds. To update, change the version and pull again; leave `--build` off, or compose builds from source instead.

For the environments, set `CHA_IMAGE_REGISTRY=ghcr.io/ban-red` and `CHA_IMAGE_TAG=0.1.0` on the node. The agent then runs each catalog image from its published copy (`cha/env-chrome:dev` as `ghcr.io/ban-red/cha-env-chrome:0.1.0`), and pulls it on the first launch that needs it; pull them ahead of time to spare that first launch the download (Steam's is the largest). Without `CHA_IMAGE_REGISTRY` it runs the images built on the node.

## Reaching nodes

The stream goes straight from the node to the browser; the portal only brokers it. A browser needs a UDP path to the node:

| Where the player is | What to do |
|---|---|
| Same LAN | Nothing |
| A tailnet or WireGuard | Nothing: streamers offer the mesh address too ([guide](../docs/guides/tailscale.md)) |
| Internet, with a port-forward | Forward UDP 7600–7647 to the node (WebRTC and WebTransport, two ports per environment), and set `CHA_PUBLIC_ADDRESS` on it |
| Internet, UDP to the node blocked | The portal's `turn` profile (coturn), with `CHA_TURN_SECRET`, `CHA_TURN_URLS` and `CHA_TURN_PEERS` |

## Ports

Every Cha Portal service sits in the 76xx range.

| Port | Used by |
|---|---|
| TCP 7676 | The portal (`cha-control`), `127.0.0.1` only by default |
| TCP 7677 | The dev portal (`bun run dev`) |
| TCP 7678 | The Vite dev server for the web app |
| TCP 7679 | An unclaimed node agent, waiting for a claim; UDP 5353 for its mDNS advertisement |
| 7600–7647 | Environment streamers: three ports each from `CHA_PORT_BASE` (TCP signalling on localhost, UDP WebRTC, UDP WebTransport), 16 environments by default |
| TCP 7660, UDP 7661–7662 | A standalone `cha-streamer` (signalling, WebRTC, WebTransport) |

The spikes under `spikes/` keep their own ports.

## Node settings

| Variable | Default | What |
|---|---|---|
| `CHA_PORTAL_URL` | | The portal's URL: `https://`, or `http://` to this machine (a tunnel). Needed with a join token; when a claim enrolls the node, it is used instead of the URL the claim brings |
| `CHA_ALLOW_INSECURE_PORTAL` | `false` | Development only: allow plain `http://` to another machine, e.g. a dev portal on your LAN (`CHA_LISTEN=0.0.0.0:7677 bun run dev`). The node's traffic, which can start containers here, then crosses the network unencrypted |
| `CHA_JOIN_TOKEN` | | One-time, to enroll; without it (and without an identity) the agent waits to be claimed ([Claiming a node](#claiming-a-node)) |
| `CHA_DISCOVERY` | `true` | An unclaimed agent advertises itself on the LAN and waits for a claim; `false` asks for a join token instead |
| `CHA_CLAIM_PORT` | `7679` | Where an unclaimed agent listens for a claim |
| `CHA_NODE_IMAGE` | `cha-node:dev` | The agent's image, for the compose file: the local build, or a published one ([Published images](#published-images)) |
| `CHA_STREAMER_IMAGE` | `cha/streamer:dev` | The streamer image: the local build, or a published one, which the agent pulls when it starts |
| `CHA_IMAGE_REGISTRY` | | Run the environments from published images, e.g. `ghcr.io/ban-red` ([Published images](#published-images)); empty runs the ones built on the node. Must name a registry's host |
| `CHA_IMAGE_TAG` | | The release of those images, e.g. `0.1.0`; needed with `CHA_IMAGE_REGISTRY` |
| `CHA_UINPUT` | `/dev/uinput` | For virtual gamepads; empty goes without (no `uinput` module) |
| `CHA_UHID` | `/dev/uhid` | For virtual DualSense and Steam Controllers; empty goes without (no `uhid` module), and those fall back to an Xbox 360 pad |
| `CHA_PUBLIC_ADDRESS` | | The router's public IP, when it forwards the streamers' UDP ports |
| `CHA_PORT_BASE` | `7600` | Streamers use three ports each from here: TCP on localhost (signalling), UDP for WebRTC, UDP for WebTransport |
| `CHA_MAX_ENVIRONMENTS` | `16` | |
| `CHA_DATA_ROOT` | `/srv/cha-portal` | Where app data lives ([below](#app-data)): a host directory the compose file also mounts into the agent at the same path |
| `CHA_NVIDIA_WINE_DIR` | `/usr/lib/x86_64-linux-gnu/nvidia/wine` | The driver's `nvngx.dll` and `_nvngx.dll`, which Proton copies into its prefixes for DLSS and the CDI spec leaves out. Bound read-only into apps at the same path when the host has it (the agent asks the engine; nothing to mount into the agent); empty goes without |
| `CHA_SHARED_DIRS` | | Keeps an app's shared directory elsewhere, `app=/absolute/path`, comma-separated (`steam=/mnt/games/steam`, a NAS). Bind each into the agent read-only at that path |

### When an environment dies

The agent removes an environment's containers when one dies on its own or a start fails, and their logs go with them. So it first reads the last 200 lines of each and keeps them in its state volume, `/var/lib/cha-node/logs/<environment id>-streamer.log` and `-app.log` (with the engine's timestamps), for the newest 50 environments. Read them from the agent's container (`docker compose exec node less /var/lib/cha-node/logs/<id>-app.log`), or from the state volume.

The same tails reach the portal: a failed environment says why in a sentence (the GPU is out of memory, with who holds it; gamescope crashed; a mount, port, `/dev/uinput` or AppArmor problem; else the last error line) and its owner and admins can open "Show log" under it. A node on an older portal still keeps the files.

### Live usage

The Nodes page shows each online node's CPU, RAM and GPUs, refreshed every few seconds. The agent reads the host's `/proc` (which its container shares) and every NVIDIA GPU through NVML (`libnvidia-ml.so.1`, which the `nvidia.com/gpu=all` CDI device brings, like `nvidia-smi`); without NVML the GPU rows are left out. The portal keeps only the latest reading in memory, so it shows nothing for a node that is offline or has been quiet for 15 seconds. An agent sends it only to a portal that says it reads it, so either can be updated first.

## Devices

An environment runs on one **device** of a node (`docs/devices.md`), and the user picks it from the Launch menu on the app's card; Launch itself takes the best one the nodes offer, naming it under the button ("on gpu-node · RTX 4090"). The agent reports what it finds, at start and every five minutes:

- **`nvidia`**: an NVIDIA GPU with NVENC, through CDI as before. Nothing to set up beyond the Container Toolkit's CDI spec.
- **`vaapi`**: an Intel GPU (an iGPU's QuickSync, or Arc) or an AMD one, composited on its render node with Mesa and encoded through VA-API. Nothing to install on the host but the kernel driver (`i915`/`xe`, `amdgpu`) and `/dev/dri/renderD*`; the streamer image carries the user-space drivers. The agent finds every render node whose driver isn't NVIDIA's from `/sys/class/drm` (which its container sees as it is, so the compose file needs no `/dev/dri` mount) and asks the streamer image what each can encode, in a throwaway container with no network that gets only that node (`cha-streamer --probe-device vaapi:/dev/dri/renderD129`). A node that encodes nothing (a virtual GPU, a driver the image lacks) isn't offered, and a streamer image that predates the probe offers no VA-API at all: rebuild it. The environment's streamer and app get that render node and its group, and nothing of NVIDIA's.
- **`cpu`**: always there. Mesa's software renderer composites and x264 encodes, H.264 only, with no GPU for the streamer or the app. It suits a desktop or a browser at modest sizes and frame rates, and costs the node's cores while it runs (the portal counts that against the node); it can't run what needs 3D, so apps marked `needsGpu` in the catalog (Steam) never run on it.

`--doctor` lists them, one line each, and says why a render node isn't one. A node whose agent predates devices is read as one `nvidia` device, so an older agent keeps working with a newer portal (and the reverse: it ignores the device a launch names and uses its NVIDIA GPU).

### Where a launch goes

`GET /api/placements` (all apps, or `?template=<id>` for one) lists every device of every online node as an option, best first, with its score and a reason when it isn't allowed. A device scores by its kind (NVIDIA 100, VA-API 60, CPU 20), less for the node's live usage (the CPU's use, and for NVIDIA the GPU's utilisation and VRAM in use) and 10 for each environment already running on it. A GPU with under 2 GB of VRAM free stays choosable, with a warning, but is never picked automatically. Not allowed: the CPU for an app that needs a GPU, a device that offers no codec browsers play, and a node whose agent can't keep the app's data. `POST /api/environments` takes an optional `node` and `device` from that list (a node alone means its best device) and refuses a choice that isn't allowed with a 400 and the reason; without them it takes the best option, or answers 409 `no_node` saying what stood in each device's way.

## Portal settings

| Variable | Default | What |
|---|---|---|
| `CHA_BIND` | `127.0.0.1:7676` | Where the portal listens (the compose file passes it as `CHA_LISTEN`; the portal runs on the host network) |
| `CHA_DISCOVER_NODES` | `true` | List unclaimed nodes found on the LAN ([Claiming a node](#claiming-a-node)) |
| `CHA_PUBLIC_URL` | | The URL nodes should use for the portal after a claim; by default the one the admin's browser used |
| `CHA_PORTAL_IMAGE` | `cha-portal:dev` | The portal's image: the local build, or a published one ([Published images](#published-images)) |
| `CHA_SECURE_COOKIES` | `true` | Keep it on behind HTTPS |
| `CHA_DOMAIN` | | For the `tls` profile (Caddy) |
| `CHA_STUN_URLS` | | STUN for players, comma-separated |
| `CHA_TURN_URLS`, `CHA_TURN_SECRET` | | TURN for players; the portal mints credentials per connection |
| `CHA_TURN_PEERS` | | coturn relays only to these addresses (the nodes) |

## App data

What apps keep between launches lives on the node, as plain directories under its **data root**, `CHA_DATA_ROOT` (`/srv/cha-portal`). The user's and the admin's settings are in the portal (each user's own, and an admin's for every app); this is the node's side.

```text
<root>/users/<user id>/<app>/           the user's home for the app: mounted at /home/cha
<root>/users/<user id>/<app>.migrated   a marker: the old home volume was copied in once
<root>/shared/<app>/                    what every user of the app shares
```

- **Settings.** Per (user, app) the user turns keeping their data on or off, and resets it (the nodes delete the directory). The admin sets, per app, whether users keep their data by default, and how much the app shares: `none`, `read` or `write`. Defaults start from the catalog: Steam keeps data and shares a library (`write`), the others do neither. Changes apply to the next launch; the user's refuse while an environment of the app is live (the mounts are made).
- **Which node.** A reset reaches every node that is connected, and says so if none is. Placement keeps a user on one node while there is one, so that is where their data is. A node that was offline when a user reset keeps the data until they reset again.
- **Permissions.** Apps run as uid 1000. The agent, which runs as root in its container, makes the directories apps mount: users' homes `1000:1000` mode 0700, shared directories `1000:1000` mode 0770. The root and the directories above them are the agent's or yours (root-owned 0755); apps never see them. Ids in paths are checked (user ids are UUIDs, app ids are catalog slugs), and the agent never follows a symlink out of the root: an app owns what is inside its mounts and can plant any link there.
- **Isolation is per mount, not per uid.** Every user's app runs as uid 1000, so a process on the node itself running as uid 1000 (your own login, if it is uid 1000) can read all of the data. Per-user uids are later work.
- **Space.** Homes and shared libraries grow with use, and nothing limits them yet. `--doctor` shows the free space under the root and warns below 20 GB.
- **Backups.** Plain directories: back up `<root>/users` with whatever you use (restic, rsync, a snapshot of the disk).
- **Moving from volumes.** Before app data lived here, Steam's home was a Docker volume per user (`cha-home-<user id>-steam`). On a user's first launch after the update, with their directory missing or empty and their volume present, the agent copies the volume into the directory (the portal shows "Moving your files…" meanwhile) and leaves the volume in place. `--doctor` lists the volumes left and which are copied; remove those with `docker volume rm` once you trust the copy.
- **Roll-out order.** Update the node's agent first (it keeps working with a portal that predates this), then the portal. A node reports its data root in its inventory; the portal refuses to start an app that needs storage on a node that doesn't (an older agent would quietly drop it).

### A shared directory on a NAS

Steam's library can live on a NAS, so games are installed once for every user and every machine. Mount the share on the node, then tell the agent where each app's shared directory is:

```bash
CHA_SHARED_DIRS=steam=/mnt/games/steam
```

and bind it into the agent read-only at the same path, next to the data root in `deploy/node/compose.yaml` (there is a commented example: `- /mnt/games/steam:/mnt/games/steam:ro`). Start the agent after the share is mounted, and restart it if it is mounted again.

- **The agent never creates, changes or removes anything in it.** It isn't the agent's, and other machines use it. Before each launch it only looks, from its read-only bind: the directory is there, and each per-user place in it (Steam: `steamapps/compatdata` and `steamapps/shadercache`) is a real directory, not a symlink. If not (the share isn't mounted and an empty mountpoint is left), the app starts without the shared directory, and with the user's home; the log says why, and Steam shows the library as unavailable. Make the places once, as a user who can write there: `mkdir -p /mnt/games/steam/steamapps/compatdata /mnt/games/steam/steamapps/shadercache` (`--doctor` prints the exact command).
- **Apps see it at the same path**, `/mnt/games/steam`, and find it in `CHA_SHARED_DIR`.
- **Ownership.** Apps write it as uid 1000. On an NFS server that maps every user to one (Unraid's `all_squash`: files `nobody:users`, modes 0777/0666) that is fine; otherwise uid 1000 must be able to write there. `--doctor` shows the filesystem (`nfs4`, from the mount table), the mode and owner, and whether uid 1000 can write.
- **A hard mount blocks.** NFS mounted `hard` (Unraid's default) makes every read and write wait for as long as the NAS is away: Steam, and anything touching the library, stops until it is back. The agent's own check gives up after 10 s and the launch goes without the share.
- **Per-user parts.** Each user's Proton prefixes and shader caches are their own, mounted over the library's `steamapps/compatdata` and `steamapps/shadercache` from their home on this node. Prefixes already in the library's own `compatdata/` from another Steam client are hidden under them and not migrated: Steam Cloud covers most saves.
- **Changes to those folders on the NAS drop the users' folders mounted over them.** When `steamapps/compatdata` or `steamapps/shadercache` changes on the server (Unraid's mover, or moving them between disks or pools; the user share's folder then changes identity), the kernel unmounts what is mounted on it, in a running app, without a word: Docker still lists the mounts. Steam would then see the library's own folders, shared by everyone, and Proton would work in them. Every 30 s the agent reads each such app's mount table from inside its container; when some are gone it logs a warning and the portal shows the user "Stop this app and start it again" on the dashboard and over the stream, and the message goes when they are back. Starting again mounts them. Steam's start-up script also refuses to start Steam without them (the app exits and the environment ends), so Proton never runs on the shared prefixes. Leave those two folders alone: `deploy/nas/move-steam-game`, which moves one game between the array and a pool, deliberately leaves a game's Proton prefix and shader cache where they are, since moving them changes the folders.
- **Experimental.** Two users updating the same game at once can clash.

## Known gaps

- Behind a proxy, the audit log records the proxy's address, not the client's.
- Users' app data lives on the node that made it. Nothing backs it up, limits its size or moves it to another node yet; the portal's reset deletes it on every connected node.
- TURN over TLS on 443 comes later.
- Steam environments need the `cha-sandbox` AppArmor profile on the node: installed by `sudo deploy/node/host/install.sh` (step 3). `--doctor` checks it.
