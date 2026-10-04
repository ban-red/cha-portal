# @cha/player

The browser side of a session (plan §6.1). Framework-agnostic: give it a `<video>` element and a function that carries a WebRTC offer to the environment. In the portal, that is `POST /api/environments/{id}/connect`.

```ts
const player = new Player({
  video,
  signal: async (offer, codec) => (await api.connect(id, { codec, offer })).answer,
});
await player.connect();
```

- **Media.** A recvonly WebRTC video track; the streamer stamps playout-delay 0. The node streams straight to the browser, while the portal only brokers the offer and answer, with a 60 s media token.
- **Codec.** The best this browser can receive, in the order measured on the baseline (P1.3): HEVC, then H.264, then AV1.
- **Input** (`input.ts`) goes up the `control` DataChannel.
  - **Pointer:** absolute positions on the picture. With `lockPointer()`, raw relative motion scaled to stream pixels, for games.
  - **Keyboard:** keys by `KeyboardEvent.code`. Held keys and buttons are released on blur, so nothing sticks.
  - **Fallback:** events with no physical key (on-screen keyboards) map unshifted US characters. Full text input comes with the text-input protocol.
- **Size.** The picture follows the element: device pixels, capped at 2560×1440 by default, resized when the element settles.
- **Stats** (`readStats()`): codec, size, fps, bitrate, RTT, decode and jitter-buffer time, losses, and **send → shown**. That last one comes from the server's per-frame send times, matched by RTP timestamp, with the clock synced by pings.
- **Probe** (`runProbe()`): click → screen, using synthetic clicks and the test pattern's centre flash.
