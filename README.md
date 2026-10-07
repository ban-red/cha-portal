# Cha Portal

A self-hosted dashboard for streaming remote environments to your browser with Moonlight-class latency. You open the portal, pick Chrome, Firefox, an XFCE or KDE desktop, or Steam Big Picture, and it starts in a container on one of your GPU servers and streams full screen to the tab, with sound, keyboard, mouse, gamepads and clipboard.

The streaming engine is our own: a headless Wayland compositor, zero-copy NVENC, VA-API or PyroWave encoding, and WebRTC or WebTransport straight from the GPU server to the browser ([ADR 0004](docs/adr/0004-own-engine-no-wolf.md)). [`docs/PROVENANCE.md`](docs/PROVENANCE.md) lists the code we ported from other projects and the libraries we build on.

> [!WARNING]
> **Early releases (0.x).** Each release publishes every image, so a machine needs nothing built: see the [Quick start](SETUP.md#quick-start) and the [releases](https://github.com/ban-red/cha-portal/releases). Expect breaking changes to the database, the node protocol and the deploy files. It is developed and tested on one setup (a MacBook Pro M4 with Google Chrome on wired 1 GbE, and an NVIDIA RTX 4090 node).

## What works

- **Environments:** Google Chrome, Firefox, XFCE, KDE Plasma 6, Steam Big Picture (inside gamescope, with Proton games) and a test pattern for measuring latency.
- **Streaming:** H.264, HEVC and AV1 over WebRTC (tested in Chrome and Safari; Firefox gets H.264 only, and hasn't had a full test run); WebTransport as a faster path in Chromium; PyroWave, a low-latency wavelet codec, decoded on WebGPU for wired LANs. Measured send → shown latency is about 5 ms at the median on a wired LAN.
- **WAN:** delay-based rate control, FEC and reference-frame invalidation, so a stream adapts to loss and throttling instead of stalling.
- **Input:** keyboard (with keyboard lock), mouse with pointer lock, text clipboard both ways, and virtual Xbox 360, DualSense and Steam Controller pads fed from the browser's Gamepad API or WebHID.
- **Sound:** stereo Opus from our own PulseAudio-protocol server.
- **Portal:** local accounts, an audit log, nodes claimed in the portal with a pairing code when it finds them on the LAN (or enrolled with a one-time join token elsewhere), live CPU/RAM/GPU usage per node, placement across NVIDIA, Intel/AMD (VA-API) and CPU-only devices, per-user app data kept between launches, and a shared Steam library (optionally on a NAS).
- **Remote access:** Tailscale or WireGuard, a port-forward, or your own TURN server. Nothing goes through a cha.sh service.

- **Moonlight hosts:** a gaming PC running Sunshine or Apollo on the LAN is found by a node, adopted from the portal with a PIN, and its apps played in the browser through `cha-gateway` (new in Phase 3; not yet run against a real host).
- **Moonlight clients:** stock Moonlight apps can play your environments: each node is one PC in Moonlight, its apps the catalog (picking one starts it), paired with a PIN typed into the portal (`CHA_GAMESTREAM`; new, not yet tried with a real client).

Not yet: sharing a session with another user, our own native client. See the [roadmap](docs/PLAN.md#9-roadmap).

## How it works

```text
             browser (portal SPA + @cha/player)
               │  HTTPS: sign-in, launch, signalling          ▲
               ▼                                               │ media: WebRTC / WebTransport (UDP)
     cha-control (portal)  ◀── one outbound WebSocket ──  cha-node (agent, on each GPU server)
     accounts, sessions,                                       │ Docker socket
     nodes, audit log, SQLite                                  ▼
                                                  per environment: app container + cha-streamer
```

- **`cha-control`** is the portal: a Rust server with SQLite that serves the web app and the API. It only brokers sessions; media never passes through it.
- **`cha-node`** runs on each GPU server. It's claimed in the portal with the pairing code it logs (or enrolls with a join token), then holds one outbound WebSocket to the portal, so a node needs no inbound port for control. It starts environments through the Docker socket.
- **`cha-streamer`** runs beside each environment's app container. It is the app's Wayland compositor, audio server and input devices, and it encodes and sends the picture straight to the browser.

The full design is in [`docs/PLAN.md`](docs/PLAN.md).

## Getting started

[`SETUP.md`](SETUP.md) walks through a first install, including a quick start with the portal and node on one machine. In short: you need a machine for the portal (anything that runs Docker) and a Linux server with a GPU for the node (NVIDIA with the Container Toolkit's CDI spec, an Intel or AMD GPU, or none at all for a CPU-only node).

1. Start the portal with `docker compose -f deploy/portal/compose.yaml up -d --build`, and serve it over HTTPS (browsers only give gamepads, keyboard lock and audio worklets to secure pages).
2. Build or pull the streamer and environment images on the node, start the agent, and claim it under **Admin → Nodes → Found on your network** with the code from its log (or use a join token from **Add node**).
3. Install the node's host files (`sudo deploy/node/host/install.sh`) and check it with `--doctor`.

[`deploy/README.md`](deploy/README.md) has every step, setting and option: HTTPS, Tailscale, TURN, app data, NAS libraries and devices.

## Development

You need Rust ≥ 1.93, [`cargo-hakari`](https://docs.rs/cargo-hakari) (`cargo install cargo-hakari --locked`), [Bun](https://bun.sh) ≥ 1.4 (the JS tooling is Bun only) and Python 3 (for `check.sh`'s script tests). The portal and agent build on macOS and Linux; `cha-streamer` and the environment images build on a Linux node, in the streamer's dev container ([`deploy/streamer/compose.dev.yaml`](deploy/streamer/compose.dev.yaml)).

```bash
bun install
```

```bash
./scripts/check.sh
```

`check.sh` runs what CI would: `cargo fmt`, a check that `crates/cha-workspace-hack` is current, clippy with `-D warnings`, the workspace tests, the Steam image's and host installer's Python tests, the portal's colour guard and theme tests, its typecheck and its production build.

`crates/cha-workspace-hack` is generated by `cargo hakari` (settings in [`.config/hakari.toml`](.config/hakari.toml)). It gives every shared third-party crate one feature set, so `cargo test -p`, clippy and workspace builds reuse the same compiled copy instead of each keeping their own in `target/`. After adding, removing or changing a dependency, run `cargo hakari generate && cargo hakari manage-deps`. `bun run dev:cleanup` reclaims disk from `target/` and the web caches; `--deep` also runs `cargo clean`.

```bash
bun run dev
```

This starts `cha-control` on port 7677 (database `data/dev.db`) and the web app on Vite with hot reload at http://localhost:7678, which proxies `/api` to it. The sign-in page shows **Login as Local Dev**: it creates a `dev` admin and signs you in, and offers a **Login as *name*** button per existing admin, with no password. It only answers requests from this machine and is not for a real portal. Flags after `--` go to `cha-control` (`bun run dev -- --turn-secret …`); Ctrl-C stops both.

To run the portal as it ships, build the web app and start `cha-control` alone. Open http://localhost:7677 and claim it: the first visitor creates the first admin.

```bash
bun run --cwd web/apps/portal build
```

```bash
cargo run -p cha-control -- --listen 127.0.0.1:7677 --database data/dev.db
```

A local agent, for working on enrollment and the node channel without a GPU server, keeps its identity in `data/`:

```bash
cargo run -p cha-node -- --portal-url http://127.0.0.1:7677 --join-token chajoin_… --state-dir data/node
```

To point a real node at a dev portal on your LAN, start the portal with `bun run dev` (it listens on every interface; `CHA_LISTEN=127.0.0.1:7677` keeps it to this machine) and the agent with `CHA_ALLOW_INSECURE_PORTAL=true` (development only: the node's traffic then crosses the network unencrypted).

## Repository layout

| Path | What |
|---|---|
| [`crates/cha-control`](crates/cha-control) | The portal server: accounts, sessions, audit log, nodes, placement, the API, serving the web app. SQLite, with numbered migrations |
| [`crates/cha-node`](crates/cha-node) | The node agent: enrollment, the portal channel, inventory, launching environments through Docker, `--doctor` |
| [`crates/cha-streamer`](crates/cha-streamer) | The media engine: compositor (Smithay), NVENC, VA-API, x264 and PyroWave encoding, audio and Opus, virtual gamepads, WebRTC (str0m) and WebTransport (quinn). Linux only |
| [`crates/cha-wire`](crates/cha-wire) | Node ⇄ portal messages and the node's Ed25519 identity |
| [`crates/cha-proto`](crates/cha-proto) | `cha-stream/1`: Sans-IO framing, fragmentation and reassembly |
| [`crates/cha-nvenc`](crates/cha-nvenc), [`cha-pyrowave`](crates/cha-pyrowave) | Our bindings to NVENC/CUDA and libpyrowave, loaded at runtime |
| [`crates/cha-gamestream`](crates/cha-gamestream) | The GameStream (Moonlight) host protocol, ported from Moonshine: the node's host and each streamer's media, behind traits and a cargo feature |
| [`crates/cha-gateway`](crates/cha-gateway) | Streams an adopted Moonlight host (Sunshine, Apollo) to the browser: GameStream in, WebRTC out, video and sound passed through |
| [`crates/cha-sysinfo`](crates/cha-sysinfo) | CPU, RAM and NVIDIA GPU use, from `/proc` and NVML |
| [`crates/cha-testpattern`](crates/cha-testpattern), [`cha-x11-clipboard`](crates/cha-x11-clipboard) | The test-pattern environment; the X11 clipboard bridge for XFCE and Steam |
| [`web/apps/portal`](web/apps/portal) | The portal web app: Vue 3, TypeScript, Tailwind 4 |
| [`web/packages/player`](web/packages/player) | `@cha/player`: WebRTC and WebTransport, input, controllers, stats |
| [`web/packages/pyrowave-webgpu`](web/packages/pyrowave-webgpu) | PyroWave decoding on WebGPU |
| [`deploy/`](deploy) | Compose stacks for the portal, a node and the streamer, the node's host files, NAS helpers |
| [`images/`](images) | Environment images and the catalog |
| [`spikes/`](spikes) | Phase 0–2 experiments (S1–S8), not product code |
| [`docs/`](docs) | The plan, decisions, research, guides and benchmark results |

## Documentation

- [`deploy/README.md`](deploy/README.md): running a portal and nodes
- [`docs/guides/tailscale.md`](docs/guides/tailscale.md): remote access over Tailscale
- [`images/README.md`](images/README.md): the environment images and the contract between an app and the streamer
- [`docs/controllers.md`](docs/controllers.md), [`docs/devices.md`](docs/devices.md): gamepads; GPUs and placement
- [`crates/cha-streamer/README.md`](crates/cha-streamer/README.md), [`web/packages/player/README.md`](web/packages/player/README.md), [`web/apps/portal/README.md`](web/apps/portal/README.md): the engine, the player, the web app and its themes
- [`docs/PLAN.md`](docs/PLAN.md): architecture and roadmap
- [`docs/adr/`](docs/adr/README.md): architecture decisions
- [`docs/PROVENANCE.md`](docs/PROVENANCE.md): ported code, its licences, and the main dependencies
- [`docs/research/`](docs/research/README.md): background survey from planning, October 2026
- [`docs/benchmarks/`](docs/benchmarks/README.md): measured latency, quality and WAN results

## Contributing and security

Contributions are welcome: read [`CONTRIBUTING.md`](CONTRIBUTING.md) first. Report vulnerabilities privately, as [`SECURITY.md`](SECURITY.md) describes, which also sets out what the portal and nodes trust.

## License

Copyright © 2026 Alex Red and the Cha Portal contributors.

Cha Portal is free software under the [GNU Affero General Public License v3.0 or later](LICENSE). If you run a modified version for others over a network, the AGPL requires you to offer them its source. Contributors sign a [CLA](CLA.md) so that Cha Player can also ship through app stores; every version stays available under the AGPL.
