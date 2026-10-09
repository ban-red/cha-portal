# Deploying Cha Portal

| Path | What |
|---|---|
| [`portal/`](portal) | The portal's compose stack: `cha-control` (API, SPA, the nodes' WebSocket, SQLite), plus optional Caddy (HTTPS with public DNS) and coturn (TURN) profiles |
| [`node/`](node) | A node's compose stack: the agent, which starts each environment's streamer and app containers through the Docker socket |
| [`node/host/`](node/host) | Host files that need root, installed by the owner with `install.sh`: the `cha-sandbox` AppArmor profile (Steam environments need it), the udev rules that keep virtual gamepads (Xbox 360, DualSense, Steam Controller) out of a desktop host's own session, and the `uinput`/`uhid` module list |
| [`quickstart/`](quickstart) | The portal and a node on one machine from the published images, and `setup.sh`, which prepares an Ubuntu or Debian machine for it ([SETUP.md](../SETUP.md#quick-start)) |
| [`proxmox/`](proxmox) | `create-node.sh`: a node, or the quick start, in an LXC container on a Proxmox VE host, sharing its Intel or AMD GPU; and the VM to use for NVIDIA ([guide](proxmox/README.md)) |
| [`streamer/`](streamer) | The streamer's image (`--target runtime`) and its dev loop on a node |

## A portal and one node

1. **The portal**, on any small machine:

   ```bash
   docker compose -f deploy/portal/compose.yaml up -d --build
   ```

   It listens on `127.0.0.1:7676`. Serve it over HTTPS: browsers only give gamepads, keyboard lock and audio worklets to secure pages. On a tailnet, run `sudo tailscale serve --bg 7676` ([guide](../docs/guides/tailscale.md)). With public DNS, set `CHA_DOMAIN` and add `--profile tls` (Caddy). A fresh portal is open to claim: the first visitor creates the first admin, so open it right after the first start.
2. **The node**, on the GPU server (NVIDIA with the Container Toolkit's CDI spec; or an Intel or AMD GPU, or no GPU at all: *Devices*, below). From a release, it needs no builds: set the published images ([Published images](#published-images)) and it pulls the rest as apps are launched. From source, build the images it runs:

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

## Moonlight hosts

A gaming PC running Sunshine or Apollo on a node's LAN can be played in the browser ([ADR 0008](../docs/adr/0008-moonlight-hosts-adopted-by-a-node.md)):

- **Finding.** Each node looks for Moonlight hosts (`_nvstream._tcp` over mDNS, `CHA_MOONLIGHT=false` to stop) and reports them to the portal, which lists them under **Admin → Nodes → Moonlight hosts**.
- **Adopting.** **Adopt** pairs the node with the host if it isn't paired yet: the portal shows a 4-digit PIN to type on the host's PIN page (Sunshine and Apollo: `https://<host>:47990/pin`). Once paired, the host is adopted and its apps listed. The node is the Moonlight client: its identity and each paired host's certificate are kept in `<CHA_DATA_ROOT>/node/moonlight/` (root only, keys 0600). Removing a host in the portal doesn't unpair it; the host's own client list does.
- **Playing.** Every user sees each adopted host as its own section of the dashboard, with the host's apps. Launching one runs `cha-gateway` on the node that paired the host (`CHA_GATEWAY_IMAGE`, `cha/gateway:dev`; build it with `docker build -f deploy/gateway/Dockerfile -t cha/gateway:dev .`, or use the published `cha-gateway`). The gateway starts the app on the host and passes its H.264 or HEVC video and stereo sound to the browser, and the browser's keyboard, mouse and gamepads back. A host plays one session at a time.
- **Limits.** No AV1; video between node and host is encrypted only when the host supports it (Sunshine and Apollo do), the browser's leg always by WebRTC; lost packets rebuilt from FEC, keyframes past that; hosts on another subnet aren't found yet.

## Playing from Moonlight

Stock Moonlight and Artemis apps (on a PC, Steam Deck, phone or TV) can play your environments, with native-client latency ([ADR 0009](../docs/adr/0009-gamestream-host-module.md)). Turn it on per node with `CHA_GAMESTREAM=true`; the streamer image needs the `gamestream` feature, which the Dockerfile builds in.

- **Each node is one PC in Moonlight**, named after the node and found on the LAN by itself (mDNS); elsewhere, add it by address (port 47989 unless `CHA_GAMESTREAM_HTTP_PORT` says otherwise). Its apps are the catalog's apps that node can run (Steam only where there's a GPU). Picking one resumes your copy if it runs there, or has the portal start it on that node, with the same rules as the dashboard's Launch (your controller and frame-rate choices, your app data, one copy of a persistent app); a first start can take a while, and Moonlight waits up to three minutes. (A portal older than this answers only with your running environments.)
- **Pairing happens in the portal.** Pick the node in Moonlight: it shows a 4-digit PIN. Open **Settings → Moonlight** in the portal, find the device under *Waiting to pair* and type the PIN. The device is then yours, and sees only your environments. Remove it there to unpair it. The node refuses any pairing that didn't get its PIN through the portal.
- **A Moonlight session is a controlling viewer.** It takes control from the browser, and sets the environment's size and frame rate (60, 90 or 120) for everyone watching, since the streamer encodes once per codec. Quitting in Moonlight ends the stream, not the environment, unless `CHA_GAMESTREAM_QUIT_STOPS=true`: then quitting an app that Moonlight itself started also stops it (never one you started in the browser; an agent restart forgets which those were).
- **Ports:** TCP 47989, 47984 and 48010 for the node's host, and UDP from `CHA_GAMESTREAM_PORT_BASE` (7700), three per environment, for the streams. Keep them to the LAN or tailnet.
- **Limits for now:** stereo sound only (a client asking for surround is refused); NumpadEnter types Enter; touch, pen and pad motion are ignored; IPv4 only. Environments started before `CHA_GAMESTREAM` was turned on don't appear: start them again.

## Users

Admins add people on the Users page with an email and a password (the username defaults to the email; either signs in). Each row has:

- **View as**: browse the portal as that user, with a banner and a way back; the header's switcher moves between users. Switch sessions last 12 hours.
- **Manage access**: limit the user to chosen nodes, set how many environments they can run at once (empty is 4), and grant one app on one node outside their list. Grants appear on their dashboard as "Shared with you".
- **Delete**: refused for yourself, the last admin and users with running environments. Their app data stays on the nodes.

Each of these is in the audit log. A node restriction is a placement rule, not isolation between users sharing a node.

## Signing in Cha Player

The native Cha Player ([ADR 0013](../docs/adr/0013-native-player-on-cha-stream.md)) signs in to a portal as a *device*: it holds a token (`chadev_…`) that opens only the routes a player needs (your apps, environments, ICE servers and your own app settings), never user management, the audit log or other devices. Two ways to sign in; the contract is in [`docs/plans/c2-device-signin.md`](../docs/plans/c2-device-signin.md).

- **From the browser (macOS).** On the dashboard, use the player icon on an app, or *Open in Cha Player* in the account menu. The portal makes a one-use ticket (valid 60 seconds) and the browser opens `cha://connect?...`; the player asks before signing in to this portal, and launches the app if you picked one.
- **With a code.** In the player choose *Add portal*, enter the portal's address and it shows a code like `ABCD-EFGH`. Open `<portal>/link` (also *Link a device* in the account menu), type the code (case and the dash don't matter), check the device name and press Approve. The code lasts 10 minutes.
- **Revoking.** **Settings → Devices** lists your installs with their last use and address; Revoke signs one out at once. Signing in again from the same install replaces its token. Admins can revoke any device (`DELETE /api/devices/{id}`). Every sign-in, approval, denial and revocation is in the audit log.
- Players send the token only to the portal that issued it. A portal on plain `http://` is refused by the player unless it sets `CHA_ALLOW_INSECURE_PORTAL=true` (or the portal is `localhost`); put the portal behind HTTPS or Tailscale otherwise.

## Sharing a game

A friend can play on a second gamepad, watch, or use the controls without an account ([ADR 0014](../docs/adr/0014-share-links-for-players.md) and [ADR 0015](../docs/adr/0015-share-links-for-viewers-and-controllers.md); the contract is [`docs/plans/share-links.md`](../docs/plans/share-links.md)).

- **Make a link.** On a running environment, press *Share* (the dashboard's Running list, or the session toolbar), then *Create link* under *Invite player 2*, *3* or *4*. The full address, `https://<portal>/s/<token>`, is shown once; copy it then. Making a new link for a slot revokes the old one.
- **What the guest gets.** The page names the app and who invited them; *Join* plays the stream. Only their first gamepad is sent, as that slot's pad, and it gets that pad's rumble and lights. No keyboard, mouse, clipboard or window size, and they never take the controls. Your own pads on that slot are dropped while they play. It needs a node whose streamer knows share links; an older one lets the guest watch but not play.
- **How long it lasts.** Until you revoke it (*Revoke* in the same dialog), the environment stops, or 24 hours pass. A guest already playing keeps playing until their stream drops; a revoked link can't start a new one. Tokens last 60 seconds once traded for a stream.
- **Watch and control links.** *Invite to watch* makes a link (as many as you like) that sees and hears the stream and sends nothing. *Invite to control* makes the one controller link (a new one revokes the old). A controller guest can use your keyboard and mouse whenever you aren't holding the controls, or when you hand them over: when you hold them, the toolbar lists who is watching with *Hand controls* by a guest controller, and *Take back* returns them. A guest controller can take the controls when nobody holds them or another guest does, never from you. Both need a streamer that knows these roles; an older one shows the guest the picture only.
- **Treat the link like a password.** It is the only credential, and 256 random bits, kept hashed. The two guest routes (`GET /api/shares/{token}` and `POST /api/shares/{token}/connect`) allow 30 requests a minute per address, held in memory. Behind a reverse proxy the portal sees the proxy's address, so everyone shares that budget. Making, joining and revoking links are in the audit log (`share.created`, `share.joined` with the address and transport, `share.revoked`); the token never is.
- **Reaching the portal.** The guest needs the portal's address, so a link only works for people who can reach it (the LAN, your tailnet or a port-forward), the same as everything else here. A guest page gets the portal's STUN and TURN servers (the `turn` profile), so a guest who can reach the node through them still uses WebRTC. To invite someone who can't reach the portal at all, tick *Over the internet* ([below](#links-over-the-internet)).

### Links over the internet

Tick **Over the internet** in the Share dialog for a friend who is nowhere near your network ([ADR 0022](../docs/adr/0022-share-links-over-a-cloudflare-tunnel.md); the contract is [`docs/plans/wan-sharing.md`](../docs/plans/wan-sharing.md)). Nothing is forwarded on your router and no address of yours is published: the portal runs `cloudflared`, which dials out to Cloudflare, and the link is an `https://` address on Cloudflare's side.

- **What runs.** Making the first internet link starts `cloudflared` (it takes a few seconds; the dialog says *Opening the tunnel…*). It stops a minute after the last internet link ends. The portal image carries a pinned `cloudflared`; outside Docker, install it yourself or point `CHA_CLOUDFLARED` at it.
- **A quick tunnel** needs no account: the link looks like `https://<random words>.trycloudflare.com/s/<token>`. The address changes every time the tunnel starts, so a restart of the tunnel, or of the portal, ends the internet links made before it. Fine for a one-off game night.
- **A named tunnel** keeps one address. In Cloudflare's Zero Trust dashboard make a tunnel (*Networks → Tunnels*, type *Cloudflared*), copy its token, and add a *Public hostname* such as `play.example.com` whose service is `http://localhost:7680`. Then set `CHA_TUNNEL_TOKEN` to the token and `CHA_TUNNEL_HOSTNAME` to that hostname. Both are needed; the portal refuses to start with only one. The token is passed to `cloudflared` in its environment (`TUNNEL_TOKEN`), never on its command line, and is not logged or shown by the API. Put no Cloudflare Access rule on the hostname: guests have no Cloudflare login.
- **What the tunnel reaches.** A second listener, `CHA_GUEST_LISTEN` (default `127.0.0.1:7680`), that serves the portal's pages and only the guest routes of internet links: a link's details, its ICE servers, joining, and the media WebSocket. Everything else, including sign-in, the API and the nodes' channel, answers 404 there, and a link that isn't an internet link is unknown on it. Cloudflare passes the guest's address in `CF-Connecting-IP`, which the portal uses for the rate limit and the audit log on that listener; keep it on loopback (anyone who can reach it could set that header). If the address is taken, the portal logs it and runs without internet links.
- **How the stream gets there.** Cloudflare Tunnel carries HTTP and WebSocket only, so the stream is not WebRTC: the guest's browser tries WebRTC (it works if it can reach the node or your TURN) and otherwise opens a WebSocket to the portal, which relays it to the node over a second WebSocket the node opens to the portal. H.264, HEVC and AV1 only, not PyroWave. It needs a node and streamer from this release; an older node answers *The host's node needs updating to stream over the internet*.
- **Privacy and latency.** Cloudflare ends the TLS connection, so it can see the stream; the Share dialog says so. The stream crosses Cloudflare, your portal and the node over TCP, so a lost packet stalls what is behind it and the delay is higher than on your network. It is for watching and playing with a friend, not for your own competitive play. Every internet guest's bitrate goes in and out of the portal's machine.
- **Off switch.** `CHA_TUNNEL=off` removes the option from the Share dialog, opens no second listener and never starts `cloudflared`. `GET /api/tunnel` (signed in) reports `mode` (`off`, `quick`, `named`), `state` (`stopped`, `starting`, `up`, `failed`), the address and the last error.
- `cloudflared` needs to reach Cloudflare outbound on port 7844 (TCP and UDP). Quick tunnels are for testing by Cloudflare's own description, with no uptime promise; use a named one for anything you rely on.

## Published images

A release (a `v*` tag) publishes every image to GitHub's container registry, built by [`.github/workflows/publish.yml`](../.github/workflows/publish.yml): `ghcr.io/ban-red/cha-portal`, `cha-node` (the agent) and `cha-streamer`, the Moonlight gateway, `cha-gateway`, and the environments, `cha-env-test-pattern`, `-chrome`, `-firefox`, `-xfce`, `-kde` and `-steam` (with their base, `cha-env-base`). Each is tagged with the version (`0.5.0`) and the commit (`sha-1a2b3c4`). There is no `latest`: use one version for all of them, since they change together. Each image carries signed build provenance (`gh attestation verify oci://ghcr.io/ban-red/cha-streamer:0.5.0 --owner ban-red`). The Chrome and Steam images contain Google Chrome and Valve's Steam bootstrap, under their owners' terms.

For a portal and a node on one machine, [`quickstart/compose.yaml`](quickstart/compose.yaml) runs both from these images ([SETUP.md](../SETUP.md#quick-start)). With the separate stacks, set the image variables and pull instead of building:

```bash
CHA_PORTAL_IMAGE=ghcr.io/ban-red/cha-portal:0.5.0 docker compose -f deploy/portal/compose.yaml pull
```

```bash
CHA_PORTAL_IMAGE=ghcr.io/ban-red/cha-portal:0.5.0 docker compose -f deploy/portal/compose.yaml up -d
```

On a node, set `CHA_NODE_IMAGE` the same way, and `CHA_STREAMER_IMAGE=ghcr.io/ban-red/cha-streamer:0.5.0` (in `deploy/node/.env`, so every run gets them). The agent pulls the streamer image when it starts, if the node doesn't have it. It only ever pulls an image whose name includes its registry: a bare name like `cha/streamer:dev` is a local build, and pulling it would fetch whatever Docker Hub's `cha` namespace holds. To update, change the version and pull again; leave `--build` off, or compose builds from source instead.

For the environments, set `CHA_IMAGE_REGISTRY=ghcr.io/ban-red` and `CHA_IMAGE_TAG=0.5.0` on the node. The agent then runs each catalog image from its published copy (`cha/env-chrome:dev` as `ghcr.io/ban-red/cha-env-chrome:0.5.0`), and pulls it on the first launch that needs it; pull them ahead of time to spare that first launch the download (Steam's is the largest). Without `CHA_IMAGE_REGISTRY` it runs the images built on the node, and when it has none, pulls the published ones for its own version ([Images](#images)).

### Images

A node pulls the images a launch needs when it needs them; there is nothing to install ahead of time ([ADR 0017](../docs/adr/0017-images-pulled-on-demand.md)). The portal sends each launch an ordered list of images (a local dev build such as `cha/env-chrome:dev`, then the published `ghcr.io/ban-red/cha-env-chrome:<version>`, where `<version>` is the agent's own release). The agent runs the first the engine already has. If it has none, it pulls the first that names a registry and tells the user as it goes ("Downloading Chrome (412 of 890 MB)"). A bare name is never pulled. If nothing can be run or pulled, the launch fails and lists every image it tried; `docker pull` it yourself or check the node's access to `ghcr.io`.

The streamer works the same way: `CHA_STREAMER_IMAGE` first, then `ghcr.io/ban-red/cha-streamer:<version>` (unless `CHA_STREAMER_IMAGE` already names a registry), settled when the agent starts. The agent reports the images it holds to the portal, which prefers a node that already has the one a launch needs. Pulls are logged with the image used, how long they took and their size. `CHA_PLACEMENT=manual` takes a node out of that automatic choice.

### Checking an image

Before adding an image to a catalog, or after building one, ask the node whether it will run as an environment:

```bash
docker compose -f deploy/node/compose.yaml run --rm agent --check-image cha/env-chrome:dev --profile browser
```

`--profile` is the security profile the app would run under (`standard`, the default, `browser` or `steam`; [Node settings](#node-settings)). An image that isn't on the node is pulled if its name includes a registry; a bare name is never pulled. The check prints one line per test (`ok`, `warn` or `FAIL`, the test, why) and exits 0 when nothing failed, 1 when something did:

- From the image's configuration: it is `linux/amd64` and has an `ENTRYPOINT` or `CMD` (both `FAIL`); `HOME=/home/cha`, `PULSE_SERVER=unix:/run/cha/pulse/native` (no sound without it) and `SDL_JOYSTICK_DISABLE_UDEV=1` (hot-plugged gamepads unseen without it) are set (`warn`); and, as `info`, whether it starts through `cha-run` and how big it is.
- By running it as a launch does (the same user `1000:1000`, capabilities dropped, no privilege gain, seccomp, AppArmor, shared memory and init for the profile, but no GPU): `sh` as that user finds a writable `$HOME` (the one check that replaces the image's entrypoint); the real entrypoint is still running after 3 seconds with no compositor, which is how an app that waits for one behaves; and, beside a real `cha-streamer` on its CPU device, the app connects to the compositor within 30 seconds. An app that needs a GPU (Steam's gamescope) can't be judged that way and gets a `warn` there instead.
- With `--profile steam`, a node without the `cha-sandbox` AppArmor profile loaded gets a `warn` (`--doctor` says how to load it), and the rest runs as `browser`.

The containers it starts are labelled `sh.cha.image-check`, kept apart from the agent's, and removed when it ends, also on an error or Ctrl-C. Needs the Docker socket, like the agent, and the streamer image.

## Updating nodes from the portal

On Admin → Nodes, a node whose agent is older than the portal shows "Update available: vX → vY" and an Update button. The button needs an agent from 0.3 or later, running a published agent image (`ghcr.io/ban-red/cha-node:<version>`). A node built from source (`cha-node:dev`) can't be updated this way, and an older agent shows "its agent predates updates from the portal". See [ADR 0018](../docs/adr/0018-node-agents-updated-from-the-portal.md).

The agent pulls the new agent and streamer images, showing progress. A helper container from the new image then replaces the agent's container with one that has the same settings and the new image. If the new agent doesn't connect within 90 seconds, the helper starts the previous agent again and the update is rolled back. Running environments keep their images until they stop. The compose project's `.env` is updated too (`CHA_VERSION` for the quick start, or `CHA_NODE_IMAGE`, `CHA_STREAMER_IMAGE` and `CHA_IMAGE_TAG` for the node stack), so a later `docker compose up` keeps the new version. If that file can't be written, the helper's log names the file to change by hand. An operator can run the same update on the node with `docker exec <agent container> cha-node --update-to <version>`. The audit log records `node.update_requested`, `node.updated` and `node.update_failed`.

Not covered, so do these by hand as the [upgrade guide](../SETUP.md#upgrading-to-a-new-release) says:

- the portal itself;
- host files, with `sudo deploy/node/host/install.sh` or `setup.sh`;
- releases whose compose file changes. Their release notes say what to do.

## Reaching nodes

The stream goes straight from the node to the browser; the portal only brokers it. A browser needs a UDP path to the node:

| Where the player is | What to do |
|---|---|
| Same LAN | Nothing |
| A tailnet or WireGuard | Nothing: streamers offer the mesh address too ([guide](../docs/guides/tailscale.md)) |
| Internet, with a port-forward | Forward UDP 7600–7647 to the node (WebRTC and WebTransport, two ports per environment), and set `CHA_PUBLIC_ADDRESS` on it |
| Internet, nothing forwarded | Share links over a Cloudflare Tunnel, media over WebSocket ([Links over the internet](#links-over-the-internet)): for guests, not your own play |
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
| UDP 7700–7747 | With `CHA_GAMESTREAM` on: Moonlight media, three ports each from `CHA_GAMESTREAM_PORT_BASE` (video, control, audio), in the same slot as the environment's streamer ports |

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
| `CHA_IMAGE_TAG` | | The release of those images, e.g. `0.5.0`; needed with `CHA_IMAGE_REGISTRY` |
| `CHA_PLACEMENT` | `auto` | `auto` lets the portal pick this node for a launch; `manual` keeps it to launches that choose it by hand, for a test bed. Anything else stops the agent at start-up ([Images](#images)) |
| `CHA_MOONLIGHT` | `true` | Look for Moonlight hosts (Sunshine, Apollo) on the LAN ([Moonlight hosts](#moonlight-hosts)) |
| `CHA_GATEWAY_IMAGE` | `cha/gateway:dev` | The image that streams an adopted Moonlight host; mapped through `CHA_IMAGE_REGISTRY` like the environments |
| `CHA_UINPUT` | `/dev/uinput` | For virtual gamepads; empty goes without (no `uinput` module) |
| `CHA_UHID` | `/dev/uhid` | For virtual DualSense and Steam Controllers; empty goes without (no `uhid` module), and those fall back to an Xbox 360 pad |
| `CHA_PUBLIC_ADDRESS` | | The router's public IP, when it forwards the streamers' UDP ports |
| `CHA_PORT_BASE` | `7600` | Streamers use three ports each from here: TCP on localhost (signalling), UDP for WebRTC, UDP for WebTransport |
| `CHA_MAX_ENVIRONMENTS` | `16` | |
| `CHA_GAMESTREAM` | `false` | Give each environment's streamer ports for a Moonlight session ([ADR 0009](../docs/adr/0009-gamestream-host-module.md), G2): `--gamestream-ports` and a random secret for the streamer's local API (in its environment, kept by the agent). Needs a streamer image built with the `gamestream` feature (the Dockerfile does); an older image fails to start with the new argument, so leave this off for those. It also runs the node's own Moonlight host ([Playing from Moonlight](#playing-from-moonlight)). Nothing else changes while it is off |
| `CHA_GAMESTREAM_QUIT_STOPS` | `false` | Quitting an app in Moonlight also stops its environment, if Moonlight started it ([Playing from Moonlight](#playing-from-moonlight)) |
| `CHA_GAMESTREAM_HTTP_PORT`, `_HTTPS_PORT`, `_RTSP_PORT` | `47989`, `47984`, `48010` | The node's Moonlight host (TCP). The standard ports, so Moonlight finds the node by itself; change them if Sunshine runs on the same machine |
| `CHA_GAMESTREAM_PORT_BASE` | `7700` | Where those ports start: UDP video, control and audio for the first environment, three more for each. The range (three ports times `CHA_MAX_ENVIRONMENTS`) must not overlap `CHA_PORT_BASE`'s; the agent refuses to start otherwise. Forward or open them for clients that aren't on the node's LAN; `cha-node --doctor` checks the first block is free |
| `CHA_DATA_ROOT` | `/srv/cha-portal` | Where app data lives ([below](#app-data)): a host directory the compose file also mounts into the agent at the same path |
| `CHA_NVIDIA_WINE_DIR` | `/usr/lib/x86_64-linux-gnu/nvidia/wine` | The driver's `nvngx.dll` and `_nvngx.dll`, which Proton copies into its prefixes for DLSS and the CDI spec leaves out. Bound read-only into apps at the same path when the host has it (the agent asks the engine; nothing to mount into the agent); empty goes without |
| `CHA_VM_MEMORY_MB` | `12288` | The memory limit, RAM and swap, of an app that runs a virtual machine (the `vm` profile), at least 1024. A guest that outgrows it is killed, not the node |
| `CHA_SHARED_DIRS` | | Keeps an app's shared directory elsewhere, `app=/absolute/path`, comma-separated (`steam=/mnt/games/steam`, a NAS). Bind each into the agent read-only at that path |
| `CHA_HOST_OPTIONS` | `off` | What custom environments may ask of this node: `off`, `allowlist` or `full` ([Host options](#host-options)). Anything else stops the agent at start-up |
| `CHA_HOST_MOUNTS`, `CHA_HOST_PORTS`, `CHA_HOST_CAPS`, `CHA_HOST_DEVICES` | | The allowlist: folders, host ports, capabilities and devices ([Host options](#host-options)) |

### Host options

An admin can duplicate a template in the portal and change it (ADR 0021). Four things a custom environment may ask for reach this machine, so the node's owner decides them here, not the portal: mounts, published ports, capabilities and devices. `CHA_HOST_OPTIONS` sets how much of that is allowed:

- `off` (the default): none. A custom environment that asks for any is refused on this node, and the portal doesn't place it here.
- `allowlist`: only what the four settings below name. A name or value that isn't listed is refused.
- `full`: anything the request can say. That includes host paths, `privileged`, the host's network, extra security options and NFS or CIFS shares mounted for the launch. **Custom environments from this portal can then run with root-equivalent access on this machine.** Turn it on only if every portal admin is trusted with that. The portal has no switch for it: changing it takes the shell on this machine and a restart of the agent.

The allowlist settings, comma-separated. A value that doesn't parse stops the agent at start-up and the message names the variable:

| Variable | Example | What |
|---|---|---|
| `CHA_HOST_MOUNTS` | `media=/mnt/media:ro,roms=/srv/roms` | Folders offered by name (lowercase letters, digits, `-`, `_`). A custom environment picks the name and where it appears in the app; the host path stays here, and isn't sent to the portal. `:ro` is a ceiling: a request for read-write gets read-only. The folder has to exist on the host (`--doctor` checks) |
| `CHA_HOST_PORTS` | `27015-27030/udp,25565/tcp` | Host ports an app may publish, single ports or ranges, each with `tcp` or `udp`. An app that doesn't ask for a particular port gets the first one in a range of its protocol that no other environment on this node publishes |
| `CHA_HOST_CAPS` | `SYS_NICE,NET_RAW` | Capabilities added to the app, which otherwise runs with none. Names are upper case, without `CAP_` |
| `CHA_HOST_DEVICES` | `/dev/dri/card1` | Device files passed to the app (read, write and `mknod`) |

In `full` mode the lists stay usable: apps can still mount a folder by name, and a port without a number is taken from your ranges (with no range for its protocol, Docker chooses). Only `full` also allows host paths as mount sources, network shares, `privileged`, the host's network, security options, and capabilities and devices that aren't listed.

Whatever the mode, the agent refuses a mount that would cover something it mounts itself (`/run/cha`, `/dev/input`, `/home/cha`, the shared directories, the controllers' `hidraw` nodes), one over a shared directory kept elsewhere (`CHA_SHARED_DIRS`), and any folder that holds the agent's Docker socket. Published ports can't be one the agent gives streamers. Every refusal names the option. A share mounted for a launch (`full` only) is a Docker volume named `cha-hostvol-<environment>-<n>`, removed when the environment stops.

The node tells the portal its mode and what it names (mount names and whether each is read-only, port ranges, capabilities, devices; never host paths). The portal places a custom environment only on a node that allows everything it asks for, and the agent checks again before it starts anything. `cha-node --doctor` shows the mode, warns on `full`, and in allowlist mode warns about mounts and devices that aren't on the host. Edit `deploy/node/.env` (the compose file passes the five variables through) and recreate the agent for a change.

Added capabilities (`NET_ADMIN`, `NET_RAW`, `SYS_NICE`…) reach the app's own processes, which still run as uid 1000. The agent starts the app as root with only those, and the image's `cha-run` drops to uid 1000 keeping them. This works only for images built on the Cha base (entrypoint `cha-run`, user `cha`); the agent refuses capabilities for any other image. `NET_ADMIN` acts on the container's own network unless the environment also uses the host's network. A VPN client also needs `/dev/net/tun` as a device.

A Steam environment (the `steam` security profile) takes no added capabilities: the portal won't save them and the agent refuses them. Every process Steam starts would hold them, and Steam's bubblewrap sandbox (its start-up check, its web helper and Proton's pressure-vessel) stops when it has capabilities without being setuid. Steam then exits at start, and its log says it "requires user namespaces". Its other host options work as for any app.

### Virtual machine environments

A template with the `vm` profile (a QEMU/KVM guest inside the app, from a catalog an admin approved) runs only on a node that has `/dev/kvm`: the host's `kvm_intel` or `kvm_amd` module loaded and virtualisation on in the firmware. The agent reports it, and the portal offers other nodes' placements as "no KVM". `/dev/udmabuf` is optional; the app gets it when the host has it. The app is limited to `CHA_VM_MEMORY_MB` and given 90 s to stop. `cha-node --doctor` says whether the node has KVM. A node under Proxmox, as a VM or an LXC container, needs the outer host to pass KVM through.

### When an environment dies

The agent removes an environment's containers when one dies on its own or a start fails, and their logs go with them. So it first reads the last 200 lines of each and keeps them in its state volume, `/var/lib/cha-node/logs/<environment id>-streamer.log` and `-app.log` (with the engine's timestamps), for the newest 50 environments. Read them from the agent's container (`docker compose exec node less /var/lib/cha-node/logs/<id>-app.log`), or from the state volume.

The same tails reach the portal: a failed environment says why in a sentence (the GPU is out of memory, with who holds it; gamescope crashed; a mount, port, `/dev/uinput` or AppArmor problem; else the last error line) and its owner and admins can open "Show log" under it. A node on an older portal still keeps the files.

### Live usage

The Nodes page shows each online node's CPU, RAM and GPUs, refreshed every few seconds. The agent reads the host's `/proc` (which its container shares) and every NVIDIA GPU through NVML (`libnvidia-ml.so.1`, which the `nvidia.com/gpu=all` CDI device brings, like `nvidia-smi`); without NVML the GPU rows are left out. The portal keeps only the latest reading in memory, so it shows nothing for a node that is offline or has been quiet for 15 seconds. An agent sends it only to a portal that says it reads it, so either can be updated first.

## Devices

An environment runs on one **device** of a node (`docs/devices.md`), and the user picks it from the Launch menu on the app's card; Launch itself takes the best one the nodes offer, naming it under the button ("on gpu-node · RTX 4090"). The agent reports what it finds, at start and every five minutes:

- **`nvidia`**: an NVIDIA GPU with NVENC, through CDI as before. Nothing to set up beyond the Container Toolkit's CDI spec.
- **`vaapi`**: an Intel GPU (an iGPU's QuickSync, or Arc) or an AMD one, composited on its render node with Mesa and encoded through VA-API. Nothing to install on the host but the kernel driver (`i915`/`xe`, `amdgpu`) and `/dev/dri/renderD*`; the streamer image carries the user-space drivers. The agent finds every render node whose driver isn't NVIDIA's from `/sys/class/drm` (which its container sees as it is, so the compose file needs no `/dev/dri` mount) and asks the streamer image what each can encode, in a throwaway container with no network that gets only that node (`cha-streamer --probe-device vaapi:/dev/dri/renderD129`). A node that encodes nothing (a virtual GPU, a driver the image lacks) isn't offered, and a streamer image that predates the probe offers no VA-API at all: rebuild it. On an AMD GPU, `deploy/node/host/73-cha-amd-video-clocks.rules` (installed by `install.sh` and the Proxmox script) holds the video engine's clocks at their top step, which cut HEVC 1440p encodes from 7.05 ms to 3.25 ms on a Radeon 780M; opt out with `install.sh --no-video-clocks`, or delete the file and reboot. The environment's streamer and app get that render node and its group, and nothing of NVIDIA's. The agent reads the group from the node itself where its container has it (NVIDIA's comes through CDI), and otherwise from a throwaway container of the streamer image given that node; if neither can tell, it logs `can't tell the render node's group`, and the app falls back to software rendering or shows black.
- **`cpu`**: always there. Mesa's software renderer composites and x264 encodes, H.264 only, with no GPU for the streamer or the app. It suits a desktop or a browser at modest sizes and frame rates, and costs the node's cores while it runs (the portal counts that against the node); it can't run what needs 3D, so apps marked `needsGpu` in the catalog (Steam) never run on it.

`--doctor` lists them, one line each, and says why a render node isn't one. A node whose agent predates devices is read as one `nvidia` device, so an older agent keeps working with a newer portal (and the reverse: it ignores the device a launch names and uses its NVIDIA GPU).

### Where a launch goes

`GET /api/placements` (all apps, or `?template=<id>` for one) lists every device of every online node as an option, best first, with its score and a reason when it isn't allowed. A device scores by its kind (NVIDIA 100, VA-API 60, CPU 20), 15 more on a node that already holds the app's image, less for the node's live usage (the CPU's use, and for NVIDIA the GPU's utilisation and VRAM in use) and 10 for each environment already running on it. A GPU with under 2 GB of VRAM free stays choosable, with a warning, but is never picked automatically. Not allowed: the CPU for an app that needs a GPU, a device that offers no codec browsers play, and a node whose agent can't keep the app's data. `POST /api/environments` takes an optional `node` and `device` from that list (a node alone means its best device) and refuses a choice that isn't allowed with a 400 and the reason. A node with `CHA_PLACEMENT=manual` is listed and can be chosen, but is never the automatic pick; without them it takes the best option, or answers 409 `no_node` saying what stood in each device's way.

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
| `CHA_TUNNEL` | `on` | `off` turns internet share links off ([Links over the internet](#links-over-the-internet)) |
| `CHA_CLOUDFLARED` | `cloudflared` | The `cloudflared` binary (the image has one) |
| `CHA_TUNNEL_TOKEN` | | A named Cloudflare Tunnel's token (secret); without it, quick tunnels |
| `CHA_TUNNEL_HOSTNAME` | | The named tunnel's public hostname, e.g. `play.example.com`; required with the token |
| `CHA_GUEST_LISTEN` | `127.0.0.1:7680` | The guest-only listener the tunnel points at; keep it on loopback |

## App data

What apps keep between launches lives on the node, as plain directories under its **data root**, `CHA_DATA_ROOT` (`/srv/cha-portal`). The user's and the admin's settings are in the portal (each user's own, and an admin's for every app); this is the node's side.

```text
<root>/users/<user id>/<app>/           the user's home for the app: mounted at /home/cha
<root>/users/<user id>/<app>.migrated   a marker: the old home volume was copied in once
<root>/shared/<app>/                    what every user of the app shares
```

- **Settings.** Per (user, app) the user turns keeping their data on or off, and resets it (the nodes delete the directory). The admin sets, per app, whether users keep their data by default, and how much the app shares: `none`, `read` or `write`. Defaults start from the catalog: Steam keeps data and shares a library (`write`), KDE keeps data and shares nothing, the others do neither. Changes apply to the next launch; the user's refuse while an environment of the app is live (the mounts are made).
- **Which node.** A reset reaches every node that is connected, and says so if none is. Placement keeps a user on one node while there is one, so that is where their data is. A node that was offline when a user reset keeps the data until they reset again.
- **Permissions.** Apps run as uid 1000. The agent, which runs as root in its container, makes the directories apps mount: users' homes `1000:1000` mode 0700, shared directories `1000:1000` mode 0770. The root and the directories above them are the agent's or yours (root-owned 0755); apps never see them. Ids in paths are checked (user ids are UUIDs, app ids are catalog slugs), and the agent never follows a symlink out of the root: an app owns what is inside its mounts and can plant any link there.
- **Isolation is per mount, not per uid.** Every user's app runs as uid 1000, so a process on the node itself running as uid 1000 (your own login, if it is uid 1000) can read all of the data. Per-user uids are later work.
- **A KDE desktop survives a node reboot.** KDE's home is `<root>/users/<user id>/kde` on the node's disk, like Steam's, so stopping the environment, recreating the agent or rebooting the node leaves it: wallpaper, panel layout, Konsole and Dolphin settings, files on the desktop. The environment itself doesn't come back by itself after a reboot: launch it again from the portal. After an unclean stop `start-plasma` clears the lock files and caches the last session left in the home (`images/README.md`, *App data*). A user who turns persistence off, or resets it, gets the image's fresh home.
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
- Steam environments need the `cha-sandbox` AppArmor profile on the node: installed by `sudo deploy/node/host/install.sh` (step 3). `--doctor` checks it. A node whose Docker has no AppArmor, such as one in an LXC container, doesn't need it ([Proxmox](proxmox/README.md)).
- The portal reads only its own catalog, `images/catalog.json`. The entry shape is ready for catalogs from elsewhere ([ADR 0017](../docs/adr/0017-images-pulled-on-demand.md)), but loading them comes later.
