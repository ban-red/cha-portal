# Deploying Cha Portal

| Path | What |
|---|---|
| [`portal/`](portal) | The portal's compose stack: `cha-control` (API, SPA, the nodes' WebSocket, SQLite), plus optional Caddy (HTTPS with public DNS) and coturn (TURN) profiles |
| [`node/`](node) | A node's compose stack: the agent, which starts each environment's streamer and app containers through the Docker socket |
| [`node/host/`](node/host) | Host files that need root, applied by the owner: the `cha-sandbox` AppArmor profile (Steam environments need it), and the udev rule that keeps virtual gamepads out of a desktop host's own session |
| [`streamer/`](streamer) | The streamer's image (`--target runtime`) and its dev loop on a node |

## A portal and one node

1. **The portal**, on any small machine:

   ```bash
   docker compose -f deploy/portal/compose.yaml up -d --build
   ```

   It listens on `127.0.0.1:8080`. Serve it over HTTPS: browsers only give gamepads, keyboard lock and audio worklets to secure pages. On a tailnet, run `sudo tailscale serve --bg 8080` ([guide](../docs/guides/tailscale.md)). With public DNS, set `CHA_DOMAIN` and add `--profile tls` (Caddy). The first start logs a one-time setup token for the first admin.
2. **The node**, on the GPU server (NVIDIA with the Container Toolkit's CDI spec). Build the images it runs:

   ```bash
   docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
   ```

   ```bash
   docker compose -f images/compose.yaml build
   ```

   Then enroll it with a join token from **Admin → Nodes → Add node**:

   ```bash
   CHA_PORTAL_URL=https://portal.example CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
   ```

3. **Check the node**:

   ```bash
   CHA_PORTAL_URL=https://portal.example docker compose -f deploy/node/compose.yaml run --rm agent --doctor
   ```

   It checks Docker, the images, the GPU through CDI, NVIDIA's Wine DLLs for DLSS, PyroWave's Vulkan device, `/dev/uinput`, the Steam sandbox, the data root and any shared directories kept outside it, the home volumes left from before app data moved, the render node, user namespaces, the clock against the portal's (media tokens last 60 s), and the streamers' ports. It says how to fix each problem and changes nothing itself.

## Reaching nodes

The stream goes straight from the node to the browser; the portal only brokers it. A browser needs a UDP path to the node:

| Where the player is | What to do |
|---|---|
| Same LAN | Nothing |
| A tailnet or WireGuard | Nothing: streamers offer the mesh address too ([guide](../docs/guides/tailscale.md)) |
| Internet, with a port-forward | Forward UDP 47000–47047 to the node (WebRTC and WebTransport, two ports per environment), and set `CHA_PUBLIC_ADDRESS` on it |
| Internet, UDP to the node blocked | The portal's `turn` profile (coturn), with `CHA_TURN_SECRET`, `CHA_TURN_URLS` and `CHA_TURN_PEERS` |

## Node settings

| Variable | Default | What |
|---|---|---|
| `CHA_PORTAL_URL` | (required) | The portal's URL: `https://`, or `http://` to this machine (a tunnel) |
| `CHA_ALLOW_INSECURE_PORTAL` | `false` | Development only: allow plain `http://` to another machine, e.g. a dev portal on your LAN (`CHA_LISTEN=0.0.0.0:8090 bun run dev`). The node's traffic, which can start containers here, then crosses the network unencrypted |
| `CHA_JOIN_TOKEN` | | One-time, to enroll |
| `CHA_STREAMER_IMAGE` | `cha/streamer:dev` | The streamer image |
| `CHA_UINPUT` | `/dev/uinput` | For virtual gamepads; empty goes without (no `uinput` module) |
| `CHA_PUBLIC_ADDRESS` | | The router's public IP, when it forwards the streamers' UDP ports |
| `CHA_PORT_BASE` | `47000` | Streamers use three ports each from here: TCP on localhost (signalling), UDP for WebRTC, UDP for WebTransport |
| `CHA_MAX_ENVIRONMENTS` | `16` | |
| `CHA_DATA_ROOT` | `/srv/cha-portal` | Where app data lives ([below](#app-data)): a host directory the compose file also mounts into the agent at the same path |
| `CHA_NVIDIA_WINE_DIR` | `/usr/lib/x86_64-linux-gnu/nvidia/wine` | The driver's `nvngx.dll` and `_nvngx.dll`, which Proton copies into its prefixes for DLSS and the CDI spec leaves out. Bound read-only into apps at the same path when the host has it (the agent asks the engine; nothing to mount into the agent); empty goes without |
| `CHA_SHARED_DIRS` | | Keeps an app's shared directory elsewhere, `app=/absolute/path`, comma-separated (`steam=/mnt/games/steam`, a NAS). Bind each into the agent read-only at that path |

## Portal settings

| Variable | Default | What |
|---|---|---|
| `CHA_BIND` | `127.0.0.1:8080` | Where the compose file publishes the portal |
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
- Steam environments need the `cha-sandbox` AppArmor profile on the node: `sudo install -m 644 deploy/node/host/apparmor/cha-sandbox /etc/apparmor.d/ && sudo apparmor_parser -r -W /etc/apparmor.d/cha-sandbox`. `--doctor` checks it.
