# Vibepollo's PyroWave contract (research, 2026-10-07)

The research for PLAN Phase 3's "Vibepollo PyroWave contract → browser WebGPU decode", which the owner chose to build before a Windows host is available. Vibepollo is GPL-3: everything here is from its prose documentation, wire constants and field names (reference-only, `docs/PROVENANCE.md`); nothing is to be ported from its source. Unverified against a live host: see §6.

Method: GitHub web and API pages plus the repo's own docs. Only documentation was read, no source code. The one exception is the upstream PyroWave commit diffs (MIT). Nothing below is code; identifiers and constants only. **Confirmed** means stated in the Vibepollo docs or a clear source. **Inferred** means my reading.

Headline: Vibepollo documents its PyroWave contract in a single file, [`docs/pyrowave-protocol.md`](https://github.com/Nonary/Vibepollo/blob/master/docs/pyrowave-protocol.md). It is detailed enough to build a client from without reading any GPL code. The contract is very young: all ten commits that touch that doc are dated 2026-09-25 to 2026-09-30 ([commit list](https://api.github.com/repos/Nonary/Vibepollo/commits?path=docs/pyrowave-protocol.md&per_page=30)). It has no version field, so everything depends on matching a pinned upstream commit.

---

## 1. What Vibepollo is

| Item | Finding | Status | Source |
|---|---|---|---|
| Repo | github.com/Nonary/Vibepollo, default branch `master`, pushed 2026-10-06, 104 open issues | confirmed | [repo](https://github.com/Nonary/Vibepollo), [API](https://api.github.com/repos/Nonary/Vibepollo) |
| Maintainer | Nonary (Chase Payne authors the PyroWave commits) | confirmed | same |
| Licence | GPL-3.0 | confirmed | API |
| Lineage | Fork of ClassicOldSong/Apollo, itself a Sunshine fork. Windows host, plus a Linux beta (Arch/CachyOS). The repo positions it as a replacement for Sunshine or Apollo. | confirmed | [README](https://github.com/Nonary/Vibepollo) |
| PyroWave release | 2.0.0 (2026-09-30), "PyroWave on Windows and Linux". It names **Nonary's VRR Moonlight Client fork** as the client. Stock Moonlight never sees PyroWave. | confirmed | [releases](https://api.github.com/repos/Nonary/Vibepollo/releases?per_page=8) |
| Reference client | github.com/Nonary/moonlight-qt: a GPL-3.0 fork of moonlight-qt, pushed 2026-10-07, 15 open issues. Open PyroWave issues are all GPU/OS decode compatibility: [#10 Intel Iris Xe](https://github.com/Nonary/moonlight-qt/issues/10), [#14 Intel UHD 730](https://github.com/Nonary/moonlight-qt/issues/14), [#19 Snapdragon X](https://github.com/Nonary/moonlight-qt/issues/19). | confirmed | [repo API](https://api.github.com/repos/Nonary/moonlight-qt), [issue search](https://api.github.com/search/issues?q=repo:Nonary/moonlight-qt+pyrowave) |
| Artemis | Artemis is not mentioned in Vibepollo's docs. A third-party port exists: joemossjr16/artemis-android-pyrowave, with a Vulkan renderer and a moonlight-common-c `pyrowave` branch, listed in [joemossjr16/pyrowave-streaming](https://raw.githubusercontent.com/joemossjr16/pyrowave-streaming/main/README.md). That README describes an earlier, different framing; see section 3. | confirmed | same |
| Other hosts | Wolf [PR #517](https://github.com/games-on-whales/wolf/pull/517) (open, by bscubed, not merged) adds PyroWave. It says only Aurora can request it. It does not say whether it matches Vibepollo's wire format. | confirmed / format unknown | [PR API](https://api.github.com/repos/games-on-whales/wolf/pulls/517) |
| Earlier history | The PyroWave feature request [#308](https://github.com/Nonary/Vibepollo/issues/308) was closed "not planned" on 2026-08-19 and moved to [Discussion #437](https://github.com/Nonary/Vibepollo/discussions/437). It shipped five weeks later, in 2.0.0. The request cited an AI-assisted Sunshine/Moonlight-Qt proof of concept by "azafrob", branch `pyrowave-codec`. | confirmed | issue and discussion |

---

## 2. Negotiation (all from `docs/pyrowave-protocol.md`, section "Negotiation")

### 2.1 `/serverinfo` (paired HTTPS)

All confirmed.

- `ServerCodecModeSupport` gains four bits:

| Bit | Name | Meaning |
|---|---|---|
| `0x00800000` | `SCM_PYROWAVE` | 8-bit 4:2:0 |
| `0x01000000` | `SCM_PYROWAVE_444` | 8-bit 4:4:4 |
| `0x02000000` | `SCM_PYROWAVE_HDR10` | 10-bit 4:2:0 |
| `0x04000000` | `SCM_PYROWAVE_HDR10_444` | 10-bit 4:4:4 |

- `SCM_MASK_10BIT` gains the two HDR10 bits. `SCM_MASK_YUV444` gains the two 4:4:4 bits.
- On paired HTTPS requests, a PyroWave-capable host also returns two fields:
  - `PyroWaveHostLinkMbps`: the host's transmit speed on its route to the client. It is 0 if the route is not a known wired link, and it is not end-to-end throughput.
  - `PyroWaveBandwidthProbeBytes=33554432`.
- The host advertises PyroWave only when the GPU can run the Vulkan encoder. The config switch `pyrowave` defaults to enabled ([configuration.md](https://raw.githubusercontent.com/Nonary/Vibepollo/master/docs/configuration.md)).
- `GET /pyrowave-bandwidth-probe` over the pinned HTTPS connection returns 32 MiB. The reference client discards a warm-up, takes the slowest of three timed runs, and reserves 20% for overhead. This is a recommendation for bitrate, not a guarantee against UDP loss. A fix for shimmering on mismatched NIC speeds is a calibration step "much like iperf3" in the next release ([issue #536](https://github.com/Nonary/Vibepollo/issues/536), Nonary's comment).

### 2.2 Client-local constants (never on the wire)

| Constant | Value |
|---|---|
| `VIDEO_FORMAT_PYROWAVE` | `0x010000` |
| `VIDEO_FORMAT_PYROWAVE_444` | `0x020000` |
| `VIDEO_FORMAT_PYROWAVE_HDR10` | `0x040000` |
| `VIDEO_FORMAT_PYROWAVE_HDR10_444` | `0x080000` |
| `VIDEO_FORMAT_MASK_PYROWAVE` | `0x0F0000` |
| `VIDEO_FORMAT_MASK_10BIT` | `0xCAA00` |
| `VIDEO_FORMAT_MASK_YUV444` | `0xACC04` |

### 2.3 RTSP DESCRIBE (host to client)

- The host sends `a=rtpmap:99 PYROWAVE/90000`. This is only a capability marker. No RTP payload type 99 is ever sent.
- The host also sends `a=x-ss-pyrowave.bitstream:186f0393`.
- The reference client picks PyroWave only if the user chose it and `SCM_PYROWAVE` is set. It never picks it automatically.
- Profile preference order is: HDR10 4:4:4, then HDR10 4:2:0, then 8-bit 4:4:4, then 8-bit 4:2:0.

### 2.4 RTSP ANNOUNCE (client to host)

All confirmed.

| Attribute | Value |
|---|---|
| `x-nv-vqos[0].bitStreamFormat` | `3` (0, 1 and 2 are H.264, HEVC and AV1) |
| `x-ss-video[0].chromaSamplingType` | `1` for 4:4:4, else `0` |
| `x-nv-video[0].dynamicRangeMode` | `1` for 10-bit, else `0` |
| `x-ss-video[0].pyrowaveAdaptiveFec` | `0`. The attribute's presence selects record framing. |
| `x-ss-video[0].pyrowaveAdaptiveBitrate` | `0` |
| `x-ss-video[0].pyrowaveFeatures` | Bitmask. `0x1` is `PYROWAVE_FEATURE_RECORD_FRAMING`. `0x2` is ignored (once reserved for partial-frame decode). |
| `x-nv-video[0].encoderCscMode` | Stock attribute, unchanged. |
| `x-ml-video.configuredBitrateKbps` | Used as given. The host subtracts audio and control overhead, not FEC. |

- The host answers `400 BAD REQUEST` for `bitStreamFormat=3` when PyroWave is unavailable.
- `/launch` uses the existing code path to emit the chroma, dynamic-range and HDR flags for PyroWave profiles. No dedicated PyroWave launch parameter is documented (inferred: the launch query is the stock one).
- HDR mode and metadata use the usual control messages.

### 2.5 Version negotiation

Confirmed: there is none. The bitstream has no version field. The host advertises an 8-hex-digit ID, and the client only warns on a mismatch.

- `PYROWAVE_BITSTREAM_ID` = `186f0393`. It is the first 8 hex digits of the vendored upstream PyroWave commit, not a hash of the bitstream.
- Vibepollo's `VENDOR.txt` pins upstream PyroWave commit `186f0393b77f7755953b5ecde994bb1cec2e4155`, Granite `b6cffd5c…`, and volk and Vulkan-Headers ([VENDOR.txt](https://raw.githubusercontent.com/Nonary/Vibepollo/master/third-party/pyrowave/VENDOR.txt)).
- Local patches (buffer pool, 4:4:4 payload sizing, zero-length-block guard, a Linux DMA-BUF fd patch) are stated not to change the bitstream.

---

## 3. How frames travel

### 3.1 Transport (confirmed)

- A frame rides the normal GameStream video path: RTP, the NV video packet header, Reed-Solomon FEC blocks and optional AES-GCM.
- `frameType` is always 2 (IDR). Every frame stands alone. The host ignores IDR and reference-invalidation requests.
- The first RTP payload carries the 8-byte short frame header.
- Payload size is `packetSize - 16`: 1376 bytes for 1392-byte packets.
- An FEC block holds at most 255 data plus parity packets. A frame is split into at most 4 blocks.
- Packet caps per frame are 3000 with critical FEC on and 4000 with it off.

### 3.2 Two framings

The client tells them apart by the first 32-bit word. Bit 31 is set on a record-framed frame's sequence header, and a packet count never has it.

**Record framing** (the host default for clients that send `pyrowaveFeatures` bit 0x1):

- The payload is a sequence of 32-bit little-endian records. The doc names the fields but gives no bit positions for the sequence header and block header. Those come from the upstream bitstream spec.
- Sequence header record: 8 bytes, `extended=1`, `code=0`. It carries width, height, chroma resolution and `total_blocks`.
- Block record: the upstream `BitstreamHeader` plus payload. `payload_words` counts the header words. `sequence` must match the sequence header.
- Padding record: the word `0xFFFFFFFF`, then a word count N, then N zero words (8 + 4N bytes). It can't be mistaken for a sequence header, because that would decode as a width of 16384.
- Layout, when the payload size is a multiple of 4 and at least 24 bytes:
  - Group 1 is the coarsest level: block indices below `12 * ceil(W/32) * ceil(H/32)`, with W and H rounded up to 32 pixels and a minimum of 128. It comes first.
  - Group 2 is everything else.
  - Within each group, oversized records (larger than payload minus 8) come first and span payloads. The rest are packed first-fit. Padding fills only when nothing else fits.
  - No record crosses a payload boundary, except the oversized ones that deliberately span payloads.
- If the payload size is not a multiple of 4 or is under 24 (for example 1390-byte packets), records are copied as they are, with no reordering, no padding and no critical prefix.
- Packets flagged `0x80` mark a record start. The reference client exposes them as `BUFFER_TYPE_RECORD_START`, and the critical packet count as `DECODE_UNIT.pyrowaveCriticalPackets`. The doc says the flag is a payload flag. The exact header byte that carries `0x80` and the placement of the critical-count field on the wire are **not documented**, and need a packet capture.
- The doc says the host "never sends conditional-replenishment records" (sequence code 1, or header-only zero blocks).
- The receiver rejects the frame on any of these:
  - `payload_words` below 2
  - a record running past the frame end
  - a sequence header whose size or chroma differs from the negotiated stream
  - a second sequence header
  - a block before the sequence header, or with a different `sequence`
  - an out-of-range `block_index`

**Length-prefixed framing** (compatibility):

- Layout: a u32 LE packet count, then for each packet a u32 LE size and that many bytes. Packets come from upstream's packetize call with 1024-byte boundaries. There is no critical prefix and no parity.
- Used by the azafrob, andygrundman and dimizago clients, including Xbox.
- Any loss drops the whole frame.

**Earlier variant (do not target):** joemossjr16's README (dated 2026-09-24) says each frame uses a `PYRW` container. That predates Vibepollo's record framing, which landed 2026-09-25 (commit 6ee6fc3). Treat third-party READMEs as stale.

### 3.3 FEC and "critical packets" (confirmed)

- `pyrowave_critical_fec_percentage` defaults to 20, range 0 to 255, and 0 disables all PyroWave FEC. `fec_percentage` does not apply to PyroWave ([configuration.md](https://raw.githubusercontent.com/Nonary/Vibepollo/master/docs/configuration.md)).
- Critical block: with aligned record framing and a percentage above 0, the coarsest-level packets (the leading shards) get parity at that percentage, with a minimum of 2 parity shards. This is skipped if the critical data plus parity cannot fit one 255-shard block.
- Adaptive detail parity, triggered only when:
  - there has been at least 250 ms of frames below the negotiated FPS, and
  - at least 75% of the encoded record bytes are unchanged, compared by block ID with a 1% timing tolerance.
- Requested rate is `min(50, 100 * (negotiatedFPS / observedFPS - 1))`. At 120 fps that is about 9% at 110, 20% at 100, 33% at 90, and 50% at 80 or below.
- It spends only unused bitrate allowance, and any motion disables it at once.
- The partial-coverage planner protects all detail with reduced parity first. If that doesn't fit, it extends the critical block, then adds protected detail blocks. The announced critical count covers only the coarse data.
- Encryption is the stock optional AES-GCM.

### 3.4 Loss handling (confirmed)

- The client repairs with parity first. A frame isn't dropped just because an FEC block is incomplete.
- Once the next block or frame arrives, missing data packets are zero-filled and delivered as `BUFFER_TYPE_LOST`.
- The frame is dropped only if the first packet (the sequence header) or a whole FEC block is missing.
- The record parser skips records that lost bytes. If a record's header was lost, it resumes at the next payload flagged record-start. With no flags, it resumes only after a finer record that fits in one payload with 8 bytes to spare.
- The coarse level is intact if no announced critical packet was lost. The frame decodes if more than 90% of its records arrived. The reference client calls the upstream "decode is ready with sideband" function, with no pristine-band requirement. Missing blocks decode as zero coefficients, so those areas blur.
- The decoder is cleared before every frame. This avoids the 3-bit sequence counter discarding frames after four or more consecutive drops.
- There are no IDRs and no reference invalidation anywhere in the contract.

### 3.5 Pacing and rate (confirmed)

- The host re-reads the routed link speed every 2 seconds and uses it for pacing. If it is unavailable, pacing follows packet demand and stream bitrate. `pyrowave_send_rate_mbps` is ignored.
- If sending falls behind, a newer frame replaces that session's pending frame. The in-flight frame finishes.
- The per-frame image budget is bitrate divided by negotiated FPS. For example 800 Mbps at 120 fps gives about 0.83 MB per frame, and that ceiling stays when fps drops. After a cadence drop, spare bandwidth is left idle.
- On static content the host re-encodes the last image after one negotiated frame interval. `minimum_fps_target` lowers the repeat cadence, and its default 0 means half the stream's FPS. An earlier commit, 6f77a32, used a fifth of the stream rate; the current doc says one frame interval by default.
- Dynamic bitrate changes update the ceiling using the originally negotiated FPS.
- Guidance: about 1.6 bits per pixel is clean for SDR 4:2:0. 4:4:4 costs about 1.6x and 10-bit about 1.15x.
- Output planes are single-channel UNORM: R8 for 8-bit and R16 for 10-bit. 4:2:0 chroma is centred on each 2x2 quad.

### 3.6 Bitstream match with our pin

| Item | Finding |
|---|---|
| Vibepollo pins | `186f0393` ([commit](https://api.github.com/repos/Themaister/pyrowave/commits/186f0393), "Fix trivial warning", 2026-09-23). |
| We pin | `89f7e47` (2026-09-25, "Fix .def file"). |
| Between them | Two commits, [compare](https://api.github.com/repos/Themaister/pyrowave/compare/186f0393...89f7e47): 7d2b456 (realtime encode queue priority) and 89f7e47. Five files changed, none of them shaders or bitstream code. **Confirmed: the bitstream is identical.** |
| Upstream master | 34 commits ahead of our pin. Notable: [a62f374](https://api.github.com/repos/Themaister/pyrowave/commits/a62f374) (2026-10-03, "Move towards freezing the bitstream spec"). `bitstream/bitstream.md` is now **v1, frozen 2026-10-03**, with a 24-byte `PWV1Header` (magic, a sequence header, frame rate, `reference_bit_depth`, `header_version=1`). The header is for disk files and MKV-style containers, where it is stored once. |
| Per-frame bits on master | The diff of a62f374 shows no change to block coding, and the `PWV1Header` move ([14ac4ac](https://api.github.com/repos/Themaister/pyrowave/commits/14ac4ac)) leaves the sequence header unchanged. **Inferred:** the on-wire per-frame bitstream is unchanged since `186f0393`. I did not diff the 34 commits' shader and coding files. Two commits could be coding-related and are unchecked: efb6230 (an out-of-bounds fix in the analyze RDO shader, encoder side) and 9dd2957 (10-bit rescaling). |

Spec-level facts from the frozen [`bitstream.md`](https://raw.githubusercontent.com/Themaister/pyrowave/master/bitstream/bitstream.md), which agree with the field layout in our `web/packages/pyrowave-webgpu/src/parser.ts`:

- Sequence header, 8 bytes: width-1 (14 bits), height-1 (14), sequence (3, mod 8), extended (1), then total_blocks (24), code (2), chroma_resolution (1), colour flags.
- Block header, 8 bytes: ballot (16), payload_words (12), sequence (3), extended (1), quant_code (8), block_index (24).
- Blocks are 32x32 and may share a packet.
- Duplicate `block_index` is permitted for crude error correction.
- The start-of-frame packet may arrive in any order.
- A decoder may decode early on a timeout or when the next sequence number appears.

---

## 4. Session controls: what Vibepollo adds

**Confirmed: there is no PyroWave or session-control REST API.** The documented REST API is host administration ([api.md](https://raw.githubusercontent.com/Nonary/Vibepollo/master/docs/api.md)):

- Auth is Basic with the admin credentials or `Authorization: Bearer <scoped token>`. Examples use port 47990. Browser POST/DELETE need CSRF.
- Endpoints include `/api/apps`, `/api/apps/close`, `/api/clients/list|unpair|unpair-all`, `/api/config` (GET and POST), `/api/pin`, `/api/restart`, `/api/token`, `/api/tokens`, `/api/auth/*`, `/api/logs`, `/api/metadata` (host readiness: encoder, providers, Linux status) and `/api/vigembus/*`.
- The doc lists no launch, cancel, session, stats or bitrate endpoints.

The only PyroWave-specific controls are:

- the config options `pyrowave` and `pyrowave_critical_fec_percentage` (settable via `POST /api/config`)
- `max_bitrate` (default 0 means use what the client requests) and `minimum_fps_target`
- the two `/serverinfo` link fields and `/pyrowave-bandwidth-probe`

So the plan's "expose Vibepollo's session controls where its API allows" reduces, for PyroWave, to this: the client picks the bitrate at ANNOUNCE and via the normal mid-stream bitrate control message. The client may also run the probe, and an admin may use config and `/api/metadata`. Nothing documented is per-session or per-tier.

---

## 5. Known clients (behaviour only)

| Client | Behaviour | Source |
|---|---|---|
| Nonary/moonlight-qt (VRR fork) | Primary client. Record framing, partial decode over 90% of records, bandwidth calibration, Vulkan renderer. Has D3D11 interop problems on some Intel and Qualcomm GPUs. | [protocol doc](https://raw.githubusercontent.com/Nonary/Vibepollo/master/docs/pyrowave-protocol.md), [issues](https://api.github.com/search/issues?q=repo:Nonary/moonlight-qt+pyrowave) |
| Aurora | Record framing. It decodes only if its pinned bitstream matches `186f0393`. | protocol doc compatibility table |
| azafrob, andygrundman and dimizago clients (including Xbox) | Length-prefixed framing, no loss tolerance. | protocol doc |
| Artemis/Android ports (joemossjr16) | Vulkan decode on Adreno, about 5.7 ms at 1972x1248. Based on the early `PYRW` variant. | [README](https://raw.githubusercontent.com/joemossjr16/pyrowave-streaming/main/README.md) |
| Stock Moonlight | Never sees PyroWave. | protocol doc |

The PyroWave topic search also turned up many VR clients (ALVR, WiVRN and others). They don't use this contract.

---

## 6. Gaps, risks, licence

**Needs a live host or capture (open questions):**

1. Exact bit positions of the NV video packet header flag `0x80` (record start) and where the critical-packet count rides on the wire. The doc names the concepts but not the bytes.
2. Which of the two or three PyroWave sequence-header words carry the "bit 31" discriminator. It follows from the upstream layout: `extended` is bit 31 of word 0, so I infer the record-vs-prefixed test is `extended`. This needs confirming with a capture.
3. How much of the first-payload 8-byte short frame header applies. Does PyroWave reuse the stock Sunshine 8-byte header, or only the first RTP payload convention? The doc says "8-byte short frame header on the first payload".
4. `pyrowaveFeatures` and `pyrowaveAdaptiveFec` behaviour for a client that sends neither: does the host fall back to length-prefixed framing? Inferred yes.
5. Whether `PyroWaveHostLinkMbps` and the probe are also served over HTTP, or HTTPS-paired only. The doc says paired HTTPS.
6. The host's behaviour when our client asks for a 4:4:4 or 10-bit profile the GPU or capture path can't do. Hosts refuse with 400, so we must test.
7. Frame-size and resolution constraints (width/height rounding, the 128 minimum), and the maximum bitrate the encoder honours.
8. What the 2.0.0 release changed relative to HEAD: the changelog was not retrievable.
9. Whether Vibepollo will bump its pin to upstream's frozen v1 (a new bitstream ID with possibly unchanged bits). We can't tell whether the ID will then be advertised differently.

**Stability:** all protocol commits fall within 2026-09-25 to 2026-09-30, and the doc says nothing about being stable. In-flight work includes the bitrate calibration promised in #536 ([comment](https://api.github.com/repos/Nonary/Vibepollo/issues/536/comments)) and open HDR PR [#559](https://github.com/Nonary/Vibepollo/pull/559) (client peak luminance). The shimmering report [#536](https://github.com/Nonary/Vibepollo/issues/536) was a pacing/NIC-speed mismatch (a 10 GbE host feeding a 2.5 GbE client), not a bitstream bug. Expect real drift in the first weeks. Pin to the host's advertised `x-ss-pyrowave.bitstream` and treat any mismatch as a hard "unsupported", not a warning. Our decoders at `89f7e47` match `186f0393` bitwise.

**Licence:**

- Vibepollo, Apollo, Sunshine and the moonlight-qt fork are all GPL-3.0. Reference-only per `docs/PROVENANCE.md`. Everything above comes from the maintainer's prose documentation, wire constants and field names.
- A clean-room approach is feasible: build from the doc, then verify against a capture. The doc is complete enough that nothing needs reading from the GPL source.
- PyroWave and our WebGPU decoder are MIT, so there is no obstacle. Vibepollo's vendored patches are hosted in a GPL repository; do not pull them.

---

## 7. What our side needs

**Existing pieces.**

- `crates/cha-gamestream/src/client/front/sdp.rs` emits `bitStreamFormat` 0 to 2 only.
- `crates/cha-gamestream/src/client/front/info.rs` already parses `ServerCodecModeSupport`.
- `crates/cha-gamestream/src/client/media/video.rs` has the full-packet RS depacketizer. It reassembles whole access units, so it needs a PyroWave mode that doesn't drop a frame for an incomplete block.
- `crates/cha-proto/src/header.rs` already has `CRITICAL`, `CONTINUES`, `CONTINUED` and `INTRA` flags for independent-unit codecs.
- `cha-gateway`'s `host::Codec` is only `H264` and `Hevc`. `signal.rs` is the only place WebTransport appears.
- `web/packages/pyrowave-webgpu`'s `BitstreamParser` consumes packets with 32x32 blocks and `payload_words`.

### 7.1 `cha-gamestream` client

1. **Capability bits.** Add the four `SCM_PYROWAVE*` bits (names above) to `info.rs`. Read `PyroWaveHostLinkMbps` and `PyroWaveBandwidthProbeBytes`, if present, into the parsed info.
2. **Codec enum.** Add `Pyrowave` (with chroma and bit depth) to the client's codec choice and `VideoCodec`. In `client/front/sdp.rs`, emit `bitStreamFormat=3`, `chromaSamplingType`, `dynamicRangeMode`, `pyrowaveAdaptiveFec=0`, `pyrowaveAdaptiveBitrate=0` and `pyrowaveFeatures=1`. Parse the DESCRIBE `x-ss-pyrowave.bitstream` value and refuse if it isn't a bitstream we support. Our pin `89f7e47` and Vibepollo's `186f0393` are bit-identical, so the gateway should keep an allow-list of known-good IDs.
3. **Launch.** Reuse the launch chroma and dynamic-range fields; no PyroWave-specific parameter is documented.
4. **Depacketizer mode.** The record framing makes the unit "frame = byte stream of records". Rework the FEC path to the Vibepollo semantics:
   - RS-recover per block.
   - Zero-fill missing data packets instead of dropping.
   - Drop the frame only if the first packet or a whole block is lost.
   - Report the per-packet loss map and the critical packet count.
   - Discriminate record from length-prefixed by the first word's bit 31.
   - Honour the receiver rejection rules above.
5. **Control.** Skip IDR and reference-invalidation requests (the host ignores them). Optionally run the 32 MiB probe via the existing paired HTTPS client and report the result to the session logic for bitrate.
6. **Tests.** Golden-packet and lossy-proxy tests per ADR 0011's approach. Since no host is available, base the packet fixtures on the doc's rules, mark them synthetic, and say so in the test names.

### 7.2 `cha-gateway`

- Add `Pyrowave` to `host::Codec`/`CodecChoice` and `StreamInfo`. Never choose it under `Auto`.
- Per frame, re-emit the record stream as `cha-stream/1` datagrams:
  - Split the records back into PyroWave units. The bitstream guarantees blocks don't cross payload boundaries in aligned mode, apart from oversized records.
  - Map the critical group to `Flags::CRITICAL` and set `INTRA` on every fragment.
  - Pass the loss information so the browser can decode partially. A frame whose sequence header was lost should be dropped.
- Do not transcode. The gateway does the depacketizing, and the page runs the existing `@cha/pyrowave-webgpu` decoder.
- Transport choice: the browser PyroWave path needs WebTransport, per `docs/PLAN.md` section 3. The current `session.rs` is WebRTC video-track based (H.264/HEVC). A PyroWave bridge therefore needs a WebTransport (or data-channel) producer in the gateway. `signal.rs` already references WebTransport, but I did not check how complete that is.
- Gate it on the client having a WebGPU PyroWave decoder, the Tier L rules (wired LAN, a probe of at least 400 Mbit/s) and the browser decode probe. Pass the Vibepollo link-speed and probe numbers into the tier logic, and cap bitrate at the minimum of host link, probe and client link.
- Advertise 4:4:4 only if the host's bit says so.

### 7.3 Host façade (our `cha-gamestream` front)

Optional and low priority. Only do this if we want Moonlight-style clients to play PyroWave from our own streamer via the stock GameStream path. That would mean advertising the four bits and implementing the same framing. It isn't needed for the Phase 3 exit criterion.

### 7.4 Suggested build order

1. Capability bits and SDP negotiation, tested against synthetic hosts built from the doc.
2. The depacketizer's PyroWave mode with loss tests (record parser, zero-fill, drop rules).
3. Gateway re-framing to `cha-stream/1` with `CRITICAL`/`INTRA`.
4. A browser integration test with a recorded synthetic stream.
5. Capture a real stream the moment a Windows host exists, and resolve the open questions in section 6 against it. Nothing here has been seen on a real host.

---

## 8. As built (2026-10-07, untested against a real host)

Built from this document alone, in the order of section 7.4. No Vibepollo host was available: every test uses frames made from the rules above and says "synthetic" in its name. The tests check our reading of the document, not the host.

### What exists

**`cha-gamestream` client negotiation** (`client/front/`):

- `info.rs`: `pyrowave_bits::SCM_PYROWAVE`, `_444`, `_HDR10`, `_HDR10_444`; `HostInfo::{supports_pyrowave, offers_pyrowave}` and the fields `pyrowave_host_link_mbps` and `pyrowave_bandwidth_probe_bytes`. The probe itself (`GET /pyrowave-bandwidth-probe`) is not run.
- `launch.rs`: `StreamRequest::pyrowave` (default false, so never automatic; `codecs` is ignored when set; `hdr` must be off). The host must be Sunshine-family and offer the profile in `serverinfo`. `StreamSetup::pyrowave: Option<PyrowaveSetup>` carries the result; `StreamSetup::codec` is then an unread H.264 placeholder (the host's shared `VideoCodec` enum has no PyroWave, and adding one would break `cha-streamer` and `cha-client-gamestream`).
- `sdp.rs`: ANNOUNCE as in 2.4 (`bitStreamFormat=3`, `chromaSamplingType`, `dynamicRangeMode=0`, `pyrowaveAdaptiveFec=0`, `pyrowaveAdaptiveBitrate=0`, `pyrowaveFeatures=1`). DESCRIBE must carry `PYROWAVE/90000` and an `x-ss-pyrowave.bitstream` in `PYROWAVE_BITSTREAMS` (`["186f0393"]`, compared case-insensitively); anything else is `ClientError::Unsupported`, after `/launch` (the app runs on, as for any RTSP failure).

**The media client** (`client/media/video.rs`, `client/media/pyrowave.rs`):

- PyroWave mode in `VideoReceiver`: per-block RS recovery as for H.264. A frame still short when any packet of a newer frame has arrived and 10 ms (`reorder_window`) have passed, or after the 100 ms stall, is delivered anyway. Missing data packets are zero-filled and mapped. A frame is dropped only if its first packet or a whole FEC block (no data packet, none recoverable) is missing, or the record rules refuse it. A recovered packet's flags are not checked (the record-start flag lives among them); its stream index still is.
- Nothing is ever asked of the host: no initial keyframe request, no retry, no invalidation. The control task also drops `request_idr()` and `invalidate()` calls on a PyroWave stream. The ENet "start A" handshake message is still sent (our own host reads it as a keyframe request; a real host ignores keyframe requests on PyroWave anyway).
- `pyrowave::parse_frame` (public): record framing and length-prefixed framing, told apart by bit 31 of the first word; the sequence header, block records (`payload_words` counts header words) and padding; every rejection in 3.2; skipping of records that lost bytes, and resync after a record whose header is lost (see "Open questions"). It returns the sequence header and the whole block records back to back, which is exactly what `BitstreamParser.pushPacket` (TypeScript) and `BitstreamParser` (`cha-pyrowave-wgpu`) take: **a record is exactly an upstream packet of one block**, so no conversion is needed.
- `VideoFrame::pyrowave: Option<PyrowaveFrame>` says where each record is, which are in the coarsest level (`critical`), how many blocks arrived, whether the coarse level is whole, and the packets and records lost. `VideoStats` counts partial frames, zero-filled packets, skipped records and rejected frames.

**`cha-gateway`** (`host.rs`, `pyrowave.rs`):

- `--codec pyrowave420` and `pyrowave444`; `auto` never picks them (tested, even on a host that offers only PyroWave). The stream is named `live-pyrowave420` and `live-pyrowave444`, as on the streamer. The default bitrate is 1.6 bits per pixel (1.6 times that at 4:4:4), capped at 80% of `PyroWaveHostLinkMbps` when the host gives it.
- `pyrowave::datagrams` re-emits a frame as `cha-stream/1` datagrams the way `cha-streamer/src/wt.rs` does: `KEYFRAME | INTRA` on every datagram, no FEC, records packed into packets up to the datagram's room, an oversized packet split with `CONTINUES` and `CONTINUED`. Coarse-level records (and the sequence header) never share a packet with finer ones, and their packets also carry `CRITICAL`. The streamer sets no `CRITICAL` and the page doesn't read it, so this is harmless today.
- **Step 3 stops here.** The gateway has no WebTransport path: `/info` reports `wt_port: 0` and `signal.rs` only mentions it in a comment. A WebRTC offer for a PyroWave stream is refused with a message saying so. Nothing is sent to browsers; the stream runs and the gateway warns.

### What the WebTransport path needs (not built)

A `wtransport` endpoint in the gateway on its own UDP port with a certificate and its hash in `/info` (`wt_port`, `cert_hash_hex`); the media token check on connect; the streamer's control stream (`hello`, `floor`, `pong`, `stats`, JSON lines, `docs/plans/c2-transport.md` sections 5 and 8); the datagram budget check before sending a frame (`datagram_send_buffer_space`) and the page's loss reports; audio datagrams; the viewers fan-out and input. Then the port has to reach the browser: `cha-node` (publishing the gateway's UDP port) and `cha-control` (handing out the address and hash) are other work. The browser needs no change if the gateway speaks the streamer's WebTransport protocol: `@cha/player` already decodes `INTRA` frames from whole packets with `partial = true`, and the sequence header leads the first packet.

### Differences from this document

- **Coarse-group formula.** 3.2 says the coarsest group is "block indices below `12 * ceil(W/32) * ceil(H/32)`, with W and H rounded up to 32 pixels and a minimum of 128". Read literally that is far more blocks than a picture has (1080p would be 24480 of 3261). We take W and H as the coarsest level's own size (the aligned picture over 32): `12 * ceil((aligned_w/32)/32) * ceil((aligned_h/32)/32)`, which is level 4 of the upstream layout and agrees with `cha-pyrowave-wgpu`'s `BlockLayout` (checked: 48 at 1080p, 72 at 1440p, 144 at 2160p).
- **Critical packet count.** The host announces a "critical packets" count we haven't found on the wire, so it isn't used. `coarse_complete` is computed from which blocks arrived instead.
- **`x-nv-clientSupportHevc:0`** is sent as for H.264. 2.4 doesn't say.
- **Bitrate attributes.** Stock ANNOUNCE sends 80% of the configured bitrate (less 500 kbps remote, capped at 100 Mbps) in the initial, peak, minimum and maximum bitrate attributes. For PyroWave they carry the configured rate unchanged and uncapped, since 2.4 says the host uses `configuredBitrateKbps` as given and 100 Mbps would be absurd.
- **Both** the `PYROWAVE/90000` marker and the bitstream line are required.
- **Frame header.** The first payload's 8-byte (or 44-byte, `0x81`) stock frame header is assumed to apply, with the last-packet length at bytes 4 to 5, as for any frame (open question 3).
- **Block recovery.** A block that is too short to recover is zero-filled, not dropped. A frame arriving with fewer than 90% of its records is still delivered; whether to decode it is the page's call (`isReady`).
- **Zero-fill timing.** 3.4 says "once the next block or frame arrives". We wait for the next *frame* (not block), after the reorder window.
- Not done: the bandwidth probe, the bitrate calibration, HDR10 (profiles and the `x-ss-pyrowave` HDR extras), `SCM_PYROWAVE_HDR10*` is read but never requested.

### Open questions a first run needs to settle

Numbered as in section 6 where they are the same.

1. **The record-start flag (6.1).** `client::media::pyrowave::RECORD_START_FLAG` is `None`. When a capture shows which byte of the 16-byte NV video header carries `0x80`, set it (`RecordStartFlag { nv_header_byte, mask }`); the receiver then collects flagged packets and the parser resyncs there. It is probably the NV header's flags byte (offset 8), but that is a guess. Until then the fallback runs: after a record whose header was lost, resume at the next payload start whose first record is a finer one (not coarsest level) of the right sequence that fits a payload with 8 bytes to spare. **Is that the rule 3.4 means?** The wording ("resumes only after a finer record that fits in one payload with 8 bytes to spare") is ambiguous. A wrong resync would feed a block garbage coefficients, so watch for visible blocks.
2. **The discriminator (6.2).** We use bit 31 of the first word (`extended`); the bit layout we parse matches upstream, so this should hold. A capture should show `0x80xxxxxx`-style first words on record-framed frames.
3. **The frame header (6.3).** Does the 8-byte header precede the records, does the record layout count it in the first payload's room (we assume the first payload holds `payload - 8` bytes of records), and does bytes 4 to 5 give the last payload's length? The test layout assumes yes to all three. If the first payload's alignment is different, only resync is affected.
4. **Features omitted (6.4).** We always send `pyrowaveFeatures=1` and never test the length-prefixed fallback against a host.
5. **Link fields over HTTP (6.5).** We read `PyroWaveHostLinkMbps` from whatever `serverinfo` we get; the gateway asks over paired HTTPS.
6. **Refusals (6.6).** 4:4:4 against a GPU that can't: we check the `SCM_PYROWAVE_444` bit first, then expect `400 BAD REQUEST` at ANNOUNCE from a host that offers it anyway.
7. **Is `start A` safe to keep, and does the host need `x-nv-clientSupportHevc`?** Try without both.
8. **Bitrate attributes.** Does the host read `configuredBitrateKbps` only, or the stock initial, peak and minimum attributes too? Do the unscaled numbers pass its checks (800000 kbps)?
9. **FEC block shape.** Do critical-FEC frames arrive as a first FEC block of the coarse packets with its own percentage in the header (what the receiver reads per block), or some other way? A block whose shard count differs from `data + ceil(data * percent / 100)` would be dropped as malformed.
10. **Packet size.** Payloads must be a multiple of 4 and at least 24 bytes for the aligned layout. We ask for `1392` (1360 with encryption), as stock; the first run shows whether the host applies the record layout at our packet size.
11. **The pin (6.9).** Whether Vibepollo moves to upstream's frozen v1 and advertises another id: add it to `PYROWAVE_BITSTREAMS` only after checking the per-frame bitstream is unchanged, then run the decoder against a capture.
12. **Real sequence header values.** `total_blocks`, the colour flags, the picture size when `sops` changes it: a size other than the launch size is refused as `SequenceMismatch` (the stream's size is the launch size).
13. **Static content.** The host re-encodes the last image every frame interval; check the receiver and the gateway handle a stream that repeats identical frames, and a send queue that replaces an in-flight frame (a skipped frame number is not a loss here).
14. **Timing.** `reorder_window` (10 ms) and `stall` (100 ms) are defaults tuned for 60 fps H.264. At 120 fps with a 0.8 MB frame, check how often a frame is delivered short only because the next one started first.
