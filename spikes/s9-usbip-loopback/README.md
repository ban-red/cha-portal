# Spike S9: a userspace USB device through `vhci_hcd` (loopback USB/IP)

**Question.** [Design B of research note 10](../../docs/research/10-usbip-and-usb-passthrough.md): can a process on the node be the USB device? It serves USB/IP on one end of a `socketpair`; the other end goes to the kernel's `vhci_hcd` through sysfs `attach`. Does the kernel enumerate a real USB device, bind `usbhid`, and make a hidraw node that carries our input reports? What does that cost in latency and rate against `uhid`?

## What it is

`server/` is one crate (`s9-usbip-loopback`, std and `libc` only). It builds on macOS (prints "Linux only"); the Linux half is behind `cfg(target_os = "linux")`.

| Module | What |
|---|---|
| `usbip` | Sans-IO: 48-byte big-endian headers of `CMD_SUBMIT`, `RET_SUBMIT`, `CMD_UNLINK`, `RET_UNLINK`, written from the [kernel's protocol doc](https://docs.kernel.org/usb/usbip_protocol.html). No other USB/IP implementation was read or copied. |
| `device` | Sans-IO full-speed HID device: device, configuration (interface, HID, endpoints 0x81 IN and 0x01 OUT, 64 bytes, bInterval 1), string and HID report descriptors; endpoint 0 requests (GET_DESCRIPTOR, SET_ADDRESS, SET/GET_CONFIGURATION, SET/GET_INTERFACE, GET_STATUS, SET/CLEAR_FEATURE(ENDPOINT_HALT), HID GET/SET_REPORT, GET/SET_IDLE, GET/SET_PROTOCOL) all answered locally. Others stall (-EPIPE). |
| `engine` | Sans-IO: parks interrupt-IN `CMD_SUBMIT`s; a produced report completes the oldest one; with none parked, a FIFO of 4 keeps the newest reports. `CMD_UNLINK` replies -ECONNRESET if the URB was parked, 0 if not. |
| `linux` | vhci sysfs (`status` port search, `attach`, `detach`), hidraw discovery, the `uhid` backend, the bench. |

Identity: the 2026 Steam Controller wired, 28de:1302. **No real-device dump was available**: the descriptors are our assumption. The HID report descriptor is copied byte for byte from `DESCRIPTOR_TRITON` in `crates/cha-streamer/src/steam_controller.rs` (itself an assumption), so the bench uses its input report 0x45: id, a `u32` sequence, a `u64` `CLOCK_MONOTONIC` ns timestamp, zeros (46 bytes).

## Method

`--backend usbip|uhid` brings the device up, waits for a new hidraw node with HID id `0003:000028DE:00001302` whose sysfs path goes through `vhci_hcd` or `uhid`, then opens it. A reader thread (`poll` + `read`) timestamps each report on arrival. The producer thread sleeps to absolute deadlines (`clock_nanosleep`), takes the timestamp right before handing the report over, and the latency is hidraw-read minus that timestamp. Scheduling lateness of the producer is reported separately (`producer_late_us`) and is not in the latency. Both backends run the same producer and reader; the only difference is the path in between:

- usbip: `Engine::push_report` completes the parked URB, `write` on the socketpair, `vhci_rx` thread, URB giveback, `usbhid` resubmits (`CMD_SUBMIT` back to us), hidraw wakes the reader.
- uhid: `write` of `UHID_INPUT2` to `/dev/uhid`, `hid_input_report`, hidraw wakes the reader.

Produced reports are only counted once the reader has hidraw open (usbhid only submits its IN URB while the node is open); the bench waits 400 ms after opening.

## Running it

Needs `vhci-hcd` loaded (`sudo modprobe vhci-hcd`), root, and `/sys`, `/dev` access. On the node, built in the streamer dev container (`cargo build --release -p s9-usbip-loopback`), run in a throwaway privileged container:

```bash
docker run --rm --name cha-s9-bench --privileged -v /sys:/sys -v /dev:/dev \
  -v cha-streamer-dev_target:/target:ro <dev image id> \
  /target/release/s9-usbip-loopback --backend usbip --rate 1000 --seconds 10
```

Options: `--rate HZ` (1000), `--seconds S` (10), `--hold S` (stay up S seconds before the bench, to look with `lsusb`). One JSON line on stdout; the enumeration evidence is in it and on stderr.

Tests (macOS or Linux): `cargo test -p s9-usbip-loopback`.

## Results (2026-10-10, iolinux, kernel 6.17.0-41-generic)

Node otherwise idle (load average 0.1-0.3; it also runs other containers). 10 s each; lost = sent - received; no out-of-order reports.

| Backend | Rate | Sent / received | min | p50 | p95 | p99 | max |
|---|---|---|---|---|---|---|---|
| uhid | 1000 Hz | 10000 / 10000 | 4.5 µs | 7.2 µs | 49.9 µs | 57.0 µs | 173 µs |
| uhid | 1000 Hz (rerun) | 10000 / 10000 | 4.5 µs | 5.8 µs | 43.9 µs | 109.7 µs | 252 µs |
| uhid | 1000 Hz (rerun) | 10000 / 10000 | 4.6 µs | 9.0 µs | 48.5 µs | 55.9 µs | 188 µs |
| usbip | 1000 Hz | 10000 / 10000 | 11.5 µs | 14.4 µs | 83.0 µs | 96.0 µs | 230 µs |
| usbip | 1000 Hz (rerun) | 10000 / 10000 | 11.7 µs | 14.3 µs | 87.1 µs | 102.3 µs | 211 µs |
| usbip | 1000 Hz (rerun) | 10000 / 10000 | 11.0 µs | 13.6 µs | 97.8 µs | 115.9 µs | 287 µs |
| uhid | 250 Hz | 2500 / 2500 | 4.7 µs | 84.2 µs | 185.8 µs | 204.8 µs | 305 µs |
| uhid | 250 Hz (rerun) | 2500 / 2500 | 4.7 µs | 81.4 µs | 154.3 µs | 186.8 µs | 286 µs |
| usbip | 250 Hz | 2500 / 2500 | 12.5 µs | 112.8 µs | 216.4 µs | 294.6 µs | 364 µs |
| usbip | 250 Hz (rerun) | 2500 / 2500 | 11.6 µs | 93.8 µs | 234.0 µs | 266.9 µs | 367 µs |

- **Rate:** every report arrived at both rates, on both backends. The per-report cycle (report out, kernel resubmits the URB, we park it) never ran dry at 1000 Hz, because the URB is resubmitted within about 100 µs, well inside 1 ms. With no parked URB the FIFO of 4 would have covered a gap; it was not needed to avoid loss here (not measured separately).
- **Cost against uhid:** about +7 µs at the minimum and +7 µs at p50 at 1000 Hz (14 against 6-9 µs), about +40 µs at p95/p99. At 250 Hz the medians are 94-113 µs against 81-84 µs, within the spread between reruns. The 250 Hz p50 is higher than the 1000 Hz one for both backends: the idle CPU is slow to wake the reader between sparse reports. That is a property of the machine, not of either backend.
- **Not the bottleneck:** producer wake-up lateness (p99 92-95 µs at 1000 Hz, 290-320 µs at 250 Hz) is of the same size as the latency tails. The tails mostly measure scheduling.

### Enumeration evidence

`lsusb -t` on the node while attached:

```
/:  Bus 009.Port 001: Dev 001, Class=root_hub, Driver=vhci_hcd/8p, 480M
    |__ Port 001: Dev 002, If 0, Class=Human Interface Device, Driver=usbhid, 12M
```

`lsusb -d 28de:1302 -v` (as the unprivileged user): `Bus 009 Device 002: ID 28de:1302 Valve Software Steam Controller`, `Negotiated speed: Full Speed (12Mbps)`, `iSerial 3 S9LOOP0001`, the configuration (`wTotalLength 0x0029`, one interface, class 3), the HID descriptor (`wDescriptorLength 63`) and EP 1 IN, interrupt, 64 bytes, bInterval 1. The report descriptor shows "UNAVAILABLE" only because `lsusb` could not open the device unprivileged.

sysfs (from the program): hidraw `/dev/hidraw0`, HID device `/sys/devices/platform/vhci_hcd.0/usb9/9-1/9-1:1.0/0003:28DE:1302.0011`, interface driver `usbhid`, `bInterfaceNumber 00`, `bInterfaceClass 03`, manufacturer "Valve Software", product "Steam Controller", serial `S9LOOP0001`, speed 12, `bConfigurationValue 1`. `vhci_hcd` `status`: `hs 0000 006 002 00010002 000004 9-1` while attached (sta 006 = in use, spd 002 = full speed), back to `004` after.

dmesg (attach, bind, detach; no warnings or oopses in the tail):

```
vhci_hcd vhci_hcd.0: devid(65538) speed(2) speed_str(full-speed)
vhci_hcd vhci_hcd.0: Device attached
usb 9-1: new full-speed USB device number 7 using vhci_hcd
usb 9-1: SetAddress Request (7) to port 0
usb 9-1: New USB device found, idVendor=28de, idProduct=1302, bcdDevice= 1.00
usb 9-1: Product: Steam Controller / Manufacturer: Valve Software / SerialNumber: S9LOOP0001
hid-generic 0003:28DE:1302.001A: hiddev0,hidraw0: USB HID v1.11 Device [Valve Software Steam Controller] on usb-vhci_hcd.0-1/input0
vhci_hcd: unlink->seqnum 38102 / urb->status -104     (our RET_UNLINK on shutdown)
vhci_hcd: stop threads / release socket / disconnect device
usb 9-1: USB disconnect, device number 7
```

`vhci_hcd: vhci_device speed not set` appears twice during enumeration, before the first control URBs; enumeration went on regardless.

## What this shows, and what it doesn't

- **Shown:** design B works on this kernel with ~600 lines and no extra module besides `vhci-hcd`: a real full-speed USB device with serial and interface number, bound by `usbhid` (through `hid-generic`), a hidraw node, 100 % delivery at 250 and 1000 Hz, clean detach. The cost over uhid is small (about 7 µs at the median, tens of µs in the tails).
- **Not verified:** a real Steam Controller descriptor (no dump); Steam or SDL's behaviour on this device (the research note's reason for design B); `hid-steam` binding (the kernel bound `hid-generic` for 28de:1302 here); OUT reports and feature requests from a real client (control requests are unit-tested, and enumeration exercises the standard ones); other transfer types; behaviour under CPU load or with the streamer's other work running; many attach/detach cycles; permissions without a privileged container (the `attach` file needs root, which the research note lists as a cost); hot-unplug while a reader is blocked.
- **Pitfall as feared, not hit:** the data-toggle desync pitfall did not appear, because the SET_CONFIGURATION / SET_INTERFACE / CLEAR_HALT requests are answered with success as the note says.
