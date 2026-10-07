# cha-player

The native game-streaming client, macOS first ([ADR 0010](../../docs/adr/0010-native-client-macos-first.md), milestone C1). It plays anything a `cha_client::Transport` can reach: Sunshine and Apollo PCs and our own nodes, through `cha-client-gamestream` (Moonlight), and your portal's apps over `cha-stream/1`, through `cha-client-portal` and `cha-client-stream`.

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

`macos/Info.plist` registers the `cha` URL scheme (`CFBundleURLTypes`), sets `LSApplicationCategoryType` to `public.app-category.games`, the bundle id `sh.cha.player`, `NSHighResolutionCapable`, and the local-network and Bonjour keys discovery needs. Signing and notarising come with C3.

## Signing in to your portal

The "Cha Portal" transport (`cha-client-portal`, default `portal` feature) signs this Mac in to a Cha Portal with a device token. Two ways, both from the portal side described in [`docs/plans/c2-device-signin.md`](../../docs/plans/c2-device-signin.md):

- **From the dashboard:** use "Open in Cha Player" on an app (or in the user menu). It opens a `cha://connect?...` link; the player asks "Sign in to https://your.portal?", and Sign in swaps the link's one-use ticket (valid 60 seconds) for a token and shows that portal's apps. Needs the app bundle (`macos/bundle.sh`), which registers the `cha` scheme; the link works when the app is closed too.
- **With a code:** under Cha Portal in the launcher, type the portal's address in the Add box (`portal.example`, or `host:7676`; https unless you write `http://`) and pick Add, then Sign in with a code. The player shows a code like `ABCD-EFGH`; open `<portal>/link` in a browser where you are signed in and approve it. The player waits up to 10 minutes.

The token, the device id and the user name are kept per portal origin in `portals.json` in the data directory (`~/Library/Application Support/Cha Player`, file mode 0600), with the install id (a random UUID made once; signing in again from this install replaces its token on the portal instead of adding a device). A token is only ever sent to the origin it was issued by, and redirects are not followed. Sign out forgets the token here; revoke the device in the portal's Settings, Devices page to cut it off there. If the portal revokes it, the portal shows as not signed in the next time the player uses it.

Plain `http://` portals are refused unless the host is `localhost`, `127.0.0.1` or `::1`, or `CHA_ALLOW_INSECURE_PORTAL=true` is set in the player's environment (for a dev portal on your LAN: `CHA_ALLOW_INSECURE_PORTAL=true cargo run -p cha-player --release`).

## Playing your portal's apps

Pick an app under your portal and Launch (or follow "Open in Cha Player" with an app: once you are signed in, a link's `launch=` starts that app at once). The player does what the browser does, with one difference, that the picture is decoded by VideoToolbox rather than WebCodecs:

1. It reuses your running (or still starting) environment of that app, or asks the portal for a new one, and shows "Starting Steam…" while it starts. A cold Steam can take minutes; the wait ends after 11 minutes, or at once if the portal says the launch failed (with its reason).
2. It picks the first of its codecs (HEVC, then H.264; Settings) that the environment's GPU encodes, asks the portal for a media token for that codec, and connects to the streamer's WebTransport address(es) over `cha-stream/1` (`cha-client-stream`, [`docs/plans/c2-transport.md`](../../docs/plans/c2-transport.md)). The streamer's certificate is trusted only if its SHA-256 is the one the portal gave.
3. The player asks the streamer for its Settings resolution (2560x1440 by default; the streamer rounds down to a multiple of 8). The picture and audio are the browser's: 10 ms Opus, FEC-protected video, loss answered with reference invalidation (`rfi`) and then keyframes.

Leaving the stream (Ctrl+Alt+Shift+Q) leaves the environment running, so launching the app again resumes where you were. Ctrl+Alt+Shift+X quits the app: it stops the environment on the portal. There is no reconnect yet: if the connection drops, the stream ends with the reason and you launch again. PyroWave (the LAN codec) and AV1 are not played yet.

The media path is the portal-brokered one: nothing leaves your network except what you route yourself (Tailscale, a port-forward). The player must be able to reach the node's streamer address that the portal reports (UDP, the streamer's WebTransport port).

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

Gamepads go through SDL3 on their own thread, with SDL's Apple GameController (MFi) backend turned off (`SDL_JOYSTICK_MFI=0`): started off the main thread it kept macOS from ever showing the window as visible, so nothing was drawn. Controllers come through IOKit and SDL's HIDAPI drivers instead (DualSense, Xbox, Steam Controller, with rumble and LEDs).

