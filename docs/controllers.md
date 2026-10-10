# Controllers

How a controller in the user's hands becomes a controller in the environment.
The browser reads it (Gamepad API, or WebHID with our own drivers), turns it
into one canonical state, and sends that up (as unreliable datagrams where the
transport has them, else on the control channel). The streamer
feeds it into a **virtual controller** of the kind the user chose for the app:

| Kind | Device | What the app sees |
|---|---|---|
| `xbox360` (default) | uinput | An Xbox 360 pad (`045e:028e`): buttons, sticks, triggers, rumble. Every game and SDL know it. |
| `dualsense` | uhid | A wired DualSense (`054c:0ce6`): the host kernel's `hid-playstation` binds it (gamepad, touchpad and motion evdev nodes), and the app also gets its `hidraw` node (SDL's HIDAPI driver, Proton's winebus, Steam). Touchpad, gyro, accelerometer, rumble, lightbar, adaptive triggers, player LEDs. |
| `steam` | uhid | The 2026 Steam Controller over Bluetooth (`28de:1303`; its wired id would meet the missing USB interface a wired original did): Steam talks to it over `hidraw` (Steam Input: layouts, gyro, trackpads, haptics); the host's `hid-generic` binds it, with no evdev devices. A real d-pad, two sticks, both trackpads, four back paddles, the quick-access button, gyro, accelerometer, battery, rumble and trackpad haptic pulses. The original Bluetooth controller (`28de:1106`) is still in the code behind `steam_controller::VARIANT`. |

### The Steam Controller as a USB device (opt-in)

Set `CHA_STEAM_PAD_TRANSPORT=usbip` on the node agent ([deploy/README.md](../deploy/README.md#steam-controller-as-a-usb-device))
and the `steam` kind is a wired controller, `28de:1302`, instead of the
Bluetooth `28de:1303` uhid device. The streamer is the USB device: it answers
the kernel's USB/IP requests on a `socketpair` whose other end `vhci_hcd` holds
(`vhci.rs`, `usbdev.rs`, `usbip.rs`; [research note 10](research/10-usbip-and-usb-passthrough.md), design B).
`usbhid` binds it, SDL's interface check no longer matters (the device has a
real interface 0), and the app sees a `hidraw` node with USB attributes. The
reports are the same: the page's pad state, rumble and trackpad pulses (by
interrupt OUT or SET_REPORT), and feature report 1 by GET/SET_REPORT on
endpoint 0. What differs for `1302`: the state report id is `0x42` (SDL's
wired state; `0x45` is the Bluetooth one, both parsed alike) and there is no
`0x79` wireless status.

What is a real 2026 controller's and what is ours, for `1302`. Taken from a
real controller on a USB-C cable (macOS's I/O Registry; its feature-report
replies could not be read, see below):
- the whole HID report descriptor (372 bytes): a mouse (report `0x40`) and a
  keyboard (`0x41`), the controller's lizard mode, then the vendor collection
  with inputs `0x42` (53 bytes: the state and an orientation quaternion), `0x44`,
  `0x79`, `0x43`, `0x7b`, `0x45`, outputs `0x80` to `0x89`, features 1 and 2
  (63 bytes each);
- the USB device: bcdUSB `0x0201`, bcdDevice `0x0307`, 64-byte endpoint 0,
  strings "Valve Software", "Steam Controller" and a serial (index 3), one
  configuration drawing 500 mA, one HID interface (class 3, no boot subclass)
  with two endpoints, and the largest report sizes (54 in, 64 out, 64 feature).

Still ours: the endpoint addresses and `bInterval` (the I/O Registry doesn't
show them; 1 ms), the configuration's attributes, the BOS descriptor that
bcdUSB `0x0201` asks for (a USB 2.0 Extension capability), what `0x44`, `0x7b`,
output `0x82` to `0x89` and feature report 2 mean (the streamer takes them,
logs the first of each, and does nothing; feature 2 reads as zeros), the
serials' digits (the unit's `FXA` and the board's `MXA`, each with ten hex
digits, made from the owner, app and slot; a real controller's have those
shapes), and the controller's behaviour: which state report it sends on a
cable (we send `0x42` and `0x43`), the battery values, the pad pressure and
touch thresholds. The replies to `0x83` (attributes: the product, a revision
of 0, the bootloader's and firmware's build times, hardware id `0x48`) and the
serials' formats were read from a real wired controller (2026-10-10). Steam
called the board serial invalid while it was answered with the unit's `FXA`
one.

