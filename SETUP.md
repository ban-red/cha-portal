# Setting up Cha Portal

This walks through a first install, from nothing to a stream in your browser. Most people want the [Quick start](#quick-start). [`deploy/README.md`](deploy/README.md) is the reference for every setting; this guide links to it rather than repeating it.

Cha Portal is pre-release, and updates can break things. Read the warning in the [README](README.md) first.

- [Quick start](#quick-start)
- [Before you start](#before-you-start)
- [One machine, from source](#one-machine-from-source)
- [Full setup: a portal and separate nodes](#full-setup-a-portal-and-separate-nodes)
- [Without HTTPS (a trusted LAN only)](#without-https-a-trusted-lan-only)
- [First launch](#first-launch)
- [Optional extras](#optional-extras)
- [Updating, backups and removal](#updating-backups-and-removal)
- [Troubleshooting](#troubleshooting)

## Quick start

The fastest way to a stream: the portal and a node together on one Linux machine, running the release's published images, so nothing is compiled. Six steps, about as long as the downloads take.

**You need:**

- an x86-64 machine with **Ubuntu or Debian**, `sudo`, and room for the images and games;
- a GPU: for **NVIDIA**, the driver already installed (`nvidia-smi` works). Intel and AMD need nothing extra;
- a free [Tailscale](https://tailscale.com) account, for HTTPS and for reaching the portal from your other devices. In its admin console, under **DNS**, enable **MagicDNS** and **HTTPS Certificates**;
- the device you'll play from, with Tailscale and Google Chrome or Safari.

The commands below use `0.1.0`; use the latest [release](https://github.com/ban-red/cha-portal/releases) instead, in both the clone and the setup step.

1. **Get the release:**

   ```bash
   git clone --branch v0.1.0 --depth 1 https://github.com/ban-red/cha-portal.git
   ```

   ```bash
   cd cha-portal
   ```

2. **Set up the machine.** One script does the rest of the installing; it's safe to run again and says what it did at each step:

   ```bash
   sudo deploy/quickstart/setup.sh --version 0.1.0 --tailscale
   ```

   <details>
   <summary>What it does</summary>

   - installs the packages it needs and turns on clock synchronisation;
   - installs Docker Engine 26 or newer, with compose and buildx, from Docker's own repository. It stops, rather than replace, a distribution's older `docker.io`;
   - with an NVIDIA driver, installs NVIDIA's Container Toolkit and generates the CDI spec that gives containers the GPU. It never installs or changes a GPU driver;
   - installs the node's host files: udev rules, the Steam sandbox's AppArmor profile, the `uinput` and `uhid` modules;
   - makes the data root, `/srv/cha-portal`, owned by root with mode 0755 (the agent makes each user's directories in it);
   - writes `deploy/quickstart/.env`, readable only by you, with `CHA_VERSION` (which keeps every image on the same release) and `CHA_IMAGE_REGISTRY` (which makes the agent run the published environment images);
   - pulls the environment images: Chrome, Firefox, XFCE, KDE, Steam and the test pattern;
   - with `--tailscale`, installs Tailscale.

   Options: `--check` reports what's left without changing anything; `--data-root DIR` keeps app data elsewhere; `--build` builds the environment images from the checkout instead of pulling them; `--no-nvidia` skips NVIDIA's toolkit. Leave out `--tailscale` if it's installed already.

   </details>

3. **Start Cha Portal**, both the portal and this machine's node, and give the portal HTTPS on your tailnet:

   ```bash
   sudo tailscale up
   ```

   ```bash
   docker compose -f deploy/quickstart/compose.yaml up -d
   ```

   ```bash
   sudo tailscale serve --bg 7676
   ```

   `tailscale up` is only needed the first time, to sign this machine in. `tailscale serve status` prints the portal's URL, `https://<machine>.<tailnet>.ts.net`.

4. **Claim the portal.** Open that URL straight away: a fresh portal shows **Claim this portal**, where you pick the first admin's username and password. Whoever opens it first claims it.

5. **Claim the node.** In the portal, open **Admin → Nodes**: this machine is under **Found on your network**. Click **Claim** and type the pairing code from the agent's log:

   ```bash
   docker compose -f deploy/quickstart/compose.yaml logs agent
   ```

   The line reads `unclaimed: … claim "<machine>" with code 4821-9375`. The code proves you can see the node, since whoever claims it controls the machine; five wrong codes make it pick a new one. Once claimed, the node shows up under **Admin → Nodes** and later starts need nothing.

6. **Check the node**, and fix anything it reports:

   ```bash
   docker compose -f deploy/quickstart/compose.yaml run --rm agent --doctor
   ```

Now go to [First launch](#first-launch).

**Good to know:**

- **`docker` needs `sudo`** unless you're in the `docker` group, which makes you root-equivalent on the machine.
- **No NVIDIA GPU?** Delete the agent's two `devices:` lines in `deploy/quickstart/compose.yaml` before step 5, or the agent won't start.
- **The portal also answers on your LAN**, as plain HTTP on port 7676. Docker's published ports bypass ufw and firewalld, so on a machine with a public address, or to keep it to this machine, add `CHA_BIND=127.0.0.1:7676` to `deploy/quickstart/.env`; `tailscale serve` still reaches it. To skip Tailscale on a trusted LAN, see [Without HTTPS](#without-https-a-trusted-lan-only).
- **Other settings** from [`deploy/README.md`](deploy/README.md) go in `deploy/quickstart/.env` too.
- **To update**, get the new release and run the script with its version, then pull and restart:

  ```bash
  git fetch --depth 1 origin tag v0.2.0 && git checkout v0.2.0
  ```

  ```bash
  sudo deploy/quickstart/setup.sh --version 0.2.0
  ```

  ```bash
  docker compose -f deploy/quickstart/compose.yaml pull && docker compose -f deploy/quickstart/compose.yaml up -d
  ```

- **Another distribution?** Follow [Before you start](#before-you-start) and [One machine, from source](#one-machine-from-source) by hand.

## Before you start

**The node** (the machine that runs environments) needs:

- Linux on x86-64, with Docker Engine 26 or newer and Docker Compose 2.30 or newer (older plugins fail on the GPU with `error gathering device information while adding custom device "nvidia.com/gpu=all"`). With Docker older than 28.2, CDI must be switched on in `/etc/docker/daemon.json` (`"features": {"cdi": true}`) and Docker restarted, which stops running containers unless `live-restore` is on; `setup.sh` leaves the restart to you when containers are running.
- A GPU, ideally:
  - **NVIDIA:** the proprietary driver and the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html), with a CDI spec generated (below). This is the best-tested path, and the only one that runs Steam well.
  - **Intel or AMD:** only the kernel driver (`i915`/`xe` or `amdgpu`) and `/dev/dri/renderD*`; the streamer image brings the rest.
  - **None:** a CPU-only node works for browsers and desktops at modest sizes, H.264 only, and can't run Steam.
- The `uinput` and `uhid` kernel modules for gamepads (the host installer loads them), and AppArmor for Steam.
- Room for the images and app data: Steam games live under the node's data root, `/srv/cha-portal` by default.
- `git`, and `sudo` for the one-time host files.

**The portal** (the web app and API) needs only Docker. It is small and can share the node's machine.

**The browser** must reach the portal over **HTTPS**. Browsers only give gamepads, keyboard lock and audio worklets to secure pages, and the session cookie is marked Secure. The easy way is [Tailscale](https://tailscale.com), which gives the portal a real certificate without exposing anything to the internet. That is what this guide uses. Chrome and Safari are tested; Firefox works with H.264.

### NVIDIA: the CDI spec

Containers get the GPU through a CDI spec. The Container Toolkit 1.18 and later keep one themselves, in `/var/run/cdi`, refreshed after driver updates; check that it lists the GPU:

```bash
nvidia-ctk cdi list
```

If `nvidia.com/gpu=all` isn't there (an older toolkit), generate one, and run this again after every driver update:

```bash
sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml
```

Don't add one in `/etc/cdi` beside the toolkit's own: the GPU would then be defined twice.

Check that containers can see the GPU:

```bash
docker run --rm --device nvidia.com/gpu=all ubuntu nvidia-smi
```


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

   From a release, you can pull all of these instead of building them: see [Published images](deploy/README.md#published-images).

6. **Install the host files** (udev rules, the Steam sandbox's AppArmor profile, the `uinput`/`uhid` modules). The script prints one line for each and is safe to run again:

   ```bash
   sudo deploy/node/host/install.sh
   ```

7. **Start and claim the node.** The agent uses host networking, so it reaches the portal at `http://127.0.0.1:7676`, which it allows without TLS because it never leaves the machine:

   ```bash
   CHA_PORTAL_URL=http://127.0.0.1:7676 docker compose -f deploy/node/compose.yaml up -d --build
   ```

   With no identity and no join token, it waits to be claimed and logs a pairing code (`docker compose -f deploy/node/compose.yaml logs agent`). In the portal, open **Admin → Nodes**, find it under **Found on your network**, check the fingerprint matches the log, and **Claim** it with the code. The node's identity is then kept in the agent's `state` volume, and later starts only need `CHA_PORTAL_URL`. (With `CHA_JOIN_TOKEN=…` from **Add node** instead, it enrolls with the token.)

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

**On each node:** clone the repository, then steps 5 and 6 of the same section. On the portal's LAN, start the agent and claim it in the portal with the code from its log, as in step 7 ([Claiming a node](deploy/README.md#claiming-a-node)):

```bash
docker compose -f deploy/node/compose.yaml up -d --build
```

Across subnets or over a tailnet, where the portal can't see the node's broadcast, enroll it with a join token from **Admin → Nodes → Add node** and the portal's HTTPS URL instead:

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
```

Then check it:

```bash
CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net docker compose -f deploy/node/compose.yaml run --rm agent --doctor
```

Set `CHA_NODE_NAME` to name a node in the portal. A join token works once: every node needs its own. The portal only brokers sessions: the browser needs a UDP path to each node (ports 7600–7647 by default), and on a LAN or tailnet it already has one. For anything else, see [Reaching nodes](deploy/README.md#reaching-nodes).

## Without HTTPS (a trusted LAN only)

If every device on your network is yours and you'd rather skip Tailscale and certificates, the portal can serve plain HTTP on the LAN. Know what that costs first:

- **Anyone on the network can take over the portal.** Passwords, session cookies and media tokens cross it unencrypted. With separate nodes, so do the portal's commands to them, and those commands start containers: someone who can tamper with that traffic can run their own on your nodes, as root-equivalent (see [SECURITY.md](SECURITY.md)).
- **The browser holds back features.** On a plain-HTTP page, Chrome gives no gamepads or controllers (the Gamepad API and WebHID), no keyboard lock (Esc, Cmd and shortcuts go to your own browser, not the environment) and no WebTransport, so streams use WebRTC only. Picture, sound, keyboard and mouse still work.
- **Never forward the portal's port to the internet** like this.

A browser on the same machine, at `http://localhost:7676`, counts as secure and keeps every feature: none of this applies to it.

**The portal.** Listen on the LAN, and stop marking the session cookie Secure, or browsers won't keep it over HTTP and sign-in never sticks. The [quick start](#quick-start) already listens on the LAN, so it needs only one line in `deploy/quickstart/.env`:

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

**A node on another machine** must be told to accept plain HTTP to the portal, or it refuses to connect, and refuses a claim from it:

```bash
CHA_PORTAL_URL=http://portal-host.lan:7676 CHA_ALLOW_INSECURE_PORTAL=true docker compose -f deploy/node/compose.yaml up -d
```

Then claim it in the portal with the code from its log, or add `CHA_JOIN_TOKEN=…` to enroll with a token.

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
