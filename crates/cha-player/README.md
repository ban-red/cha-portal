# cha-player

The native game-streaming client, macOS first ([ADR 0010](../../docs/adr/0010-native-client-macos-first.md), milestone C1). It plays anything a `cha_client::Transport` can reach: today Sunshine and Apollo PCs and our own nodes, through `cha-client-gamestream`.

`winit` window with pointer lock and raw keys, `wgpu` on Metal for everything drawn, VideoToolbox decode (H.264, HEVC) shown without a copy, Opus to CoreAudio through `cpal`, SDL3 for gamepads and rumble only, `egui` for the launcher, pairing, settings and the stats overlay. On other systems the crate builds as a stub that says "macOS only for now", so the workspace still builds on Linux CI.

## Run

```bash
brew install cmake                          # SDL3 and libopus are built from source
cargo run -p cha-player --release           # the launcher
cargo run -p cha-player --release -- --demo # plus a fake host: no network needed
```

Flags: `--demo` (a host that encodes a moving test pattern with VideoToolbox in this process, plus a sine tone as Opus; the square follows the mouse), `--autostart` (launch the first app of the first paired host), `--frames N` (exit after N frames and print the session's stats, for smoke tests; implies `--autostart`), `--data-dir DIR`.

Settings (`config.json`) live in `~/Library/Application Support/Cha Player`, and the same directory is handed to the GameStream transport for its client identity and paired hosts.

### Game Mode

macOS turns on Game Mode (doubled Bluetooth controller sampling, CPU and GPU priority) only for **full-screen apps in the games category**. That needs the app bundle, and full screen (Cmd+Ctrl+F):

```bash
cargo build -p cha-player --release
crates/cha-player/macos/bundle.sh            # target/Cha Player.app, unsigned
open "target/Cha Player.app"
```

`macos/Info.plist` sets `LSApplicationCategoryType` to `public.app-category.games`, the bundle id `sh.cha.player`, `NSHighResolutionCapable`, and the local-network and Bonjour keys discovery needs. Signing and notarising come with C3.

## Controls while streaming

| Keys | |
|---|---|
| click in the picture | capture the pointer (relative motion); the click is not sent |
| Ctrl+Alt+Shift+Q | leave: release the pointer, stop the stream, back to the launcher (the app keeps running on the host) |
| Ctrl+Alt+Shift+X | leave and quit the app on the host |
| Ctrl+Alt+Shift+S | stats overlay |
| Cmd+Ctrl+F | full screen (also in the launcher) |

Losing focus releases the pointer and every held key and button. Relative motion is sent 1:1 in the units macOS reports (not scaled to the picture). Cmd+Q still quits the player.

## Gamepads

Gamepads go through SDL3 on their own thread, with SDL's Apple GameController (MFi) backend turned off (`SDL_JOYSTICK_MFI=0`): started off the main thread it kept macOS from ever showing the window as visible, so nothing was drawn. Controllers come through IOKit and SDL's HIDAPI drivers instead (DualSense, Xbox, Steam Controller, with rumble and LEDs). `CHA_PLAYER_NO_PADS=1` starts without gamepads, to rule them out when something looks wrong.

## Wi-Fi: AWDL latency spikes

On Wi-Fi, macOS periodically leaves your channel for AWDL (AirDrop, Handoff, Continuity, Sidecar), roughly once a second, for 80 to 200 ms. In a stream that is a small freeze that repeats about every second while everything between is smooth. When the stats overlay sees video arrive with that rhythm for a few seconds it shows "Wi-Fi latency spikes: likely AWDL (AirDrop/Continuity)".

Fixes: use Ethernet (best), turn off AirDrop and Handoff (System Settings, General), or take the interface down until the next reboot:

```bash
sudo ifconfig awdl0 down     # undo with: sudo ifconfig awdl0 up
```

The player never runs this itself. The detector (`src/awdl.rs`) looks only at video arrival times: at least three gaps of 80 to 200 ms about 0.5 to 1.8 s apart, and few other long gaps. A host that sends nothing while the picture is still can in principle trigger it.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | args, logging, the tokio runtime, building the transports (GameStream behind the default `gamestream` feature, `--demo`) |
| `src/app.rs` | the `winit` handler: Launcher and Streaming states, pointer lock, hotkeys, drawing |
| `src/ui/` | egui launcher, settings, stats overlay |
| `src/render/` | `wgpu` device and surface, the YCbCr video pipeline (aspect-fit, WGSL), egui layer |
| `src/video/` | `VideoDecoder` trait; Annex-B to AVCC; the VideoToolbox decoder |
| `src/present/` | `FrameImporter` trait; Metal zero-copy import |
| `src/audio/` | Opus decode, playout buffer (30 ms target), `cpal` output |
| `src/input/` | W3C key codes, mouse and wheel, hotkeys, SDL3 gamepads |
| `src/session.rs` | threads for a running session, shared stats |
| `src/demo.rs` | the fake host |
| `src/awdl.rs` | the AWDL detector |
| `macos/` | `Info.plist`, `bundle.sh` |

A Linux or Windows port adds a decoder (`VideoDecoder`) and an importer (`FrameImporter`); `render`, `ui`, `audio`, `input` and `session` are not Apple-specific.

## How the picture gets on screen

The decoder returns an IOSurface-backed `CVPixelBuffer`. `CVMetalTextureCache` wraps each plane as an `MTLTexture` over the same memory, and `wgpu`'s Metal HAL adopts it (`Device::texture_from_raw`, `create_texture_from_hal`) as an `R8Unorm` and an `Rg8Unorm` texture (`R16Unorm`/`Rg16Unorm` for 10-bit, when the GPU has them). No copy is made. A WGSL shader does YCbCr to RGB with the range and matrix (BT.601/709/2020) read from the buffer. Only the newest decoded frame is kept; a newer one replaces an unshown older one (counted as dropped). The surface uses Mailbox, else Immediate, else Fifo, with one frame of latency; Metal gives Immediate here.

## Versions

`winit 0.30`, `wgpu 29`, `egui`/`egui-wgpu`/`egui-winit 0.35` (0.36 needs Rust 1.95, the workspace is 1.93), `objc2 0.6` and `objc2-*` 0.3, `cpal 0.18`, `opus 0.4` (bundled libopus, built with cmake), `sdl3 0.20` (SDL 3.4, built from source and linked statically). All MIT, Apache-2.0, ISC or zlib: AGPL-compatible.

## Not tested without a real host

Pairing and launching against Sunshine, Apollo or a node; HEVC (only the H.264 path ran, from the demo); gamepads, rumble and hot-plug (SDL3 runs on its own thread; `GamepadAdded` was never exercised); the pointer lock feel and Cmd+Ctrl+F in a real window; the AWDL detector on a real link; Game Mode (needs the bundle and full screen).
