# cha-streamer

One environment's media engine (plan §2.2, [ADR 0004](../../docs/adr/0004-own-engine-no-wolf.md)). The app runs as a client of our own headless Wayland compositor. Composited frames go to NVENC zero-copy through our own binding ([`cha-nvenc`](../cha-nvenc)), or to PyroWave on the LAN ([`cha-pyrowave`](../cha-pyrowave)), then to the browser over WebRTC (str0m) or WebTransport. Sound comes in through our own PulseAudio-protocol server and goes out as Opus. Keyboard and mouse come back into the compositor's seat; gamepads become virtual Xbox 360 controllers. There is no GStreamer, FFmpeg, PulseAudio, PipeWire or Wolf.

```
app ─Wayland─▶ compositor (Smithay, GLES on the render node)
                 │ 240 Hz: frame callbacks + presentation feedback → apps draw at 240 fps
                 │  60 Hz: composite (only if something changed) into a GBM buffer
                 ▼
        output pool (4 GBM buffers, each registered with CUDA once)
                 │ newest-frame-wins mailbox per codec
                 ▼
        encoder thread per codec (NVENC: P1 / ultra-low-latency, CBR, 1-frame VBV,
                 │                 infinite GOP, zero reorder delay)
                 ▼
        WebRTC session (str0m, playout-delay 0) ──▶ browser
                 ▲
                 └── input (control DataChannel) → the seat: pointer, relative motion, buttons, wheel, keys
```

## How it works

- **Two clocks.**
  - A timerfd ticks at the compositor rate (`--compositor-fps`, default 240). Every tick sends frame callbacks and presentation feedback, so apps draw at that rate. S2 showed this halves click → screen latency.
  - Only every encode tick (`--fps`, default 60) composites, and only if something changed. Nothing is composited that won't be encoded.
  - Chrome paces itself to presentation feedback, not frame callbacks: with feedback at 60 Hz it drew at 60 fps even with 240 Hz callbacks.
- **Zero-copy.**
  - The output pool is four GBM buffers in the GPU's own tiling (`ARGB8888`).
  - Each buffer is registered once with CUDA (an EGL image), and once with NVENC per encoder. Encoding a frame maps and encodes it; nothing is copied.
  - A buffer is reused when every encoder has dropped its frame.
- **Encoders.**
  - One thread per codec, started by the first viewer of that codec and kept warm afterwards.
  - A one-frame mailbox means a slow encoder skips frames instead of queueing them.
  - A keyframe request with nothing new on screen re-encodes the last frame.
  - On resize the session is reconfigured in place, up to 3840×2160.
- **Window policy (kiosk).** Every toplevel fills the output: maximized, or fullscreen when it asks. Decorations are server-side, i.e. none. Dialogs are centered. Popups get proper grabs.
- **Protocols:**
  - compositor, xdg-shell, xdg-decoration;
  - shm, linux-dmabuf v5 with feedback naming the render node;
  - seat (keyboard, pointer), relative-pointer, pointer-constraints;
  - data-device, primary-selection;
  - viewporter, presentation-time, single-pixel-buffer, xdg-output.

  Xwayland comes with XFCE in P1.4.

## WebTransport (P2.3)

The Chromium fast path, `cha-stream/1` (plan §3.1), beside WebRTC on its own UDP port (`--wt-port`):

- **Media:** each encoded frame goes out as datagrams with `cha-proto`'s 16-byte header (frame id, fragment index and count, keyframe flag, send time); Opus frames one datagram each. The player reassembles them in a worker and decodes with WebCodecs into track generators feeding `<video>` and `<audio>`, the fastest path to the screen in S1d.
- **Control:** the session's first bidirectional stream, with the same JSON lines as WebRTC's DataChannel (input, pings, resize), plus `keyframe` and `rfi` requests.
- **No silent eviction:** a frame QUIC's send buffer can't take whole isn't sent, and nothing after it is until a keyframe or a recovery frame (asked for at once, as under RFI below). The page also asks when a frame can't be completed or a gap shows.
- **Congestion:** our own rate control and FEC (P2.5, below).
- **Certificate:** self-signed, 13 days, ECDSA P-256; browsers accept it by its SHA-256 (`serverCertificateHashes`), which the portal hands out with the URLs (one per node address, each carrying the media token).
- **Several viewers** (P2.6, below): WebTransport sessions coexist on the one endpoint.
- **Codec switches in place** (`{"t":"codec","codec":…}` on the control stream). The session subscribes to the other encoder and keeps sending the current stream until that encoder's first frame. From there it sends the new codec under the next `stream` number (the header's stream byte), and the page starts that stream afresh. Answered with `{"t":"codec","codec":…,"stream":…}`, or an `error`. In the app's browser, every switch among H.264, HEVC, AV1 and both PyroWave modes left the picture without a gap over 35 ms, cold encoders included (a reconnect costs a new handshake and a blank picture).
- **Stats** carry composited → encoded and encoded → sent percentiles per report window, so a codec's cost shows within half a second of switching to it.
- **First numbers** (the app's browser, which wasn't painting, so no click → screen yet): H.264 at 1616×1256 decodes in 1.5–1.6 ms with nothing lost; click → sound 55 ms p50, against 85–90 over WebRTC (no NetEq buffer).

