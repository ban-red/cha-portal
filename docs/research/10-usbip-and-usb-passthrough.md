# 10: USB/IP and USB passthrough, October 2026

> Background research ([index](README.md)). Nothing here is built yet. What Cha Portal actually ported is in [PROVENANCE.md](../PROVENANCE.md).

Can USB/IP (the kernel's `vhci_hcd` and the USB/IP protocol) give Cha three things?

1. Better controller compatibility in Steam and Steam Input, starting with Valve's 2026 Steam Controller.
2. Lower input latency.
3. Generic USB passthrough into any image, with hot plugging.

Researched 2026-10-10 from the kernel sources and docs, SDL's source, vendor pages and a read of our own input path. Claims we could not confirm are tagged **(unverified)**. **[I]** marks our own inference.

## Verdict

| Goal | USB/IP over the network | What gets us there instead |
|---|---|---|
| 1. Steam compatibility | **Yes, the highest fidelity.** Steam sees the real device: descriptors, serial, every interface, firmware feature reports. But only from a native helper on Linux or Windows; macOS and browsers can't export a HID-class device (below). | A **raw HID relay**: forward the controller's own reports and its real descriptor, not our parsed pad state. Optionally present it on the node as a real USB device through a *local* USB/IP loopback. This works from Chrome on the Mac. |
| 2. Latency | **No.** It keeps the same network hop we already have, adds TCP, and caps the report rate near 1/RTT unless the exporter pre-reads. Stadia's gain came from removing the *client device* from the path, which an off-the-shelf controller can't do. | Send pad input as **unreliable, latest-wins datagrams** instead of on the ordered, reliable control channel. Forward WebHID reports **as they arrive** rather than on the 4 ms poll. Measure controller-to-photon first. |
| 3. Any USB device, hot plugged | **Feasible on the node, with real cost.** It needs a host-root helper for `vhci_hcd` attach, broad device-cgroup rules fixed at container create, our own `/run/udev` entries, and a validating proxy, because the node kernel parses everything the client sends. Clients: Linux and Windows helpers; macOS only for non-HID devices. | Ship it as an **owner-only, opt-in** feature for custom images and Steam, behind the same authenticated transport. Never expose raw USB/IP (TCP 3240). |

**Recommendation:**

1. Do the raw HID relay first. It is the compatibility win for the controllers we care about, on every client, Mac and Chrome included.
2. Next, spike a streamer-side *loopback* USB/IP device: the streamer is the exporter on a `socketpair`, and `vhci_hcd` is the importer. Do it only if Steam still treats the uhid device differently from a real one.
3. Treat true remote USB/IP as a later native-client feature for Linux and Windows, aimed at goal 3 rather than at controllers.

## How USB/IP works, and what it costs

- **Protocol.** USB/IP v1.1.1 runs over TCP (port 3240 by convention), big-endian, with **no authentication or encryption**.
  - `OP_REQ_IMPORT` (busid) is answered by `OP_REP_IMPORT` (busnum, devnum, speed, ids, classes).
  - After that, every URB is a `USBIP_CMD_SUBMIT` with a 48-byte header (seqnum, `devid = busnum<<16 | devnum`, direction, endpoint, flags, length, interval, 8-byte setup packet) and OUT data. It is answered by `USBIP_RET_SUBMIT` with IN data. `CMD_UNLINK` cancels by seqnum.
  - Isochronous transfers are supported.
  - Source: [kernel docs](https://docs.kernel.org/usb/usbip_protocol.html).
- **Nobody polls the endpoint on the wire.** The exporter (`stub_rx.c`) submits each URB to the real device once and replies from its completion handler; there is no timer. `vhci_hcd` sends each URB immediately. Linux `usbhid` keeps **one** interrupt-IN URB outstanding and resubmits it from completion ([`hid-core.c`](https://raw.githubusercontent.com/torvalds/linux/master/drivers/hid/usbhid/hid-core.c)).
  - **[I]** So a report costs only one-way latency, because the URB is already parked at the exporter. But between `RET_SUBMIT` and the next `CMD_SUBMIT` arriving (one RTT) nothing reads the device, and reports in that gap are lost.
  - **[I]** The rate is capped near 1/RTT: fine on a LAN, about 50 Hz at 20 ms over Tailscale.
  - **[I]** Control and OUT transfers (feature reports, Steam's settings writes, haptics, rumble) each cost a full round trip.
  - **Fix: pre-read on the exporter.** The exporter keeps its own URB queued on the device, buffers a few reports (a FIFO of 4 or more, so a quick tap's press survives), and answers each `CMD_SUBMIT` from the buffer. That lifts the cap. It makes reports fresher, not more frequent, unless the buffer is drained by a matching rate.
- **Published latency numbers:** none found for USB/IP. Users comparing VirtualHere with native report about 1 ms, rising to 6–15 ms under server load ([moonlight-qt #1156](https://github.com/moonlight-stream/moonlight-qt/issues/1156), [VirtualHere forum](https://www.virtualhere.com/node/4519)). We'd measure our own.
- **Pitfalls a USB/IP exporter has to handle (from the protocol and from prototypes):**
  - **Requests to intercept.** Intercept `SET_CONFIGURATION`, `SET_INTERFACE`, `CLEAR_HALT` and `SET_ADDRESS` and answer success rather than replaying them on a device the exporter already configured. Otherwise data toggles desync, and the device enumerates but never sends input.
  - **Session end.** Cancel and drain in-flight URBs when a session ends.
  - **Attach timing.** After an attach, the vhci port reads `NOTASSIGNED` for roughly 300 ms; wait for `USED` before looking for device nodes.
  - **Releasing local drivers.** Composite devices with CDC interfaces need their local drivers released in two passes, and rebound on teardown.
  - **Socket options.** Set `TCP_NODELAY` on both ends and send each header and payload in one write.

## The node side (importer)

- **Attach without the GPL tools.**
  - **The write.** Write `"<port> <sockfd> <devid> <speed>"` to `/sys/devices/platform/vhci_hcd.0/attach` ([`vhci_sysfs.c`](https://raw.githubusercontent.com/torvalds/linux/master/drivers/usb/usbip/vhci_sysfs.c)).
  - **`sockfd`.** It is looked up in the writer's own fd table and must be a connected `SOCK_STREAM`; the kernel then owns it.
  - **Port and speed.** SuperSpeed and above take a free port on the USB 3 hub, everything else the USB 2 hub; a busy port gives `-EBUSY`.
  - **Detach and status.** `detach` takes a port; `status`, `status.N` and `nports` are read-only.
  - **Code.** Writing this ourselves in Rust is a few dozen lines, so we never link the GPL-2.0 `usbip` userspace. `vhci_hcd` itself is GPL-2.0+ kernel code, used as a module.
- **Any connected stream socket works.** Through **a `socketpair`**, the streamer can be the exporter while the kernel thinks it's talking to a remote host. That is the hinge of the designs below. **[I]**
- **Permissions.**
  - The attach file is mode 0200 root, with no capability check beyond that.
  - Our node agent isn't privileged and has no `/sys` or `/dev` write (`deploy/node/compose.yaml`).
  - The streamer has only `/dev/uinput` and `/dev/uhid` (`crates/cha-node/src/environments.rs` ~2445).
  - Attaching therefore needs either a small root host helper or the streamer gaining a writable `/sys/devices/platform/vhci_hcd.0`.
  - **[I]** The second is narrower: a bind of that one directory.
- **Host setup.**
  - `vhci-hcd` joins `modules-load.d/cha.conf` (`deploy/node/host/install.sh`, today `uinput`, `uhid`), and `--doctor` checks for it (`doctor.rs`).
  - On Proxmox LXC nodes it loads on the outer host.
- **Devices are host-global.**
  - A vhci device enumerates on the **host** kernel. Host drivers bind it (`usbhid`, `hid-steam`, `cdc_acm`).
  - Its sysfs entries are visible to every container, and host udev rules (`70-uaccess`, Steam's `60-steam-input.rules`) would hand its nodes to the person at the desk.
  - It needs a rule like our `72-cha-virtual-pads.rules`, matched on the vhci parent path.
  - There's no way to scope a USB device to one container's namespace; the device cgroup only gates `open()`.
- **Getting nodes into a running container.**
  - Docker sets device-cgroup rules at create time only. Our rules are exact (`c 13:* rw` for evdev plus one exact `maj:min` per hidraw node, `environments.rs` ~2764).
  - Today a pad added after start gets evdev but no hidraw ([controllers.md](../controllers.md)).
  - Hot plugging hidraw or `/dev/bus/usb` (major 189) needs wildcard rules for those majors at create. The hidraw major is dynamic: read it from `/proc/devices`.
  - The streamer then mknods the node into the shared, live input volume, as it already does (`gamepad.rs` `install`/`share`), with a `/run/udev/data` entry so libudev (Steam, SDL) sees it.
  - **The trade-off:** a container allowed `189:*` can open any USB node that appears in its `/dev`. Only nodes we mknod appear there, but the rule is broader than today's.

## The client side (exporter)

| Client | Can it export a game controller? |
|---|---|
| Linux | Yes: `usbip-host` in the kernel, or userspace over usbfs (`USBDEVFS_DISCONNECT_CLAIM` evicts local drivers). |
| Windows | Yes, but `usbipd-win` is GPL-3.0 and an exporter needing admin ([repo](https://github.com/dorssel/usbipd-win)). Our own would go through WinUSB/libusb with a driver swap. |
| macOS | **Not for HID-class devices.** libusb can't claim a HID interface without detaching the kernel driver, and on Darwin that needs capture entitlements or root ([Apple forum](https://developer.apple.com/forums/thread/723186), [libusb](https://flutter.googlesource.com/third_party/libusb/+/abd788c203624930241f45f32b501c16936bd806)). Vendor-class (0xFF) devices are fine. IOKit's `IOHIDDevice` gives reports **and the raw report descriptor** (`kIOHIDReportDescriptorKey`) under Input Monitoring, which is a HID relay, not USB/IP. |
| Chrome (any OS) | **No.** WebUSB blocks the protected classes, HID among them (also audio, mass storage, hub, smart card, video, wireless): `claimInterface` fails ([WebUSB spec](https://raw.githubusercontent.com/WICG/webusb/main/index.bs)). WebHID gives reports and feature reports, but not the USB descriptors or the raw report descriptor (only the parsed collections). |

Our baseline client, Chrome on an M4 MacBook, is the one that can't do USB/IP at all. That alone rules USB/IP out as the default controller path.

Rust building blocks:

- [`jiegec/usbip`](https://github.com/jiegec/usbip): MIT, v0.9.0; a server library on tokio with `rusb`/`nusb` host and virtual-device examples (HID keyboard, CDC ACM).
- `nusb` (MIT/Apache).
- Writing our own server is also small: we'd need only `OP_REQ_IMPORT` and the four URB messages.

## The 2026 Steam Controller

- **The controller.** Launched 4 May 2026 ([Wikipedia](https://en.wikipedia.org/wiki/Steam_Controller_(2nd_generation))).
  - USB-C, Bluetooth, and a 2.4 GHz puck that docks and charges.
  - The puck takes up to 4 controllers, with about 8 ms end to end and 4 ms polling.
  - Hardware: gyro, two pressure-sensitive haptic trackpads, capacitive Grip Sense, TMR sticks with capacitive touch, and back paddles.
  - Lizard mode (keyboard and mouse) when Steam isn't driving it.
- **IDs and interfaces.**
  - The ids are wired `28de:1302`, Bluetooth `1303`, puck `1304` and receiver `1305`. These match our `steam-protocol.ts`, ported from SDL.
  - The wired controller is one HID interface: interrupt IN `0x81` and OUT `0x01`, 64 bytes, 1 ms **(unverified)**.
  - The puck is 7 interfaces **(unverified)**:
    - 0–1: CDC ACM.
    - 2–5: one HID interface per controller slot.
    - 6: the dock's pogo pins.
- **SDL's interface check (verified in SDL `main`, `SDL_hidapi_steam_triton.c`, `IsSupportedDevice`).**
  - SDL accepts the puck and receiver (`1304`, `1305`) only when hidapi reports interface 2–5. Other Triton ids have no interface check.
  - hidapi's Linux backend sets `interface_number` from a USB parent only. It stays −1 for Bluetooth and for a uhid device, which has no USB parent.
  - So a uhid copy of the **puck** would be rejected. That is why our virtual controller presents as Bluetooth `1303` (`steam_controller.rs`), which SDL takes without the check.
  - Whether Steam's own (closed) client applies the same rule is unknown. **[I]** It likely shares the logic.
- **Kernel.** `hid-steam` gains initial support for the 2026 controller in Linux 7.3 (**unverified**, [Phoronix](https://phoronix.com/news/Linux-7.3-HID) was unreachable). The node runs 6.17, so `hid-generic` binds ours and Steam drives it over hidraw, as it does a real one.
- **Our gaps today ([controllers.md](../controllers.md)).**
  - The page parses the controller's reports into our canonical pad state (`steam-triton.ts`), and the streamer re-encodes them into a descriptor we reconstructed.
  - We guessed the descriptor, the serial, the battery voltage, the attributes and the haptic side encoding.
  - Steam may notice. That is a compatibility gap a relay of the real reports closes, with or without USB/IP.

## Latency, and what Stadia actually did

- **Stadia.** The Stadia controller joined Wi-Fi and talked to Google's servers directly, so input skipped the Chromecast or laptop entirely (protocol and pairing not public).
  - Digital Foundry measured about 166 ms button-to-screen at GDC 2019 (AC Odyssey, 30 fps). PC Gamer measured 163 ms on Stadia against 83 ms native in Destiny 2. Both figures are second-hand **(unverified)**.
  - Amazon claims 17–30 ms saved by the Luna Controller's "Cloud Direct" Wi-Fi mode over Bluetooth through a client device ([Amazon](https://press.aboutamazon.com/2020/9/introducing-luna-amazons-new-cloud-gaming-service-where-its-easy-to-play-on-the-devices-you-already-own)); no independent test found.
  - The saving is the client device's Bluetooth stack, OS input path and app polling, not the network.
- **USB/IP keeps that path.** Off-the-shelf controllers can't join our network on their own. A USB/IP export still goes device → client OS → our helper → network → node, with the same network hop as today and TCP in place of datagrams.
  - **[I]** Against WebHID it saves the browser's event dispatch and our 4 ms poll, on the order of 1–4 ms. It loses that much or more to TCP retransmits on Wi-Fi.
  - It is not a latency feature.
- **Where our input latency goes today (from the code survey).**
  - **Polling.** The page polls pads every 4 ms (`manager.ts` `POLL_MS`) and sends on change.
  - **Transport.** Pad state rides the **ordered, reliable** control channel: the WebRTC `control` datachannel, or the WebTransport control stream. A lost packet on Wi-Fi stalls every later input behind its retransmit.
  - **Streamer.** It writes uhid at once.
  - **Measurements.** Click-to-shown is p50 about 16 ms ([performance table](../performance-table.md)). Controller-to-game has never been measured.
- **Stadia-like gains we can get.**
  - **Input datagrams.** Send pad state as unordered, unreliable, latest-wins snapshots: WebTransport datagrams, or a WebRTC channel with `ordered:false, maxRetransmits:0`. Rumble, feature reports and Steam's settings writes stay on the reliable stream.
  - **No poll.** Forward each WebHID `inputreport` as it arrives instead of waiting for the poll.
  - **Native player.** Use Game Mode's doubled Bluetooth sampling (research [09](09-native-client-tech-2026-10.md)).
  - **Order of work.** Measure first. A spike should put a photodiode or high-speed camera on a button press with a test pattern that flips on input, over wired and Wi-Fi.

## Three designs

### A. Raw HID relay into uhid (no USB/IP)

- **Client.** It sends the controller's input reports verbatim and relays feature and output reports both ways.
  - WebHID gives reports but not the raw descriptor. The streamer would carry a **descriptor per vid:pid captured from a real controller**: a one-time `usbhid-dump` on a Linux box, which we record ourselves.
  - The native player can read the real descriptor from IOKit.
- **Streamer.** The uhid code already takes arbitrary descriptors and handles `GET_REPORT`, `SET_REPORT` and `OUTPUT`. It would create the device with the real ids and descriptor, and pass Steam's feature reports through to the real controller.
- **What it fixes.** Steam drives the real firmware (settings, lizard mode off, haptics on the trackpads) instead of our emulation, and our guessed descriptor and serial go away.
- **What it costs.**
  - Every feature report becomes a round trip to the client. Steam's start-up handshake is chatty and may time out; check its timeouts.
  - Guests on share links must stay on parsed pad state, since raw reports let a guest's device answer Steam's firmware queries.
- **What it doesn't fix.** The puck's interface check. Present every controller as wired (`1302`, no check) or Bluetooth (`1303`), whatever the user plugged in. One puck with four controllers becomes four devices.

### B. Loopback USB/IP: the streamer *is* the USB device

- **How it works.**
  - The streamer opens a `socketpair`, writes one end to `vhci_hcd`'s `attach`, and serves USB/IP on the other.
  - It answers enumeration with a device it describes: real descriptors, any interface layout, the puck's interfaces 2–5 if we want them.
  - It completes parked interrupt-IN URBs from the reports design A relays.
- **What Steam sees.** A genuine USB device: `usbhid` binds it, hidraw nodes carry real `bInterfaceNumber`, serial and USB sysfs attributes, and SDL's interface check passes.
- **No network round trips per URB.** The kernel talks to a process on the same machine, and our transport stays datagram-friendly.
- **Cost.**
  - Attach rights on the node (above).
  - A USB device-model in the streamer: configuration and interface descriptors, endpoint 0 requests, data toggles via the intercepts listed earlier.
  - The host udev and cgroup work for hidraw plus major 189.
- **The same machinery** emulates any USB device we can describe, e.g. a USB audio device or a webcam fed from the browser. That is a different and more interesting answer to goal 3 than forwarding hardware.

### C. Remote USB/IP through the streamer (real hardware, any class)

- **How it works.**
  - A native helper (Linux and Windows; macOS for non-HID) exports devices over userspace usbfs or WinUSB.
  - It tunnels USB/IP **inside our authenticated transport**: one reliable QUIC stream per device beside the control stream, gated by a media-token claim reserved for the owner.
  - The streamer terminates it and, as in B, feeds `vhci_hcd` through a `socketpair`.
- **The streamer as a validating proxy.**
  - Allowlist the vid:pid and the classes.
  - Reject isochronous unless allowed.
  - Cap lengths, `number_of_packets` and URB rates.
  - Drop malformed headers before the kernel sees them.
  - The helper pre-reads interrupt endpoints, as above.
- **Why the proxy matters.**
  - `vhci_hcd` has had memory-safety bugs driven by the exporter: CVE-2016-3955, plus 2026 reports of a heap out-of-bounds write from a server-chosen `number_of_packets` and a NULL dereference in `status` **(unverified, from search summaries; check NVD)**.
  - The kernel also binds drivers to whatever device the client claims to be, BadUSB-style.
- **Fit.**
  - This is the only design that passes *arbitrary* hardware: license dongles, flight sticks with odd firmware, USB storage.
  - Hot plugging is natural: attach, mknod and a udev entry; detach on unplug.
  - It is not available to the baseline Mac and Chrome setup for controllers.

## Prior art

- **Parsec** builds a virtual USB controller driver from client gamepad state ([Parsec](https://support.parsec.app/hc/en-us/articles/32381705301908-Setup-Gamepad)), as we do today. **Shadow** forwards controllers with a USB redirection driver ([Shadow](https://support.shadow.tech/hc/en-us/articles/32731852093201-Connecting-a-Controller-to-Shadow-PC)).
- **SPICE usbredir** is a USB-over-stream protocol independent of SPICE: LGPL-2.1+ library, client on libusb, guest side only in QEMU ([spice-space.org](https://www.spice-space.org/usbredir.html)).
  - It is the reference for design C's protocol choices. It drops isochronous packets on slow links.
  - **[I]** It probably streams interrupt-IN data rather than re-arming per URB.
  - It's tied to QEMU on the guest side, so it's relevant to our VM environments ([ADR 0020](../adr/0020-vm-environments.md)) rather than to containers.
- **VirtualHere** is the common proprietary USB-over-network tool among Moonlight users. Moonlight and Sunshine have no USB redirect.
- **Raw Gadget with `dummy_hcd`** ([xairy/raw-gadget](https://github.com/xairy/raw-gadget), Apache-2.0 with GPL-2.0 kernel parts) is another way to make a userspace USB device. It needs two extra host modules, and `dummy_hcd` has no isochronous or USB 3. Its author doesn't recommend it for production. Design B's `vhci_hcd` loopback does the same with one module.

## Proposed spikes (in order)

1. **Measure controller latency as it is**: button to photon on the RTX 4090 node, wired and Wi-Fi, Chrome and the native player. Without this we can't claim a latency win.
2. **Input datagrams:** pad state on a lossy, latest-wins channel, plus WebHID reports forwarded as they arrive. Re-measure.
3. **Raw HID relay (design A)** for the 2026 Steam Controller, wired and via the puck.
   - Capture its real descriptor first.
   - Then check against Steam: its log, `virtualgamepadinfo.txt`, Steam Input's configurator showing grips, trackpads and gyro, and haptics.
4. **Loopback USB/IP (design B)**, only if 3 shows Steam treating uhid devices differently from USB ones. Start with `vhci-hcd` loaded on the node with the owner's OK, a narrow `/sys` bind for the streamer, and a hand-built wired controller device.
5. **Remote USB/IP (design C)** as a native-client milestone: the Linux helper first, then Windows; macOS for vendor-class devices only.

## Open questions

- Does the Steam client gate on `bInterfaceNumber`, the USB serial or `bcdDevice` the way SDL does, or does it accept a Bluetooth uhid device as fully as a USB one?
- How chatty is Steam's start-up handshake with the 2026 controller, and do its timeouts survive a round trip over Wi-Fi or Tailscale?
- How many vhci ports does the node's kernel give (`nports`)? Is one `vhci_hcd` controller enough for several environments?
- How do we keep a vhci device's host drivers (`cdc_acm` on the puck, `hid-steam` on newer kernels) and desk-user udev rules off it? Match on the vhci parent path, as `72-cha-virtual-pads.rules` does for uhid.
