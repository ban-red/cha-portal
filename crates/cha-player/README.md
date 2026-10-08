# cha-player

The native game-streaming client, macOS first ([ADR 0010](../../docs/adr/0010-native-client-macos-first.md), milestone C1). It plays anything a `cha_client::Transport` can reach: Sunshine and Apollo PCs and our own nodes, through `cha-client-gamestream` (Moonlight), and your portal's apps over `cha-stream/1`, through `cha-client-portal` and `cha-client-stream`.

`winit` window with pointer lock and raw keys, `wgpu` on Metal for everything drawn, VideoToolbox decode (H.264, HEVC) shown without a copy, Opus to CoreAudio through `cpal`, SDL3 for gamepads and rumble only, `egui` for the launcher, pairing, settings and the stats panel. On other systems the crate builds as a stub that says "macOS only for now", so the workspace still builds on Linux CI.

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

Leaving the stream (Ctrl+Alt+Shift+Q) leaves the environment running, so launching the app again resumes where you were. Ctrl+Alt+Shift+X quits the app: it stops the environment on the portal. The app list shows which apps have an environment running or starting, whether it was started here, in a browser or from another player, and lists them again every 5 seconds while the portal is shown and whenever a stream ends; those apps read Resume, with a Quit button that stops the environment without streaming it first. There is no reconnect yet: if the connection drops, the stream ends with the reason and you launch again. PyroWave (the LAN codec) and AV1 are not played yet.

The media path is the portal-brokered one: nothing leaves your network except what you route yourself (Tailscale, a port-forward). The player must be able to reach the node's streamer address that the portal reports (UDP, the streamer's WebTransport port).

## When the stream drops

A stream that drops (a Wi-Fi blip, the node's network, nothing heard for 4 seconds) comes back by itself, in the same session: "Reconnecting…" shows over the last picture, the transport asks the portal for a fresh media token and reconnects, waiting 1, 2, 4, 8, then 15 s between attempts for as long as it takes, as the browser player does. It gives up only when that can't work: signed out of the portal, the environment gone, or another of your sessions took the seat. Keys and buttons held at the drop are released; pads, the size and the frame rate are sent again. Moonlight streams (`cha-client-gamestream`) don't reconnect yet.

## Controls while streaming

| Keys | |
|---|---|
| click in the picture | capture the pointer (relative motion); the click is not sent |
| Ctrl+Alt+Shift+Q | leave: release the pointer, stop the stream, back to the launcher (the app keeps running on the host) |
| Ctrl+Alt+Shift+X | leave and quit the app on the host |
| Ctrl+Alt+Shift+S | show or hide the stats panel |
| Ctrl+Alt+Shift+T | show the toolbar, and release the pointer if it is captured |
| Esc | closes an open toolbar menu or cancels a power-off countdown; otherwise it goes to the app, at once, as a tap |
| hold Esc (1 s) | with the pointer captured: release the pointer (after 0.3 s a bar shows the hold filling); Esc's key-up goes to the host and your real key-up is dropped. Click the picture to capture again |
| Cmd+Ctrl+F | full screen (also in the launcher) |

The hold is `cha_ui_spec::esc_hold::EscHold` (pure state, shared cases in `capture-cases.json`); the lengths and the hint's words are `toolbar.json`'s. winit's key repeats for a held Esc are dropped like every key's (the host repeats keys itself) and neither restart nor break the hold; losing focus cancels it. Losing focus releases the pointer and every held key and button. Relative motion is sent 1:1 in the units macOS reports (not scaled to the picture). Cmd+Q still quits the player.

While the pointer is free, the toolbar and the stats panel are clickable: a click, drag or scroll on either belongs to it, so it captures no pointer and nothing is sent to the host. Everywhere else the pointer behaves as above. While the pointer is captured every event goes to the stream. The keyboard always goes to the stream; the panel needs no typing.

## Toolbar

A bar across the top of the picture, the native twin of the portal's in-stream toolbar (`SessionToolbar.vue`), drawn in the same dark overlay style (its background takes the stats panel's opacity). It is up when the stream starts, folds into a thin bar at the top a moment after the pointer leaves it or moves below the top 72 points, and comes back when you hover or click the thin bar, or press Ctrl+Alt+Shift+T. It stays up while the stream is reconnecting, while the pointer is on it and while a menu or the power-off countdown is open. "Hide the toolbar" (the tab on its lower edge) folds it at once. While the pointer is captured it is folded and takes no input; Ctrl+Alt+Shift+T releases the pointer and shows it. A click on the bar, a menu or the thin bar is the toolbar's alone: it captures nothing and reaches no host. The stats panel's top corners sit below the bar while it shows.

