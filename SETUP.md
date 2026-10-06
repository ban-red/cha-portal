# Setting up Cha Portal

This walks through a first install, from nothing to a stream in your browser. [`deploy/README.md`](deploy/README.md) is the reference for every setting; this guide links to it rather than repeating it.

Cha Portal is pre-release, and updates can break things. Read the warning in the [README](README.md) first.

- [Before you start](#before-you-start)
- [Quick start: published images](#quick-start-published-images)
- [One machine, from source](#one-machine-from-source)
- [Full setup: a portal and separate nodes](#full-setup-a-portal-and-separate-nodes)
- [Without HTTPS (a trusted LAN only)](#without-https-a-trusted-lan-only)
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

## Quick start: published images

The portal and the node on one Linux machine, from the images a release publishes (`ghcr.io/ban-red/cha-portal`, `cha-node` and `cha-streamer`) and one compose file, [`deploy/quickstart/compose.yaml`](deploy/quickstart/compose.yaml). Only the environment images (Chrome, Firefox, XFCE, KDE, Steam, the test pattern) are still built here. Until the first release is tagged there are no published images to pull: follow [One machine, from source](#one-machine-from-source) instead.

You'll need Tailscale on the machine (the setup script can install it) and on the device you'll play from. Enable **MagicDNS** and **HTTPS Certificates** in the Tailscale admin console under **DNS**.

1. **Get the code at the release you'll run.** The environment images are built from it and talk to the streamer, so they must come from the same release:

   ```bash
   git clone https://github.com/ban-red/cha-portal.git
   ```

   ```bash
   cd cha-portal
   ```

   ```bash
   git checkout v0.1.0
   ```

2. **Prepare the machine** with [`deploy/quickstart/setup.sh`](deploy/quickstart/setup.sh). On Ubuntu or Debian it does everything up to starting the portal:
   - installs the packages it needs and turns on clock synchronisation;
   - installs Docker Engine 26 or newer, with compose and buildx, from Docker's own repository (it stops, rather than replace, a distribution's older `docker.io`);
   - with an NVIDIA driver already installed, installs NVIDIA's Container Toolkit and generates the CDI spec. It never installs or changes a GPU driver;
   - installs the host files (udev rules, the Steam sandbox's AppArmor profile, the `uinput` and `uhid` modules);
   - makes the data root, `/srv/cha-portal`, owned by root with mode 0755 (the agent makes each user's directories in it);
   - writes `deploy/quickstart/.env` with `CHA_VERSION`, readable only by you;
   - builds the environment images. This is the slow part.

   ```bash
   sudo deploy/quickstart/setup.sh --version 0.1.0 --tailscale
   ```

   Leave out `--tailscale` if Tailscale is already installed, or if you're going [without HTTPS](#without-https-a-trusted-lan-only). `--data-root DIR` puts app data elsewhere, `--no-build` skips the images, and `--check` reports what's left to do without changing anything. It's safe to run again, and it says what it did at each step. On another distribution, do the same by hand: [Before you start](#before-you-start), then [One machine, from source](#one-machine-from-source) steps 5 and 6.

   Compose reads `deploy/quickstart/.env` on every run, so the portal, the agent and the streamer always stay on the same version. The other settings in [`deploy/README.md`](deploy/README.md) go in the same file.

3. **Start the portal**, and serve it over HTTPS on your tailnet. The quick start's portal listens on port 7676 on every interface, so it also answers on the LAN; set `CHA_BIND=127.0.0.1:7676` in `.env` to keep it to this machine, and do so on a machine with a public address, since Docker's published ports bypass ufw and firewalld:

   ```bash
   docker compose -f deploy/quickstart/compose.yaml up -d portal
   ```

   ```bash
   sudo tailscale serve --bg 7676
   ```

   If the setup script has just installed Tailscale, join your tailnet first with `sudo tailscale up`. `tailscale serve status` prints the portal's URL, `https://<machine>.<tailnet>.ts.net`.

4. **Claim the portal.** Open its URL: a fresh portal shows **Claim this portal**, where you pick the first admin's username and password (at least 3 characters each). Whoever opens it first claims it, so do this right after it starts.

5. **Start the node.** In the portal, open **Admin → Nodes → Add node**, copy the join token, and start the agent with it. On its first start the agent pulls the streamer image, then enrolls:

   ```bash
   CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/quickstart/compose.yaml up -d
   ```

   The token is used once; the node's identity is kept in the `agent-state` volume. On a machine without an NVIDIA GPU, first delete the agent's `devices:` lines from the compose file.

6. **Check the node**, and fix what it reports:

   ```bash
   docker compose -f deploy/quickstart/compose.yaml run --rm agent --doctor
   ```

Then go to [First launch](#first-launch). To update, check out the new release (`git fetch --tags && git checkout v0.2.0`), run `sudo deploy/quickstart/setup.sh --version 0.2.0` to rebuild the environment images and set the version, then `docker compose -f deploy/quickstart/compose.yaml pull` and `up -d`.

## One machine, from source

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

4. **Claim the portal.** Open its URL: a fresh portal shows **Claim this portal**, where you pick the first admin's username and password (at least 3 characters each). Whoever opens it first claims it, so do this right after it starts.

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

**On the portal's machine:** steps 1–4 of [One machine, from source](#one-machine-from-source). With Tailscale, the portal is at `https://<portal-host>.<tailnet>.ts.net` ([guide](docs/guides/tailscale.md)). With a public domain instead, set `CHA_DOMAIN` and add the `tls` profile (Caddy, ports 80 and 443):

```bash
CHA_DOMAIN=portal.example.com docker compose -f deploy/portal/compose.yaml --profile tls up -d --build
```

**On each node:** clone the repository, then steps 5 and 6 of the same section. Enroll and check it with the portal's HTTPS URL:

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
```

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net docker compose -f deploy/node/compose.yaml run --rm agent --doctor
```

Set `CHA_NODE_NAME` to name a node in the portal. Every node needs its own join token. The portal only brokers sessions: the browser needs a UDP path to each node (ports 7600–7647 by default), and on a LAN or tailnet it already has one. For anything else, see [Reaching nodes](deploy/README.md#reaching-nodes).

## Without HTTPS (a trusted LAN only)

If every device on your network is yours and you'd rather skip Tailscale and certificates, the portal can serve plain HTTP on the LAN. Know what that costs first:

- **Anyone on the network can take over the portal.** Passwords, session cookies and media tokens cross it unencrypted. With separate nodes, so do the portal's commands to them, and those commands start containers: someone who can tamper with that traffic can run their own on your nodes, as root-equivalent (see [SECURITY.md](SECURITY.md)).
- **The browser holds back features.** On a plain-HTTP page, Chrome gives no gamepads or controllers (the Gamepad API and WebHID), no keyboard lock (Esc, Cmd and shortcuts go to your own browser, not the environment) and no WebTransport, so streams use WebRTC only. Picture, sound, keyboard and mouse still work.
- **Never forward the portal's port to the internet** like this.

A browser on the same machine, at `http://localhost:7676`, counts as secure and keeps every feature: none of this applies to it.

**The portal.** Listen on the LAN, and stop marking the session cookie Secure, or browsers won't keep it over HTTP and sign-in never sticks. The [quick start](#quick-start-published-images) already listens on the LAN, so it needs only one line in `deploy/quickstart/.env`:

```bash
CHA_SECURE_COOKIES=false
```

With the separate stacks, `deploy/portal/compose.yaml` listens on localhost by default, so set both:

```bash
CHA_BIND=0.0.0.0:7676
CHA_SECURE_COOKIES=false
```

Skip the `tailscale serve` step, and open `http://<portal-host>:7676`.

**A node on the same machine** needs nothing more: the agent reaches the portal over loopback, which never leaves the machine.

**A node on another machine** must be told to accept plain HTTP to the portal, or it refuses to connect:

```bash
CHA_PORTAL_URL=http://portal-host.lan:7676 CHA_ALLOW_INSECURE_PORTAL=true CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d
```

Put both `CHA_PORTAL_URL` and `CHA_ALLOW_INSECURE_PORTAL=true` in `deploy/node/.env`, so later runs and `--doctor` get them too. The agent logs a warning on every start while the flag is set.

**Getting the browser features back, in Chrome only:** open `chrome://flags/#unsafely-treat-insecure-origin-as-secure`, add `http://<portal-host>:7676`, enable it and relaunch. Chrome then treats the portal as secure, so gamepads, keyboard lock and WebTransport work again. The traffic is still unencrypted, and the flag applies to that one Chrome profile.

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
