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

   It checks Docker, the images, the GPU through CDI, `/dev/uinput`, the render node, user namespaces, the clock against the portal's (media tokens last 60 s), and the streamers' ports. It says how to fix each problem and changes nothing itself.

## Reaching nodes

The stream goes straight from the node to the browser; the portal only brokers it. A browser needs a UDP path to the node:

| Where the player is | What to do |
|---|---|
| Same LAN | Nothing |
| A tailnet or WireGuard | Nothing: streamers offer the mesh address too ([guide](../docs/guides/tailscale.md)) |
| Internet, with a port-forward | Forward UDP 47001, 47003, … (one per environment) to the node, and set `CHA_PUBLIC_ADDRESS` on it |
| Internet, UDP to the node blocked | The portal's `turn` profile (coturn), with `CHA_TURN_SECRET`, `CHA_TURN_URLS` and `CHA_TURN_PEERS` |

## Node settings

| Variable | Default | What |
|---|---|---|
| `CHA_PORTAL_URL` | (required) | The portal's URL |
| `CHA_JOIN_TOKEN` | | One-time, to enroll |
| `CHA_STREAMER_IMAGE` | `cha/streamer:dev` | The streamer image |
| `CHA_UINPUT` | `/dev/uinput` | For virtual gamepads; empty goes without (no `uinput` module) |
| `CHA_PUBLIC_ADDRESS` | | The router's public IP, when it forwards the streamers' UDP ports |
| `CHA_PORT_BASE` | `47000` | Streamers use two ports each from here: TCP on localhost, UDP for WebRTC |
| `CHA_MAX_ENVIRONMENTS` | `16` | |

## Portal settings

| Variable | Default | What |
|---|---|---|
| `CHA_BIND` | `127.0.0.1:8080` | Where the compose file publishes the portal |
| `CHA_SECURE_COOKIES` | `true` | Keep it on behind HTTPS |
| `CHA_DOMAIN` | | For the `tls` profile (Caddy) |
| `CHA_STUN_URLS` | | STUN for players, comma-separated |
| `CHA_TURN_URLS`, `CHA_TURN_SECRET` | | TURN for players; the portal mints credentials per connection |
| `CHA_TURN_PEERS` | | coturn relays only to these addresses (the nodes) |

## Known gaps

- Behind a proxy, the audit log records the proxy's address, not the client's.
- TURN over TLS on 443 comes later.
- Steam environments need the `cha-sandbox` AppArmor profile on the node: `sudo install -m 644 deploy/node/host/apparmor/cha-sandbox /etc/apparmor.d/ && sudo apparmor_parser -r -W /etc/apparmor.d/cha-sandbox`. `--doctor` checks it.
