# Provenance

Where the code in this repository comes from. Cha Portal is AGPL-3.0-or-later. Everything here is either written for this project, a dependency used under its own licence, or ported from a project whose licence allows it, with that project's notice kept beside the code.

If you find code that should be listed here and isn't, open an issue.

## Ported code

Code translated or adapted from another project. Each ported file starts with a notice naming the source, its licence and what changed. Our changes are AGPL-3.0-or-later.

| Here | From | Licence | Notice |
|---|---|---|---|
| `crates/cha-gamestream` (the host: pairing, nvhttp, RTSP, control, input, video and audio packetizers, HDR) | [Moonshine](https://github.com/hgaiser/moonshine) | BSD-2-Clause | [`LICENSE-MOONSHINE`](../crates/cha-gamestream/LICENSE-MOONSHINE), a header on each file, and a file-by-file table in the [crate README](../crates/cha-gamestream/README.md#what-was-ported-and-from-where) |
| `web/packages/pyrowave-webgpu/src/shaders/*.wgsl` (vendored unchanged) and the TypeScript decoder host (ported) | [PyroWave](https://github.com/Themaister/pyrowave), WebGPU branch by [imbcmdth](https://github.com/imbcmdth/pyrowave) | MIT | [`LICENSE-PYROWAVE`](../web/packages/pyrowave-webgpu/LICENSE-PYROWAVE), [package README](../web/packages/pyrowave-webgpu/README.md) |
| `web/packages/player/src/controllers/` (DualSense and Steam Controller drivers over WebHID) | [SDL3](https://github.com/libsdl-org/SDL) HIDAPI drivers | zlib | a header on each file |
| `crates/cha-nvenc/src/sys.rs` (NVENC API types) | NVIDIA Video Codec SDK headers | MIT | the header in the file |

## Main dependencies

Used as libraries, unmodified, under their own licences. `Cargo.lock` and `bun.lock` have the full list.

| Dependency | Used for | Licence |
|---|---|---|
| Smithay | the streamer's Wayland compositor | MIT |
| str0m | WebRTC | MIT or Apache-2.0 |
| quinn, wtransport | QUIC and WebTransport | MIT or Apache-2.0 |
| libopus (via `opus`) | audio encoding | BSD-3-Clause (libopus); MIT or Apache-2.0 (bindings) |
| libpyrowave | PyroWave encoding on the node, loaded at runtime | MIT |
| tokio-enet, fec-rs | GameStream control channel and FEC | BSD-2-Clause |
| SDL3 (via `sdl3`) | the native client's window, input and gamepads | zlib (SDL); MIT (bindings) |
| moonlight-common ([moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust)) | GameStream client in `cha-gateway`, `cha-node`, `cha-client-gamestream` and `cha-moonlight-input`, being replaced by our own ([ADR 0011](adr/0011-own-gamestream-client.md)); after that, test-only, as an independent client that `cha-gamestream`'s tests drive against our host | GPL-3.0-or-later |

## Protocols we interoperate with

`cha-gamestream` speaks the GameStream protocol so that stock Moonlight clients and Sunshine-family hosts work with Cha. Where the protocol is undocumented we match the behaviour of the reference client, moonlight-common-c, and comments name the function or file whose behaviour we follow. That covers wire formats, constants and protocol behaviour. Interop constants from it are cited where they're used; the audio FEC parity matrix, for example, came through Moonshine, which credits Moonlight for it.

The client half (`cha-gamestream/src/client`) is new and still under review: each file is being checked to confirm it's written from the protocol and not adapted from moonlight-common-c's code. This section will say the result.

## Not used

Some projects are named in the docs because we compared approaches or decided against them ([ADR 0004](adr/0004-own-engine-no-wolf.md)). No code from them is in this repository:

- Wolf / Games on Whales, GStreamer, FFmpeg, PulseAudio, PipeWire, inputtino: the media engine is our own.
- Magic Mirror's `mm-server` (BUSL-1.1): its licence is incompatible, and contributors must not copy from it ([CONTRIBUTING](../CONTRIBUTING.md)).

The notes in [`docs/research/`](research/README.md) are background surveys from planning. Naming a project there doesn't mean any of its code is here; this page is the record of what is.
