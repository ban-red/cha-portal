# 0004: Our own streaming engine, without Wolf

- **Status:** accepted (2026-10-03). Supersedes [0003](0003-gateway-as-a-service-beside-wolf.md).
- **Context:**
  - The plan bootstrapped Phase 1 on Wolf: Wolf hosts the environments, and `cha-gateway` bridges its Moonlight stream to the browser. Our own engine, `cha-streamer`, was to replace it in Phase 2.
  - Phase 0 then built a minimal `cha-streamer` (S2). It runs Chrome in a headless compositor, encodes with NVENC and streams over str0m WebRTC to Chrome on the Mac, with keyboard and mouse back. That takes ~6 ms from the node's compositor to the Mac's (p50, 1440p60, 1 GbE), and the stream resizes in 30–48 ms.
  - Everything Wolf-specific in Phase 1 would be thrown away in Phase 2: the adapter for Wolf's API, pairing with Wolf, Wolf's encoder settings, and the gateway beside Wolf.
  - We want to own every piece we can, and to keep only the most efficient ones.
- **Decision:**
  - No Wolf, and no Games-on-Whales images or container contract. Phase 1 ships on `cha-streamer`.
  - **Build vs borrow.** We write every piece on the hot path, and every piece small enough to own. We use a library only when rebuilding it would be a project of its own *and* the library is already lean. GStreamer and FFmpeg leave the node's media path.

    | Piece | Ours or borrowed | Why |
    |---|---|---|
    | Wayland compositor | **Ours**, built on Smithay | Frame pacing (2–4× the encode rate, S2), damage, resize, cursor and input are product features, so we own them. Smithay is the protocol toolkit (wayland-server, xdg-shell, dmabuf, seat, Xwayland); rebuilding it gains nothing. It replaces gst-wayland-display. |
    | Video encode | **Ours**: NVENC and the CUDA driver API, loaded at runtime, with bindings generated from NVIDIA's headers | Zero-copy from the compositor's buffers, registered once; no framework in between. AMD/Intel (VA-API or Vulkan Video) come in a later phase. |
    | WebRTC | str0m | ICE, DTLS, SRTP, SCTP and congestion control are a project of their own. str0m is sans-IO and already measured in S1–S3. |
    | QUIC / WebTransport (Phase 2) | quinn, with our own congestion controller (§3.1) | Same reason as WebRTC |
    | Audio server | **Ours**: a minimal PulseAudio-protocol server inside the streamer | Apps speak the Pulse protocol. The samples land in the streamer directly, with no sound daemon per environment and no extra buffering. |
    | Opus | libopus | The reference encoder; rewriting an audio codec is out of scope |
    | Gamepads | **Ours**: uinput (Xbox) and uhid (DualSense) virtual devices, hotplugged into the environment with our own device nodes and udev events | Small kernel APIs. Replaces inputtino and fake-udev. |
    | Containers | Docker Engine, driven by **our own** small API client | A container runtime is out of scope; the client is a handful of endpoints over the socket |
    | Environment images | **Ours** (`images/`) | |
    | PyroWave (Phase 2) | Encoder: libpyrowave, pinned. Decoder: **our** WebGPU port | |
    | External Moonlight hosts (Phase 3) | **Our** gateway (S3), on moonlight-common-rust | The GameStream protocol is a project of its own |
- **Consequences:**
  - Phase 1 grows: it now includes the compositor, the NVENC binding, audio, gamepads and the images. Steam moves to the first item of Phase 2.
  - Each `cha-streamer` milestone must match or beat S2's GStreamer numbers on the same node. S2's spike stays in the repo as that baseline.
  - `cha-gateway` serves only external Moonlight hosts, in Phase 3. ADR 0003 no longer applies. Its principle stays: the agent is never in the media path, and the streamer runs as its own process that the agent supervises.
