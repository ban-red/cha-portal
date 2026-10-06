# Steam's virtual gamepad

Proposal, 2026-10-06. Nothing here is built.

## Problem

In the Steam environment with the `steam` controller kind, Steam reads our virtual Steam Controller (`28de:1303`, uhid, `hidraw`) but can't make the virtual Xbox pad (`28de:11ff`) it hands games: its console log says `Couldn't initialize virtual gamepad: Couldn't open /dev/uinput for writing` (repeated at every launch, `console_log.txt` 2026-10-06 20:30–20:31). Steam also tells games to ignore physical Steam Controllers, and Proton's winebus accepts only `11ff` on the `input` subsystem (ValveSoftware/wine `bus_udev.c` ~1741, `bus_sdl.c` ~939; unverified, from the task brief). A Proton game sees no controller. The per-game launch option `SDL_GAMECONTROLLER_IGNORE_DEVICES= PROTON_DISABLE_HIDRAW=1` works but skips Steam Input.

Goal: Steam makes its pad as on any Linux PC, games see it, the app never gets the host's `/dev/uinput` (with it the app could make keyboards and mice on the node's kernel).

## Findings

Read on the node's running Steam container (read-only; no restarts, no input):

- **Who touches uinput.** `/dev/uinput` is a string in `steamclient.so` only (`ubuntu12_32`, `linux64` and `linux32` copies); `steam`, `steamwebhelper` and the other binaries don't contain it. `steamclient.so` is mapped by pid 203, `ubuntu12_32/steam`, a 32-bit i386 ELF, and by the 64-bit `steamwebhelper` (pid 505, `linux64/steamclient.so`). The 32-bit `steam` process also holds `/dev/hidraw0` (fd 93), and the "Created virtual controller at slot 0" lines come from the client. So the opener is almost certainly the **32-bit client**; the 64-bit copy is a second possible one.
- **Traced (2026-10-06, logging-only probe).** An `LD_PRELOAD` probe that only logged opens of a `uinput` path ran under Steam for one launch. The 32-bit client (`ubuntu12_32/steam`, the process that holds `hidraw0`) calls libc `open()` on `/dev/uinput` and then `/dev/input/uinput`, with `O_RDWR | O_NONBLOCK | O_CLOEXEC` (`0x80802`). It retries both paths about 30 times in the first seconds, matching the "Couldn't initialize virtual gamepad" lines. No 64-bit process tried, and nothing went through `syscall()`. `LD_PRELOAD=/opt/cha/$LIB/…` set just before start-steam's `exec steam` survives `steam.sh` and the runtime: `$LIB` expands to `lib/i386-linux-gnu` in the client and `lib/x86_64-linux-gnu` in the 64-bit helpers (`gldriverquery`, `vulkandriverquery`), which load the 64-bit copy harmlessly.
- **How it calls the kernel.** The 32-bit `steamclient.so` imports `open`, `open64`, `ioctl`, `read`, `write`, `close` and `syscall` by name from libc (symbol-name scan of the file). A shim can catch the first six; whether `/dev/uinput` goes through `syscall` is unknown.
- **What it reads back.** The same file holds `/dev/input/`, `/sys/devices/virtual/` and `virtualgamepadinfo.txt`. Steam writes `~/.local/share/Steam/config/virtualgamepadinfo.txt` as `[slot N] name, VID, PID, handle, type`; today it lists the Steam Controller as slot 0. It probably does `UI_GET_SYSNAME`, then finds `/sys/devices/virtual/input/<name>/event*` and opens `/dev/input/eventN` (unverified). The container sees the host's sysfs (`/sys/devices/virtual/input`, `/sys/class/input/event*` exist), so a device the streamer makes on the host shows up there and in our `/dev/input` volume, as our Xbox pads already do.
- **Container today.** No capabilities, seccomp `steam` (Docker default plus namespaces), `cha-sandbox` AppArmor. Device cgroup allows input devices (major 13) and the streamer's `hidraw` nodes only. `/run/cha` is a volume both the streamer and the app mount (the Wayland socket lives there). The app's `/dev/input` and `/run/udev` are read-only volumes the streamer fills (`gamepad.rs` `share`, `install`, `udev_entry`).
- **Node.** Kernel 6.17. `CONFIG_CUSE=m` and the `cuse` module is **not loaded**; `/dev/cuse` is `root:root 0600`. `uinput` and `uhid` are loaded.
- **Host rule.** `72-cha-virtual-pads.rules` matches `phys=="cha/pad*"`, so a pad we make with phys `cha/pad<N>` needs no host change.

## Options

