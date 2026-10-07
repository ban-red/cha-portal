# cha-gateway

Streams a Moonlight host (Sunshine or Apollo) to the browser player, in place of `cha-streamer` ([ADR 0008](../../docs/adr/0008-moonlight-hosts-adopted-by-a-node.md)). The node agent starts it as an environment's only container; it needs no GPU. It is the S3 spike's gateway, promoted.

```
Sunshine/Apollo ──GameStream (RTSP, ENet, RTP+FEC)──▶ cha-gateway ──WebRTC──▶ @cha/player
                                                       moonlight-common-rust → str0m
                                                       no transcoding
```

## What it does

- Binds signalling on `--listen:--http-port` at once, then connects to `--host` as the node's paired Moonlight client (identity from `--identity-dir`: `client-cert.pem`, `client-key.pem`, `hosts/<uniqueid>/server-cert.pem`), and launches `--app-id` at `--width`×`--height`@`--fps` (resuming it if the host already runs it; any other running app is closed first). Stereo audio, no video encryption.
- The codec is the host's choice at launch: HEVC if it can encode it, else H.264 (`--codec` forces one). A viewer asking for another codec gets a 400.
- Keeps that one stream for the environment's life. Up to 4 WebRTC viewers share it; each starts at the next keyframe (the gateway asks the host for one when a viewer joins or falls behind).
- Passes video access units and the host's Opus packets through unchanged. Audio is sent only when the host's stream is 2-channel single-stream Opus; surround is logged and dropped.
- Serves what the streamer serves: `GET /info`, `GET /streams`, `POST /webrtc/media?name=live-<codec>&token=<media token>` (the portal's token, checked with `cha_wire::verify_media_token`), and the `control` DataChannel. WebTransport is off (`wt_port: 0`), and the picture size is fixed for the session (`resize: false`).
- Maps the player's input to Moonlight: keys by `KeyboardEvent.code` to Windows virtual keys (US layout), absolute and relative mouse, buttons, wheel (a 100 px page notch is 120 units), and Gamepad API pads as Xbox controllers. Host rumble goes back as `rumble`.
- On SIGTERM or SIGINT it quits the app on the host, flushes the disconnect and exits 0. If the host's stream dies it logs why and exits 1, and the agent ends the environment as failed.

## Arguments

The node agent passes `--listen --http-port --webrtc-port --portal-key --environment-id --width --height --fps [--public-address] --host --host-http-port --host-https-port --host-unique-id --app-id --identity-dir`. Optional extras: `--mbps` (0 picks about 0.18 bit per pixel, 40 Mbit/s at 1440p60), `--codec auto|h264|hevc`, `--advertise <ip,...>`.

## Limits (moonlight-common-rust at the pinned commit)

No AV1, no video encryption (the node and host share a LAN; the browser leg is DTLS), loss recovery by IDR only, one session per host. WebRTC uses one UDP socket per advertised address on `--webrtc-port`, shared by all viewers.

## Develop

```bash
cargo test -p cha-gateway
docker build -f deploy/gateway/Dockerfile -t cha/gateway:dev .   # from the repository root
```

The unit tests need no host. What needs a real Sunshine or Apollo host: pairing, launch and resume, the video and audio passthrough, keyboard, mouse and pad input reaching the game, rumble, and the shutdown path.
