# Controllers

How a controller in the user's hands becomes a controller in the environment.
The browser reads it (Gamepad API, or WebHID with our own drivers), turns it
into one canonical state, and sends that on the control channel. The streamer
feeds it into a **virtual controller** of the kind the user chose for the app:

| Kind | Device | What the app sees |
|---|---|---|
| `xbox360` (default) | uinput | An Xbox 360 pad (`045e:028e`): buttons, sticks, triggers, rumble. Every game and SDL know it. |
| `dualsense` | uhid | A wired DualSense (`054c:0ce6`): the host kernel's `hid-playstation` binds it (gamepad, touchpad and motion evdev nodes), and the app also gets its `hidraw` node (SDL's HIDAPI driver, Proton's winebus, Steam). Touchpad, gyro, accelerometer, rumble, lightbar, adaptive triggers, player LEDs. |
| `steam` | uhid | The 2026 Steam Controller over Bluetooth (`28de:1303`; its wired id would meet the missing USB interface a wired original did): Steam talks to it over `hidraw` (Steam Input: layouts, gyro, trackpads, haptics); the host's `hid-generic` binds it, with no evdev devices. A real d-pad, two sticks, both trackpads, four back paddles, the quick-access button, gyro, accelerometer, battery, rumble and trackpad haptic pulses. The original Bluetooth controller (`28de:1106`) is still in the code behind `steam_controller::VARIANT`. |

The kind is the user's per-app choice (the portal's Controllers page), with a
default per app from the catalog. It is fixed for an environment's life: hotplug
events don't reach the app's network namespace, so devices are made before the
app starts. Any physical controller can drive any kind (a Steam Controller's
trackpads become the DualSense's touchpad, and so on); what the physical one
lacks stays at rest.

## Page → streamer: `pad`

On the control channel, in the input envelope (`{"t":"input", …}`), sent when
something changed:

```
{ k:"pad", i: 0..3,
  b: [buttons 0..1],       // 0–16: the Gamepad API's standard layout; extras below
  a: [LX, LY, RX, RY],     // -1..1
  ty?: "xbox"|"playstation"|"switch"|"steam"|"generic",   // the physical controller
  gyro?: [x, y, z],        // rad/s, SDL's convention (x pitch, y yaw, z roll)
  accel?: [x, y, z],       // m/s², SDL's convention
  touch?: [{ id, x, y, down }],   // x, y 0..1 from the top left
  bat?: 0..1,
  gone?: true }
```

- **Extra buttons**, after the standard 17: 17 touchpad click (DualSense) or
  right trackpad click (Steam Controller); 18 left trackpad click; 19 left back
  paddle or grip (L4); 20 right back paddle or grip (R4); 21 L5; 22 R5; 23 mute
  or quick-access. Missing ones are released.
- **Touch ids:** a Steam Controller's left trackpad is id 0 and its right one id
  1; a DualSense's two finger slots are ids 0 and 1. A pad's touch point is
  present only while touched.
- The type is `ty`, not `t`: `t` is the envelope's.

## Streamer → page

JSON lines on the control channel, to the session holding the controls; the
page plays them on the physical controller in slot `i` when it can, and
ignores them otherwise.

- `{t:"rumble", i, lo, hi, ms}`: motors 0..1 (lo: strong, low frequency; hi:
  weak, high frequency) for `ms` (0 stops).
- `{t:"haptic", i, side:"left"|"right", amp: 0..1, on_us, off_us, count}`: a
  trackpad pulse train (Steam Controller haptics; a page with a Steam
  Controller plays it on that pad, others may map it to rumble).
- `{t:"led", i, r, g, b}`: the lightbar, 0..255 (DualSense).
- `{t:"players", i, mask}`: the player LEDs, bits 0–4 (DualSense).
- `{t:"trigger", i, side:"left"|"right", effect: [11 bytes]}`: an adaptive
  trigger effect, the DualSense output report's block as the game wrote it.

## Choosing the kind

- Catalog: an optional `gamepad` per template (`"xbox360"` when absent).
- Portal: the user's choice per app (`null` = the app's default);
  `GET /api/controllers/apps` → `{ apps: [{ template, name, kind, default }] }`,
  `PUT /api/controllers/apps/{template}` with `{ kind: "xbox360"|"dualsense"|"steam"|null }`.
  A change applies from the next launch.
- `EnvironmentSpec.gamepad` (cha-wire, optional, `xbox360` when absent) →
  the node → the streamer's `--pad-kind`.

## The node

- The streamer gets `/dev/uhid` beside `/dev/uinput` (`CHA_UHID`, default
  `/dev/uhid`; empty goes without, and `dualsense`/`steam` fall back to
  `xbox360` with a warning).
