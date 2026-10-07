# C2.2: cha-stream/1 over WebTransport, as the browser speaks it

What the browser player and the streamer do today, as the spec for Cha Player's transport ([ADR 0013](../adr/0013-native-player-on-cha-stream.md)). Mapped from the code on 2026-10-07; `file:line` references are as of then. Where the code changes, the code wins.

Paths are relative to /Users/red/code/portal.cha.sh. Abbreviations: PT = web/packages/player/src/player.ts, WW = web/packages/player/src/wt-worker.ts, WT = crates/cha-streamer/src/wt.rs, CT = crates/cha-streamer/src/control.rs, HDR = crates/cha-proto/src/header.rs, REA = crates/cha-proto/src/reassembly.rs, ENV = crates/cha-control/src/environments.rs.

## 0. Findings that change how you build it

1. **The browser WT path uses only one bidirectional stream and datagrams.**
   - The client opens one bidi stream (the control stream). No unidirectional streams are used by either side. (WW:230; WT:201 `accept_bi` is the only stream call.)
   - All video, audio, input, feedback and control ride on that stream plus datagrams.
   - Datagram kinds Input (2), Feedback (3) and Probe (4) exist in HDR:10-17 but are never sent or read on WT. The streamer has no `read_datagram` at all, so client-to-server datagrams are ignored.
   - Input and feedback are JSON lines on the control stream.
2. **There is no Config or parameter-set message.**
   - Keyframes carry SPS/PPS/VPS (or the AV1 sequence header) in-band, as Annex-B. NVENC `repeatSPSPPS=1` (crates/cha-nvenc/src/nvenc.rs:715,724,733).
   - The browser configures WebCodecs with a codec string only and no `description`.
   - A VideoToolbox client must parse the parameter sets out of each keyframe and build a CMVideoFormatDescription itself.
3. **`cha_proto::Reassembler` (REA:236-348) does not handle FEC.**
   - `push` indexes `parts[frag_index]`. A parity datagram has `frag_index >= frag_count`, so it would panic or be ignored.
   - Do not use it for the hardware codecs on this path. Port WW `fragment/rebuild/deliver/expire` instead.
   - `cha_proto::{DatagramHeader, Flags, Fragmenter, fec::recover/encode/blocks}` are directly reusable.
4. **`backlog_ms` is not client feedback.**
   - The streamer computes it from its own QUIC datagram send buffer (WT:823-838).
   - The only client-to-streamer feedback is the `report` JSON line (§7) and ping/pong.
5. **"Raced" addresses is really sequential** (WW:165-187). See §1.
6. **Device tokens already exist.** `PlayerUser` (crates/cha-control/src/auth.rs:170-205) accepts either the session cookie or `Authorization: Bearer chadev_...` on launch, environment get/stop, and connect. ADR: docs/adr/0013-native-player-on-cha-stream.md.

## 1. Launch and connect

### HTTP calls
Base path is `/api`. Auth is `Authorization: Bearer chadev_<43 chars>` (cookie also works). The routes are at ENV:55-62.

**Launch:** `POST /api/environments`
- Request body (camelCase), ENV:384-395: `{"templateId":"<catalog id>", "node"?: "<node id>", "device"?: "<device id>"}`.
  - Omit `node` and `device` for automatic placement. `device` requires `node`.
- Response: an `EnvironmentView` (ENV:165-193) with `state:"starting"`. Fields are `id, templateId, templateName, ownerId, nodeId, nodeName, state, detail, warning, log, codecs[], device{kind,name}, usage, createdAt, updatedAt, streamer{host,httpPort,webrtcPort}`.
- Poll `GET /api/environments/{id}` until `state=="running"`. Other states are `starting`, `stopping`, `destroyed` and `failed`, with `detail` giving the reason.
- `codecs` lists what the device encodes. Per ENV:257-270, NVIDIA devices also offer `pyrowave420` and `pyrowave444`.
- Limit: 4 live environments per user (ENV:43). Launch timeout up to 600 s (ENV:40).

**Connect:** `POST /api/environments/{id}/connect`
- Request body, ENV:672-681: `{"codec":"h264"|"hevc"|"av1"|"pyrowave420"|"pyrowave444", "transport":"webtransport"}`.
  - `transport` defaults to `webrtc`. `offer` is not needed.
  - PyroWave is accepted only with `transport:"webtransport"` (ENV:749).
- Response, ENV:683-698:
  ```json
  {"codec":"hevc","transport":"webtransport","urls":["https://10.0.0.5:PORT/media?codec=hevc&token=<tok>", ...],"certHash":"<64 hex chars, lowercase>"}
  ```
  - `answer` is omitted for WT.
- Errors: 409 `not_running`, 409 `no_webtransport`, 409 `no_node`, 400 `bad_codec`, 401.
- Browser reconnect classification (web/apps/portal/src/reconnect.ts:34-44):
  - 401 means sign in again.
  - 409 `not_running` means wait for the environment to run.
  - 408, 429 and 5xx mean retry.
  - Any other 4xx means stop.

### URL and token
- URL format (ENV:705-726): `https://{host}:{wt_port}/media?codec={codec}&token={token}`.
  - IPv6 hosts are bracketed.
  - There is one URL per address, in the streamer's `/info` `addresses` order: routed primary IPv4 first, then the other host addresses, then public addresses (crates/cha-streamer/src/signal.rs:401-413, 588).
  - The browser never URL-encodes the token. It is base64url with one `.`, so it is URL-safe already.
- The token travels only in the query string. There are no custom headers. The server reads `?token=` and `?codec=` from the CONNECT path (WT:145-170).
- Token format (crates/cha-wire/src/lib.rs:896-950): `base64url(JSON claims) "." base64url(Ed25519 sig)`.
  - Signature is over `"cha-media/v1." + <body>`.
  - Claims are `{"env":<env id>,"sub":<user id>,"role":"owner"|"admin","exp":<unix s>}`.
  - TTL is 60 s (ENV:48). It is checked only at session start and is not one-use.
  - Call `/connect` immediately before opening the session, and again on every reconnect.
