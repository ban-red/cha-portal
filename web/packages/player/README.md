# @cha/player

The browser side of a session (plan §6.1). Framework-agnostic: give it a `<video>` element and a function that carries a WebRTC offer to the environment. In the portal, that is `POST /api/environments/{id}/connect`.

```ts
const player = new Player({
  video,
  signal: async (offer, codec) => (await api.connect(id, { codec, offer })).answer,
});
await player.connect();
```

- **Transports.** `transport: "auto"` (the default) takes WebTransport where the browser has WebTransport, WebCodecs and track generators (Chromium), and falls back to WebRTC. Pass `webTransport` (the portal's URLs and certificate hash) to allow it.
  - **WebTransport** (`wt-worker.ts`): `cha-stream/1` datagrams reassembled in a worker, frames handed over in order from a keyframe, a keyframe asked for when one is lost; WebCodecs decodes into track generators feeding `<video>` and `<audio>`.
  - **WebRTC:** recvonly video and audio tracks; the streamer stamps playout-delay 0 on video. The node streams straight to the browser, while the portal only brokers the offer and answer, with a 60 s media token.
- **Codec.** The best this browser can receive, in the order measured on the baseline (P1.3): HEVC, then H.264, then AV1.
  - **`switchCodec()`** changes codec in place over WebTransport: a decoder for the new codec is ready first, and the picture stays up until the new stream's first frame (gaps under 35 ms). Over WebRTC, or when the streamer doesn't answer within 3 s, it reconnects.
  - **PyroWave** (`pyrowave420`, `pyrowave444`; `pyro.ts`): the LAN tier, over WebTransport only, where WebGPU has subgroups (`supportsPyroWave()`). Frames are intra-only, so the worker hands over every frame, partial ones at the deadline, and never waits for a keyframe. `@cha/pyrowave-webgpu` decodes into an offscreen canvas; each frame becomes a VideoFrame for the same track generator as WebCodecs.
- **Input** (`input.ts`) goes up the `control` DataChannel.
  - **Pointer:** absolute positions on the picture. With `lockPointer()`, raw relative motion scaled to stream pixels, for games.
  - **Keyboard:** keys by `KeyboardEvent.code`. Held keys and buttons are released on blur, so nothing sticks. On a Mac, ⌘ goes as Ctrl (`commandAsControl`), so the Mac's shortcuts work in Linux apps. The keys of a ⌘ chord are released with ⌘, since macOS sends no keyups for them.
  - **Floor:** one page has the controls (`onFloor(control, viewers)`); a watching page sends no input or resizes, draws the controller's pointer over the picture when the picture doesn't show it, and can ask for the controls (`takeControl()`).
  - **Cursor:** in desktop mode the environment's cursor is the element's CSS cursor (a CSS keyword, or the app's image through `image-set()` at the stream's scale), so it moves with no stream delay. With a locked pointer the picture has it.
  - **Clipboard:** on Ctrl/⌘+V the V is held back until the browser's own paste event (no permission prompt) supplies the device's clipboard, which goes up first. Text an app copies arrives as a control message and is written to the device's clipboard (`onClipboard` says whether that worked or waits for the next click).
  - **Fallback:** events with no physical key (on-screen keyboards) map unshifted US characters. Full text input comes with the text-input protocol.
- **Sound.** Stereo Opus, 10 ms frames, NetEq kept at its minimum, in its own MediaStream so video is never held back for lip sync. Browsers play sound only after a click or key press; `onAudioBlocked` says when it's waiting, `setMuted()` turns it on or off.
- **Gamepads** (`gamepad.ts`): the Gamepad API polled at 250 Hz, each pad's buttons and axes sent when they change (standard mapping). The streamer makes them virtual Xbox 360 controllers. Unfocused, pads are released.
- **Size.** The picture follows the element: device pixels, capped at 2560×1440 by default, resized when the element settles.
- **Stats** (`readStats()`): codec, size, fps, bitrate, RTT, decode and jitter-buffer time, losses, and **send → shown**. That last one comes from the server's per-frame send times, matched by RTP timestamp, with the clock synced by pings.
- **Probe** (`runProbe()`): click → screen, using synthetic clicks and the test pattern's centre flash; with sound on, also click → speaker and the A/V offset, from the test pattern's tone (timed by an AudioWorklet, mapped to output time).
