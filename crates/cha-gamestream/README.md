# cha-gamestream

A GameStream (Moonlight protocol) **host** as a library, ported from [Moonshine](https://github.com/hgaiser/moonshine) ([ADR 0009](../../docs/adr/0009-gamestream-host-module.md)). It knows the protocol and nothing of our engine, so it can be read, tested and removed as a unit. Stock Moonlight and Artemis clients pair with it, list apps, launch, resume and cancel, and stream.

It comes in two halves that do not depend on each other. A node runs the **front**; each environment's streamer, a separate process, runs the **media** of its session.

```
Moonlight ──nvhttp, pairing, RTSP, mDNS──▶  front::Host            (node agent)
                                              │ Directory (launch, resume, start_media, ...)
                                              │ PairingStore
                                              ▼   SessionHandoff (serde)
Moonlight ◀──ENet control, RTP video+FEC, RTP audio+FEC──  media::MediaSession   (streamer)
                                              ▲
                                              │ MediaBackend (encoded video, Opus, input, feedback)
```

| Path | What |
|---|---|
| `src/front/` | `Host` (builder, `start`, `run`, `HostHandle`), `nvhttp/` (HTTP and HTTPS, `xml.rs`, `tls.rs`), `pairing/` (the five phases and their crypto), `rtsp/` (parser, `sdp.rs`), `identity.rs`, `mdns.rs`, `session.rs` (the session state machine) |
| `src/media/` | `MediaSession`, `MediaSockets`, `control/` (ENet loop, AES-GCM wrapper, messages, feedback), `video/` (packetizer, FEC plan, GSO sender), `audio/` (packetizer), `ping.rs` |
| `src/handoff.rs` | what both halves share: `StreamParams`, `SessionHandoff`, `MediaPorts`, `ClientId`, Opus layouts |
| `src/directory.rs`, `src/backend.rs` | the trait boundary (below) |
| `src/input.rs`, `src/hdr.rs` | input packets to neutral `InputEvent`s; HDR SEI and OBU metadata |
| `LICENSE-MOONSHINE` | Moonshine's BSD-2-Clause text. Each ported file starts with its notice and a line on what changed; our changes are AGPL-3.0-or-later like the rest of the repo |

## The boundary

Everything the embedding application decides is a trait method; the crate decides nothing about users, apps or processes.

- **`Directory`** (front): `apps`, `app_image`, `launch -> SessionTarget { media_ports }`, `resume`, `start_media(SessionHandoff)`, `stop_media(session_id)`, `cancel`, and `pin_for(PairingAttempt) -> PinWaiter`. The front calls `launch` after it has checked the client's certificate and that no session runs, and expects the media ports **reserved**: RTSP `SETUP` tells the client the ports before `ANNOUNCE` says what the stream is. `start_media` comes at RTSP `PLAY`. The directory runs `media::MediaSession::start` in process, or sends the handoff to a streamer.
- **`PairingStore`**: `is_paired`, `add`, `remove`, `list`, by certificate fingerprint (`ClientId`). `MemoryPairingStore` is there for tests.
- **`MediaBackend`** (media): `capabilities()` and `start(StreamParams) -> MediaStreams { video, audio, feedback, control }`. Video is `EncodedVideo { data, key, index, captured }` (Annex-B or OBUs), audio `OpusPacket { data, samples }`, feedback `Feedback` (rumble, trigger rumble, LED, motion enable, trigger effect, HDR mode). `control` is a `MediaControl`: `request_keyframe`, `invalidate(first, last)` (in the backend's own frame indexes; the host maps the client's), `set_bitrate`, `input(InputEvent)`, `release_input`, `stop`. No compositor or evdev types cross it.
- **`HostHandle`** (from `RunningHost::handle()`): `media_ended(session_id)` tells the front the media ended by itself (the session then waits for a resume), and `cancel_session()`.

`SessionHandoff` is plain serde data: session id, the `rikey`/`rikeyid`, which streams are encrypted, the ENet connect data and ping payload the client was told, and `StreamParams` (client address and certificate, codec, size, fps, bitrate, packet size, FEC, HDR, chroma, audio layout). It holds the session key; send it only over a channel the two halves trust.

## Security fixes made while porting

| Gap in Moonshine | Now | Test |
|---|---|---|
| A PIN was accepted from `POST /submit-pin` with no authentication | The PIN comes only from `Directory::pin_for`; the HTTP pages are gone, and a pairing request waits (bounded) for the directory | `pairing.rs`: `the_pin_cannot_be_submitted_over_http`, `pairing_waits_for_the_directory_and_gives_up_without_a_pin` |
| Video and audio sockets followed whoever sent `PING` | A `PING` counts only from the IP of the client whose session it is (IPv4-mapped addresses compared as IPv4), and is the legacy `PING` or carries the session's `X-SS-Ping-Payload`; others are counted | `media.rs`: `video_and_audio_follow_only_pings_from_the_sessions_client`; `ping.rs` units |
| Any paired client could resume or cancel the session | A session belongs to the certificate that launched it; resume and cancel by another are refused, launch over a running session is refused, and RTSP answers only the launching address | `security.rs`: `another_paired_client_cannot_resume_or_cancel_a_running_session`; `session.rs` units |
| `/unpair` did nothing | It removes the client through `PairingStore::remove` (over HTTPS, by the client's own certificate; plain-HTTP `/unpair?uniqueid=`, which anyone can send and which many clients share one `uniqueid` for, is off unless `unauthenticated_unpair` is set; devices are otherwise removed where they were paired, the portal) and ends that client's session | `pairing.rs` (happy path), `security.rs`: `unpairing_a_client_ends_its_session`, `plain_unpair_can_be_switched_off` |
| A malformed control packet ended the session | It is dropped and counted (`MediaStatsSnapshot`); so is a packet that doesn't authenticate or isn't encrypted | `media.rs`: `a_malformed_control_packet_is_dropped_and_counted_never_fatal` |
| One feedback message per 10 ms tick | Every pending message goes out each tick | `media.rs`: `every_feedback_message_of_a_burst_reaches_the_client` |
| Unbounded or slow requests, panics on hostile input | HTTP: header limit, body limit (413), header-read timeout, connection cap; RTSP: head and body limits, read timeout, connection cap; ranges checked on every number; the parsers (input packets, control framing, RTSP, SDP) run on random bytes in tests | `security.rs`: `nvhttp_bounds_request_size_time_and_connections`, `rtsp_answers_only_with_a_session_and_refuses_what_it_should`, `the_https_port_survives_rubbish`; `hostile_bytes_never_panic` in `input.rs`, `rtsp/mod.rs`, `rtsp/sdp.rs`, `control/messages.rs` |

Three more differences, beyond the brief, that follow from the same reasoning:

- **Control messages must be encrypted.** The host advertises `encryptionRequested` for control (Moonlight and moonlight-common-rust then enable it) and refuses an `ANNOUNCE` that doesn't, and drops any plain control message. Moonshine accepted plain ones, so anyone who could reach the ENet port could inject input.
- **The ENet peer is authenticated.** Only the session client's address, presenting the session's `X-SS-Connect-Data`, may connect, and only one peer.
- **A pending pairing is bound to the requester's address, is capped at 32 and expires.** A pairing finishes with a `pairchallenge` over HTTPS that must come from a certificate now in the store.

## What was ported, and from where

| Here | Moonshine | How |
|---|---|---|
| `front/pairing/crypto.rs`, `pairing/mod.rs` | `clients.rs`, `webserver/pairing.rs` | maths kept (SHA-256 key from salt and PIN, AES-ECB challenges, RSA PKCS#1 signatures via aws-lc-rs), state machine rewritten around the Directory and the store |
| `front/nvhttp/` | `webserver/mod.rs`, `tls.rs` | endpoints and the lenient client-certificate verifier kept; apps, launching and sessions rewritten; XML built from typed values |
| `front/rtsp/` | `rtsp.rs` | handlers and the SDP attributes kept; the `rtsp-types` and `sdp-types` crates replaced by a small parser; SETUP reports the reserved ports |
| `front/identity.rs` | `tls.rs` (certificate generation) | RSA-2048 from aws-lc-rs (the `rsa` crate is slow in debug builds) |
| `front/mdns.rs` | `discovery.rs` | without `network-interface` and `gethostname` |
| `front/session.rs` | `session/manager.rs` (the state machine only) | rewritten; no engine |
| `media/control/` | `stream/control/{mod,feedback}.rs` | message types, framing and encryption kept; loop rewritten for the fixes above |
| `input.rs` | `stream/control/input/{mod,keyboard,mouse,touch,gamepad}.rs`, parse halves | neutral types; pen and touch contact areas, controller touch added |
| `media/video/` | `stream/video/{packetizer,shard_batch,gso_socket}.rs` | NV video RTP layout, GCM and `tokio`/`quinn-udp` GSO kept; no `unsafe`; the FEC block plan is new (parity counted as Moonlight counts it, at most four blocks, a frame too large for the FEC setting sheds FEC) |
| `media/audio/packetizer.rs` | `stream/audio/encoder.rs` (RTP, FEC and CBC halves) | RS(4,2) with Moonlight's parity matrix kept; the Opus encoder is the backend's |
| `hdr.rs` | `stream/video/pipeline/hdr_sei.rs` | neutral metadata type |
| not ported | compositor, pipeline encode loop, PulseAudio server, inputtino, application launching, healthcheck, app scanners | ADR 0004 |

## The client's media (`client::media`)

One stream's media as a Moonlight client receives it ([ADR 0011](../../docs/adr/0011-own-gamestream-client.md)): `MediaClient::start(&StreamSetup)` (or `start_with(.., MediaOptions)`) returns a `Media` with `video: Receiver<VideoFrame>` (whole access units in frame order, keyframe flag), `audio: Receiver<AudioPacket>`, `feedback: Receiver<Feedback>`, `ended: oneshot::Receiver<Ended>` (`Stopped`, `Terminated { code, graceful }`, `Failed`) and a cloneable `MediaHandle` (`input`, `request_idr`, `invalidate`, `stop`, `stats`). The stream stops when the last handle is dropped.

| Path | What |
|---|---|
| `client/media/mod.rs` | the API and the three tasks: video and audio sockets (`PING` every 500 ms, Sunshine's with the session payload or the legacy four bytes), the ENet control peer (start A and B, a ping every 100 ms, requests, input, feedback, termination) |
| `client/media/video.rs` | `VideoReceiver`, sans-IO: AES-GCM per shard, FEC blocks, whole-packet Reed-Solomon recovery, frame assembly, loss handling |
| `client/media/audio.rs` | `AudioReceiver`: RS(4,2) with the host's parity matrix, AES-CBC, in order |
| `client/media/control.rs` | the client's control messages, the host's decoded into `Feedback` and terminations |
| `client/media/input.rs` | `InputEvent` to packets (the inverse of `input::parse`), channels as Moonlight uses them, and the batching queue |

**Video recovery is over whole packets.** FEC covers each shard from the RTP header on, and a lost shard is a full-size buffer, as the host's packetizer does it. A recovered packet has its headers rebuilt from what the client knows and its start and end flags checked against its position, so a nonsense recovery is refused. (`moonlight-common-rust` ran the maths over payloads with empty buffers for the missing shards and failed with `IncorrectShardSize`.)

**A lost frame never ends the stream.** The receiver keeps up to 32 frames in flight, so reordering across frames is harmless. A frame that can't complete (past the FEC, or idle for 10 ms with a newer frame arriving, or 100 ms alone) is dropped; the receiver asks the host for a keyframe once for a burst (again after 1 s if none comes), drops frames until one that can start a picture arrives, and resumes there. With `invalidate_refs` it asks for a reference-frame invalidation of the lost range instead and resumes at a keyframe, an intra refresh or a frame predicted past the loss. A consumer too slow to take a frame counts as a loss too.

**Input** is queued and batched: relative mouse motion adds up and goes out at most once a millisecond (a click or wheel event is a barrier), an absolute position, a pad's axes (until its buttons change) and a motion sensor keep the latest.

Tests: unit tests in each file (golden frames from the host's packetizer, every loss position of single- and multi-block frames up to the parity, reordering, duplicates, the keyframe wait, hostile bytes), and `tests/client_media.rs`, the client against `MediaSession` with a fake backend through a lossy UDP proxy (drops chosen shards by frame position, holds back and repeats packets; video encrypted and not; frames of 200 B to 1 MB; audio with loss; every kind of input; feedback; both ways of stopping). Not covered without a real Sunshine or Apollo host: their reference-frame invalidation and LTR acknowledgements, frame-type values other than ours (Sunshine's 104 header), AV1 and HDR on the wire, Sunshine's own FEC percentage rules at the edges, and real network loss.

## Dependencies

`socket2` (MIT or Apache-2.0, already in the lock) sets the video socket's receive buffer. New to the workspace: `tokio-enet` (BSD-2-Clause, Moonshine's author) and `pem` 4 (MIT, via `rcgen`'s `pem` feature). Already in the lock and used here: `fec-rs` (BSD-2-Clause), `aws-lc-rs`, `rcgen`, `rustls`, `tokio-rustls`, `hyper`, `x509-parser`, `aes`, `aes-gcm`, `cbc`, `sha2`, `mdns-sd`, `quinn-udp`, `form_urlencoded`. `rtsp-types` and `sdp-types` (MIT) were not needed. `moonlight-common` (GPL-3.0-or-later) is a **dev-dependency** of this crate only, the independent client the cross-check tests drive; no shipped crate depends on it ([ADR 0011](../../docs/adr/0011-own-gamestream-client.md)).

## Tests

```bash
cargo test -p cha-gamestream
cargo clippy -p cha-gamestream --all-targets -- -D warnings
```

Runs on macOS and Linux. Unit tests (golden packets, pure functions, hostile bytes) live next to the code: the video packetizer's shard layout, FEC recovery, AES-GCM against a NIST vector; the audio packetizer's parity against Moonlight's matrix; control framing both ways; every input event; serverinfo and applist XML; the SDP; the session state machine against a stub `Directory`. The integration tests in `tests/` run against fake traits (`tests/common/rig.rs`, which `cha-client-gamestream`'s loopback test includes by path): a `FakeDirectory` reserves sockets at launch and starts `MediaSession` at `start_media`, which is the production seam in process. `tests/common/mod.rs` adds `moonlight-common-rust` as an independent real client on loopback.

- `pairing.rs`: a full pairing with the PIN from the fake directory (then apps over HTTPS and unpair), a wrong PIN refused by the client, no PIN, no PIN over HTTP, unpaired certificates refused.
- `stream.rs`: launch, RTSP, control, then video (a keyframe and frames of 200 B to 100 kB reassembled), audio, keyboard, mouse, scroll, a pad, a keyframe request, a reference invalidation (client frame numbers mapped to backend indexes), a burst of rumble, cancel; resume after the client leaves; encrypted audio decrypted by the client; HEVC.
- `media.rs`: the media half alone with a hand-written control client: refused peers, malformed and unauthenticated packets, PING sources (over IPv6 and IPv4 on one dual-stack socket), feedback bursts, goodbye on stop, timeout, client leaving, backend ending, HDR mode and SEI.
- `security.rs`: ownership, unpair, RTSP and nvhttp bounds.

Not covered: video encryption end to end (moonlight-common-rust cannot decrypt video, so the client tests run it off; the encrypted shards are checked in the packetizer test by decrypting them), real Moonlight clients, mDNS on a real network, and the Linux build (nothing here is Linux-only; quinn-udp uses GSO there).

## Trying it with a real Moonlight client (without the node's host)

`examples/dev_host.rs` runs the front with an in-memory pairing store and one app, "Environment", and gives each launched session to a `cha-streamer` built with `--features gamestream`, through its local API. The PIN a client shows is read from stdin. Nothing is kept: every run is a new host (a new id and certificate), so pair again and remove the old entry from the client.

On the node (`iolinux.lan`), in the streamer's dev container (`deploy/streamer/compose.dev.yaml`, after syncing the repository):

```bash
# 1. The streamer, with the module on. The secret is for you to invent (16+ characters).
docker compose -f deploy/streamer/compose.dev.yaml exec dev cargo build --release -p cha-streamer --features gamestream
docker compose -f deploy/streamer/compose.dev.yaml exec -e CHA_GAMESTREAM_SECRET=dev-secret-0123456789 dev \
  /target/release/cha-streamer --listen 127.0.0.1 --gamestream-ports 7700,7701,7702 --run '<an app, as in crates/cha-streamer/README.md>'

# 2. The host, on the same machine (it talks to the streamer on localhost). It needs a Rust toolchain: the dev container has one.
docker compose -f deploy/streamer/compose.dev.yaml exec -e CHA_GAMESTREAM_SECRET=dev-secret-0123456789 dev \
  cargo run --release -p cha-gamestream --example dev_host -- --streamer http://127.0.0.1:7660 --ports 7700,7701,7702
```

The dev container uses the host network, so the host's ports (TCP 47984, 47989, 48010, UDP 5353 for mDNS) and the media ports (UDP 7700 to 7702) are the node's. Then:

1. In Moonlight (desktop, Steam Deck, Artemis), the host "Cha dev host" appears by itself, or add the node's address. Click it to pair: the client shows a PIN; type it in the terminal where `dev_host` runs and press Enter.
2. Launch "Environment". `dev_host` prints the launch and when its media starts and ends. Ask for stereo audio and H.264, HEVC or AV1 (the streamer says which in `/streams`), at 60, 90 or 120 fps.
3. The streamer logs the size applied and the codec; `curl -H 'Authorization: Bearer dev-secret-0123456789' http://127.0.0.1:7660/gamestream/status` says whether a session runs.

A browser session of the same environment (through the portal) keeps watching, and loses the controls while Moonlight has them.

Options: `--name`, `--bind`, `--http`, `--https`, `--rtsp` (see the file's header). Only one client can stream at a time.

## What G2 added

The streamer's adapter lives in `crates/cha-streamer/src/gamestream/` (the `gamestream` feature, see that README): `MediaBackend` over the shared encoder, the Opus mixer (with 5 ms frames for Moonlight), the compositor's input and the virtual pads; the local API the host drives; and the agent's port block (`CHA_GAMESTREAM`, `deploy/README.md`). What needs a real client to verify is listed in the G2 report: pad kinds and rumble, trigger effects, the mouse and wheel feel, keys, 5 ms audio, size and fps changes, and resume.

## What G3 added

The node's host is `crates/cha-node/src/gamestream/` (the `gamestream` cargo feature of `cha-node`, on by default; `CHA_GAMESTREAM=true` at run time): `directory.rs` is the `Directory` (a client's apps are its owner's running environments, launches name the environment's port block, `start_media` posts the handoff to that environment's streamer), `pairing.rs` the `PairingStore` (the portal's list, cached) and the pending pairings, `identity.rs` the host's certificate and `uniqueid` under `<data root>/node/gamestream/`. Pairing is the portal's: the PIN a client shows is typed by a signed-in user in the portal (`ToPortal::GameStreamPairRequest`, `NodeRequest::GameStreamPin`), and the portal keeps the devices (`gamestream_devices`) and sends the node its list on every connect and change (`NodeRequest::GameStreamDevices`). `cargo test -p cha-node --test gamestream` pairs, lists, launches (through RTSP `PLAY`, against a fake streamer) and unpairs with this crate's client against it, with a fake portal on the agent's channel. What needs a real client is in the G3 report: mDNS discovery on a real network, a launch that reaches PLAY with a real streamer, and a Moonlight client's own unpair button.

## Not done in G1

- Key rotation under a live media session. A resume stops the old media and starts new media under the new key, so there is no rotating key to watch.
- Plain (unencrypted) control for clients that cannot do control v2; none of the clients we know of (Moonlight, Artemis, moonlight-common-rust) is one.
- DSCP/QoS marking of the UDP sockets (`x-nv-vqos[0].qosTrafficType`).
- Surround, 4:4:4, HDR and AV1 are protocol-complete (negotiated, passed to the backend, HDR mode and metadata sent) but only HDR and HEVC are tested; the backends of G2 decide what is real.

## The client's front (`src/client/front/`)

`HostClient` is a Moonlight client's control plane, the other side of `front` ([ADR 0011](../../docs/adr/0011-own-gamestream-client.md)). `ClientIdentity` (RSA-2048 self-signed, load/save PEM) is what a host pins; `HostClient::new(address, identity)` then does `server_info()` (HTTPS with the pinned host certificate once paired, else HTTP), `pair(pin, device_name)` (the five phases; returns the host certificate to keep and give to `with_server_cert` next time; `PairingError::{WrongPin, AlreadyInProgress, Declined, Mitm, ...}`), `unpair()`, `app_list()`, `app_asset(id)`, `launch(&StreamRequest)`, `resume(..)` and `cancel()`. A launch ends in a `StreamSetup`: `/launch` with a fresh `rikey`, then RTSP (OPTIONS, DESCRIBE, SETUP audio/video/control, ANNOUNCE, PLAY). The codec is the first of the request's preference the host offers in the asked dynamic range and chroma; video and audio encryption are `Encrypt::{Off, IfSupported, Required}`. Hosts older than app version 7.1.431 (GFE 3.22) and `rtspenc://` session URLs are refused. Every request is bounded in time (`Timeouts`) and size; parsers run on random bytes in tests. The host's pairing crypto (`front::pairing::crypto`) and TLS signature check (`nvhttp::tls`) are shared with it. Discovery isn't here: the node browses mDNS itself.

Tests: `tests/client_front.rs` (our client against our host: pairing, wrong PIN, launch/resume/cancel/unpair, codecs, HDR/4:4:4/surround, encryption, hostile hosts, and the same session as moonlight-common-rust's) and unit tests per module.
