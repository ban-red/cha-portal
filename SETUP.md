# Setting up Cha Portal

This walks through a first install, from nothing to a stream in your browser. [`deploy/README.md`](deploy/README.md) is the reference for every setting; this guide links to it rather than repeating it.

Cha Portal is pre-release: you build every image from source, and updates can break things. Read the warning in the [README](README.md) first.

- [Before you start](#before-you-start)
- [Quick start: one machine](#quick-start-one-machine)
- [Full setup: a portal and separate nodes](#full-setup-a-portal-and-separate-nodes)
- [First launch](#first-launch)
- [Optional extras](#optional-extras)
- [Updating, backups and removal](#updating-backups-and-removal)
- [Troubleshooting](#troubleshooting)

## Before you start

**The node** (the machine that runs environments) needs:

- Linux on x86-64, with Docker Engine 26 or newer and Docker Compose.
- A GPU, ideally:
  - **NVIDIA:** the proprietary driver and the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html), with a CDI spec generated (below). This is the best-tested path, and the only one that runs Steam well.
  - **Intel or AMD:** only the kernel driver (`i915`/`xe` or `amdgpu`) and `/dev/dri/renderD*`; the streamer image brings the rest.
  - **None:** a CPU-only node works for browsers and desktops at modest sizes, H.264 only, and can't run Steam.
- The `uinput` and `uhid` kernel modules for gamepads (the host installer loads them), and AppArmor for Steam.
- Room for the images and app data: Steam games live under the node's data root, `/srv/cha-portal` by default.
- `git`, and `sudo` for the one-time host files.

**The portal** (the web app and API) needs only Docker. It is small and can share the node's machine.

**The browser** must reach the portal over **HTTPS**. Browsers only give gamepads, keyboard lock and audio worklets to secure pages, and the session cookie is marked Secure. The easy way is [Tailscale](https://tailscale.com), which gives the portal a real certificate without exposing anything to the internet. That is what this guide uses. Chrome and Safari are tested; Firefox works with H.264.

### NVIDIA: generate the CDI spec

On an NVIDIA node, after installing the Container Toolkit:

```bash
sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml
```

Check that containers can see the GPU:

```bash
docker run --rm --device nvidia.com/gpu=all ubuntu nvidia-smi
```

Run the `generate` command again after every driver update.

## Quick start: one machine

The portal and the node on the same Linux machine, with the browser anywhere on your tailnet. Install Tailscale on that machine and on the device you'll play from, and enable **MagicDNS** and **HTTPS Certificates** in the Tailscale admin console under **DNS**.

1. **Get the code:**

   ```bash
   git clone https://github.com/ban-red/cha-portal.git
   ```

   ```bash
   cd cha-portal
   ```

2. **Start the portal.** It listens on `127.0.0.1:7676` only:

   ```bash
   docker compose -f deploy/portal/compose.yaml up -d --build
   ```

3. **Serve it over HTTPS** on your tailnet:

   ```bash
   sudo tailscale serve --bg 7676
   ```

   `tailscale serve status` prints the portal's URL, `https://<machine>.<tailnet>.ts.net`.

4. **Create the first admin.** The portal's first start logs a one-time setup token:

   ```bash
   docker compose -f deploy/portal/compose.yaml logs portal
   ```

   Open the portal's URL and create your admin account with it.

5. **Build the images the node runs:** the streamer, and the environments (Chrome, Firefox, XFCE, KDE, Steam, the test pattern). This is the slow step:

   ```bash
   docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
   ```

   ```bash
   docker compose -f images/compose.yaml build
   ```

   From a release, you can pull the portal, agent and streamer images instead of building them: see [Published images](deploy/README.md#published-images). The environment images are always built here for now.

6. **Install the host files** (udev rules, the Steam sandbox's AppArmor profile, the `uinput`/`uhid` modules). The script prints one line for each and is safe to run again:

   ```bash
   sudo deploy/node/host/install.sh
   ```

7. **Enroll the node.** In the portal, open **Admin → Nodes → Add node** and copy the join token. The agent uses host networking, so it reaches the portal at `http://127.0.0.1:7676`, which it allows without TLS because it never leaves the machine:

   ```bash
   CHA_PORTAL_URL=http://127.0.0.1:7676 CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
   ```

   The token is used once. Later starts only need `CHA_PORTAL_URL`; the node's identity is kept in the agent's `state` volume.

8. **Check the node:**

   ```bash
   CHA_PORTAL_URL=http://127.0.0.1:7676 docker compose -f deploy/node/compose.yaml run --rm agent --doctor
   ```

   It checks Docker, the images, the GPU, gamepads, the Steam sandbox, the host files, disk space and the clock, and says how to fix each problem. Fix what it reports, then go to [First launch](#first-launch).

Don't run the portal with `--dev-login` here. Behind `tailscale serve`, every request comes from `127.0.0.1`, so every tailnet user would count as local.

## Full setup: a portal and separate nodes

The same steps, split across machines. The portal can run on any small always-on machine; each GPU server is a node.

**On the portal's machine:** steps 1–4 above. With Tailscale, the portal is at `https://<portal-host>.<tailnet>.ts.net` ([guide](docs/guides/tailscale.md)). With a public domain instead, set `CHA_DOMAIN` and add the `tls` profile (Caddy, ports 80 and 443):

```bash
CHA_DOMAIN=portal.example.com docker compose -f deploy/portal/compose.yaml --profile tls up -d --build
```

**On each node:** clone the repository, then steps 5 and 6. Enroll and check it with the portal's HTTPS URL:

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
```

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net docker compose -f deploy/node/compose.yaml run --rm agent --doctor
```

Set `CHA_NODE_NAME` to name a node in the portal. Every node needs its own join token. The portal only brokers sessions: the browser needs a UDP path to each node (ports 7600–7647 by default), and on a LAN or tailnet it already has one. For anything else, see [Reaching nodes](deploy/README.md#reaching-nodes).

## First launch

1. **Admin → Nodes** should show the node online, with its CPU, RAM and GPUs.
2. Open **Environments** and launch the **Test pattern**. Connect, and open the stats overlay: a moving bar, a frame counter and low latency mean the path works. Clicks flash the screen and play a tone; a connected gamepad shows its state.
3. Then try **Google Chrome**, or a desktop.
4. **Steam** downloads about 500 MB on its first launch, and the page shows its progress. Sign in to Steam inside the stream. Your Steam home is kept between launches.

In Chrome, Esc goes to the environment; hold **Esc** to leave full screen. The environment keeps running when you close the tab; stop it from the dashboard.

## Optional extras

All of these are in [`deploy/README.md`](deploy/README.md):

- **Play from outside your network without Tailscale:** forward UDP 7600–7647 to the node and set `CHA_PUBLIC_ADDRESS`, or run the portal's `turn` profile (coturn) for networks that block UDP. See [Reaching nodes](deploy/README.md#reaching-nodes).
- **A shared Steam library on a NAS**, so games install once for every user: [A shared directory on a NAS](deploy/README.md#a-shared-directory-on-a-nas).
- **More users:** create them under **Admin → Users**. Each user's app data lives on the node, under `CHA_DATA_ROOT`: [App data](deploy/README.md#app-data).
- **Intel, AMD or CPU-only nodes, and choosing where a launch goes:** [Devices](deploy/README.md#devices).
- **Controllers:** Xbox, DualSense and Steam Controller, set per app on the **Controllers** page: [`docs/controllers.md`](docs/controllers.md).

## Updating, backups and removal

**To update**, pull on every machine, then update the nodes before the portal. A newer agent keeps working with an older portal:

```bash
git pull
```

On each node, rebuild the images (steps 5 and 6) and restart the agent:

```bash
CHA_PORTAL_URL=… docker compose -f deploy/node/compose.yaml up -d --build
```

Then on the portal's machine:

```bash
docker compose -f deploy/portal/compose.yaml up -d --build
```

The portal migrates its database on start. Running environments keep the old images until they are stopped and launched again.

**Back up:**

- **The portal:** its SQLite database in the `cha-portal_data` volume (`/var/lib/cha` in the container). It holds accounts, nodes, settings and the audit log. Stop the portal, or use `sqlite3 .backup`, for a consistent copy.
- **Each node:** `<CHA_DATA_ROOT>/users` holds users' homes. Back it up with whatever you use (restic, rsync, a disk snapshot). The node's identity is in the `cha-node_state` volume; without it, the node has to enroll again.

**To remove** a node, stop the agent (`docker compose -f deploy/node/compose.yaml down`) and delete the node under **Admin → Nodes**. `down -v` also deletes its identity. The host files stay until you remove them yourself; [`deploy/node/host/README.md`](deploy/node/host/README.md) lists them.

## Troubleshooting

Start with `--doctor` on the node: it covers most problems and says how to fix each one.

| Symptom | Likely cause |
|---|---|
| The node never shows online | `CHA_PORTAL_URL` is wrong or unreachable from the node, or it uses plain `http://` to another machine (only `https://`, or `http://` to the same machine, is allowed). See `docker compose -f deploy/node/compose.yaml logs agent` |
| Connect fails with a token error | The node's clock is off. Media tokens last 60 s; `--doctor` compares the clocks |
| No gamepad in the environment | `/dev/uinput` is missing: run `sudo deploy/node/host/install.sh`. A DualSense or Steam Controller also needs `/dev/uhid` |
| Gamepads or keyboard lock don't work at all | The portal isn't on HTTPS |
| Steam won't start | The `cha-sandbox` AppArmor profile isn't loaded: run the host installer again |
| Chrome asks to reach devices on your network | Chrome's Local Network Access prompt, from an HTTPS portal to a LAN or tailnet node. Allow it |
| The stream connects but stutters, or the RTT is high | `tailscale ping <node>` goes through DERP rather than directly. See [the Tailscale guide](docs/guides/tailscale.md#check-the-path) |
| An environment fails to start | The dashboard says why and has **Show log**. The agent also keeps the last log lines under `/var/lib/cha-node/logs/` ([details](deploy/README.md#when-an-environment-dies)) |

If you're stuck, open an issue with the `--doctor` output and the agent's log.