- Server rejections, from WT:154-177:
  - Bad token: HTTP 403.
  - Missing `codec`, or a codec the streamer lacks: 404.
  - 4 sessions already watching (MAX_SESSIONS=4, crates/cha-streamer/src/viewers.rs:22): 429.

### Certificate pinning
- `serverCertificateHashes: [{algorithm:"sha-256", value: hexToBytes(certHash)}]` (WW:166-171), with `requireUnreliable:true`.
- Native equivalent: a custom TLS verifier that accepts the leaf certificate whose SHA-256 over the DER equals `certHash`.
- Server certificate, from WT:86-101:
  - Self-signed, ECDSA P-256 (wtransport's default identity).
  - SANs are all node host IPs, public IPs and "localhost".
  - `validity_days(13)` (the browser limit is 14 days). It is generated once at streamer start and never rotated while the process lives.
  - `certHash` is the lowercase hex SHA-256 of the leaf DER.
- A native verifier can skip the validity-date check. The browser cannot, so it breaks after 13 days of streamer uptime.
- Pin the hash only. Do not fall back to the system trust store.

### Address selection and timeouts
- Browser (WW:165-187):
  - Tries the URLs sequentially in the order given.
  - Creates a `WebTransport`, then waits for `ready` up to `CONNECT_TIMEOUT_MS = 2500` per URL.
  - On timeout or failure it closes and moves to the next. The first URL that becomes ready wins.
  - If none answer, it posts `closed: "none of the streamer's addresses answered over WebTransport"`.
- A native client may race them in parallel (e.g. 2.5 s happy-eyeballs) and take the first ready. That is compatible, but the first token is valid for 60 s, so do not stagger more than that.
- Correction (found in C2.2): the streamer takes a viewer seat, and the floor as the newest owner session, when the CONNECT request arrives, before it accepts the session and before any control stream (WT:171-189). Every address a native client reaches therefore holds a seat until it is closed (it counts towards the 4, and moves the floor to the newest). `cha-client-stream` races the addresses with a 50 ms head start each and closes the losers at once, rather than starting them all together.
- Outer timeout: `WT_READY_TIMEOUT_MS = 10 000` from worker start to the control stream being open (PT:229, 1292).
- Transport "auto" falls back to WebRTC on failure (PT:362-377). A native client has no fallback.
- Quirk: because the worker sets `ready` only after `createBidirectionalStream()`, the "ready" the page sees means QUIC and H3 are up and the stream is created locally.

## 2. WebTransport session

### Transport and ALPN
- Normal WebTransport over HTTP/3 (ALPN `h3`). The server is `wtransport` 0.7 on Quinn.
- Client side: the `wtransport` client with `with_server_certificate_hashes` (verify the exact API in 0.7), or Quinn + h3 + WT.
- The CONNECT request is `:authority = host:port`, `:path = /media?codec=..&token=..`.
- No `Origin` check, no required headers, no subprotocol. The server never inspects Origin (WT:143-190).
- Server QUIC config (WT:102-110):
  - `max_idle_timeout` 10 s.
  - `keep_alive_interval` 2 s.
  - `datagram_receive_buffer_size` 1 MiB.
  - `datagram_send_buffer_size` 8 MiB (`SEND_BUFFER`).
  - Custom "media window" congestion controller: cwnd 8 MiB, never limits (crates/cha-streamer/src/congestion.rs).
- Datagrams must be supported by the client (QUIC DATAGRAM and H3 datagrams). The server calls `conn.max_datagram_size()` and fails the session if it is None (WT:211-213).
- Sizing: the server fragments to its `max_datagram_size()` minus overhead. It re-reads this before each frame and rebuilds the fragmenter if the PMTU shrank (WT:617-627).
  - The datagram payload before the 16-byte header is about 1100 to 1300 B, depending on the path MTU.
  - The server's value is reported in `hello.stream.maxDatagram`.
  - The client must advertise a large `max_datagram_frame_size` and have enough receive buffer. The browser sets `incomingHighWaterMark = 4096` datagrams (WW:226).
  - Keyframes are hundreds of datagrams in a burst. Size the UDP SO_RCVBUF and the Quinn datagram receive buffer to at least several MB.

### Streams
| Stream | Opened by | Purpose | Framing |
|---|---|---|---|
| bidi #1 (control) | **client** | everything except media | UTF-8 JSON, one object per line terminated by `\n`, both directions |
| unidirectional | nobody | not used | n/a |

- The server accepts the first bidi stream with a 5 s timeout (`CONTROL_STREAM_TIMEOUT`, WT:55, 201-210).
  - On timeout it closes the connection with code 3 and reason "no control stream".
  - A QUIC or WT stream is only visible to the peer after the opener sends data. **The client must write its first line promptly.**
  - The browser's first line is `{"t":"ping",...}`, written immediately in `onControlOpen` (PT:840).
  - Video and audio do not start until the server has accepted the stream (the `Video`/subscription starts after `accept_bi`, WT:260-267).
- Client write: `JSON + "\n"` (WW:249-251). Server read: `BufReader.lines()` (WT:218-225).
- The server writes with `serde_json::to_vec + '\n'` (WT:227-235).
- The browser trims each line and ignores empty ones.
- Control-stream end closes the session (WW:271).
- Connection close codes sent by the server: 1 "replaced" (a new session took the seat), 2 "&lt;codec&gt; encoder stopped", 3 "no control stream".

### Datagram demultiplexing (WW:282-309)
- Header (16 B, §3) byte 0 low nibble = kind. High nibble = wire version = 1 (HDR:2, 131-133). The browser ignores the version, but Rust `decode` rejects != 1.
- Datagrams shorter than 16 B are ignored.
- Kind 1 (Audio): the payload after 16 B goes to the audio path.
- Kind 0 (Video): goes to the video reassembly path.
- Any other kind is dropped.
- Bytes counted for the `r` field of reports include all datagram bytes (audio included, header included).

## 3. Video

### Datagram header (cha-proto, HDR:63-90, 114-125). All multi-byte fields are little-endian.
| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | `(ver<<4)\|kind` | ver=1; kind 0=Video, 1=Audio |
| 1 | 1 | flags | bit0 KEYFRAME, bit1 CRITICAL (unused on WT HW), bit2 PARITY, bit3 CONTINUES, bit4 CONTINUED, bit5 INTRA, bit6 RECOVERY |
| 2 | 1 | `stream` | codec-switch generation, u8 wrapping; starts at 0 |
| 3 | 1 | `fec` | parity fragments **per block** of this frame; 0 = no FEC |
| 4 | 4 | `frame_id` | u32; increments per sent frame; wraps |
| 8 | 2 | `frag_index` | data: 0..frag_count-1; parity: `frag_count + block*fec + r` |
| 10 | 2 | `frag_count` | number of **data** fragments only (parity is not counted) |
| 12 | 4 | `send_ts_us` | µs since the streamer's per-session epoch (`Instant::now()` in `run`, WT:200), truncated to u32 (wraps about 71.6 min) |

- Valid iff `frag_count != 0` and (`frag_index < frag_count` or (PARITY flag and `fec > 0`)) (HDR:156-162).
- `frame_id` is not reset on a codec switch. It restarts at 0 on a new session.
- The server increments `frame_id` only for frames it actually sent (WT:570). Frames the server drops locally leave no gap in ids.

### Fragmentation
- No FEC (`fec=0`): payload per fragment is `max_datagram - 16`. The frame is a plain concatenation of fragments in `frag_index` order (REA:126-154).
- With FEC (`fec>0`), per `fragment_fec` (REA:65-121):
  - `shard = max_datagram - 16 - 4`. Data fragments carry `shard` bytes each. The last may be shorter.
  - `k = ceil(len/shard)`, and `frag_count = k`.
  - Data fragments are emitted first, then parity.
  - Parity fragments have flags = base flags | PARITY, and header `fec` = m.
  - **Parity payload = `frame_len` as u32 LE (4 B) followed by the parity shard (`shard` bytes)**.
  - Parity fragment `frag_index = k + b*m + r`. Block `b` covers data fragments `[b*128, min((b+1)*128, k))`. `r` is the parity row 0..m-1.
  - Total datagrams = `k + ceil(k/128)*m`. Per-datagram expectation used for loss accounting: `total + fec*ceil(total/128)` (WW:451).
- PyroWave (INTRA) frames never use FEC (`fec=0`). See "PyroWave" below.

### FEC code (crates/cha-proto/src/fec.rs; browser port web/packages/player/src/fec.ts, identical)
- Systematic Reed-Solomon over GF(2^8), poly 0x11d, Cauchy matrix.
- `BLOCK = 128` data shards per block at most, `MAX_PARITY = 64`.
- Coefficient for parity row `r`, data shard index `i` within the block, `m` = parity count per block: `inv((r ^ (m + i)) & 0xff)` (fec.rs:66-68).
- Parity shard `r` = XOR over `i` of `coef(r,i,m) * data_i`. A shorter data shard counts as zero-padded.
- Recovery (fec.ts:63-103, `fec::recover`):
  - Needs, per block, at least as many parity shards as missing data shards (any rows).
  - It builds the syndromes and Gauss-Jordan solves the e×e Cauchy system.
  - Reconstructed shards are `shardLen` long (zero-padded).
  - Trim the final assembled frame to `frame_len` from the parity prefix. The browser sets `bytes = frameLen` and clamps the final copy (WW:401-420).
- Browser recovery path (WW:351-404): attempted whenever a parity fragment arrives, and after each data fragment while `fec>0 && received<total`. A frame is complete when all data shards are present, either received or rebuilt.
- Sender parity sizing (WT:577-593): `parity_for(k, loss, failure).min(ceil(k/2))`.
  - `loss` is the page-reported loss held for 5 s and floored at 0.3% (`update_fec`, WT:792-813).
  - `failure` is 1e-4 at 60 fps, scaled by 60/fps, for normal frames. It is 1e-6 for keyframes and recovery frames, which also use at least 0.3% loss.
  - On a clean link, `fec` is 0 for non-resync frames and nonzero only on keyframes and recovery frames.
  - Some frames therefore arrive without parity. The client must handle both.

### Reassembly and delivery (hardware codecs; WW:311-534)
Per frame id, keep `parts[frag_count]` plus `parity[fec*ceil(total/128)]`.
- Drop duplicates. Drop frames already given up on, i.e. `before(id, next)` using u32 wrap-aware comparison.
- A frame is complete when all data fragments are present (or rebuilt). Concatenate `parts` in order and trim to the frame length.
- `key` = KEYFRAME flag from the **first-seen** datagram of the frame. `recovery` = RECOVERY flag from the same.
- **Stream (codec switch):** if `header.stream != current`:
  - Accept it only if it is newer (`(b-a)&0xff in 1..0x7f`), else drop the datagram.
  - On accept, clear all partials and completes, set `next=null`, `needKey=true`.
- **In-order delivery** (`deliver()`, WW:490-534):
  - Frames go out by id from `next`.
  - Start condition: `next==null || needKey`. Start at the **newest complete frame** that is a keyframe, or a RECOVERY frame whose `id >= lostFrom` (the lost id).
  - On start, discard older partials and completes, and set `next=start`.
  - Then emit consecutive ids. Wait while `complete[next]` is absent. A frame past a gap is held.
  - Cap on frames held while waiting for a keyframe: 240 (`MAX_WAITING`). Over the cap, trim to the newest 120.

### What counts as loss (`expire()` every 10 ms, WW:573-617)
For the next-needed id `missing=next`:
- "stale" if any held complete frame has been complete longer than `ORDER_WAIT_MS = 15 ms`, i.e. it was overtaken; or the partial for `missing` has had no datagram for `SILENCE_MS = 250 ms`.
- A large keyframe still arriving and not overtaken is waited for.
- When stale:
  - Delete all partials with id >= missing and all completes.
  - Count `lost`.
  - Set `needKey=true`.
  - Remember `lostFrom = missing` if not already set.
  - Call `askToResync()`.

### Loss recovery messages (WW:619-649, WT:361-363, 481-509)
- `askToResync()` is rate limited to `KEYFRAME_RETRY_MS = 250 ms`.
- First ask, with `lostFrom` set and RFI not yet asked: send `{"t":"rfi","id":<lostFrom frame_id>}`. This is reference invalidation (RFI).
- Further asks (or no `lostFrom`): send `{"t":"keyframe"}`, repeated every 250 ms until a start frame arrives.
- If the recovery frame itself was lost (a partial with the RECOVERY flag went stale or was overtaken), reset `rfiAsked` and ask again immediately (`retryLostRecovery`).
- Server side on `rfi`:
  - Looks up `id` in its last `SENT_FRAMES=256` sent ring (`(frame_id, encoder_index)`).
  - If found, `lost = min(lost, index)` and `media.request_invalidate(codec, index)`.
  - If not found (too old), it requests a keyframe.
  - Asks are at most every `KEYFRAME_RETRY = 300 ms`.
  - The encoder tries `invalidate_from`. NVENC `DPB_FRAMES = 8` (reach about 8 frames back). On success the next frame is non-key with the RECOVERY flag. Otherwise it sends a keyframe.
  - While `lost` is set, a recovery frame (`frame.recovery <= lost`) or a key resets it.
- Server drop policy (`Video::send`, WT:514-575):
  - If the QUIC datagram send buffer cannot hold the entire frame (`budget = datagrams*(16+8+payload_capacity)`), the whole frame is dropped.
  - Dependents are dropped too: `resync=true`, and it sends nothing but a keyframe or recovery frame until then.
  - It asks the encoder to resync, retrying at the 50 ms pace tick while not holding.
  - The client sees a gap in time but not in ids.
- `{"t":"keyframe"}` also requests a keyframe in other cases: decoder error (PT:1455), video track rebuild, and the first decoder start.

### Parameter sets and decoder input
- Payload is one access unit per frame.
  - H.264 and HEVC: Annex-B (start codes), with VPS/SPS/PPS in-band on every IDR. `outputAUD=0`. Streamer-side x264 and other encoders also set `repeat-headers`, `annexb` (x264.rs:254-257).
  - AV1: low-overhead OBU stream (`outputAnnexBFormat=0`), sequence header repeated on keyframes (nvenc.rs:733-735). It is not Annex-B and there is no length framing.
- The browser's WebCodecs configs (PT:34-36) are: H.264 `avc1.640033` (High, L5.1), HEVC `hev1.1.6.L153.B0` (Main, L5.1), AV1 `av01.0.13M.08`. Plus `hardwareAcceleration:"prefer-hardware"`, `optimizeForLatency:true`. No `description`, so Annex-B passes through.
- `EncodedVideoChunk{type: key?"key":"delta", timestamp: frame_id, data}` (PT:1513).
- Chunks before the first key are skipped (`sawKey`).
- On decoder error: rebuild the decoder, set `sawKey=false`, send `{"t":"keyframe"}`. Three failures in 10 s means reconnect (`RECOVERY_LIMIT/WINDOW`).
- Native/VideoToolbox: split Annex-B into NALs, take VPS/SPS/PPS from the IDR, build the format description (avcC/hvcC), convert to length-prefixed AVCC/HVCC samples, and treat a changed SPS (after a resize) as a new session.
- Recovery frames (RECOVERY flag) are P-frames that reference only frames before the lost one. The decoder accepts them without a keyframe.

### Resolution changes
- A `resize` forces an encoder reconfigure plus a keyframe (media.rs:829-832).
- Expect a new SPS and a new frame size mid-stream on the same `stream` number.
- `fps` changes are in place, with no keyframe (media.rs:385).

### PyroWave (INTRA path)
- Flags are `KEYFRAME | INTRA` on every fragment, `fec=0`, `frame_id` shared by all fragments of a frame (WT:917-990).
- Each datagram carries one wavelet packet, or a slice of one:
  - A packet larger than `max_datagram-16` is split. All parts but the last get CONTINUES. All parts but the first get CONTINUED.
- Client (WW:411-424, 536-571, 573-585): the frame goes out the moment it is complete.
  - A frame missing datagrams at `FRAME_DEADLINE_MS = 60` after its first fragment is delivered with only the **whole** packets.
  - A packet is whole if it starts at a fragment not CONTINUED and every following fragment up to the last with CONTINUES is present.
  - Concatenate whole packets and pass to the PyroWave decoder with `partial=true`.
  - Frames older than `lastDelivered` are dropped. Frames count as `lost` only if no packet was usable.
  - No keyframe or RFI is needed.
- The first packet starts with an 8-byte sequence header (pyro.ts:96-112):
  - `w0 = u32 LE`. Bit 31 set marks a start-of-frame header.
  - `width = (w0 & 0x3fff) + 1`, `height = ((w0>>14) & 0x3fff) + 1`.
  - `w1 = u32 LE` at offset 4. `chroma = ((w1>>26)&1) ? 4:4:4 : 4:2:0`.
- The native Rust PyroWave decoder is the `cha-pyrowave` crate (plan item C2.3).

## 4. Audio

- Datagram kind 1 (WT:992-1015). Header: `flags=0`, `stream=0`, `fec=0`, `frag_count=1`, `frag_index=0`.
- `frame_id` = `samples_at_packet_start / 480` (a 10 ms index, u32). `send_ts_us` is the same epoch as video.
- Payload is a single Opus packet: 48 kHz, stereo, 10 ms (480 samples/channel), 128 kbit/s (audio/mod.rs:23-28, 66).
- Sent every 10 ms while a session is subscribed, one datagram each, with no FEC.
- Decoder: WebCodecs `{codec:"opus", sampleRate:48000, numberOfChannels:2}`. Chunk timestamp = `id*10000` µs, type "key" (PT:1571, 1657). A native client should use libopus at 48 kHz/2 ch.
- Jitter buffer (web/packages/player/src/audio-jitter.ts):
  - Plays in id order. A packet is held until `send_ts + base_transit + delay`.
  - `base` = minimum (arrival minus unwrapped send time) over a 10 s window.
  - `delay` = p99 of (transit minus base) over 5 s. It is 0 if under a 1.5 ms deadband, max 80 ms (`MAX_DELAY_MS`), rises instantly and decays at 2 ms/s.
  - A packet more than 50 ms past its slot is dropped (`MAX_LATE_MS`). An id already played or duplicated is dropped. Queue max 200.
  - A missing packet at its slot is concealed with 10 ms of silence. WebCodecs has no Opus PLC; a native client may use libopus PLC.
  - A gap larger than 100 ids (`MAX_SKIP`) is skipped without concealment.
  - If the id jumps back by more than 1000, reset (restart of the count).
- Output FIFO (audio-fifo.ts): prefill 480+128 frames, cap 60 ms extra beyond prefill, drops oldest on overflow, emits silence plus underrun count on empty.
- Audio is not on the rate-controller feedback path except via byte counts in `r`.

## 5. Control messages (JSON lines on the control stream)

All objects have a `"t"` string tag.

### Server to client (CT:27-155, WT:245-258)
- **hello** (first line):
  ```json
  {"t":"hello","stream":{"codec":"hevc","width":2560,"height":1440,"input":true,"audio":true,"gamepads":true,"fps":60,"overlay":<null|0..4|"custom">,"transport":"webtransport","maxDatagram":1200}}
  ```
  - It carries no session id. The browser only reads `stream.fps` and `stream.overlay` (PT:1036). `audio`/`gamepads` say what the streamer supports.
- **floor**: `{"t":"floor","control":bool,"viewers":N}`. Sent right after hello and on every change.
  - Only the session with `control:true` has its input, resize, clipboard, cursor, fps and overlay messages applied. Others are silently dropped (CT:226-232).
  - See "floor rules" below.
- **pong**: `{"t":"pong","c":<echoed number>,"s_us":<u64 µs since streamer session epoch>}`.
- **codec**: `{"t":"codec","codec":"h264","stream":N}`, plus `"error":"…"` on failure (then codec/stream are what still runs). Answer to a codec switch (§8).
- **fps**: `{"t":"fps","fps":N}`, plus `"error"` if refused. The valid rates are 60, 90 and 120.
- **overlay**: `{"t":"overlay","level":0..4|"custom","error"?}`. `level` is omitted if the app has no overlay.
- **resized**: `{"t":"resized","w":W,"h":H,"s_us":N}`. Answer to resize, with the size actually applied.
- **stats** every 500 ms. `{"t":"stats","elapsed_ms":N, frames_generated, frames_sent, bytes_sent, keyframes, keyframe_requests, frames_dropped?, first_frame_ms, composite_to_encoded_ms_p50/p99, encoded_to_sent_us_p50/p99, frame_interval_ms_p50/p99, fps?, overlay?, target_mbps?, queue_ms?, inputs, inputs_unmapped, resizes, audio_packets, audio_bytes}`.
  - `frames_sent` and `fps` are used by the browser for the send-rate display.
- **system** about every 1 s: `{"t":"system","cpu","cores","load1","mem_used","mem_total", gpu?, vram_used?, vram_total?, enc?, dec?, temp?, power?, power_limit?, clock?}` (crates/cha-streamer/src/system.rs:18-43). Informational.
- **clipboard**: `{"t":"clipboard","text":"..."}`. App copied text; sent to the controller only.
- **cursor**: `{"t":"cursor","kind":"hidden"|"named"|"image", ...}`.
  - `named` has `name` (a CSS cursor name).
  - `image` has `id` (16 hex chars), `w`, `h`, `x`, `y` (hotspot), and `rgba` (base64 of `w*h*4` straight-alpha RGBA bytes). `rgba` is present **only the first time that `id` is sent in the session**; later messages carry `id`, `w`, `h`, `x`, `y` only. Cache by id.
  - Images larger than 128 px are sent as `default` (compositor/cursor.rs:121).
- **pointer**: `{"t":"pointer","x":0..1,"y":0..1,"drawn":bool}`. Sent only to non-controllers, to draw the controller's pointer.
- **status**: `{"t":"status","label":"…","done"?:n,"total"?:n,"unit"?:"MB"}`. An empty `{"t":"status"}` means cleared. Sent to every viewer.
- Pad events (controller only; rate-limited to 16 ms per pad per kind; led/players/trigger are replayed when the session gains control):
  - **rumble**: `{"t":"rumble","i":slot,"lo":0..1,"hi":0..1,"ms":N}`. `ms=0` stops. `lo` is the strong motor, `hi` the weak one.
  - **haptic**: `{"t":"haptic","i","side":"left"|"right","amp","on_us","off_us","count"}`.
  - **led**: `{"t":"led","i","r","g","b"}`.
  - **players**: `{"t":"players","i","mask"}`.
  - **trigger**: `{"t":"trigger","i","side","effect":[11 bytes]}`.
- Not sent over WT: `sent`, `probe` reply (only if input has a `probe`), `done`.

### Client to server (CT:213-308)
- **ping**: `{"t":"ping","c":<number>}`. `c` is any f64, echoed. Browser: `performance.timeOrigin + performance.now()` in ms.
- **resize**: `{"t":"resize","w":W,"h":H}`. Controller only. See §8.
- **cursor**: `{"t":"cursor","client":bool}`. `true` means this client draws the cursor and the picture is composed without it. Browser sends `client=!pointerLocked` on connect, on pointer-lock change, on window resize, and when gaining the floor (PT:819, 994). Controller only.
- **clipboard**: `{"t":"clipboard","text":"..."}`. Send **before** the paste keystroke is released (see §6). Controller only.
- **keyframe**: `{"t":"keyframe"}` and **rfi**: `{"t":"rfi","id":<u32>}`. See §3.
- **codec**: `{"t":"codec","codec":"h264"|"hevc"|"av1"|"pyrowave420"|"pyrowave444"}`. See §8.
- **fps**: `{"t":"fps","fps":60|90|120}`. **overlay**: `{"t":"overlay","level":0..4}`. Both are controller-only and answered by a message of the same type (timeouts 3 s).
- **take_control**: `{"t":"take_control"}`. Succeeds for owner and admin roles only (viewers.rs:92-101).
- **report**: see §7.
- **input**: see §6.
- Unknown or garbled lines are ignored.

### Floor rules (viewers.rs:1-12, 120-190)
- Up to 4 sessions per environment.
- The newest owner session takes the floor on join. An admin takes it only if it is empty or held by a lower role.
- When the controller leaves, the floor goes to the newest session of the highest role that may control.
- On gaining control, the browser re-sends size, cursor mode and all pad states (PT:988-997; `pads.resync()` clears `sent`).
- On losing it, input stops (`sendInput` is a no-op when `!hasControl`, PT:779).

## 6. Input (all on the control stream, one JSON line per event, no sequence numbers)

Envelope: `{"t":"input","k":"<kind>",...}`. Order is preserved because it is a single reliable ordered stream (keyboard and pads interleave correctly).

| k | Fields | Notes |
|---|---|---|
| `move` | `x`, `y` (0..1) | Absolute. Normalized to the picture rectangle (letterbox excluded, input.ts:195-201). The server multiplies by output width/height after clamping to 0..1. Sent on every pointer move. Also sent before a button-down. |
| `rel` | `dx`, `dy` | Relative, while the pointer is locked (games). Units are stream pixels: the browser's `movementX` times (video pixel width / displayed width). Sent only if non-zero. |
| `button` | `b`, `down` | `b`: 0 left (BTN_LEFT 0x110), 1 middle (0x112), 2 right (0x111), 3 back/side (0x113), 4 forward/extra (0x114). Others are unmapped (ignored). |
| `wheel` | `dx`, `dy` | Pixels as the browser reports (`deltaX`, `deltaY`; line mode ×40, page mode ×800). The server treats them as scroll pixels (`Axis`). Note browser sign convention: positive dy is scroll down; sent unmodified. |
| `key` | `code`, `down` | `code` is a DOM `KeyboardEvent.code` string; see below. |
| `pad` | see below | Gamepad state snapshot. |

- Optional `probe: <u64>` on any input is echoed as `{"t":"probe","id","s_us"}`. It is only for latency tests.
- **Keys** (crates/cha-streamer/src/input.rs:67-154). Supported codes:
  - `KeyA`..`KeyZ`, `Digit0`..`Digit9`, `F1`..`F12`.
  - `Escape, Minus, Equal, Backspace, Tab, BracketLeft, BracketRight, Enter, ControlLeft, Semicolon, Quote, Backquote, ShiftLeft, Backslash, Comma, Period, Slash, ShiftRight, NumpadMultiply, AltLeft, Space, CapsLock, NumLock, ScrollLock, Numpad0..9, NumpadSubtract, NumpadAdd, NumpadDecimal, IntlBackslash, NumpadEnter, ControlRight, NumpadDivide, PrintScreen, AltRight, Home, ArrowUp, PageUp, ArrowLeft, ArrowRight, End, ArrowDown, PageDown, Insert, Delete, Pause, MetaLeft, MetaRight, ContextMenu`.
  - Anything else is unmapped and ignored. Map macOS virtual keycodes to these `code` strings in the Rust client.
- Browser behaviour you may replicate (input.ts):
  - Key auto-repeat is suppressed; the app repeats held keys itself.
  - On a Mac, Cmd is sent as Ctrl (`ControlLeft` or `ControlRight`) by default (`commandAsControl`, input.ts:46,244).
  - Keys pressed while Cmd was down never get a keyup on macOS, so release them when Cmd comes up.
  - On window or pointer-lock loss, send `down:false` for all held keys and buttons.
  - Paste: on Ctrl/Cmd+V the V keydown is held until the local clipboard text is sent as `{"t":"clipboard","text":...}` (timeout 150 ms, `PASTE_WAIT_MS`), then the held V keydown is sent.
  - There is no text/IME input message yet (comment at input.ts:353-355).
- **Gamepad** (`k:"pad"`; controllers/manager.ts:47-60, gamepad.rs:226-252):
  ```json
  {"t":"input","k":"pad","i":<slot 0..3>,"b":[24 numbers 0..1],"a":[LX,LY,RX,RY],"ty":"xbox|playstation|switch|steam|generic","gyro":[x,y,z]?,"accel":[x,y,z]?,"touch":[{"id","x","y","down":true}]?,"bat":0..1?}
  {"t":"input","k":"pad","i":<slot>,"gone":true}
  ```
  - `b` indices (types.ts:8-40): 0 south, 1 east, 2 west, 3 north, 4 LB, 5 RB, 6 LT (analog), 7 RT (analog), 8 back, 9 start, 10 L3, 11 R3, 12 up, 13 down, 14 left, 15 right, 16 guide, then extras 17 touchpad click, 18 left pad click, 19..22 L4 R4 L5 R5, 23 mute.
  - `a` is -1..1, with down and right positive.
  - Values are rounded to 3 decimals (gyro in rad/s, accel in m/s²). Buttons are pressed above 0.5 for digital ones. Triggers are analog 0..255 on the Xbox target.
  - It is a full snapshot, **sent only when something changed** (polled every 4 ms, `POLL_MS`), plus on gaining the floor.
  - Slots: the first free of 4, stable while the pad stays connected. `gone` means the pad is released and goes back to rest.
  - **No sequence numbers or timestamps.** Ordering comes from the stream. A page that loses focus sends `gone` for every slot.
  - `ty` is optional; the field is `ty`, not `t`.

## 7. Feedback the rate controller depends on

### Message and cadence
`{"t":"report","r":<Mbit/s>,"d":<ms>,"l":<fraction>}`, sent every **100 ms** from a timer in the worker (WW:244, 458-487). Each of `r`, `d`, `l` is optional.

| Field | Definition | Rounding |
|---|---|---|
| `r` | bytes of **all datagrams** (video+audio, with headers) received over roughly the last 200 ms, in Mbit/s. Computed from per-interval byte buckets covering <=250 ms, divided by the actual span. | 2 decimals |
| `d` | the **median** of per-frame (completion time minus send time) over frames completed in the last 150 ms, in ms. Sent time = `send_ts_us` mapped to the local clock via the ping offset. Recorded at the instant the frame completes (`noteDelay`, WW:440-445). Omitted if no frame completed in that window or the clock offset is unknown. | 1 decimal |
| `l` | missing / expected datagrams over frames that **settled** in the last 1 s. | 4 decimals |

- `l` details: a frame settles 200 ms (`REORDER_MS`) after its first datagram arrived. `expected = frag_count + fec*ceil(frag_count/128)`; `missing = max(0, expected-got)`. Late stragglers within 200 ms are counted as received, and FEC-rebuilt frames still count the original missing fragments only if they never came. It is omitted if the window is empty.
- The delay `d` is used as a difference over a windowed minimum floor (rate.rs:98-117, 195-199). **Any constant clock offset cancels.** A native client without exact clock sync can compute `d = local_arrival − server_ts` with any fixed offset (handle the 32-bit wrap), as long as the offset stays constant.

### Clock sync
- Client sends `ping {c}` immediately on connect, then every 1000 ms (PT:840-843). Pong gives `s_us`.
- On a pong: `rtt = now_ms − c`; if this is the lowest RTT so far, `offset_ms = c + rtt/2 − s_us/1000` (= local clock − server clock) (PT:959-967). Pass it to the worker.
- Server stamp to local time: `serverUs = nowUs-based unwrap` of the u32 `ts` to the nearest wrap around `(now − offset)*1000`, then `/1000 + offset` (WW:136-145).
- The offset resets on reconnect because each session has its own server epoch (PT:578-582).

### Server use and missing-feedback behaviour (rate.rs, wt.rs:815-904)
- Updated every `PACE_INTERVAL = 50 ms` from `Pacing::tick`.
- Sample inputs:
  - Server-side: QUIC smoothed RTT, QUIC sent/lost packet counts (2 s window), tx bitrate, and `backlog_ms`.
  - `backlog_ms = backlog_bytes / frame_bytes × 1000 / fps`, where `backlog_bytes` is the datagram send buffer in use (8 MiB minus free space).
  - Client-supplied: `report.d`, `report.r`, `report.l`, each used only if received within the last **300 ms** (`REPORT_FRESH`).
- Encoder hold: when `backlog_ms > 25 ms` (`HOLD_MS`) the encoder is held (frames skipped); resync is asked after a drop.
- Verdicts:
  - Hard back-off to 0.85 × the received rate (0.7 × sent if there is no `r`): queue (backlog or delay growth over its floor) above 40 ms, or loss above 10%.
  - Soft back-off ×0.9 (max once per 200 ms): backlog above 20 ms or delay growth above 15 ms.
  - Hold: loss above 2%.
  - Climb: 50% per second after a calm second (10% near the last ceiling), up to a cap of 2 × the average send rate and up to the session ceiling.
  - Stall: reports exist but `r` is below 25% of what is sent, for up to 400 ms (`STALL_GRACE`), then normal rules apply.
- Floor: 1.5 Mbit/s (`MIN_BPS`). Video share is 0.92 of the target, less FEC overhead (`VIDEO_SHARE`).
- If the client sends **no reports**:
  - `delivery_ms` is None, so the delay-growth signal falls back to QUIC RTT growth over its 10 s minimum.
  - `loss` falls back to QUIC `lost_packets/sent_packets` over 2 s (needs ≥20 packets sent).
  - `rx_bps` None means no stall detection. Hard back-off uses 0.7 × tx.
  - FEC sizing uses the QUIC loss estimate.
  - The stream still works but reacts later and less accurately.
- If the client stops reporting after having reported, `rx_bps=None` triggers the stall branch after 400 ms and then backs off from tx; a client that stalls is treated as congestion.
- The browser's report timer runs even when the tab is hidden (a worker). When the page is hidden, a delay is not reported because no frames are decoded; the streamer holds its rate (PT:870-872, WW:33-34).

## 8. Codec negotiation, resolution and fps

### Codec
- The client chooses the codec in the `/connect` request, and it is repeated as `?codec=` in the URL. The streamer validates it against `media.codecs()` (WT:162-169).
- The browser's preference order is hevc, h264, av1 (PT:62), restricted to what the device offers (`codecs` in the environment view) and what the browser supports. PyroWave is added when WebGPU supports it. Browser default `codec`: `supportedCodecs()[0]`.
- **In-session switch.**
  - Client sends `{"t":"codec","codec":"<name>"}`.
  - Server subscribes to the other encoder (cold start about 0.2 s) and keeps sending the old stream until the new encoder's first frame, which is a keyframe.
  - Then it sends `{"t":"codec","codec":"<new>","stream":N}`, where N = old+1 (u8 wrapping), and the video datagrams from that frame on carry `stream=N`. `frame_id` continues counting.
  - Asking for the current codec returns the same stream, with no error. Unknown or unoffered codec: `error:"<name> isn't offered here"`.
  - The browser creates the new decoder before asking, switches to it on the first frame with a newer `stream`, and times out after 3000 ms (`SWITCH_TIMEOUT_MS`), then reconnects with the new codec.
  - The client must have a decoder ready for the new codec and discard the old stream's stragglers (newer-stream test, §3).

### Resolution
- The streamer does not take a decode-size capability. The client tells it the **output size** with `{"t":"resize","w","h"}` (controller only). The server answers `{"t":"resized","w","h","s_us"}`.
- Server rounding (compositor/mod.rs:87-97): each dimension clamped to MIN 320×240 and MAX 3840×2160, then `& !7` (floor to a multiple of 8).
- Browser policy (PT:927-948):
  - Size = element size × devicePixelRatio, scaled down to fit `maxSize` (default 2560×1440), floored to multiples of 8.
  - Debounce 250 ms. Skip if either dimension is under 320 or 240 (w<320 || h<240), or if unchanged since the last request.
  - Never send for `fixedSize` apps (e.g. Steam gamescope; the picture is letterboxed instead) or when not controller.
- Initial size is the environment's size. It is 2560×1440 at launch (ENV:52-53) and is reported in `hello.stream.width/height`.
- The encoder resizes at the next frame and sends a keyframe (media.rs:829-832). The new size is only discoverable from the SPS or the `resized` message.
- Letterbox, aspect and mouse mapping use the decoded frame size. Reuse the last `resize` size as a fallback.

### FPS
- The environment's fps is a server setting: 60, 90 or 120. It is chosen at launch and reported in `hello.stream.fps` and in each `stats`.
- It changes only with `{"t":"fps","fps":N}` (controller only). The encoder is reconfigured in place on its next frame, with no keyframe. Bitrate scales by `(fps/60)^0.75` for NVENC (framerate.rs:26,45).
- The client does not otherwise declare a rate. The server sends only when the picture changes, so an idle desktop sends almost nothing.

## 9. Liveness, health and reconnect

### Client heartbeat and liveness (liveness.ts, PT:738-772)
- Ping every 1000 ms. The server answers every ping and also sends stats every 500 ms and system every 1 s, so a live session always has inbound control traffic.
- `lastInbound` is bumped by any control line, any completed video frame and any audio datagram. It is checked every second.
- If the page is **visible** and no data has arrived for **4000 ms** (`SILENCE_MS`), the connection is declared dead and the page reconnects.
- A check gap over 2.5 s (`FROZEN_TICK_MS`) means the page was asleep: grant a fresh 4 s grace instead of judging stale numbers.
- On returning to the foreground with the last pong older than 3000 ms, send an immediate ping and reconnect if no pong within 1500 ms.
- Hidden pages are never judged.
- A native client should use the same 4 s silence rule on the control stream.

### Server-side liveness
- QUIC idle timeout is 10 s (with a 2 s server keep-alive, so the client must ack). The client should also set a keep-alive.
- The server ends a session on:
  - control-stream EOF (WT:356);
  - connection close (WT:406);
  - being replaced (close code 1, `stopped` signal).
- No app-level heartbeat timeout exists server side.

### Reconnect (PT, SessionView.vue, reconnect.ts)
- The browser's Player reports `disconnected`. The page calls `connect()` again, which means a **new `/connect` call, a new token, a new QUIC session**.
- Backoff before each retry: 1, 2, 4, 8, then 15 s for as long as it takes (`BACKOFF_MS`). It reconnects immediately on `online`, `pageshow` or when the page becomes visible.
- A new session restarts `frame_id` at 0 and `stream` at 0, with a new server clock epoch. Each session needs a fresh clock sync.
- The old session's seat is freed when it closes. Reconnecting while the old one is still open may hit the 4-session limit; the old one is "replaced" only in the sense of the floor, not closed by the new session.

## 10. What a native client can skip, and what could block it

### Skippable
- WebCodecs, `MediaStreamTrackGenerator`, AudioWorklet, Web Audio, WebHID and the Gamepad API wrappers. Use VideoToolbox, a Metal/CoreAudio output, and GCController/IOKit.
- Worker and main-thread split, `postMessage`, transferables, and `requestVideoFrameCallback` stats.
- `incomingHighWaterMark` (use equivalent receive-buffer sizing).
- Fallback to WebRTC.
- Cursor handling via CSS `cursor`, the `image-set` scaling, and the viewer pointer overlay (only needed when not controller).
- The `system`, `stats`, `status`, `overlay` and `pointer` messages: ignore, or show for diagnostics.
- Probe/latency measurement (`probe` field) and recording.

### Blockers and cautions
- **No Origin, header, or ALPN gate beyond `h3`.** The portal checks nothing at WT connect time, only the token and codec in the query.
- **Certificate.** The browser requires ECDSA P-256 and validity of at most 14 days for hash-pinned certs. The streamer issues 13 days, from process start. A native verifier pinned by hash can skip the date check; do not rely on the system trust store.
- **Control stream must receive the first write quickly** (5 s limit), or the session is closed with code 3.
- **Token TTL is 60 s.** Connect and open QUIC in one go. Do not cache the `/connect` response across reconnects.
- **FEC parity datagrams would break `cha_proto::Reassembler`** (see §0).
- **Datagram size.** The server fragments to the client's QUIC datagram limits. The client must support datagrams at about 1200 B or larger and a big receive queue; very small advertised limits break frame sizing (`max > HEADER_LEN + 4`).
- **Annex-B to AVCC/HVCC conversion and format-description rebuilds** on resize are required for VideoToolbox. AV1 is plain OBU with no description needed for dav1d-style decoders. VideoToolbox AV1 hardware decode needs recent Apple silicon.
- **The `report` timer and clock sync must keep running while the window is in the background**, or the streamer will back off.
- **Unmapped keys and buttons are dropped silently.** There is no Meta/Cmd handling server-side, so decide the Cmd to Ctrl mapping client-side.
- **The `hello` message has no negotiation fields for client capabilities.** Do not wait for a `hello` reply to anything; the first server line is `hello`, then `floor`.
- **Clipboard from the app and cursor image data go to the controller only**, and `rgba` arrives once per id per session, so keep the cache per session.

## Quick implementation order (suggested)
1. HTTP: launch, poll, connect. Parse `urls` and `certHash`.
2. QUIC/H3 WebTransport with hash pinning. Open the bidi stream and write `ping` at once. Read the `hello`/`floor` lines.
3. Datagram loop: parse the 16 B header. Video reassembly with FEC and in-order delivery (port WW). Audio to the jitter buffer and Opus.
4. Decoder: Annex-B to VideoToolbox, gate on keyframe or RECOVERY, `rfi`/`keyframe` with the 250 ms retry.
5. 100 ms `report`, 1 s `ping`, 4 s silence watchdog.
6. Input JSON, resize, cursor mode, clipboard.
7. Codec switch, fps, floor, pad events, then PyroWave.