- `hidraw` nodes: the streamer makes its uhid devices before the app starts,
  creates their `hidraw` nodes (owned by the app's uid) in its input volume
  (the one the app has at `/dev/input`) under `hidraw/`, writes their udev
  entries beside the input ones, and once they exist lists them in its
  `GET /info`: `"hidraw": [{ "name": "hidraw3", "major": 240,
  "minor": 3 }]`. The node then mounts each into the app at `/dev/<name>` (a
  volume mount with that subpath) and allows exactly that device
  (`DeviceCgroupRules` `c <major>:<minor> rwm`). libudev, hidapi and Wine find
  them by the kernel's name under `/dev`. The evdev nodes the host's
  `hid-playstation`/`hid-steam` make go into the app's `/dev/input` as the
  Xbox pad's do.
- Host: `deploy/node/host/72-cha-virtual-pads.rules` keeps the virtual devices
  (input and hidraw) off the host's own seat; the doctor checks `uhid`,
  `hid-playstation` and `hid-steam`.

## Known gaps (2026-10-05)

Verified on the node's kernel (6.17): `hid-playstation` binds the virtual
DualSense with our calibration (gyro and accelerometer come out 1:1), makes its
gamepad, touchpad and motion devices and a `hidraw` node, and its output
reports come back as `rumble`/`led`/`players`/`trigger`. The virtual Steam
Controller is a Bluetooth `28de:1303` (`HID_ID` bus 0005): the node's kernel
has no `hid-steam` entry for it (newer ones do), so `hid-generic` binds it,
with a `hidraw` node and no input devices; the node's `hidraw` answers feature
report 1 (settings, attributes, the serial), and output reports `0x80`
(rumble) and `0x81` (a trackpad pulse) come back as `rumble` and `haptic`.

- **Steam Input on the Steam Controller kind.** Steam opens the virtual
  controller over `hidraw` (2026 model, `28de:1303`): its menus, the Steam
  button and Quick Access work. Steam Input then hands a game its own virtual
  Xbox pad (`28de:11ff`), which it makes through `/dev/uinput`. The app has no
  `/dev/uinput` (it could make keyboards and mice on the node with it), so the
  Steam image loads a shim into Steam (`images/steam/uinput-shim/`) that fakes
  that file and asks the streamer's broker for the device; the broker allows
  only a gamepad, makes it, and passes it into the app like our own pads
  (`crates/cha-streamer/README.md`, `docs/plans/steam-virtual-gamepad.md`).
  Steam also tells games to ignore every physical Steam Controller
  (`SDL_GAMECONTROLLER_IGNORE_DEVICES`), and Proton's winebus applies that list
  to SDL and hidraw alike: without Steam's pad, a Proton game sees no controller.
  - **On the other kinds (found 2026-10-10).** Steam Input handles our Xbox 360
    and DualSense pads too (its log applies `controller_xbox360` configs to
    them), so at a game's launch it asks for its own virtual pad and hides ours
    from the game. With the broker started only for the `steam` kind, that open
    failed (`no broker` in `uinput-shim.log`): Big Picture worked, and a game
    (Balatro) saw no controller. The node now sets `CHA_STEAM_UINPUT=1` on the
    Steam environment's streamer, which serves the broker for every kind. No
    other app gets the socket. Confirmed by the owner on the iolinux node
    (2026-10-10).
  - **Verified 2026-10-06:** Steam makes its pad through the shim (the legacy
    uinput calls, "Microsoft X-Box 360 pad 0", rumble asked for) and re-makes it
    when a game starts; the broker follows. Balatro (Proton) plays with no
    launch options. Steam calls are logged to `/run/cha/uinput-shim.log` in the
    app.
  - **Not yet verified:** Cyberpunk 2077, rumble through the relay, and Steam's
    exit leaving no device behind.
  - **After Steam's in-game menu:** gamescope gives the game the X focus back
    with XSetInputFocus alone, and Wine doesn't make it the foreground window
    again (`WINEDEBUG=+event`: the FocusIn arrives, the foreground stays Wine's
    desktop), so the game ignored all input, the controller's too (SDL drops
    pad events without keyboard focus), until a click. `steam-focus` in the
    Steam image sends the game `WM_TAKE_FOCUS` when the focus moves from
    Steam's window back to it, which gamescope never does: input comes back
    without a click (Balatro, 2026-10-06).
  - **At a game's launch:** the same, with the controller doing nothing in
    Cyberpunk 2077 until a click (2026-10-07). The focus can reach the new
    window before Steam tags it (`STEAM_GAME`) or Wine sets its
    `WM_PROTOCOLS`, or come via no window, which the first `steam-focus`
    missed. It now sends `WM_TAKE_FOCUS` whenever a game's window has the
    focus and asks for it, once per focus and at most once a second per
    window, reading the window's properties on every poll.
  - **Workaround without the shim:** the launch option
    `SDL_GAMECONTROLLER_IGNORE_DEVICES= PROTON_DISABLE_HIDRAW=1 %command%` lets
    Proton's SDL read the controller itself, bypassing Steam Input.
  - An earlier note here said Cyberpunk played through Steam Input on
    2026-10-05: Steam's logs show that was the Xbox 360 pad.
- **More players.** Pads past those made at start (`--gamepads`) get evdev
  nodes but no `hidraw` in the app (the node mounts what `/info` listed at
  start).
- **Late viewers** get the lightbar, player LEDs and trigger effects as the
  apps last set them when they gain the controls (the streamer keeps the last
  of each per pad). Rumble and haptics aren't replayed. Unit-tested only so far.
- **The Steam Controller's descriptor** (a single vendor collection with the
  reports' ids and sizes SDL and the kernel use: feature 1, inputs `0x45`,
  `0x43`, `0x79`, outputs `0x80`, `0x81`) is ours; no dump of a real 2026
  controller was at hand, and Steam may expect more of it. The same goes for
  the pad pressure, the battery's voltage, the serial's digits (a real one is
  13 characters beginning `FXA`), the attributes' values and the stick-touch
  bits, which are guesses.
- **Haptic pulse side encoding** (which byte means the left pad) isn't
  documented by SDL. The streamer decodes `0x81` as the kernel sends it (1
  left, 0 right, 2 both); the web player's `steam-triton.ts` assumes 1 and 2
  for a physical controller, which may be wrong.
