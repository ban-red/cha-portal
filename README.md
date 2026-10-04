# Cha Portal

A self-hosted dashboard for "portaling" into remote environments (a Chrome instance, a KDE desktop, Steam Big Picture, or an external Moonlight-protocol host) with Moonlight-class latency, in the browser or in a thin native client. Environments run on **Cha Nodes**: docker compose stacks on your GPU servers that enroll with the portal.

**Status: Phase 1 in progress.** Phase 0's spikes are done (see the plan). Phase 1 builds the MVP on our own streaming engine ([ADR 0004](docs/adr/0004-own-engine-no-wolf.md)): the portal, nodes, and Chrome, Firefox and XFCE environments streamed to the browser. The portal's foundation (P1.1: accounts, sessions, audit log, SPA) and nodes (P1.2: enrollment, the node channel, inventory) work, and the streamer core (P1.3: our compositor, NVENC binding and WebRTC) streams Chrome from the GPU node; environments come next.

- Plan: [`docs/PLAN.md`](docs/PLAN.md)
- Research (October 2026 landscape): [`docs/research/`](docs/research/README.md)

## Layout

| Path | What |
|---|---|
| `crates/cha-control` | The portal's server: accounts and sessions, audit log, nodes, the API, serving the SPA (SQLite) |
| `crates/cha-node` | The node agent: enrolls with a join token, then keeps one WebSocket to the portal (inventory, heartbeats, requests) |
| `crates/cha-streamer` | One environment's media engine: our headless Wayland compositor (Smithay), zero-copy NVENC, WebRTC ([README](crates/cha-streamer/README.md)) |
| `crates/cha-nvenc` | Our NVENC + CUDA binding, loaded from the driver at runtime |
| `crates/cha-wire` | Node ⇄ portal messages and the node's Ed25519 identity ([ADR 0001](docs/adr/0001-node-channel-json-over-websocket.md)) |
| `web/apps/portal` | The portal SPA (Vue 3, Tailwind) |
| `deploy/node` | The node's compose stack (the agent) |
| `deploy/streamer` | The streamer's image and its dev loop on a node |
| `crates/cha-proto` | `cha-stream/1` wire framing: Sans-IO datagram header, fragmentation, reassembly |
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

To run the portal locally, build the SPA, then start `cha-control`, which serves it on port 8090 with a database in `data/`:

```bash
bun run --cwd web/apps/portal build
```

```bash
cargo run -p cha-control -- --listen 127.0.0.1:8090 --database data/dev.db
```

On first start, the log prints a one-time setup token: open http://localhost:8090 and create the first admin with it. For live UI work, also run `bun run --cwd web/apps/portal dev`. It serves the SPA on port 5190 and proxies `/api` to port 8090.

To add a node, open **Admin → Nodes → Add node** and run the command it shows on the node. Locally, keep the agent's identity in `data/`:

```bash
cargo run -p cha-node -- --portal-url http://127.0.0.1:8090 --join-token chajoin_… --state-dir data/node
```

On a GPU server, use the compose stack instead (NVIDIA through CDI):

```bash
CHA_PORTAL_URL=https://portal.example CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
```

`cha-node --print-inventory` shows what the agent will report.

## License

[AGPL-3.0-or-later](LICENSE)