| | (a) `LD_PRELOAD` shim + streamer broker | (b) CUSE `uinput` | (c) host `/dev/uinput` + seccomp | (d) stopgap: our pad wears `28de:11ff` |
|---|---|---|---|---|
| Idea | Shim fakes `/dev/uinput` in Steam's processes; a Unix socket in `/run/cha` carries a narrow protocol; the streamer validates and makes the real device | Streamer serves `/dev/uinput` itself through `/dev/cuse` | Pass the real node, filter `ioctl` in seccomp | The streamer's existing Xbox pad, with Steam's identity, fed from the browser |
| Keeps Steam Input | Yes | Yes | Yes | **No** (mapping, gyro, paddles bypassed) |
| Effort | Medium: shim ~500 lines of C in two bitnesses, validator and broker in Rust | High: FUSE ioctl retry protocol, compat layouts, no Rust CUSE crate | Low | Very low |
| Risk | Steam updates; `syscall` or unknown helper processes bypass the shim | Compat of a 32-bit caller; FUSE edge cases | Not safe (below) | Steam may try to make its own pad too (unverified) |
| Security | Validator in the streamer is the boundary; the socket is reachable by games too, so it must hold alone | Same validator; kernel path is the real uinput ABI | Can't restrict `UI_DEV_SETUP` or writes | Strong: nothing new |
| Host needs | None | `modprobe cuse` (new in `modules-load.d`), `/dev/cuse` in the streamer, owner's sudo | `/dev/uinput` in the app | None |

**(a)** Needs `libcha-uinput.so` for i386 and amd64, loaded through `LD_PRELOAD=/opt/cha/$LIB/libcha-uinput.so` (the dynamic loader expands `$LIB`; unverified under Steam's runtime). `steam.sh` and pressure-vessel handle `LD_PRELOAD` themselves, so check it survives into `ubuntu12_32/steam`. Games run inside pressure-vessel and don't need the shim: Steam makes the pad, the game only reads it.

**(b)** Needs the streamer to open `/dev/cuse` (root-only today), a device name that isn't `uinput` (it would collide with the real misc node in devtmpfs; use `cha-uinput`), the node `mknod`ed into the input volume and mounted into the app at `/dev/uinput` with a cgroup rule, as `hidraw` is. The server must use the unrestricted-ioctl flag and the retry protocol to read and write each ioctl's pointer argument, and learn a 32-bit caller per file from `FUSE_IOCTL_COMPAT` (the caller's pid is 0 across pid namespaces, so `/proc` can't tell). `write()` has no such flag, and a 16-byte i386 `input_event` against 24 bytes on x86-64 is ambiguous by length alone. Crates: `fuser` has no CUSE (unverified); libfuse3's `cuse_lowlevel` is LGPL C (compatible with AGPL, but a C dependency, against ADR 0004's "ours, small"). The AppArmor profile adds no rule for it, but the container's seccomp doesn't touch `ioctl`.

**(c)** seccomp can allow `UI_SET_EVBIT`, `UI_SET_KEYBIT` and the other int-argument ioctls only with argument values from a list. It can't look inside `UI_DEV_SETUP` or `UI_ABS_SETUP` (pointers) or limit what `write` sends, and any game could open the node: the device's identity and event stream would be the app's to choose. Rejected.

**(d)** No Steam config or SDL mapping for a pre-made virtual pad turned up (unverified, from reading Steam's strings and SDL's documented hints; Steam has no public switch to point at another backend). (d) is a few lines in `gamepad.rs` and worth keeping as a fallback if (a) stalls, but it only gives games a pad, not Steam Input.

## Recommendation

Build **(a)**, with the streamer's broker as the whole trust boundary, so a later (b) could reuse the same validator unchanged. Reasons: no host change and no owner sudo (important, since every rollout step so far has needed one), the 32-bit layouts are known at compile time, and the cost is contained in one small file in the Steam image. The first step, a logging-only probe, is done: it settled open questions 1 and 2.

## Design

**Components**

- `images/steam/uinput-shim/` (C, built in the image for i386 and amd64): wraps `open`/`open64`/`openat`, `ioctl`, `read`, `write`, `close`. `open` of exactly `/dev/uinput` connects to `/run/cha/uinput.sock` (seqpacket) and returns that socket as the fd, so `poll`, `epoll` and `O_NONBLOCK` work natively. Every other path passes through. The shim tracks its fds in a small table; on any failure it returns `-1/ENODEV` like today.
- `crates/cha-streamer/src/uinput_broker.rs`: listens on the socket (mode 0660, uid 1000). One connection is one device. Spawned only for the `steam` kind.
- `crates/cha-uinput-proto` is not needed: the protocol lives beside the broker, with the same bytes defined in the shim's header and a test that both agree.

**Wire protocol (seqpacket, little-endian, one request per packet, fields normalized so no ABI differences cross it)**

