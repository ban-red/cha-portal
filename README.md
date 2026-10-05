# Cha Portal

A self-hosted dashboard for "portaling" into remote environments (a Chrome instance, a KDE desktop, Steam Big Picture, or an external Moonlight-protocol host) with Moonlight-class latency, in the browser or in a thin native client. Environments run on **Cha Nodes**: docker compose stacks on your GPU servers that enroll with the portal.

**Status: Phase 1 in progress.** Phase 0's spikes are done (see the plan). Phase 1 builds the MVP on our own streaming engine ([ADR 0004](docs/adr/0004-own-engine-no-wolf.md)): the portal, nodes, and Chrome, Firefox and XFCE environments streamed to the browser. The portal's foundation (P1.1: accounts, sessions, audit log, SPA) and nodes (P1.2: enrollment, the node channel, inventory) work. The streamer core (P1.3: our compositor, NVENC binding and WebRTC) streams from the GPU node. Environments (P1.4) launch from the portal, each running its app beside a streamer on a node, and **Connect** opens them full screen in the browser (P1.5): the portal brokers the WebRTC session with a short-lived media token, and media flows straight from the node. Sound (our own PulseAudio-protocol server, Opus) and gamepads (virtual Xbox 360 pads) work (P1.6). The portal and nodes deploy with compose, `cha-node --doctor` checks a node, and remote access works over Tailscale, a port-forward or TURN (P1.7, [`deploy/`](deploy/README.md)). Phase 1's exit runs (Firefox and Safari clients, WAN) are next.

- Plan: [`docs/PLAN.md`](docs/PLAN.md)
- Research (October 2026 landscape): [`docs/research/`](docs/research/README.md)

## Layout

| Path | What |
|---|---|
| `crates/cha-control` | The portal's server: accounts and sessions, audit log, nodes, the API, serving the SPA (SQLite) |
| `crates/cha-node` | The node agent: enrolls with a join token, then keeps one WebSocket to the portal (inventory, heartbeats, requests) |
| `crates/cha-streamer` | One environment's media engine: our headless Wayland compositor (Smithay), zero-copy NVENC and PyroWave, our PulseAudio-protocol server and Opus, virtual gamepads, WebRTC and WebTransport ([README](crates/cha-streamer/README.md)) |
| `crates/cha-x11-clipboard` | The XFCE environment's clipboard helper: bridges the X server's CLIPBOARD to the streamer over a socket in `/run/cha` |
| `crates/cha-testpattern` | The test-pattern environment: our own Wayland client (moving bar, frame counter, frame-ID strip, a flash and a tone on input, gamepad state) |
| `crates/cha-nvenc` | Our NVENC + CUDA binding, loaded from the driver at runtime |
| `crates/cha-pyrowave` | Our PyroWave binding (libpyrowave, loaded at runtime): dma-bufs in, network packets out |
| `crates/cha-wire` | Node ⇄ portal messages and the node's Ed25519 identity ([ADR 0001](docs/adr/0001-node-channel-json-over-websocket.md)) |
| `web/apps/portal` | The portal SPA (Vue 3, Tailwind) |
| `web/packages/player` | `@cha/player`: the browser player: WebRTC, input, stats, click probe ([README](web/packages/player/README.md)) |
| `deploy/portal` | The portal's compose stack, with optional Caddy (HTTPS) and coturn (TURN) ([README](deploy/README.md)) |
| `deploy/node` | The node's compose stack (the agent), and host files for the owner |
| `deploy/streamer` | The streamer's image and its dev loop on a node |
| `images` | Our environment images (test pattern, Chrome, Firefox, XFCE, KDE Plasma, Steam) and the catalog ([README](images/README.md)) |
| `crates/cha-proto` | `cha-stream/1` wire framing: Sans-IO datagram header, fragmentation, reassembly; and the clipboard helper's frames |
| `web/packages/pyrowave-webgpu` | `@cha/pyrowave-webgpu`: PyroWave decode on WebGPU (TypeScript host for the WGSL port), draws straight to a canvas |
| `spikes/s1-browser-pyrowave` | Spike S1: can a browser receive PyroWave-shaped traffic? ([README](spikes/s1-browser-pyrowave/README.md)) |
| `spikes/s1b-pyrowave-webgpu` | Spike S1b: can a browser decode PyroWave fast and bit-exact? ([README](spikes/s1b-pyrowave-webgpu/README.md)) |
| `spikes/s1c-codec-compare` | Spikes S1c/S1d: PyroWave vs H.264/HEVC/AV1 on the same transport, and four ways to put a decoded frame on screen ([README](spikes/s1c-codec-compare/README.md)) |
| `spikes/s1e-encode-latency` | Spike S1e: per-frame encode latency on the node GPU, NVENC vs PyroWave ([README](spikes/s1e-encode-latency/README.md)) |
| `spikes/s2-compositor` | Spike S2: gst-wayland-display compositor with Google Chrome on the node GPU, NVENC zero-copy ([README](spikes/s2-compositor/README.md)) |
| `spikes/s3-gateway` | Spike S3: Moonlight (Wolf) → WebRTC passthrough gateway, measured by the S1d page ([README](spikes/s3-gateway/README.md)) |
| `docs/` | Plan, research, [ADRs](docs/adr/README.md) and benchmark results |
| `scripts/check.sh` | Every check: rustfmt, clippy, tests, the portal's typecheck and build |