The keyboard and mouse the descriptor brings are made input devices by the
host's `hid-generic`. They never send anything, so they are not shared into
the app (only the `hidraw` node is); the vhci rule in `72-cha-virtual-pads.rules`
matches the HID parent, so it covers them too: no seat, root only, ignored by
libinput. Default is uhid. If `attach`
can't be written (module not loaded, or AppArmor, see the deploy README) the
streamer uses uhid. Under AppArmor the streamer runs under `cha-streamer-vhci`
(installed by `install.sh`): Docker's default profile with the `/sys` denial
rewritten to leave exactly `.../vhci_hcd.0/attach` and `detach` writable.
`CHA_STEAM_PAD_APPARMOR=unconfined` also works but is a test-only escape hatch.

The kind is the user's per-app choice (the portal's Controllers page), with a
default per app from the catalog. It is fixed for an environment's life (the
first pad is made before the app starts, and its nodes are mounted into it);
more pads of that kind can join while it runs ([More players](#more-players-pads-that-join-mid-session)).
Any physical controller can drive any kind (a Steam Controller's
trackpads become the DualSense's touchpad, and so on); what the physical one
lacks stays at rest.

## Page → streamer: `pad`

In the input envelope (`{"t":"input", …}`), sent when something changed.
Pad state goes by the unreliable path when there is one, else on the control
channel ([below](#reliability-and-datagrams)):

```
{ k:"pad", i: 0..3,
  q?: u32,                 // the pad's sequence number, see below
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

### Reliability and datagrams

A lost packet on Wi-Fi holds up everything behind it on an ordered, reliable
channel, so pad state, which is latest-wins, doesn't need one. What goes where:

| Message | Channel |
|---|---|
| `pad` state | An unreliable datagram when the session offers them (below), else the control channel |
| `pad` with `gone:true`, rumble, haptic, LED, trigger, feature traffic, floor and pad grants | Control channel (reliable, ordered) |
| Keyboard, mouse, clipboard, everything else | Control channel |

- **Negotiation.** The streamer's `hello` has `"padDatagrams": true` on
  WebTransport and WebRTC (not on the WebSocket transport, which is TCP
  anyway). The page uses datagrams only after it has seen that and the
  transport can send one: WebTransport's worker has a datagram writer, or the
  WebRTC `input` channel is open. Otherwise, and for any message too big for a
  datagram (over 1000 bytes), the pad goes on the control channel as before. An
  old page never sees the flag, an old streamer never sends it, and a streamer
  ignores a channel label it doesn't know, so every pairing works.
- **WebTransport:** a QUIC datagram. Media datagrams run streamer → page and
  start with the 16-byte `DatagramHeader`; an input datagram runs the other
  way and starts with byte `0x12` (wire version 1 in the high nibble,
  `Kind::Input` in the low one), then a format byte, then the payload
  (`crates/cha-proto/src/input.rs`, `web/packages/player/src/input-datagram.ts`).
  Format `0` is one pad message, `{"t":"input","k":"pad",…}` as JSON, without
  the newline. The streamer ignores formats it doesn't know, and any
  non-`pad` message in a datagram.
- **WebRTC:** a second DataChannel, `input`, created by the page with
  `ordered:false, maxRetransmits:0`. Its messages are binary and framed the
  same way. The streamer (str0m) takes the reliability from the channel's open
  message; it only needs the label.
- **Sequence numbers.** The page numbers each pad's messages (`q`, a `u32`
  counting from 1 per slot for the page's life, `gone` included) and the
  streamer applies a message only if its `q` is newer than the newest it
  applied for that pad index (modulo 2^32). A datagram that arrives late, or
  after the `gone` that overtook it on the reliable channel, is dropped; a copy
  with the same `q` is ignored. A message without `q` (an older page) is always
  applied. The streamer counts the stale ones (`pad_stale` in its stats) and the
  datagrams it took (`pad_datagrams`).
- **Resending.** Datagrams can be lost, so a lost release must not leave a
  button down: the page sends a held pad's state again every 50 ms with the
  same `q`, and a pad at rest 3 more times, 50 ms apart, after its last
  change. 50 ms is three frames at 60 fps, so a lost press or release shows
  for at most that, at about 20 small datagrams a second per held pad. With 5%
  loss a release sent four times is lost with probability 0.05⁴ ≈ 6·10⁻⁶. Held
  means a button down, a stick more than 0.15 from centre or a finger on a
  pad. If the datagram path goes away, the next resend goes on the control
  channel and the pad stays on it.
- **Stale pads.** The streamer has no timer that rests a pad: it keeps the last
  applied state until a newer one or `gone` (sent when a pad leaves, the page
  loses focus or stops). The resend is what repairs a lost datagram. A
  session that ends puts its player slot to rest as before.
- **When it's sent.** WebHID pads (DualSense, Steam Controller, generic HID) are
  sent on each input report that changes the state; the Gamepad API's pads, which
  have no events, on the page's 4 ms poll. The native client
  (`cha-client-stream`) still sends pad state on the control stream, without `q`.

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
  "minor": 3, "devpath": "/devices/virtual/misc/uhid/…/hidraw/hidraw3" }]`.
  The node then mounts each into the app at `/dev/<name>` (a volume mount
  with that subpath) and allows the whole `hidraw` major in the app's device
  cgroup (`c <major>:* rwm`, for pads that join later). libudev, hidapi and
  Wine find them by the kernel's name under `/dev`. The evdev nodes the host's
  `hid-playstation`/`hid-steam` make go into the app's `/dev/input` as the
  Xbox pad's do.
- Pads that join later: [More players](#more-players-pads-that-join-mid-session).
- Host: `deploy/node/host/72-cha-virtual-pads.rules` keeps the virtual devices
  (input and hidraw) off the host's own seat; the doctor checks `uhid`,
  `hid-playstation` and `hid-steam` (and `vhci-hcd` with the USB opt-in).

## More players: pads that join mid-session

A pad made after the app started (the page sends a second controller, or one
in a later slot) gets its evdev nodes in the shared `/dev/input` volume at
once, as before. Its `hidraw` node needs more, because Docker can't add a
mount or a device cgroup rule to a running container. What happens
(`check_pads` in `crates/cha-node/src/environments.rs`):

1. At creation the app's device cgroup gets a **wildcard rule for the
   `hidraw` major** (`c <major>:* rwm`, the major read from the streamer's
   `/info`) instead of one exact rule per start-up pad. It lets the app open
   any `hidraw` device the host has, but only nodes that exist in the app's
   own `/dev` can be opened, those are the ones the agent puts there, and the
   app (`CapDrop ALL`) can't make a node itself. `m` is for the agent's
   `mknod`. The pads made at start are still mounted at `/dev/hidraw<n>`
   from the input volume.
2. Once a second the agent reads each streamer's `/info` (`hidraw`: name,
   major, minor, and the node's sysfs `devpath`). A node it hasn't put in
   and the app doesn't have mounted is made in the app's `/dev` by a
   **privileged `docker exec`** as root: `mknod -m 660 /dev/hidraw<n> c M m`
   and `chown 1000:1000`, each a fixed command with numbers it checked. This
   is the only privilege added: the agent already holds the Docker socket
   (root-equivalent), and the exec lasts for two short commands, in the app's
   mount namespace. Docker can't give an exec one capability (`CAP_MKNOD`);
   `Privileged` is the only way to get it.
3. The agent tells the app, the way udev would. A node nobody announces is
   there, but nobody looks (see the spike below), so a throwaway container of
   the streamer image runs `cha-streamer --udev-event add <devpath> <name>
   <maj:min>` in the app's **network namespace** (`NetworkMode:
   container:<app>`), as root with `CAP_NET_ADMIN` and nothing else, a
   read-only root and no label. It sends one libudev-format message to
   netlink group 2 (`crates/cha-streamer/src/udev_event.rs`, our own code
   written from the format). Sending to a uevent group takes `CAP_NET_ADMIN`,
   and libudev ignores messages from non-root senders.
4. The streamer writes an empty `udev/control` in the udev volume (the app's
   `/run/udev/control`). libudev joins udev's multicast group only when udev
   "runs", which it decides by that file; without it a "udev" monitor is
   subscribed to nothing and hears nothing.
5. An agent restart adopts running apps: it reads each app's cgroup rules and
   mounts, finds the nodes listed but not mounted, and makes and announces
   them again (it removes a stale node first). An app an older agent made has
   exact rules only, so it keeps the old behaviour: pads made after its start
   have no `hidraw` in it.

The streamer keeps its pads for its life (a controller that comes back gets
its slot and so its Steam configuration), so the agent's removal path (node
removed, `remove` event sent) runs only if a pad's node leaves `/info`; it is
unit-tested but nothing makes that happen yet.

### The spike (2026-10-10, iolinux, kernel 6.17)

Question: does Steam notice a `hidraw` node that appears mid-session? Not by
itself; it does within a second of a libudev-format message.

- **Steam finds controllers through sysfs and tries `/dev/hidraw<n>`.** Its
  log (`~/.local/share/Steam/logs/controller.txt`) prints `Local Device Found`
  for every `/sys/class/hidraw` entry, the host's own pads too
  (`Unable to open local device: /dev/hidraw0`, a node the app doesn't have).
  It scans when it starts and again about 3 s after a controller opens. A
  pad whose sysfs entry appeared later, with its node made in the app's
  `/dev` by a privileged `docker exec ... mknod`, was never opened (25 to
  60 s watched, twice).
- **The kernel's uevent does reach the app's network namespace** (found while
  looking for why not). A libudev monitor on the `kernel` source in the app
  got `add` and `remove` for `hid` and `hidraw` as a pad was made and
  destroyed. So the premise that hotplug events don't cross was wrong for
  Docker's bridge network on this node. It doesn't help: Steam opens `udev`
  monitors, and with libudev 259 (the Steam runtime's) such a monitor joins
  group 2 only when `/run/udev/control` exists. Without the file
  `/proc/net/netlink` showed group 0 for all of Steam's four uevent sockets;
  with it, group 2. udevd's messages never come (none runs in the app).
- **libudev takes a group-2 message from a root sender** with its
  `libudev\0` header (magic `htonl(0xfeedcafe)`, header and property sizes in
  host order, subsystem hash and tag blooms in network order) and
  `KEY=value` properties (`ACTION`, `DEVPATH`, `SUBSYSTEM`, `DEVNAME`,
  `MAJOR`, `MINOR`, `SEQNUM`). A Python sender and a libudev monitor
  (ctypes) in the app showed it. A unicast message to a monitor's port is
  dropped (only a trusted udevd worker may), and a group-1 message from
  anywhere but the kernel is too.
- **One `SUBSYSTEM=hidraw` message is enough.** Steam logged `Steam controller
  device opened for index 1` for `/dev/hidraw2` within a second of the pad
  being made, with the pad created through the real streamer (frame tap
  `PAD` command with `--frame-tap-pad 1`: the first pad at start, the second
  later) and the agent doing steps 1 to 4 (`Controller uses V1 HID protocol
  via USB`, the `usbip` transport, `28de:1302`). The parent `hid` event isn't
  needed. A restart of the agent in the middle re-made the node and the
  event without trouble.
- **`N:` in the udev database doesn't move a node.** Putting the node in the
  shared `/dev/input/hidraw/` and naming it in the udev entry (`N:` or
  `E:DEVNAME=`) was tried: libudev 259's `udev_device_get_devnode` still
  answers `/dev/hidraw<n>`. The node has to be in `/dev` itself.
- **SDL**: the app sets `SDL_JOYSTICK_DISABLE_UDEV=1`, and the Steam client's
  SDL3 holds one inotify watch on the `/dev/input` volume, which is how the
  evdev nodes of late pads are seen. Proton's winebus and SDL's HIDAPI in a
  game were not tried with a late pad (a game in a Steam environment gets
  Steam's virtual Xbox pad anyway, below).

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
