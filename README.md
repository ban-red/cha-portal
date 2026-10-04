# Cha Portal

A self-hosted dashboard for "portaling" into remote environments (a Chrome instance, a KDE desktop, Steam Big Picture, or an external Moonlight-protocol host) with Moonlight-class latency, in the browser or in a thin native client. Environments run on **Cha Nodes**: docker compose stacks on your GPU servers that enroll with the portal.

**Status: Phase 0** (spikes and scaffolding). Nothing here is usable as a product yet.

- Plan: [`docs/PLAN.md`](docs/PLAN.md)
- Research (October 2026 landscape): [`docs/research/`](docs/research/README.md)

## Layout

| Path | What |
|---|---|
| `crates/cha-proto` | `cha-stream/1` wire framing: Sans-IO datagram header, fragmentation, reassembly |
| `web/packages/pyrowave-webgpu` | `@cha/pyrowave-webgpu`: PyroWave decode on WebGPU (TypeScript host for the WGSL port), draws straight to a canvas |
| `spikes/s1-browser-pyrowave` | Spike S1: can a browser receive PyroWave-shaped traffic? ([README](spikes/s1-browser-pyrowave/README.md)) |
| `spikes/s1b-pyrowave-webgpu` | Spike S1b: can a browser decode PyroWave fast and bit-exact? ([README](spikes/s1b-pyrowave-webgpu/README.md)) |
| `spikes/s1c-codec-compare` | Spikes S1c/S1d: PyroWave vs H.264/HEVC/AV1 on the same transport, and four ways to put a decoded frame on screen ([README](spikes/s1c-codec-compare/README.md)) |
| `spikes/s1e-encode-latency` | Spike S1e: per-frame encode latency on the node GPU, NVENC vs PyroWave ([README](spikes/s1e-encode-latency/README.md)) |
| `spikes/s2-compositor` | Spike S2: gst-wayland-display compositor with Google Chrome on the node GPU, NVENC zero-copy ([README](spikes/s2-compositor/README.md)) |
| `spikes/s3-gateway` | Spike S3: Moonlight (Wolf) → WebRTC passthrough gateway, measured by the S1d page ([README](spikes/s3-gateway/README.md)) |
| `docs/` | Plan, research and benchmark results |

## Development

Requires Rust ≥ 1.93 and Bun ≥ 1.4.

```bash
cargo test --workspace
```

```bash
bun install
```

## License

[AGPL-3.0-or-later](LICENSE)