## Development

Requires Rust ≥ 1.93 and Bun ≥ 1.4. Install the web dependencies, then run every check:

```bash
bun install
```

```bash
./scripts/check.sh
```

To run a dev instance of the portal:

```bash
bun run dev
```

It starts `cha-control` on port 8090 (database `data/dev.db`) and the SPA on Vite with hot reload at http://localhost:5190, which proxies `/api` to it. The sign-in page shows **Login as Local Dev** (`--dev-login`): it creates a `dev` admin and signs you in, and a **Login as <name>** button per existing admin account signs you in as that account (no password), so a local portal can show your own environments and settings. It only answers requests from this machine, and is not for a real portal. Flags after `--` go to `cha-control` (`bun run dev -- --turn-secret …`); Ctrl-C stops both.

To run the portal as it ships instead, build the SPA and start `cha-control` alone, which serves it on port 8090. On first start, its log prints a one-time setup token for creating the first admin at http://localhost:8090:

```bash
bun run --cwd web/apps/portal build
```

```bash
cargo run -p cha-control -- --listen 127.0.0.1:8090 --database data/dev.db
```

To add a node, open **Admin → Nodes → Add node** and run the command it shows on the node. Locally, keep the agent's identity in `data/`:

```bash
cargo run -p cha-node -- --portal-url http://127.0.0.1:8090 --join-token chajoin_… --state-dir data/node
```

On a GPU server, use the compose stack instead (NVIDIA through CDI). The agent runs environments as containers through the Docker socket, so first build the images it starts:

```bash
docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
```

```bash
docker compose -f images/compose.yaml build
```

```bash
CHA_PORTAL_URL=https://portal.example CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
```

Run `sudo deploy/node/host/install.sh` once on the node: it installs the udev rules, the Steam sandbox's AppArmor profile and the module list (`--check` only reports). Gamepads need the host's `uinput` module (`/dev/uinput`); without it, start the agent with `CHA_UINPUT=` and environments go without. The DualSense and Steam Controller kinds also need `uhid` (`/dev/uhid`; `CHA_UHID=` goes without, and they fall back to an Xbox 360 pad). `docker compose -f deploy/node/compose.yaml run --rm agent --doctor` checks the node and says how to fix what it finds; `cha-node --print-inventory` shows what the agent will report. For the portal itself, HTTPS and remote access, see [`deploy/README.md`](deploy/README.md). Once the node is online, **Environments** launches anything in the catalog on it.

## License

[AGPL-3.0-or-later](LICENSE)