- `SetBit {kind: ev|key|abs|ff, code}`; `AbsSetup {code, min, max, fuzz, flat, res}`; `DevSetup {ff_effects_max}` (name, bus and ids from the client are read and ignored); `Create`; `Destroy`; `GetSysname`.
- `Events [{type, code, value}]` for each `write()` (the shim converts the 16- or 24-byte `input_event`).
- Streamer to shim: `Reply {errno, payload}`, `FfUpload {request_id, effect}`, `FfErase {request_id, effect_id}`, `FfPlay/Gain {code, value}` (what the kernel's `EV_FF` events carry). The shim answers `UI_BEGIN_FF_UPLOAD`/`ERASE` from the queued request, and `UI_END_*` goes back as `FfDone {request_id, retval}`. Reads of `EV_UINPUT` and `EV_FF` events by Steam come from the socket as `input_event` structs, laid out for the shim's bitness.

**Validator** (unit-tested without a kernel): event types `SYN`, `KEY`, `ABS`, `FF` only; keys `0x130..=0x13e` (`BTN_SOUTH`..`BTN_THUMBR`) and `0x220..=0x223` (d-pad); axes `ABS_X..ABS_RZ`, `ABS_HAT0X/Y`; force feedback `FF_RUMBLE`, `FF_PERIODIC` and its waveforms, `FF_GAIN`; `ff_effects_max` ≤ 16; each axis range within ±65535; ≤ 64 events per write, a rate cap; at most 4 live devices per environment (the pad limit of `gamepad.rs`), one per connection. Anything else gets `EINVAL` and a warning logged once. The identity is fixed by the streamer, never read from the client: `28de:11ff`, bus USB, name "Steam Virtual Gamepad", phys `cha/pad<8+N>`.

**Making the device.** The streamer opens its own `/dev/uinput` (it already does), applies the validated bits, creates it, waits for `eventN` (the existing `share`), and installs the node in `/dev/input` plus a udev entry `ID_INPUT_JOYSTICK=1`, `ID_VENDOR_ID=28de`, `ID_MODEL_ID=11ff` (an `Identity` for it beside `XBOX_360`). Then it answers `GetSysname` with the real kernel name, which Steam can read under the host's sysfs the container sees. The broker's `Events` go straight to the real fd. The existing `serve_force_feedback` thread is replaced for these pads by a relay: kernel `EV_UINPUT` requests become `FfUpload`/`FfErase` to Steam, whose `FfDone` completes them (Steam, not the streamer, then drives the Steam Controller's haptics through `hidraw`, as today).

**Cleanup.** The socket closing (Steam exits or crashes) destroys the device: drop the fd, remove the node and udev file, and finish any pending FF requests with an error. The streamer's exit removes everything as it does for pads now. Steam makes and destroys a pad per game launch (the log shows several), so this path runs often.

**Node agent and host.** No new device, mount or cgroup rule: `/run/cha` is already shared, and the input volumes and `device_cgroup_rules` already allow evdev nodes. The agent only sets the Steam image's `LD_PRELOAD` environment (or the image does in `start-steam`) and passes `--steam-uinput` to the streamer for the `steam` kind. No `deploy/node/host` change; `deploy/README.md` and `docs/controllers.md` get a line (the "planned" fix becomes the behavior), and the node needs the new streamer and Steam images, a normal rollout with the owner's OK to recreate the agent.

## Open questions

1. ~~Which process and which call opens `/dev/uinput`?~~ Settled: the 32-bit client, through libc `open()` (see Findings). The shim needs only the i386 build to start with, and must also take `/dev/input/uinput`.
2. ~~Does `$LIB` and `LD_PRELOAD` survive `steam.sh` and the runtime?~~ Settled: yes (see Findings).
3. Does Steam size its device from `UI_GET_SYSNAME` and sysfs, or from an `inotify` on `/dev/input`? Does it need `/sys/.../input/<name>/id` to match?
4. Does Steam advertise force feedback on its pad, or relay rumble another way?
5. Does pressure-vessel's `/dev/input` snapshot include a pad created after the game's container starts (Steam makes it first, so probably yes)?
6. Is `28de:11ff` enough for Proton (it also reads the `phys`/`uniq`, unverified)?

## Test plan

- **Unit:** the validator (allowed and refused bits, ranges, counts, rate), the protocol codec both bitnesses (16- and 24-byte `input_event`, `uinput_ff_upload`), device-limit and cleanup on disconnect, `udev_entry` for the new identity.
- **Shim, in the dev container:** a C test program opens `/dev/uinput`, sets bits, creates, writes events, reads FF upload requests; run as i386 and amd64 against a fake broker, then against the real streamer on the node.
- **Live on the node (owner's OK first):** `steam` kind, no launch options. Pass: Steam's console log no longer says it couldn't open uinput; `virtualgamepadinfo.txt` lists the `11ff` pad; Cyberpunk and Balatro see a controller and play; Steam's in-game menu shows the controller and focus returns after closing it; rumble reaches the Steam Controller; killing Steam leaves no device node, udev file or `/sys/devices/virtual/input` entry behind; two launches in a row work.