## Rate control and FEC (P2.5)

The WAN tier over WebTransport (plan §3.1 rules 1, 2 and 6). Spike S7 ([`spikes/s7-wan`](../../spikes/s7-wan/README.md)) drives it through netem.

- **Receiver truth.** Every 100 ms the page's worker reports:
  - how long frames took from send to complete there (the median of the last 150 ms, clock-synced);
  - what arrived (the last 200 ms);
  - the share of datagrams, data and parity, that never did (the last second).

  QUIC's RTT and loss count stand in only until the first report. Its RTT lags by seconds on a draining queue, and it counts reordered packets as lost.
- **Rate control** (`rate.rs`), every 50 ms, delay first and loss last:
  - **Cut:** a queue over 40 ms (delay growth over its 10 s minimum, or our own send queue) cuts the rate to 0.85 of what arrived. No second cut until frames sent after the first can have reported: the queue plus 300 ms, or 1.2 s for loss alone (the page counts an overflow's loss that long).
  - **Back off:** over 15 ms, 10 % down.
  - **Hold:** at 2–10 % loss. Random loss isn't congestion; FEC covers it.
  - **Climb:** after a calm second, 50 % a second, or 10 % near where the path last pushed back. Never past twice what goes out: NVENC's low-latency CBR makes about two thirds of its target, and a target far above what's sent would let the next busy picture burst into the bottleneck.
  - **Longer path:** a "queue" that holds while the rate falls by a third for 2 s is a longer path, and the delay floor moves up to it.
  - **Stall:** next to nothing arriving, or the reports stopping, is mostly a busy browser, so it's waited out for up to 400 ms. What arrives after it is a burst whose delays show the queue, and a cut goes to what arrives then.
- **Encoder.** The target goes to NVENC in place, without an IDR (`nvEncReconfigureEncoder`), when it moves by more than 1/32. Video gets 92 % of the rate, less what parity takes.
  - **Hold:** while more than 1.5 frames wait in QUIC's send buffer, the encoder holds, skipping frames rather than queueing them. A subscriber held for over a second (a stuck page) stops holding the shared encoder.
  - **Keyframes:** a keyframe re-encode happens at most once per frame interval.
- **Congestion window** (`congestion.rs`): our own quinn controller, a fixed 8 MB window that ignores loss, since rate control paces the media. Cubic halved its window at every random loss and starved the stream at 1 % loss.
- **FEC** (`cha-proto::fec`): systematic Reed-Solomon over GF(2⁸) with a Cauchy matrix, in blocks of up to 128 data fragments.
  - **How much:** per block (header byte 3), the least parity that keeps a frame's odds of being lost under 10⁻⁴ (10⁻⁶ for keyframes and recovery frames, the binomial tail), and never more than half its data.
  - **For what loss:** 1.5× the loss measured while the path was calm, at least 0.3 %, held for 5 s after the last loss. A queue's overflow is rate control's to fix; parity would only add to it.
  - **Keyframes and recovery frames** always have parity for at least 0.3 % loss: one is mostly asked for because something was lost, and a resync point has to arrive. Before recovery frames had it, one sent at the onset of loss was often lost itself.
  - **Wire:** parity datagrams, flagged `PARITY` with indices after the data, carry the frame's length and a shard. The page rebuilds a frame as soon as any k of a block's k + m shards are in.
- **Resync (RFI).** A lost frame costs a P-frame instead of a keyframe: the next frame refers around it.
  - **Request:** the page's worker sends `{"t":"rfi","id":N}` on the control stream, N being the first lost frame's id. The session maps it to the encoder's frame index (a ring of the last 256 frames it sent), records it as the page's loss point, and asks the encoder to invalidate from there (`Media::request_invalidate`).
  - **Encoder:** `nvEncInvalidateRefFrames` for each frame from the loss on; the DPB holds 8 frames (`cha-nvenc`'s `DPB_FRAMES`). A loss more than 7 frames back, or before the last keyframe, gets a keyframe instead. With nothing new on screen, the last frame is encoded again around the loss. Frames the sender drops itself (QUIC's buffer full) take the same path rather than always a keyframe.
  - **Flag:** the next frame is `RECOVERY` for every session whose loss point is at or after the invalidation point; the others get it as a normal frame. It has keyframe-grade parity.
  - **Page:** the worker drops frames until a keyframe or a `RECOVERY` frame that answers its loss. It asks again at once if that frame is itself lost (overtaken by later frames, or silent for 250 ms), and asks for a keyframe if nothing came 250 ms after asking.
  - **Log:** the encoder's 10 s line has `recoveries=` and `rfi_keyframes=` (the fallbacks).
  - **Checked** in S8 ([`spikes/s8-rfi`](../../spikes/s8-rfi/README.md)): H.264, HEVC and AV1 decode bit-exactly from a recovery frame, in hardware and software. A recovery frame costs what a P-frame does, a keyframe 2–3 times as much.
  - **S7, HEVC** (in-order jitter): the gap at a 1 % loss onset is ~120 ms (234 ms with keyframes), 117–120 ms at the WAN scenario's onset (131), with no keyframe fallbacks. The drop is still a keyframe (294 ms, against 255): the overflow loses more frames than the DPB reaches back by the time the request arrives. A first version, with no parity on recovery frames and no immediate re-ask, did worse than keyframes at the onset (340–380 ms): the unprotected recovery frames were lost, and the page fell back to keyframes 250 ms later.
  - **WebRTC** keeps PLI and keyframes: Chrome's receiver has no RFI message, and NACK recovers most losses.
- **Path MTU.** When QUIC lowers its datagram size (a black-hole fallback), the next frame is cut to the new size.

**S7 results** (HEVC unless noted; test pattern with 20 % noise, 60 fps at the bench page's size, about 1792×1008; [JSON](../../docs/benchmarks/s7-2026-10-04-electron152-gpu-node-wan.json)):

| Scenario | Link | Result |
|---|---|---|
| WAN | 25 Mbit/s, 30 ± 5 ms, 0.5 % loss | 60.8 fps at 21.7 Mbit/s; after the first 3 s, 3 frames lost and the longest gap 51 ms. AV1: 60.3 fps at 21.5 Mbit/s, longest gap 25 ms after the onset |
| Loss | 20 ms; 1 %, then 3 % | 3 %: nothing lost, 693 frames rebuilt, longest gap 18 ms. 1 %: 8 frames lost at the onset, none after |
| Drop | 60 → 10 → 60 Mbit/s, 20 ms | 8.5 Mbit/s on the 10; one 255 ms gap at the drop; back over 20 Mbit/s 6 s after the link |

Before P2.5, 3 % loss brought the stream down to 10 fps and then 1, and a drop to 10 Mbit/s froze it for good.

**WebRTC** uses the same rate control, with the page's reports coming over the control DataChannel. No FEC; Chrome's NACKs and retransmissions recover losses.
- **Send times** come from each frame's RTP timestamp, the encode time on the session clock. The `sent` messages share the DataChannel, whose retransmissions hold them back under loss, just when the delay matters.
- **Delay:** the page reports the lower quartile of presented frames' send → complete (`receiveTime`), not the median. A frame that needed a retransmission completes a round trip late, and at a few percent loss most frames do; a queue delays them all.
- **Updates:** only a report with a delay updates the rate. A page that presents no frames (hidden) holds it.
- **Resync bound:** when frames stop decoding for 300 ms while data still arrives, the page asks for a keyframe. Otherwise Chrome waits seconds on retransmissions a congested path doesn't deliver.
- **Not str0m's GCC** (S7): it paces media at 1.1× its estimate (~14 ms per frame on a LAN), and its estimate stalls at 1.5× what NVENC's undershooting CBR sends.
- **S7, WebRTC, HEVC:**
  - unshaped: 24 Mbit/s at 60 fps, as before;
  - WAN (in-order jitter): 59.4 fps at 17 Mbit/s, no freezes;
  - 3 % loss: 58 fps at 14.6 Mbit/s, no freezes;
  - the drop: one 0.5 s freeze, back to full rate 5 s after the link.

**Open:**
- A loss further back than the DPB (a queue's overflow, as at the drop) still costs a keyframe. Intra-refresh isn't pursued: it feeds frames past a loss, which H.264 and AV1 decoders show as garbage until it heals, and RFI recovers exactly (S8).
- Loss starting before parity: the first ~300 ms of it costs frames.

## PyroWave (P2.4)

The LAN tier (plan §3.2–3.3): Themaister's wavelet codec, intra-only, a few tenths of a millisecond of GPU work per frame, at hundreds of Mbit/s. WebTransport only. Offered as `pyrowave420` (games) and `pyrowave444` (desktops) when libpyrowave loads and the render node's PCI ids are known; the player offers it where WebGPU has subgroups.

- **Encoder** ([`cha-pyrowave`](../cha-pyrowave), our binding to `libpyrowave-shared`, loaded at run time): its own Vulkan device on the same GPU, picked by PCI vendor and device id. One device per streamer, shared by both modes (their calls take turns), made in the background at start-up. Making one takes ~0.4–0.6 s and stalls the GPU's other work (NVENC, the compositor) for ~0.2 s, which a first switch to PyroWave mid-session would show as a freeze. The warm device costs ~57 MiB of VRAM (an idle streamer: 394 → 451 MiB); leave PyroWave out of `--codecs` to skip it. Each output buffer is imported once as a dma-buf with its DRM modifier. The encoder takes RGB and converts to YCbCr on the GPU, then splits the frame into packets of about 1100 bytes that each decode on their own.
- **Modifiers.** NVIDIA's GL picks compressed modifiers that its Vulkan driver can't import, so the output pool leaves those out (they gain nothing for a buffer that is read once).
- **Budget:** `--pyrowave-mbps` (default 290) for 4:2:0 at 1440p and `--fps`, scaled with the picture's area; 4:4:4 gets twice as much.
- **No keyframes.** Every frame stands alone, so a lost datagram blurs its region of one frame and nothing waits for a resync. When the screen goes still, the last frame is sent once more after 250 ms (a heal), so a loss doesn't stay on screen.
- **Wire.** One PyroWave packet per datagram. Its blocks are atomic, so a packet can come out larger than asked; it is then split across datagrams flagged `CONTINUES` (more follow) and `CONTINUED` (a tail). The player assembles whole packets, decodes everything that arrived by the 60 ms deadline, and drops older frames.
- **Browser:** `@cha/pyrowave-webgpu` decodes into an offscreen WebGPU canvas; each frame becomes a VideoFrame for the same track generator and `<video>` as the hardware codecs, so presentation, stats and the probe work alike.
- **Failures are visible.** If the encoder can't start, the session closes with the reason, and the next viewer gets a fresh try. `cha-node --doctor` makes the device the way a streamer does (`cha-streamer --probe-pyrowave`). NVIDIA's Vulkan driver (`libGLX_nvidia`) links libX11 and libXext, so the image carries both.

**First numbers** (RTX 4090, 1 GbE, 2026-10-04):

| | 4:2:0 | 4:4:4 |
|---|---|---|
| 2560×1440, raw WebTransport session from the app's browser | 267 Mbit/s, ~650 datagrams per frame, every frame complete | 574 Mbit/s at 58.5 fps, 702 of 703 frames complete |
| 1920×1440 through the portal, decoded and shown in the app's browser | 218–221 Mbit/s at 60 fps; 12 frames lost at start (the resize), none after | 442 Mbit/s at 60 fps, none lost |

- GPU work per frame: 0.14 ms at 1920×1440 4:2:0 (scale, DWT, quantize, analyze, resolve, pack), about 0.22 ms at 1440p.
- Wall clock per encode: 2.5 ms p50, 2.7–3.0 ms p99, while a ComfyUI job held the GPU. Of that, recording and submitting take 0.14 ms and packetizing 0.05 ms. The other 2.3 ms is waiting for the GPU: one full timeslice of the other job's, every frame, since each encode is submitted just after the job gets the GPU back. NVENC under the same load waits 1.8–3.8 ms p50 and up to 5.4 ms p99, since its RGB input conversion runs on the same shaders. S1e's idle-GPU figure for the whole wait plus readback was 0.4 ms. See *Shared GPUs* below.
- The decode call in the browser: 0.35–0.5 ms (CPU side; the GPU work isn't timed yet).
- Send → shown and click → screen need a painted browser (the app's pane mostly wasn't).

## Shared GPUs

A homelab GPU often runs other work (image generation, LLMs). NVIDIA time-slices the GPU between processes, so whenever another process has work queued, the compositor's render and each encode wait for that process's timeslice to end: up to ~2.3 ms each on the RTX 4090 at the default timeslice. Fewer GPU contexts per frame mean fewer waits. Sharing one Vulkan device between the compositor and PyroWave (plan §3.3) would make it one.

- **The owner's lever:** a shorter compute timeslice, `sudo nvidia-smi compute-policy --set-timeslice=1` (SHORT; 0 restores the default). It costs the other jobs some throughput, and it doesn't persist across reboots.
- **Queue priority:** PyroWave can ask for a high-priority (async compute) queue, but on driver 595.71 the first encode on it never completes, so we stay at the default priority until that's understood.

## Sound (P1.6)

```
app ─libpulse─▶ our PulseAudio-protocol server ($XDG_RUNTIME_DIR/pulse/native, sink "cha")
                 │ per stream: decode, downmix to stereo, resample to 48 kHz (polyphase sinc)
                 ▼
        mixer thread, 10 ms clock ── Opus (libopus, our binding; CELT-only, 128 kbit/s VBR)
                 ▼
        audio track (its own MediaStream: no lip-sync hold on video) ──▶ browser (NetEq at its minimum)
```

- **Why a server of our own.** Every Linux audio stack speaks the PulseAudio protocol: libpulse, PipeWire's and SDL's backends, Chrome, Firefox, Wine. One small server replaces PulseAudio or PipeWire in the container, and the samples go straight to our mixer. The `pulseaudio` crate (MIT) supplies the wire format; the server is ours (`src/audio/pulse.rs`).
- **What it has.** One sink, playback streams, volumes and mutes, subscriptions (so mixers like pavucontrol work), exact latency and timing replies. No sources (microphones), sample cache or modules yet. Samples arrive over the socket; there is no shared memory.
- **Flow control** follows PulseAudio's: a stream is kept `tlength` ahead (REQUEST), starts once `prebuf` is queued (STARTED) and stops again when it runs dry (UNDERFLOW). A stream that leaves `tlength` to the server gets 60 ms, not PulseAudio's 2 s. Nothing gets less than 20 ms (two mixer ticks).
- **The clock runs with or without a viewer.** Apps, and media players that pace video by audio, need a sink that consumes. Encoding happens only while a session listens.
- `--no-audio` turns it all off.

## Viewers (P2.6)

Up to four sessions watch an environment at once (`viewers.rs`, plan §3.4). Viewers with the same codec share its encoder. WebTransport sessions coexist; WebRTC ones go one at a time, since each binds the environment's WebRTC port.

- **One has the controls (the floor):** its keyboard, mouse, gamepads, resizes, cursor mode and clipboard reach the environment, and only it gets the apps' clipboard. The others watch: what they send is ignored.
- **Who:** the newest owner session takes the floor on joining; an admin's only when no owner is watching. When the controller leaves, the newest session of the highest role that may control gets it. Owners and admins can take it (`{"t":"take_control"}`); viewers can't. Share links and their roles are Phase 3.
- **Pages hear** `{"t":"floor","control","viewers"}` on every change. A watching page gets `{"t":"pointer","x","y","drawn"}` at encode ticks, and draws the controller's pointer over the picture when the controller's page draws its own cursor (`drawn` false).
- Tested with an owner and an admin on one Chrome environment: the admin joined watching ("2 watching", the owner's pointer drawn where it was), took the controls, the owner took them back, and the admin's leaving left the owner in control. Both shared one HEVC encoder.

## Clipboard (P2.6)

Text both ways, on the control channel (`{"t":"clipboard","text":…}`), up to 1 MiB:

- **Environment → browser:** when an app sets the clipboard, the compositor asks it for the text (UTF-8 first) at the next tick. Smithay reports a selection before storing it. A thread reads the pipe, and the text is published to the sessions, which send it to the page. The player writes it to the device's clipboard, or on the next click if the browser wants a gesture.
- **Browser → environment:** the page sends its clipboard just before a paste shortcut's keys, so the paste uses it. The text becomes the compositor's own selection, written on a thread to each app that pastes.
- **Wayland apps** (Chrome, Firefox) take part directly.
- **X11 apps** (XFCE, under rootful Xwayland; Steam, under gamescope's Xwayland, which doesn't pass the clipboard through) keep their clipboard inside the X server, which only the app's container reaches. The `xfce` and `steam` images run a helper, `cha-x11-clipboard` ([`crates/cha-x11-clipboard`](../cha-x11-clipboard), x11rb), as the desktop's user. It talks to the streamer over a Unix socket in the shared volume (`/run/cha/clipboard`, owned by the app's uid; `src/x11_clipboard.rs`), in `cha_proto::clipboard`'s frames: kind, sequence number, length, UTF-8 text.
  - **X → page:** the helper watches CLIPBOARD's owner (XFixes). On a new one it reads `TARGETS`, then the text as `UTF8_STRING` (or Latin-1 `STRING`), INCR included, up to 1 MiB, and sends a `copied` frame. The streamer publishes it where Wayland copies go, so the sessions forward it to the page. Every copy goes up, the same text again too, since the device's clipboard may have changed since; the helper's own taking of the clipboard (for the page's text) isn't a copy, so the page's text doesn't echo back.
  - **Page → X:** a `set` frame makes the helper own CLIPBOARD (with a server timestamp, checked afterwards), then answer `ack`. It serves `TARGETS`, `TIMESTAMP`, UTF-8 text (`UTF8_STRING`, `TEXT`, `text/plain;charset=utf-8`, `text/plain`) and Latin-1 `STRING` (`?` for what it lacks), by INCR in 64 KiB chunks past the server's request limit. `Media::set_clipboard` waits for the `ack` (30 ms at most; not at all with no helper connected) before returning, so the paste's V key, which follows on the same ordered channel, reaches Xwayland after the X selection is owned. The wait is on the session's task, never the compositor's.
  - Plain threads: one accepts, each helper has a reader and a writer. The helper reconnects every second if the streamer restarts, and exits with the X server. Only CLIPBOARD is bridged, not PRIMARY (Wayland apps' isn't either).
  - `cha-x11-clipboard --get` and `--put TEXT` are an ordinary X client's reading and owning, for tests and ops.
- Tested in XFCE end to end through the portal: copies from xfce4-terminal and Thunar reach the page, and the page's text pastes into both, 293 KB (by INCR both ways) and accents, CJK and emoji included, byte-exact.
- Tested in the Chrome environment: text with accents and emoji pasted into the omnibox from the page, then copied back, each exactly once. The helper is tested against a real X server (Xvfb) and a pretend streamer: copies both ways, accents and emoji, INCR both ways (300 KB), refusal over 1 MiB, and `xclip`'s reads of every target and its Latin-1 `STRING` ownership.

## Cursor (P2.6)

In desktop mode the page draws the cursor (`{"t":"cursor","client":true}`), so it moves with no stream delay. The compositor then leaves it out of the picture, and pointer motion over still content no longer costs a frame: 60 moves across a static Chrome page gave 2 frames, where each move used to be composited and encoded. With a locked pointer (games) the page asks for it back in the picture; each session starts that way.

- **Shapes** go out on change as `{"t":"cursor"}`. A named cursor is a CSS keyword: we implement the cursor-shape protocol, whose names are CSS's, and Chrome and GTK 4 use it, so `text` over a text field costs a few bytes. Apps that draw their own cursor surface (Xwayland, GTK 3, Qt) send the image as straight-alpha RGBA, base64, once per session per image (later only its `id`); larger than 256 px goes as `default`.
- **The page** sets the element's CSS cursor. Images go through `image-set()` at the stream's scale, so a 30 px cursor in a 2× stream shows at 15 CSS px, crisp.
- Tested: Chrome's cursor-shape names (`default`, `text` over the omnibox), and XFCE's arrow image through rootful Xwayland.

## Setup status (P2.1)

What an app's long first-run setup is doing, for the pages to show while the picture is still black (Steam's first launch downloads ~500 MB). The contract is in [`images/README.md`](../../images/README.md): the app replaces `/run/cha/status`, one JSON object, atomically.

- **Reading.** A plain thread (`src/status.rs`) reads the file every 250 ms: an open and a few dozen bytes, with no inotify binding to maintain. `label` is required (the rest is optional: `done`, `total`, `unit`); `done` is held to `total`, long text is cut, and a missing file, `{}` or no label clears the status. A file that doesn't parse or is over 4 KiB changes nothing, so an app that doesn't write atomically costs at most a moment's old status. It is published on a `watch` channel, like the clipboard.
- **Sending.** Every session, WebRTC and WebTransport alike, sends `{"t":"status","label","done","total","unit"}` when its control channel opens (if there is a status) and on each change, and `{"t":"status"}` with no label once it is cleared. All viewers get it, not only the controller. The player's `onStatus` and the portal's progress notice take it from there.

## Gamepads (P1.6)

- The page sends each pad's Gamepad API state (standard mapping) when it changes. The streamer turns each pad into a **virtual Xbox 360 controller** through `/dev/uinput` (045e:028e, the xpad driver's ranges), the pad SDL, Steam and Wine know best.
- The kernel creates the devices on the host, so the app gets them through two volumes the streamer fills (`--input-dir`): the `eventN`/`jsN` nodes (the app's `/dev/input`) and udev database entries marking them joysticks (the app's `/run/udev`; Chrome, Firefox and Wine find pads through udev). The app's device cgroup allows input devices; these are the only ones in its `/dev/input`.
- Hotplug events don't reach the app's network namespace, so `--gamepads` (default 1) are made at start and kept. SDL is told to skip udev (`SDL_JOYSTICK_DISABLE_UDEV=1` in the base image) and watches `/dev/input`, so it sees later pads too.
- **Extra pad state.** The `pad` message may also carry `ty` (the family the page recognized, not `t`, which is the control envelope's type: `xbox`, `playstation`, `switch`, `steam`, `generic`), `gyro` (rad/s), `accel` (m/s²), `touch` (`[{id,x,y,down}]`, 0..1) and `bat` (0..1). They are optional and kept on `PadState`; the Xbox 360 pad has no use for them, and a malformed one is ignored rather than costing the buttons. A DualSense or Steam Controller device over uhid will use them.
- **Rumble.** The pads advertise force feedback (`FF_RUMBLE`, `FF_PERIODIC` with sine, square and triangle, `FF_GAIN`; 16 effects, as the xpad driver's memless layer does). A thread per pad answers the kernel's uinput requests: `UI_FF_UPLOAD` and `UI_FF_ERASE` (the app's ioctl waits for the answer), and turns the app's play/stop events into the motors' drive (the playing effects added up, scaled by the gain; periodic effects drive both motors from their magnitude). Sessions forward it as `{"t":"rumble","i","lo","hi","ms"}` (`lo` the strong motor, `hi` the weak one, 0..1; `ms` 0 stops) to the controller only, both on WebRTC and WebTransport, at most ~60 per second per pad (the newest wins; a stop is never held back). An effect with no length plays until stopped and is announced for 60 s.

## Running it (node, dev loop)

The dev container builds and runs the streamer from the synced repository. The GPU comes from CDI, and the container uses host networking.

```bash
docker compose -f deploy/streamer/compose.dev.yaml up -d --build
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev cargo build --release -p cha-streamer
```

```bash
docker compose -f deploy/streamer/compose.dev.yaml exec dev /target/release/cha-streamer --run 'google-chrome-stable --no-sandbox --ozone-platform=wayland --no-first-run --force-device-scale-factor=1 --user-data-dir=/tmp/chrome-profile --kiosk file:///src/spikes/s2-compositor/bench/page/live.html'
```

- Chrome runs with `--no-sandbox` only because the dev container runs as root. The environment images in P1.4 run apps unprivileged.
- Signalling is on TCP 4495 and WebRTC on UDP 4496.
- On its own like this, starting a stream needs the token printed at startup, kept in `/state/token`. Measure with the S1c/S1d page, as for S2: host `gpu-node.lan`, port 4495, the token, the WebRTC present path. The streams are `live-hevc`, `live-h264` and `live-av1`.

## Run by the agent (P1.5)

The agent starts one streamer container per environment, with signalling on `127.0.0.1` only: `--listen 127.0.0.1 --portal-key <key> --environment-id <id>`.

- **Brokering.** A browser's offer reaches the streamer through the portal and the agent. It carries a **media token**: Ed25519, signed by the portal, valid for 60 s, for this environment only. The streamer checks it offline against the portal's key.
- **Reconnects.** One viewer at a time. A new connection takes over: the old session stops first, which frees its UDP port. A reconnect gets a fresh keyframe, and the environment keeps running in between.
- **Interactive sessions** have no time limit (`secs=0`). They end when the browser leaves or another connection takes over.
- **Keyboard focus.** A new window gets the keyboard once it first shows a buffer. Chrome ignores a keyboard `enter` for a surface it hasn't drawn, and focusing the same surface again later sends nothing.

## Results (RTX 4090, Chrome 154 in the compositor, 2026-10-03/04)

**Browser side** (Chrome 154 on the M4 Pro, 1 GbE, the S1c/S1d page's WebRTC path with its click probe, 2026-10-04; raw data: [`docs/benchmarks/p13-2026-10-04-m4pro-chrome154-gpu-node.json`](../../docs/benchmarks/p13-2026-10-04-m4pro-chrome154-gpu-node.json)). All times are ms, p50 / p95; S2 M2 for comparison:

| Stage | HEVC | H.264 | AV1 | S2 M2 (HEVC / H.264) |
|---|---|---|---|---|
| Composited → encoded (node, p50 / p99) | 1.77 / 4.49 | 2.01 / 5.25 | 2.08 / 2.54 | 1.85 / 1.89 |
| Sent → browser compositor | 3.86 / 7.31 | 4.47 / 8.13 | 6.10 / 10.2 | 4.15 / 3.84 |
| **Node compositor → Mac compositor** (sum of p50s) | **≈ 5.65** | ≈ 6.5 | ≈ 8.2 | ≈ 6.0 / ≈ 5.8 |
| Browser decode only | 1.90 / 2.20 | 2.50 / 3.00 | 3.80 / 4.60 | |
| **Click → browser compositor** | **24.6** / 40.4 | 27.4 / 44.1 | 24.2 / 40.6 | 34–35 (240/60) |
| … of which Chrome reacting in our compositor | 17.9 / 33.9 | 20.3 / 37.0 | 19.4 / 36.4 | 29 |
| Frames lost / dropped | 0 / 0 | 0 / 0 | 1 lost (NACK'd, 1 PLI) | |

- **About 6 ms from the node's screen to the Mac's, and ~25 ms from click to screen.** Click → screen is 10 ms faster than S2 at the same 240 Hz / 60 fps. Chrome draws to presentation feedback sent every compositor tick, and each composite takes the newest buffer it committed.
- **HEVC is the fastest on this client.** H.264 decodes ~0.6 ms slower in Chrome on the M4, and AV1 ~1.9 ms slower.

**Node side, A/B against S2's GStreamer streamer (`gst-wayland-display` → `nvh265enc`).**
- Both ran back to back with Chrome at 240 fps on S2's live page, at 2560×1440, 60 fps, 40 Mbit/s.
- The GPU was shared with the owner's ComfyUI and llama.cpp, at up to 100% utilization and 346 W. That load sets the tail latency, so only back-to-back runs compare.
- Raw data: [`docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json`](../../docs/benchmarks/p13-2026-10-03-gpu-node-rtx4090-node.json).

| HEVC, compositor → encoded (ms) | p50 | p99 |
|---|---|---|
| S2 (GStreamer), S2 M1 on a quiet GPU | 1.72 | 1.83 |
| **cha-streamer**, GPU busy | **1.75** | 4.25 |
| S2 (GStreamer), GPU busy (straight after) | 1.78 | 4.32 |

**Where the time goes** (cha-streamer, HEVC, µs, p50 / p99), from the encoder thread's breakdown:

| Stage | p50 | p99 |
|---|---|---|
| composited → picked up | 16 | 686 |
| CUDA view of the buffer | 4 | 9 |
| NVENC map | 9 | 19 |
| submit | 71 | 165 |
| **wait for NVENC** | **1598** | **3915** |

**Other numbers:**
- **Composite:** 0.3–0.4 ms of GPU time at p50, ~2.3 ms at p99 under the shared load.
- **New viewer:** first frame 50–67 ms with a cold encoder (NVENC session init 31–59 ms), **10–11 ms** with a warm one. S2 kept every encoder always on: 14–17 ms.
- **All three codecs:** H.264, HEVC and AV1 all reach the browser over WebRTC; S2 had no AV1.
- **Resize:** 2560×1440 → 1920×1080 → 1280×720 → 2560×1440 mid-stream reallocates the pool and sends a keyframe each time. 897 of 902 frames went out, so each resize skipped about one frame.
- **Under heavy load** (another process at ~350 W), the NVENC wait grows by ~2 ms for every codec.

**Where the tail comes from.**
- With RGB input, NVENC converts to YUV with a CUDA kernel. That kernel waits behind other processes' CUDA work on a busy GPU, and S2 has the same exposure.
- Doing the conversion ourselves removes that wait. Probed on this stack:
  - GL can render R8, but CUDA's EGL interop won't import an R8 image;
  - NV12 can't be a GL render target;
  - GBM ignores a linear request and returns an implicit layout that GL can't bind.

  So the path is a Vulkan compute pass that writes NV12 into a buffer CUDA imports (external memory), and NVENC reads it as `CUDADEVICEPTR` NV12 (`cha-nvenc` already takes `InputFormat::Nv12`). PyroWave needs the same Vulkan import of the composite (plan §3.3), so both land together.