What it says and when each control shows (icons, tooltips, the menus' rows, the countdown, the fold timing) is `cha_ui_spec::toolbar::build_toolbar`'s, from `web/packages/ui-spec/toolbar.json`: the browser builds the same model from the same file. `ui/toolbar.rs` lays the controls out, draws them and carries out what you pick. The session tells the model what it can do as capabilities (`toolbar::capabilities`, from `TransportStats`): a `cha-stream/1` stream with a keyboard-and-mouse floor reports `fps-change` and `control-handoff`, one with viewers `viewers`, one with an overlay `steam-overlay`; sound restart is always there; a Moonlight stream reports nothing else, so its stream settings stay read-only with a note. To change a control, a tooltip or a menu row, see "Toolbar" in `web/packages/ui-spec/README.md`.

- **← Back** leaves the stream and keeps the app running. **Power off** counts down 5 seconds (Cancel, Esc or a click outside cancels), then quits the app on the host and leaves.
- **Stream settings:** the codec and the transport actually in use, read-only (the codec is chosen at launch, from Settings). For `cha-stream/1` streams: the frame rate (60, 90, 120) and, for an app with one (Steam), the Gamescope overlay level (Off, FPS, Bar, Detailed, Full), both changed with the streamer's `fps` and `overlay` messages and only while this session has the controls. A Moonlight stream shows its frame rate read-only while the host says one: it is set in Settings and applies to the next launch. No frame rate row shows until the host reports one.
- **Exclusive input** captures the pointer, as a click on the picture does. **Mouse** switches the mouse off (no motion, buttons or wheel are sent; keyboard and pads still are; it survives a reconnect).
- **Sound:** mute, volume 0 to 100 (turning it up unmutes; the tooltip says the level), and Restart sound, which opens the output device again.
- **Controllers:** the pads SDL sees, with their slots; each is sending its input to the stream (the button's tooltip counts them). When macOS has denied Input Monitoring and no pad shows, a note and a button open System Settings.
- **Full screen** (Cmd+Ctrl+F) and **Stats**, with the health grade's letter in the panel's colours.
- **Watching and Take back** show for `cha-stream/1` streams when other sessions watch, or when another has the controls.
- **Remembered per app** (transport, host and app id) in `config.json` under `toolbar`: `muted`, `volume`, `mouse`, `fps` and `overlay`, each only once you have picked it, applied when a stream of that app starts (the frame rate goes in the launch request). A malformed field is dropped on its own. The stats panel keeps its own choices under `overlay`.
- **Not here:** Share, the GPU badge and "Hand controls" need the portal's share and session APIs, which the player doesn't call yet; the codec switch (the decoder is fixed per stream; the browser rebuilds its pipeline); connecting a controller (WebHID has no counterpart; macOS and SDL find pads); the test pattern's click probe.

## Stats panel

The panel over the picture is the native twin of the portal's stats panel, and grades the stream the same way (`src/health.rs` is `web/packages/player/src/health.ts` in Rust).

- **Header.** A health grade, A to F (green for A and B, amber for C and D, red for F, a dash while it measures), then a word for the worst problem ("Smooth", "Dropped frames", "Slow network"). The buttons switch between the compact and the full view, collapse the panel to its header, open the settings strip and hide it. The grade is judged over the last 8 seconds; one bad second never gives an F.
- **Compact view.** One line, `60 fps · 12 ms · 58 Mbit/s · HEVC/WT · 1 reconnect`, and the top problem with a copy button.
- **Full view.** The problems (what was measured, what it means, what to try, a copy button each, "Copy all" when there are several), the 0 to 100 score, then Stream, Latency, Network and Node. Click a heading to fold its section: the heading then shows that section's summary. A number is coloured only when the grade found it bad, amber or red as the problem's severity; node numbers turn amber at 90% (85 °C). Hover a label for what it means. Copying puts the problems with the numbers around them on the clipboard, ending with a "Player:" line (player and macOS versions).
- **What each transport gives.** The portal's `cha-stream/1` streams fill everything: round trip (QUIC's), frames lost and rebuilt from parity, the node's CPU, memory, GPU, temperature and power (the streamer's `system` messages), and the frame rate the node encodes at. A Moonlight (GameStream) stream (tag `GS`, as in "HEVC/GS") gives the round trip (ENet's smoothed estimate on the control channel, read by the control task), Lost (frames the video path gave up on, which includes frames the player was too slow to take), Recovered (frames completed with a block rebuilt from Reed-Solomon parity, counted once per frame) and the frame rate it was asked for, all counted since the stream began; it has no Node section, send rate or encode time, because a Sunshine or Apollo host reports none, so those are left out, never shown as zeros. Loss is counted in frames, so health judges it on the same scale as `cha-stream/1`. The frame rate, bitrate, decode time, "received to shown" latency, dropped frames, decode errors, partial PyroWave frames and audio rows come from the player and are always there.
- **What is not graded.** The player cannot measure send-to-shown time, the jitter-buffer wait, a Web Audio restart or the sound that reached the speakers, and its audio buffer sits at 30 ms by design, so those checks of the browser's panel are left out (`src/health.rs` lists them). "Wi-Fi latency spikes" (AWDL, see below) is a problem like the others.
- **Moving it.** Drag the header. With "Snap to corners" on (the default) it jumps to the nearest corner on release; with it off it stays where you drop it, and is pulled back inside when the window shrinks. It starts top left, 12 points in.
- **Settings strip** (the three dots): the background opacity, 30% to 100%, and "Snap to corners". The lower the opacity, the lighter the dimmer text and the darker its halo, so it stays readable over the video. The strip closes by itself after a minute, and when the panel is collapsed or hidden.
- **Hidden.** A faint grade chip stays in the corner (full strength under the pointer); click it, or press Ctrl+Alt+Shift+S, to bring the panel back.
- **Where the choices live.** `config.json`, under `overlay`: `open`, `compact`, `collapsed`, `folded` (the folded sections), `corner`, `pos` (the free position), `snap` and `opacity`. They are saved a moment after the last change and when you leave the stream. A config without `overlay` gets the defaults: open, full, top left, snapping, 90%. The panel is always dark so it reads over video: it takes the theme's `dark` palette (`dark-more` with Increase contrast), with `panel` as the background.

## Gamepads

Gamepads go through SDL3 on their own thread, with SDL's Apple GameController (MFi) backend turned off (`SDL_JOYSTICK_MFI=0`): started off the main thread it kept macOS from ever showing the window as visible, so nothing was drawn. Controllers come through IOKit and SDL's HIDAPI drivers instead (DualSense, Xbox, Steam Controller, with rumble and LEDs).

macOS gates those drivers behind **Input Monitoring** (System Settings → Privacy & Security). It asks the first time a controller is opened; until Cha Player is allowed, a Steam Controller in particular is simply not there, while keyboard and mouse (read from the window) still work. When access has been denied the launcher says so, with a button to the settings pane; reopen the player after allowing it. macOS ties the permission to the app's designated requirement. `bundle.sh` signs ad hoc, whose default requirement is the binary's hash, so each rebuild used to lose the grant while System Settings still showed Cha Player switched on; the script now pins the requirement to the bundle identifier (`sh.cha.player`), and the grant survives rebuilds. An app bundled before that change still holds the stale grant: remove Cha Player from the Input Monitoring list with −, add it again and reopen the player. The trade-off is that any ad-hoc app signed as `sh.cha.player` inherits the permission. To sign with a real identity instead: `CHA_SIGN_IDENTITY="Apple Development: …" crates/cha-player/macos/bundle.sh` (Xcode makes one from a free Apple ID; `security find-identity -v -p codesigning` lists them). `CHA_PLAYER_NO_PADS=1` starts without gamepads, to rule them out when something looks wrong.

## Wi-Fi: AWDL latency spikes

On Wi-Fi, macOS periodically leaves your channel for AWDL (AirDrop, Handoff, Continuity, Sidecar), roughly once a second, for 80 to 200 ms. In a stream that is a small freeze that repeats about every second while everything between is smooth. When the stats panel sees video arrive with that rhythm for a few seconds it lists "Wi-Fi latency spikes" with the other problems.

Fixes: use Ethernet (best), turn off AirDrop and Handoff (System Settings, General), or take the interface down until the next reboot:

```bash
sudo ifconfig awdl0 down     # undo with: sudo ifconfig awdl0 up
```

The player never runs this itself. The detector (`src/awdl.rs`) looks only at video arrival times: at least three gaps of 80 to 200 ms about 0.5 to 1.8 s apart, and few other long gaps. A host that sends nothing while the picture is still can show the same rhythm over Ethernet, so the hint also needs `awdl0` up (it is down with Wi-Fi off).

## Themes

The launcher, pairing and settings look like the portal. The colours are the portal's own: `web/packages/ui-spec/themes/cha-magenta.json` (the default) and `cha-jade.json` are generated from `web/apps/portal/src/themes/*.css` by `scripts/export-player-themes.ts` and compiled into the binary by `crates/cha-ui-spec`. After changing a portal theme, run `bun scripts/export-player-themes.ts` and commit the JSON; do not edit it by hand. Each file has the portal's colour roles (`canvas`, `panel`, `ink`, `accent`, `danger`, ...) in four variants: `dark`, `light`, and `dark-more` and `light-more` for more contrast.

Settings, Appearance picks the theme, Light or dark, Contrast and the UI size (egui's zoom factor, 75% to 200%, applied when you let go of the slider). The choices are saved in `config.json` under `theme`; a config without it uses Cha Magenta, System, System, 100%.

**Following the portal.** The portal saves a theme, light or dark and contrast per user, and Cha Player reads them from `GET /api/me/prefs` with its device token (the token goes only to that portal's origin; the player never writes the prefs). A player that has never chosen a look in Settings and is signed in to a portal follows the first portal it is signed in to. Settings, Appearance then shows "Follow <portal host>" (with the portal's current pick beside it) and "This Mac only". Changing the theme, Light or dark or Contrast while following switches to This Mac only, and a note under the controls says so; the size is always this Mac's own. Signing out of the followed portal also switches to This Mac only, keeping the look it had. The portal's `appearance` maps to System, Dark or Light and its `contrast` to System, Standard or More; its `motion` and `transparency` are ignored, and so are values the player doesn't know (that field falls back to the default, as in the browser).

- The theme is fetched on start, right after a sign-in, about every 5 minutes while the launcher is shown, and when the window regains focus (at most once per 20 s). A change applies at once. After a failed fetch the next try is 60 s later.
- Offline, or on any fetch error, the last look the portal sent stays: it is kept in `config.json` (`theme.source`, `theme.portal_look`), so an offline start shows it too. The failure is logged once until a fetch succeeds. Older configs load as they were (a look that isn't the default counts as chosen here, so it stays This Mac only).

- **System light or dark** follows macOS: the window's appearance at start and whenever it changes (`WindowEvent::ThemeChanged`).
- **System contrast** follows System Settings, Accessibility, Display, Increase contrast, read from `NSWorkspace` about every two seconds while the launcher is shown. More contrast uses the `-more` palettes and draws borders 1.5 times as thick.

To look at every theme and variant without a window, run `CHA_SNAPSHOT_DIR=/some/dir cargo test -p cha-player --release snapshot -- --ignored --nocapture`: it draws the launcher, Settings and status lines offscreen and writes `<theme>-<variant>-<screen>.png` (`src/ui/snapshots.rs`). `... snapshot_toolbar ...` draws the toolbar (bar, each menu, folded, power-off, a Moonlight stream, reconnecting, with the stats panel below it). `... snapshot_stats_panel ...` does the same for the stats panel over a colour-bar stand-in for video (full, a section folded, compact, collapsed, hidden, 40% opacity, settings, bottom right).

The stats panel is on top of the video, so it stays dark whatever the launcher shows: see Stats panel.

### Your own themes

Put `*.json` files in `~/Library/Application Support/Cha Player/themes` (Settings, Open themes folder makes the folder and shows it; Reload themes reads it again, so you can edit a file and see it without restarting). A theme only says what it changes; everything else comes from the theme it `extends` (Cha Magenta if it does not say):

```json
{
  "id": "my-theme",
  "name": "My theme",
  "extends": "cha-magenta",
  "variants": {
    "dark": { "accent": "#00ff88", "accent-fill": "#00a85a" },
    "light": { "accent": "#007a3d" }
  },
  "metrics": { "radius_medium": 2.0, "font_body": 15.0 },
  "fonts": { "proportional": "Inter.ttf", "bold": "Inter-SemiBold.ttf", "monospace": "/Users/me/Fonts/JetBrainsMono.ttf" }
}
```

- Only `id` is required. Colours are `#rgb`, `#rrggbb` or `#rrggbbaa`. The roles are the ones in `web/packages/ui-spec/themes/cha-magenta.json`; a misspelt role is an error.
- Changes to `dark` also apply to `dark-more` (and `light` to `light-more`) unless that variant sets the role itself.
- `metrics` keys: `radius_small`, `radius_medium`, `radius_large`, `item_spacing_x`, `item_spacing_y`, `button_padding_x`, `button_padding_y`, `window_margin`, `control_height`, `stroke_width`, `stroke_width_strong`, `font_heading`, `font_body`, `font_button`, `font_small`, `font_monospace` (points).
- Fonts are SF Pro and SF Mono from `/System/Library/Fonts` unless a theme names a `.ttf`/`.otf` file, absolute or relative to the theme file. Font files are not bundled. A file that is missing or cannot be read is skipped with a message in Settings and egui's built-in font is used instead.
- Headings and strong text use a bold face. By default that is SF Pro at weight 600: `SFNS.ttf` is a variable font and the player sets its `wght` axis (if it had none, Helvetica Neue Bold from `HelveticaNeue.ttc` stands in). A theme that names its own `proportional` font gets that font at weight 600 if it is variable, as it is otherwise; `fonts.bold` names a file for the bold face (first face of the file). A bold file that can't be read is skipped with a message in Settings and the regular font is used.
- Disabled accent and danger buttons keep a muted fill with text at least 3:1 against it, in every theme variant (a test checks the palettes).
- A theme with the id of a built-in replaces it. Themes can extend each other.
- A file that does not parse, extends a theme that does not exist, loops, or repeats another file's `id` is skipped, and Settings lists the file and the reason under the buttons. The rest still load.

All of it lives in `src/theme/`: `palette.rs` (roles), `metrics.rs`, `fonts.rs`, `registry.rs` (built-ins and your folder), `resolve.rs` (preferences and the system pick a variant), `apply.rs` (the one place roles become an egui style) and `widgets.rs` (accent and danger buttons, cards, status text). Code in `src/ui/` asks for roles (`ui.palette().danger`), never RGB.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | args, logging, the tokio runtime, building the transports (GameStream behind the default `gamestream` feature, the portal behind `portal`, `--demo`) |
| `src/app.rs` | the `winit` handler: Launcher and Streaming states, pointer lock, hotkeys, drawing |
| `src/ui/` | egui launcher (`mod.rs`, with the brand logo from `cha_ui_spec::LOGO_RGBA`), settings, the toolbar (`toolbar.rs`: layout, drawing and actions; what it says comes from `cha-ui-spec`'s `build_toolbar`) |
| `src/ui/chrome.rs` | what the stats panel and the toolbar share: `Look` (colours, text), the lit button background and `icon_button`, the shadowed menu box and its widget style, `chip`, and `Press`, the rule that egui takes the pointer over any overlay area |
| `src/ui/stats/` | the stats panel: `mod.rs` (state, drawing a frame), `snapshot.rs` (the reading and its value keys), `header.rs`, `settings.rs`, `compact.rs`, `sections.rs`, `chip.rs`, `placement.rs`, `report.rs`; what it says comes from `cha-ui-spec`'s `build_panel` |
| `src/stream_prefs.rs` | the toolbar's per-app choices (fields and limits from `prefs.json`) |
| `src/health.rs` | the stream's health grade, ported from the browser player |
| `src/overlay_prefs.rs` | the stats panel's saved choices (fields and limits from `prefs.json`) |
| `src/theme/` | themes: palettes, user themes, fonts, the egui style (see Themes) |
| `web/packages/ui-spec/themes/` | built-in palettes, generated from the portal's CSS (compiled in by `crates/cha-ui-spec`) |
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
- **No keyframes.** Every frame stands alone; a frame that fails to decode is counted under "decode errors" and the next one is decoded. A frame the transport delivered at the 60 ms deadline with only its whole packets decodes as a partial frame (softer where blocks are missing) and is counted under "Partial frames" in the stats panel.
- **Decode time in the overlay** is the GPU time for dequant plus inverse transform when timestamps are available (measured on about one frame in a few, the latest reading is shown), else the CPU time to parse and record.

## Versions

`winit 0.30`, `wgpu 29`, `egui`/`egui-wgpu`/`egui-winit 0.35` (0.36 needs Rust 1.95, the workspace is 1.93), `objc2 0.6` and `objc2-*` 0.3, `cpal 0.18`, `opus 0.4` (bundled libopus, built with cmake), `sdl3 0.20` (SDL 3.4, built from source and linked statically). All MIT, Apache-2.0, ISC or zlib: AGPL-compatible.

## Not tested without a real host

Pairing and launching against Sunshine, Apollo or a node; launching a portal app and streaming it over `cha-stream/1` from a real node (the transport is tested against an in-process streamer on loopback); HEVC (only the H.264 path ran, from the demo); gamepads, rumble and hot-plug (SDL3 runs on its own thread; `GamepadAdded` was never exercised); the pointer lock feel and Cmd+Ctrl+F in a real window; the AWDL detector on a real link; Game Mode (needs the bundle and full screen).