macOS gates those drivers behind **Input Monitoring** (System Settings → Privacy & Security). It asks the first time a controller is opened; until Cha Player is allowed, a Steam Controller in particular is simply not there, while keyboard and mouse (read from the window) still work. When access has been denied the launcher says so, with a button to the settings pane; reopen the player after allowing it. `bundle.sh` signs the app ad hoc, and macOS keeps an ad-hoc app's permission only for that exact binary: after a rebuild, turn Cha Player off and on again in Input Monitoring. To keep it across rebuilds, sign with a stable identity: `CHA_SIGN_IDENTITY="Apple Development: …" crates/cha-player/macos/bundle.sh` (Xcode makes one from a free Apple ID; `security find-identity -v -p codesigning` lists them). `CHA_PLAYER_NO_PADS=1` starts without gamepads, to rule them out when something looks wrong.

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
| `src/main.rs` | args, logging, the tokio runtime, building the transports (GameStream behind the default `gamestream` feature, the portal behind `portal`, `--demo`) |
| `src/app.rs` | the `winit` handler: Launcher and Streaming states, pointer lock, hotkeys, drawing |
| `src/ui/` | egui launcher, settings, stats overlay |
| `src/render/` | `wgpu` device and surface, the YCbCr video pipeline (aspect-fit, WGSL), egui layer |
| `src/video/` | `VideoDecoder` trait; Annex-B to AVCC; the VideoToolbox decoder; `pyrowave.rs`, the PyroWave presenter (decode on the window's `wgpu` device, via `cha-pyrowave-wgpu`) |
| `src/present/` | `FrameImporter` trait; Metal zero-copy import |
| `src/audio/` | Opus decode, playout buffer (30 ms target), `cpal` output |
| `src/input/` | W3C key codes, mouse and wheel, hotkeys, SDL3 gamepads |
| `src/urlscheme.rs` | the `cha://` Apple Event handler, installed before the event loop runs |
| `src/session.rs` | threads for a running session, shared stats |
| `src/demo.rs` | the fake host |
| `src/awdl.rs` | the AWDL detector |
| `macos/` | `Info.plist`, `bundle.sh` |

A Linux or Windows port adds a decoder (`VideoDecoder`) and an importer (`FrameImporter`); `render`, `ui`, `audio`, `input` and `session` are not Apple-specific.

## How the picture gets on screen

The decoder returns an IOSurface-backed `CVPixelBuffer`. `CVMetalTextureCache` wraps each plane as an `MTLTexture` over the same memory, and `wgpu`'s Metal HAL adopts it (`Device::texture_from_raw`, `create_texture_from_hal`) as an `R8Unorm` and an `Rg8Unorm` texture (`R16Unorm`/`Rg16Unorm` for 10-bit, when the GPU has them). No copy is made. A WGSL shader does YCbCr to RGB with the range and matrix (BT.601/709/2020) read from the buffer. Only the newest decoded frame is kept; a newer one replaces an unshown older one (counted as dropped). The surface uses Mailbox, else Immediate, else Fifo, with one frame of latency; Metal gives Immediate here.

## PyroWave

PyroWave (`PyroWave420`/`PyroWave444` from a node over `cha-stream/1`) is intra-only, decoded by compute shaders in [`cha-pyrowave-wgpu`](../cha-pyrowave-wgpu/README.md) and drawn straight from the planes buffer: no VideoToolbox, no IOSurface. It is meant for the wired LAN: roughly 300 to 600 Mbit/s at 1440p60.

- **Needs GPU subgroups.** The player's `wgpu` device requests `Features::SUBGROUP` (and `TIMESTAMP_QUERY` for decode timing) when the adapter has them; Apple silicon does. Without subgroups starting a PyroWave stream fails with a message saying so, and H.264/HEVC are unaffected.
- **Decoded on the render thread.** The decode shares the window's device and queue, so the video thread only keeps the newest raw frame (a newer one replaces an unshown older one, counted as dropped). `draw_stream` decodes it into the same command encoder and submit as the frame's render pass, then draws it aspect-fit like any other picture. The pipelines compile at the first PyroWave stream; the decoder is made from the first frame's sequence header and remade if the size or chroma changes.
- **No keyframes.** Every frame stands alone; a frame that fails to decode is counted under "decode errors" and the next one is decoded. A frame the transport delivered at the 60 ms deadline with only its whole packets decodes as a partial frame (softer where blocks are missing) and is counted under "partial" in the stats overlay.
- **Decode time in the overlay** is the GPU time for dequant plus inverse transform when timestamps are available (measured on about one frame in a few, the latest reading is shown), else the CPU time to parse and record.

## Versions

`winit 0.30`, `wgpu 29`, `egui`/`egui-wgpu`/`egui-winit 0.35` (0.36 needs Rust 1.95, the workspace is 1.93), `objc2 0.6` and `objc2-*` 0.3, `cpal 0.18`, `opus 0.4` (bundled libopus, built with cmake), `sdl3 0.20` (SDL 3.4, built from source and linked statically). All MIT, Apache-2.0, ISC or zlib: AGPL-compatible.

## Not tested without a real host

Pairing and launching against Sunshine, Apollo or a node; launching a portal app and streaming it over `cha-stream/1` from a real node (the transport is tested against an in-process streamer on loopback); HEVC (only the H.264 path ran, from the demo); gamepads, rumble and hot-plug (SDL3 runs on its own thread; `GamepadAdded` was never exercised); the pointer lock feel and Cmd+Ctrl+F in a real window; the AWDL detector on a real link; Game Mode (needs the bundle and full screen).
