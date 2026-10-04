# 02 — PyroWave and the low-latency codec landscape

Research slice for **Cha Portal** (portal.cha.sh). Snapshot as of **2026-10-03**. Sources are linked inline. Anything I could not confirm from a primary source is tagged **(unverified)**. Source code was read from shallow clones (nothing was built or run):

- `Themaister/pyrowave` @ `89f7e47` (2026-09-25)
- `Themaister/pyrofling` @ `99620da` (2026-10-01)
- `imbcmdth/pyrowave` branch `webgpu` @ `5e80f92` (2026-09-26)
- `imbcmdth/ffrwd-package-pyrowave`
- `Nonary/Vibepollo` @ master (2026-10-03)

Project decisions that shape the verdicts below: Cha Portal is copyleft (AGPL/GPL), so embedding GPL-3 code is fine. It targets homelabs and small groups first. LAN and WAN are equally first-class, so the codec ladder has to adapt to the link.

---

## TL;DR

- **PyroWave** is an intra-only GPU wavelet codec. It uses a CDF 9/7 DWT with 5 levels, deadzone quantization and raw bit-planes with **no entropy coding**. Rate control is exact and single-pass, with a hard per-frame byte cap. The whole thing runs as Vulkan compute.
  - Encode and decode each take about **0.1 ms at 1080p and about 0.2 ms at 4K** on a desktop GPU.
  - The price is bandwidth: **~200–300 Mbit/s at 1080p60–4K60 for couch viewing**, and **0.5–1.1 Gbit/s** for desktop-distance viewing at 1440p/4K. 120 fps doubles all of these.
  - It supports 4:2:0 and 4:4:4, and SDR or HDR10/PQ. The decoder is floating-point, so bit depth is a presentation choice.
  - License: **MIT**, including its Granite dependency. The bitstream is a **draft** and the C API is **v0.6.0, explicitly unstable**.
- **The status changed a lot in September 2026.**
  - **Valve shipped PyroWave in the Steam Remote Play beta** (2026-09-21). Sam Lantinga (SDL, Valve) contributed a Metal port.
  - **Vibepollo v2.0.0 (2026-09-30) ships PyroWave** on Windows and Linux hosts, paired with Nonary's VRR Moonlight fork. Its Moonlight-protocol extension is documented in `docs/pyrowave-protocol.md`.
  - A Wolf PR (#517) adds a GStreamer PyroWave plugin. It is still open.
  - Several ALVR/VR forks use PyroWave.
  - A **WebGPU/WGSL port** of both encoder and decoder appeared on 2026-09-26 (MIT, `imbcmdth/pyrowave@webgpu`). Its output is bit-exact against the Vulkan build.
- **Error resilience is good by design.** Every 32×32 coefficient block decodes independently, and a missing block reads as zeros (local blur). Only the coarsest band (LL4 plus the next level) is critical. The API exposes "critical packets" so FEC can be applied unequally.
- **Browser decode is feasible.** The decoder has two stages: dequant, which uses subgroups, and iDWT, which uses textureGather and workgroup memory. Both are already ported to WGSL; GPU time is about 0.09 ms at 1080p on an RTX 4090 via wgpu-native.
  - Chrome has subgroups. **Firefox does not** (bug 1955417). Safari subgroups landed in WebKit main in July 2026 (shipping **unverified**).
  - A subgroup-free dequant path is easy to write, because the scans fit in a 128-thread workgroup.
  - **The real browser risks are transport and the compositor, not the codec.** Pushing 300–900 Mbit/s (50–100k datagrams/s) through WebTransport/QUIC in a browser is unproven, and the canvas compositor probably adds about a frame.
- **Recommendation**
  - Make PyroWave the **LAN, wired, ≥1 GbE "lossless-feel" tier**.
  - Make **HEVC/AV1 (and H.264 for compatibility) the WAN and Wi-Fi tiers**, using hardware encode, intra-refresh, LTR/RFI and FEC.
  - Ship PyroWave in the **native thin client first**: Vulkan via libpyrowave, with the Metal port on Apple.
  - In parallel, build a **WebGPU decoder** (TS + WGSL, forked from the MIT WebGPU port) behind a transport benchmark gate.
  - Be **protocol-compatible with Vibepollo's PyroWave extension** so Cha Portal clients can use Vibepollo hosts.

---

## 1. PyroWave deep-dive

### 1.1 Identity and status

| Item | Value |
|---|---|
| Repo | https://github.com/Themaister/pyrowave (created 2025-04-17) |
| Author | Hans-Kristian Arntzen (Themaister), the vkd3d-proton developer working with Valve |
| License | **MIT** (`LICENSE`). Granite, the required subset of Themaister's engine, is also MIT ([Granite LICENSE](https://github.com/Themaister/Granite/blob/master/LICENSE)) |
| Activity | 649★, 31 forks, 10 open issues. Last default-branch commit 2026-09-25 ("Expose support for realtime encode queues"). Pushed 2026-10-03. There are no releases or tags; only branches (`c-api`, `scaler-api`, `external-api`, `fs-idwt`, …) |
| API | `pyrowave.h`, `PYROWAVE_API_VERSION 0.6.0`: *"API and ABI is not considered stable until MAJOR version hits 1!"* |
| Bitstream | [`bitstream/bitstream.md`](https://github.com/Themaister/pyrowave/blob/master/bitstream/bitstream.md): *"This specification is considered a draft and may change at any time."* It has **no version field**, so Vibepollo advertises the vendored commit hash instead (see §2.3) |
| Platforms | Linux, Windows (MinGW, msys2, MSVC), Steam Runtime (Sniper SDK), Android (NDK) and aarch64. There is an in-tree **Metal port** (`metal/`) for macOS and iOS (Apple7+ family) |
| Blog | ["I designed my own ridiculously fast game streaming video codec – PyroWave"](https://themaister.net/blog/2025/06/16/i-designed-my-own-ridiculously-fast-game-streaming-video-codec-pyrowave/) (2025-06-16). [HN discussion](https://news.ycombinator.com/item?id=44714914): 432 points |
| Adoption | Steam Remote Play beta (2026-09-21) and Vibepollo 2.0.0 (2026-09-30). See §2 |

The README states the intended use case precisely: *"local network game streaming over ethernet with absolute minimum latency where bandwidth is less of a concern."* Target bitrates are "~200+ mbit/s".

The README says "64×64 blocks", but the bitstream spec and the code use **32×32** blocks (`BLOCK_SIZE = 32`, `block_count_32x32`). The README is stale.

### 1.2 Algorithm (from source)

**Colour.** YCbCr in **4:2:0 or 4:4:4**. In 4:2:0 mode, chroma uses one fewer decomposition level (4 instead of 5), because the level-0 chroma bands are never coded. The sequence header signals:

- full or limited range
- BT.709 or BT.2020 primaries and matrix (NCL)
- BT.709 or PQ transfer
- centre or left chroma siting

**Transform.** A 2-D DWT using the **CDF 9/7 irreversible filter** (the JPEG 2000 lossy filter), implemented as four lifting steps:

- constants α = −1.586…, β = −0.0529…, γ = 0.8829…, δ = 0.4435…, K = 1.2301… in `shaders/dwt_common.h`
- **5 decomposition levels**
- mirrored edge extension, done with `VK_SAMPLER_ADDRESS_MODE_MIRRORED_REPEAT` plus `textureGather`
- images padded to multiples of 32, minimum 128

The 2025 blog post discussed 5/3; the current code is 9/7. An ALVR field report re-tested CDF 5/3 and Haar: 5/3 lost 0.37 dB and needed about 18 % more bytes ([issue #17](https://github.com/Themaister/pyrowave/issues/17)), which confirms 9/7 as the right choice.

**Decode precision.** The decode process is floating-point and **not bit-exact**. It needs at least FP16 for the iDWT. `PYROWAVE_PRECISION` selects the mode:

- 0: FP16 math
- 1 (default): FP16 storage for the two finest levels and FP32 for coarse levels
- 2: FP32 throughout

**Block hierarchy.** Each band is tiled into **32×32 coefficient blocks**, each with an 8-byte header. Every block is self-contained, with no prediction or context across blocks.

- Inside a block are sixteen 8×8 blocks. A 16-bit `ballot` marks which of them are non-zero.
- Each 8×8 block holds eight 4×2 sub-blocks.
- Each GPU thread handles one 4×2 sub-block, which is 8 coefficients and therefore exactly one byte per bit-plane.

**Quantization**

- A **deadzone quantizer** (reconstruction at ±0.5 offset).
- Per 32×32 block: an 8-bit `quant_code`, a custom mini-float covering (0, 16).
- Per 8×8 block: a 4-bit scale, `code/8 + 0.25`.
- Per 4×2 sub-block: a bit-plane count of base `q_bits` (4 bits per 8×8) plus a 2-bit delta.
- Distortion is weighted per band by a CSF-like resolution weighting (`get_quant_rdo_distortion_scale`), so high frequencies are quantized harder.

**"Entropy" coding.** There is none. Magnitudes are emitted as **raw bit-planes**, MSB first, 8 coefficients per byte. Sign bits for non-zero coefficients are packed at the end of the block. The author's rationale: *"Entropy coding is an absolute nightmare for parallelization"* (blog). The README notes that an HTJ2K-style entropy coder could be added, but calls it out of scope.

**Rate control.** It is exact, single-pass and RDO-like:

1. `analyze_rate_control.comp` measures, for each block, the distortion and byte savings of dropping 1, 2, 3, … bit-planes, and buckets each option by distortion per saved byte.
2. `analyze_rate_control_finalize.comp` prefix-sums the buckets.
3. `resolve_rate_control.comp` picks the cheapest set of drops that meets `maximum_bitstream_size`.

The result is a **hard per-frame cap**, typically *"~10-20 bytes under the target"* (blog). There is no VBR or quality-target mode in the codec. pyrofling and Vibepollo pick the cap from link speed or from a perceptual model (§1.7).

**Intra-only.** Bitrate scales **linearly with frame rate**, and a static screen costs as much as a moving one unless the application skips frames.

### 1.3 Bitstream and packetization

From [`bitstream.md`](https://github.com/Themaister/pyrowave/blob/master/bitstream/bitstream.md) and `pyrowave_encoder.cpp`:

- **Block header** (8 bytes, little-endian):

  ```c
  uint16_t ballot;            // 1 bit per 8x8 sub-block; 0 => block not sent
  uint16_t payload_words:12;  // block size in u32 words incl. header (max 16 380 B)
  uint16_t sequence:3;        // frame counter mod 8
  uint16_t extended:1;        // 1 => this is a sequence/extended header
  uint32_t quant_code:8;
  uint32_t block_index:24;    // global linear block index
  ```

  It is followed by `u16 CodeWords[N]`, `u8 QScale[N]`, the magnitude bit-planes and the sign bits, where N = popcount(ballot).

- **Start-of-frame header** (8 bytes, `extended=1`, `code=0`). It carries width and height (14 bits each, up to 16K), sequence, `total_blocks` (the number of non-zero blocks in the frame), chroma 420/444, primaries, transfer, matrix, range and siting. It **may arrive in any order** relative to the blocks. Once `total_blocks` distinct blocks have arrived, the frame can be decoded immediately.
- **Block ordering is coarse to fine.** `block_index` runs from level 4 (LL4, then HL/LH/HH) through level 0, interleaving components within each level. The packetizer emits blocks in index order, so **the first packets of a frame carry the most important data**.
- **Packetization API.**
  - `pyrowave_encoder_compute_num_packets(packet_boundary)` and `pyrowave_encoder_packetize[_with_padding](...)` greedily pack whole blocks into packets of at most `packet_boundary` bytes. The `_with_padding` variant reserves header room in the first packet.
  - **`packet_boundary` is not a hard cap.** A single block larger than the boundary becomes one oversized packet (`payload_words` allows up to about 16 KB). The transport must fragment those.
  - `pyrowave_encoder_get_mapped_raw_bitstream()` gives raw access for custom packers; Vibepollo uses it for record framing.
- **Unequal error protection hooks.**
  - `pyrowave_encoder_compute_num_critical_packets(bands, …)` returns how many leading packets hold the critical coarse bands. The recommended value is `bands` = 2–3.
  - `pyrowave_encoder_compute_block_active_words()` produces an active-block bitmask that can be sent as sideband data.
  - The decoder takes that bitmask through `pyrowave_decoder_decode_is_ready_with_sideband()`.

### 1.4 Error resilience and packet loss

- **Each packet decodes independently** as long as it holds only whole blocks. There is no inter-frame dependency at all: **no keyframe requests, no RFI, no intra-refresh crawl.**
- A **missing block decodes as all-zero coefficients.** In a high-pass band this shows up as a small, momentary local blur. A loss in **LL4 is severe** (big blotches); the spec recommends *"selectively applying Forward Error Correction to those packets"*.
- **Duplicate blocks** are allowed as crude FEC, and the decoder ignores the second copy.
- **Partial decode.** `decode_is_ready(allow_partial_frame=true)` defaults to requiring 2 pristine coarse bands and at least 90 % of blocks (`pyrowave_decoder.cpp`). Applications usually force a decode at a deadline or when the next sequence starts.
  - pyrofling viewer: `pyro://ip:port?phase_locked=0.0&deadline=0.008`
- **Known gotcha:** the sequence counter is only 3 bits. After 4 or more consecutive lost frames, the decoder can misjudge the frame as "old" and drop it. Vibepollo works around this by calling `pyrowave_decoder_clear()` before every frame ([Vibepollo `docs/pyrowave-protocol.md`](https://github.com/Nonary/Vibepollo/blob/master/docs/pyrowave-protocol.md)).
- **Field experience.**
  - The README says it has been *"battle tested over long distance streaming over fiber links."*
  - Vibepollo's adaptive FEC design (§2.3) shows what a production transport around it looks like.
  - A Vibepollo user saw "shimmering" at 4K120 HDR on 5 GbE with zero loss; it was labeled *fixed* ([Vibepollo #536](https://github.com/Nonary/Vibepollo/issues/536)).
  - A Steam crash report (`pyrowave` [#19](https://github.com/Themaister/pyrowave/issues/19)) appears to be a Steam host memory leak rather than a codec bug.

### 1.5 Implementation and API surface

**Shaders and dependencies.** About 2.4k lines of GLSL compute in `shaders/`, compiled to SPIR-V by Granite's `slangmosh`. The library links a small Granite subset (`checkout_granite.sh`: vulkan-headers, volk, the Vulkan wrapper and `video/scaler.cpp`). The output is `libpyrowave-shared.so`, with a pkg-config file and a CMake package.

**GPU requirements** (from `pyrowave.h`):

| Side | Required | Optional |
|---|---|---|
| Encoder | Vulkan 1.3; subgroups: ARITHMETIC, SHUFFLE, SHUFFLE_RELATIVE, VOTE, BALLOT, **CLUSTERED**; **subgroup size control able to force wave16/32/64**; `shaderInt16`; `storageBuffer8BitAccess` | `shaderFloat16` |
| Decoder | Vulkan 1.3; basic, arithmetic, shuffle, shuffle-relative, vote and ballot subgroups; any subgroup size from 4 to 128 with size control; 8-bit storage **or** large texel buffers | FP16 |

**Encoder shader stages**

| Stage | Shader | Notes |
|---|---|---|
| DWT | `dwt.comp` | 64 threads; shared-memory tile of 32×32 plus apron; packed FP16 math helps "even on RDNA4" (blog) |
| Quantize and cost | `wavelet_quant.comp` | 128 threads; clustered subgroup ops; FP16 and int16 stats |
| Rate control | `analyze_rate_control.comp`, `analyze_rate_control_finalize.comp` (512 threads), `resolve_rate_control.comp` | |
| Block packing | `block_packing.comp` | 64 threads; ballot, clustered add, shared-memory atomics; 8-bit stores |

**Decoder shader stages**

| Stage | Shader | Notes |
|---|---|---|
| Dequant | `wavelet_dequant.comp` | 128 threads, one workgroup per 32×32 block, about 40 dispatches (one per level×component×band); `subgroupInclusiveAdd` and `subgroupShuffleUp` for byte-offset and sign-offset scans, with an LDS fallback for small waves. Three storage modes: 8/16-bit SSBO, texel buffers, or linear 2-D textures (the mobile path) |
| iDWT (compute) | `idwt.comp` | 64 threads; textureGather with a mirror-repeat sampler; ~6.5 KB shared tile; **no subgroup ops** |
| iDWT (fragment, mobile) | `idwt.frag` / `idwt.vert` | Pure render passes with R16F/RG16F MRTs. Auto-selected for Qualcomm-proprietary, Mali and PanVK drivers. On Adreno 740 the compute path was faster in one field report ([#17](https://github.com/Themaister/pyrowave/issues/17)) |

Intermediate storage is an R16F (or R32F) 2-D array of 12 layers (4 bands × 3 components) with mips.

**C API highlights** (`pyrowave.h`):

- **Device**
  - `pyrowave_create_default_device`
  - `pyrowave_create_device` **borrows an app's `VkInstance`/`VkDevice`/queues**, so encode can run in the same device as a compositor
  - `pyrowave_create_device_by_compat[2]` matches a device by UUID or LUID and can request a **HIGH/REALTIME global queue priority** (needs `CAP_SYS_NICE` on Linux)
  - `pyrowave_device_set_command_buffer` records into the app's own command buffer
  - `pyrowave_device_set_queue_type` chooses graphics or async compute
- **External memory and sync**
  - `pyrowave_image_create` imports OPAQUE_FD, Win32 or KMT handles, D3D11/D3D12 textures, and **DMA_BUF with DRM format modifiers** (explicit modifier create-info). Tests cover this in `test_drm_modifier_interop`.
  - `pyrowave_sync_object_create` imports or exports OPAQUE_FD, **SYNC_FD**, Win32 and D3D12 fences, binary or timeline.
- **Encode**
  - `pyrowave_encoder_encode_gpu_synchronous` takes three plane views.
  - **`pyrowave_encoder_encode_gpu_scaled_synchronous` takes an RGB(A) image, or NV12 via the COLOR aspect ("special case for pipewire dmabuf screen capture")**. It handles sinc or linear scaling, crop, sRGB, scRGB or HDR10 input, HDR10 PQ output, an R8 (dithered) or R16 intermediate, and the YCbCr conversion.
  - `pyrowave_encoder_encode_cpu_synchronous` exists for bring-up.
  - `pyrowave_rate_control{maximum_bitstream_size}` sets the cap.
- **Decode**
  - `pyrowave_decoder_create{fragment_path}`, `push_packet`, `decode_is_ready[_with_sideband]`, `decode_gpu_buffer` (into app images, R8 or R16 UNORM storage) and `decode_cpu_buffer_synchronous`.
- **Ports**
  - **Metal**: `metal/pyrowave_metal.h`, API 0.5.0. Described as *"an AI assisted port … a quick and dirty port was needed by clients"*; upstream will maintain it only minimally and *"may remove"* it. It was produced by running GLSL → SPIR-V → SPIRV-Cross → MSL (`metal/shaders/transpile.sh`) and is roughly 2× faster than the Vulkan version running on KosmicKrisp. Contributor: Sam Lantinga ([PR #8](https://github.com/Themaister/pyrowave/pull/8)).
  - **WebGPU**: out of tree, see §3.3.
  - **Rust**: [`lutyjj/pyrowave-rs`](https://github.com/lutyjj/pyrowave-rs) (MIT; 4 commits; has a `dmabuf` feature; early).

### 1.6 Performance numbers

| Measurement | Number | Source |
|---|---|---|
| Encode or decode, 1080p | < ~0.1 ms | README |
| Encode or decode, 4K | < ~0.2 ms | README |
| Encode, 1080p 4:2:0, hard content (E33), RX 9070 XT, RADV | 0.13 ms | blog |
| Encode, "normal games" | ~80 µs | blog |
| Encode, 4K ParkJoy | 0.25 ms | blog |
| Vulkan GPU stages, RTX 4090, 1080p, 250 KB/frame | DWT 0.023, quant 0.047, analyze 0.016, resolve 0.005, pack 0.022 ms (**encode ≈0.11 ms**); dequant 0.032, iDWT 0.024 ms (**decode ≈0.056 ms**) | [WebGPU port README](https://github.com/imbcmdth/pyrowave/blob/webgpu/webgpu/README.md) |
| Same, WebGPU via wgpu-native on Vulkan | encode ≈0.16 ms, decode ≈0.089 ms (dequant 0.055, iDWT 0.034) | same |
| Wall clock incl. CPU upload and readback (Vulkan C API CPU path) | encode 0.77 ms, decode 0.54 ms | same. Bus transfers dominate; a GPU-resident pipeline avoids most of this |
| Vibepollo / Moonlight-fork, desktop GPU | "encode in about 0.5 ms" | [pyrowave-streaming](https://github.com/joemossjr16/pyrowave-streaming) |
| Snapdragon 8 Elite, 1972×1248, FP16 fragment | ≈5.7 ms decode | same |
| Quest 3 (Adreno), 2944×1536, 400 Mbit/s | 7.1 ms → **4.3 ms** after the fragment iDWT path | [#4](https://github.com/Themaister/pyrowave/issues/4) |
| Galaxy XR (Adreno 740), 1984×896 4:4:4, 400 Mbit/s, 90 Hz | 3.0–3.9 ms decode; motion-to-photon ~60 ms vs 85–98 ms for tuned ALVR H.264 | [#17](https://github.com/Themaister/pyrowave/issues/17) |
| 4:4:4 vs 4:2:0 cost, RTX 3090, 3328×1472 | 1.51× decode, 1.39× encode time | #17 |
| Samsung S22 (Xclipse, RDNA2) | 1080p decode "sub-millisecond easily" | Themaister in #4 |
| Steam Deck | decode power "barely measurable" | blog |

Takeaway: on desktop GPUs the codec effectively costs nothing. On mobile and tiled GPUs, decode takes **3–6 ms**, which is still fine for 60–90 fps but no longer negligible.

### 1.7 Bitrate requirements

Both evaluations are by the author: a subjective BT.500-like DSIS test ([`eval-results/bitrate-evaluation.md`](https://github.com/Themaister/pyrowave/blob/master/eval-results/bitrate-evaluation.md)) and an objective one using a custom **PSNR-HVS-M-H** metric, which adds viewing distance H = distance / screen height ([`objective-bitrate-evaluation.md`](https://github.com/Themaister/pyrowave/blob/master/eval-results/objective-bitrate-evaluation.md)). The objective regression ships as a C header, `pyrowave_regression_results.h`, which pyrofling uses for `--pyrowave-auto-quality <dB> <H>`.

I evaluated that published regression myself; it is a polynomial lookup, evaluated in Python from the header's coefficients. The author's "good quality" anchor is **≈35 dB**, which he calls visually transparent at H = 2. Values below are **Mbit/s at 60 fps**; multiply by 2 for 120 fps.

| 35 dB | 720p | 1080p | 1440p | 4K |
|---|---|---|---|---|
| H=1.0 (close desktop) 4:2:0 | 144 | 292 | 479 | 734 |
| H=1.0 4:4:4 | 170 | 355 | 589 | 889 |
| H=1.5 (normal desk) 4:2:0 | 149 | 261 | 381 | 422 |
| H=1.5 4:4:4 | 177 | 319 | 465 | 476 |
| H=2.0 (couch / TV) 4:2:0 | 135 | 220 | 289 | 291 |
| H=2.0 4:4:4 | 165 | 267 | 342 | 314 |
| H=2.5 4:2:0 | 122 | 182 | 207 | 243 |

At 40 dB, a "pristine" setting, the H = 1.0 figures for 1080p / 1440p / 4K are 460 / 721 / 1095 Mbit/s (4:2:0) and 554 / 887 / 1338 Mbit/s (4:4:4).

Other reference points:

- **Rule of thumb: ~1.6 bits/pixel** for clean 4:2:0 SDR (200 Mbit/s at 1080p60). Vibepollo's guidance adds that 4:4:4 costs ~1.6× and 10-bit ~1.15×. The author's objective data says 4:4:4 costs **15–20 % more for equal luma quality**; the gap is chroma quality, which the metric cannot see.
- **Steam Remote Play.** The initial beta notes said PyroWave *"ignores bitrate settings, using at least 250 Mbit/s"* (quoted by [Brad Lynch on X](https://x.com/SadlyItsBradley/status/2072470278802743691)). Valve's later announcement says manual bitrate is **100–500 Mbit/s** and that it uses *"5-10 times the amount of bandwidth of other streaming codecs"*, recommending **wired Gigabit Ethernet to the router** for both machines ([Steam thread](https://steamcommunity.com/groups/homestream/discussions/0/564794422009744473/), [Phoronix](https://www.phoronix.com/news/Valve-Steam-Beta-Pyrowave)).
- **Comparison with HEVC.** In the author's own test (pyroenc HEVC with intra-refresh and low latency on an RX 9070 XT), HEVC needs far fewer bits for the same metric. That is expected for an intra-only codec. However, intra-refresh crawl at 20–30 Mbit/s is visible, and the metric does not capture it.

**Implications for Cha Portal**

- Couch gaming at 1080p–4K60 fits comfortably in 1 GbE.
- **A desktop at the desk (H ≈ 1–1.5), 1440p/4K, 4:4:4, 120 Hz needs about 0.9–1.8 Gbit/s, so 2.5 GbE or faster.**
- Wi-Fi is marginal. ALVR and Galaxy XR users run 400 Mbit/s over Wi-Fi with deadline-based partial decode, but that is a best-effort mode.

### 1.8 Limitations

- High bandwidth (above). There is no inter prediction, so **static desktops cost full bitrate** unless the sender skips unchanged frames. pyrofling and Vibepollo re-encode the last frame at the frame interval, partly so a lost frame recovers quickly.
- The bitstream is a draft with no version field. The API is unstable (0.x). The project has a single maintainer, who said *"I have no interest in [Moonlight/Sunshine] myself"* ([#15](https://github.com/Themaister/pyrowave/issues/15)) and declined a ProRes/APV comparison ([#12](https://github.com/Themaister/pyrowave/issues/12)).
- GPU-specific bugs have occurred. The encoder produced garbage on Intel at subgroup size 16 until a fix landed on 2026-08-12 ([#7](https://github.com/Themaister/pyrowave/issues/7)).
- Decode is not bit-exact: it uses floats, and NVIDIA's R8 store rounds differently.
- Encode runs on the same GPU as the game. Under a heavy game load the streamed frame rate can drop ([#10](https://github.com/Themaister/pyrowave/issues/10)). The REALTIME queue priority added on 2026-09-25 helps.
- At most 16384×16384. Only 16:9 has been evaluated.

### 1.9 Steal for Cha Portal

1. **Unequal error protection.** Packetize coarse-to-fine, protect the first `num_critical_packets` with FEC, and send the detail unprotected or with adaptive FEC.
2. **Deadline-based partial decode.** Decode when all `total_blocks` have arrived, or at a deadline or the next frame, whichever comes first. Missing blocks are just blur.
3. **Zero-copy encode input.** Import a compositor DMA-BUF (with DRM modifier) plus a SYNC_FD, and use `encode_gpu_scaled_synchronous` to scale, crop, convert colour space and handle HDR10 in a single call. The NV12 path suits PipeWire, for example gamescope or xdg-desktop-portal streams.
4. **Device sharing.** `pyrowave_create_device` lets the node's own compositor or encoder process run PyroWave on its existing `VkDevice` with no cross-process copies.
5. **REALTIME compute queue priority** (`create_device_by_compat2`). Grant `CAP_SYS_NICE` to the node container.
6. **Viewing-distance-aware bitrate model.** Vendor `pyrowave_regression_results.h` and pick a bitrate from resolution, fps, 4:4:4, H and target dB. Use H ≈ 1.25 as the desktop default and H ≈ 2 for "TV mode".
7. **Exact per-frame cap.** The rate is deterministic, so pacing and FEC budgeting are trivial.

**Verdict: depend-on.** Vendor a pinned commit of libpyrowave (MIT) on nodes and in native clients. Advertise a bitstream ID (the commit hash) in session negotiation. Contribute upstream where possible: bitstream versioning, GPU output in the WebGPU port.

---

## 2. Ecosystem: where PyroWave is integrated (as of 2026-10-03)

| Project | What | License | Status | Verdict for Cha Portal |
|---|---|---|---|---|
| **Steam Remote Play** (Valve) | Beta "Pyrowave Video" toggle, 100–500 Mbit/s, YUV 4:4:4 and HDR (auto). Hosts: Windows and macOS; Linux needs the experimental SteamRT3 client. Steam Link mobile apps "coming soon". Valve's slouken pointed users to beta build 1790036264 | Proprietary (codec MIT) | Beta since 2026-09-21 ([Phoronix](https://www.phoronix.com/news/Valve-Steam-Beta-Pyrowave), [GamingOnLinux](https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/), [Steam thread](https://steamcommunity.com/groups/homestream/discussions/0/564794422009744473/)) | Inspiration and validation. This is the strongest signal that PyroWave will be maintained |
| **pyrofling** (Themaister) | Vulkan-layer capture, "basic compositor", FFmpeg/pyroenc/pyrowave encoders, UDP+TCP `pyro://` protocol, viewer for Linux, Windows, Android and Deck | MIT (FFmpeg linkage caveat) | Active, 2026-10-01 | **Inspiration-only.** Transport, FEC, phase-locking and gamepad hand-off ideas (§2.1) |
| **Vibepollo 2.0.0** (Nonary; Sunshine/Apollo fork) | PyroWave on **Windows (D3D11 → Vulkan zero-copy) and Linux (DMA-BUF) hosts**. SDR/HDR, 8/10-bit, 4:2:0/4:4:4. Critical and adaptive FEC, record framing, bandwidth probe | **GPL-3.0** | Released **2026-09-30** ([release feed](https://github.com/Nonary/Vibepollo/releases)). Upstream PyroWave pinned at `186f0393` | **Protocol-compatible** (and code is borrowable under GPL). Desired feature #8 makes this the most important interop target (§2.3) |
| **Nonary VRR Moonlight fork** | Moonlight-qt client that decodes PyroWave | GPL-3.0 | Active | Reference client for interop testing |
| Aurora (`Koloses/aurora-qt`) and Solarflare (`Koloses/Solarflare`) | Moonlight/Sunshine forks with an older, WiVRn-derived PyroWave decoder | GPL-3.0 | Last pushed 2026-07 | Watch only |
| `azafrob/Sunshine`, `azafrob/moonlight-qt` (`pyrowave-codec` branches) | Earlier forks; a commenter in #5 called them "vibe coded" | GPL-3.0 | 2026-07 | Ignore |
| `joemossjr16/pyrowave-streaming` (+ `pyrollo`, `moonlight-qt-pyrowave`, `artemis-android-pyrowave`) | Vibepollo plus Moonlight builds for Deck and Android; Adreno fragment-iDWT experiments | mixed / none | 2026-09-24 | Data points only |
| **Wolf** (games-on-whales) | [Issue #515](https://github.com/games-on-whales/wolf/issues/515) and [PR #517](https://github.com/games-on-whales/wolf/pull/517): a **GStreamer PyroWave plugin**. Negotiation via the Aurora client; Vulkan 1.3 required; changes to bitrate limits and pacing | MIT | **Open PR, not merged** (2026-09-26) | Watch. If merged, the GStreamer element is reusable on Wolf-like nodes |
| ALVR / WiVRn experiments: [Galaxy XR 4:4:4](https://github.com/Terminal-ennui/galaxy-xr-alvr-pyrowave-444), [Quest3-Pyrowave](https://github.com/JMS1717/Quest3-Pyrowave), TobiH-GE Vision Pro, WiVRn proto branch | VR streaming over Wi-Fi/USB at 400 Mbit/s with deadline decode | MIT / GPL | Hobby or research. WiVRn master has no PyroWave | Data points (mobile decode cost, Wi-Fi behaviour) |
| **WebGPU port** ([`imbcmdth/pyrowave@webgpu`](https://github.com/imbcmdth/pyrowave/tree/webgpu/webgpu)) and [`ffrwd-package-pyrowave`](https://github.com/imbcmdth/ffrwd-package-pyrowave) (wasm32-wasip2 + `wasi:webgpu`) | Full WGSL encoder and decoder behind a `webgpu.h` C API; runs on wgpu-native and Dawn | MIT | Committed 2026-09-26, 0★ | **Fork** as the basis of the browser decoder (§3.3) |
| `lutyjj/pyrowave-rs` | Rust bindings with a DMA-BUF feature | MIT | Early (2026-10-03) | Fork or inspiration if the node agent or client is Rust |
| FFmpeg / GStreamer upstream | None. Themaister: *"almost zero chance pyrowave will ever make it into FFmpeg upstream"* ([#3](https://github.com/Themaister/pyrowave/issues/3)) | n/a | n/a | Call the C API directly |

### 2.1 pyrofling transport (MIT)

Details below are from `proto/pyro_protocol.h` and `pyro-server/pyro_server.cpp`.

- **Control.** TCP for HELLO → COOKIE → KICK → CODEC_PARAMETERS. The client must send a PROGRESS report every 5 s or it is dropped.
- **Media.** UDP, with a 1024-byte payload plus a 28-byte `pyro_payload_header` (pts, dts delta, size, FEC counts, packet and subpacket sequence, key-frame and stream-type bits).
- **FEC (`--fec`).** About **25 % overhead** using **"HybridLT"**, a seeded sparse-XOR (LT-like fountain) code in `lt/`. Frames of 8 blocks or fewer get one full-XOR parity block.
- **Pacing.** The client sends `pyro_phase_offset` so the server nudges its frame clock and frames arrive just after the client's vblank (`phase_locked=0.0&deadline=0.008`).
- **Other ideas.** The client rate multiplier serves 60 fps to a client presenting at 120 Hz. Gamepad hand-off works through the "mode" button. The README warns: *do not use* `tc` rate limiting with PyroWave, because it adds latency.
- **Security:** *"There is none at this time."*

**Steal:** phase-locked pacing feedback, deadline decode, and the LT-style FEC design (cheap XOR, no GF(256) math).

### 2.2 Valve's Steam Remote Play integration

Closed source. The Metal port, D3D11/D3D12 interop tests, Steam Runtime build scripts, Android build and the realtime-queue API were all added between August and September 2026. They line up with Valve's needs on Windows and macOS hosts and Android/Apple Steam Link clients (inferred, **unverified**).

### 2.3 Vibepollo's PyroWave-over-Moonlight extension (GPL-3.0)

The spec is [`docs/pyrowave-protocol.md`](https://github.com/Nonary/Vibepollo/blob/master/docs/pyrowave-protocol.md) and is mirrored in Nonary's moonlight-qt. These are the details that matter for interop.

**Capability advertisement (`/serverinfo` → `ServerCodecModeSupport`)**

| Bit | Value | Profile |
|---|---|---|
| `SCM_PYROWAVE` | `0x00800000` | 8-bit 4:2:0 |
| `SCM_PYROWAVE_444` | `0x01000000` | 8-bit 4:4:4 |
| `SCM_PYROWAVE_HDR10` | `0x02000000` | 10-bit 4:2:0 |
| `SCM_PYROWAVE_HDR10_444` | `0x04000000` | 10-bit 4:4:4 |

The paired host also returns `PyroWaveHostLinkMbps` and `PyroWaveBandwidthProbeBytes=33554432`. **`GET /pyrowave-bandwidth-probe`** sends 32 MiB; the client takes the slowest of 3 runs and reserves 20 %.

**RTSP negotiation**

- DESCRIBE: `a=rtpmap:99 PYROWAVE/90000`, a marker only (no RTP payload type 99 is ever sent), plus `a=x-ss-pyrowave.bitstream:186f0393`.
- ANNOUNCE: `x-nv-vqos[0].bitStreamFormat=3` plus `pyrowaveFeatures` (`0x1` = record framing), `pyrowaveAdaptiveFec` and `pyrowaveAdaptiveBitrate`. The stock `chromaSamplingType` and `dynamicRangeMode` attributes are reused.

**Framing**

- The existing Moonlight RTP / `NV_VIDEO_PACKET` path is reused with up to 4 Reed-Solomon FEC blocks and optional AES-GCM. `frameType` is always IDR, and IDR/RFI requests are ignored.
- **Record framing:**
  - one sequence header, then block records, then padding records (`0xFFFFFFFF`, N, then N zero words)
  - coarsest-level blocks (index < `12·ceil(W/32)·ceil(H/32)`) are packed first into "critical" shards
  - the rest is packed first-fit so records rarely straddle 1376-byte payloads
  - oversized records come first and span payloads

**FEC**

- `pyrowave_critical_fec_percentage` adds parity on the critical shards (at least 2 parity shards).
- **Adaptive detail FEC** turns on only when the frame rate falls below the negotiated rate on mostly static content: `min(50, 100·(fps_neg/fps_obs − 1))` %. It only uses spare budget.

**Decode and sender behaviour**

- The decoder is cleared each frame. Lost payloads are zero-filled. A frame decodes if all critical packets arrived and more than 90 % of records arrived. The library's pristine-band check is *not* used, because it cannot tell a lost block from a never-sent zero block.
- Frames are capped at 3000 or 4000 packets. When the sender is behind, a new frame replaces the queued one. The last image is re-encoded once per frame interval when nothing new is captured.

**Steal (or implement compatibly):** record framing, critical-shard FEC, the bandwidth probe, "newest frame replaces queued frame", and re-encoding on idle for loss healing. Under the project's GPL/AGPL decision, the code can be borrowed directly.

---

## 3. Browser feasibility (WebGPU / WASM)

### 3.1 What the decoder needs from the GPU

| Stage | Ops used (GLSL) | WebGPU equivalent | Blocker? |
|---|---|---|---|
| CPU packet parse | 8-byte headers, block-offset table, payload concat | JS or WASM, trivial (~200 LoC) | No |
| Dequant (`wavelet_dequant.comp`) | 128-thread workgroups; `subgroupInclusiveAdd`, `subgroupShuffleUp`, `gl_SubgroupID`/`gl_NumSubgroups`; 8/16-bit SSBO loads; `imageStore` to an R16F 2-D array | `subgroups` feature (`subgroupInclusiveAdd`, `subgroupShuffleUp`, `subgroupBroadcast`); `subgroup_id`/`num_subgroups` via the WGSL `subgroup_id` extension (Chrome 144+) or an atomic "ticket" emulation; u8/u16 extracted from `array<u32>`; storage texture **r32float** (core) or **r16float** (`texture-formats-tier1`) | **Soft.** All scans fit in 128 threads, so a pure `var<workgroup>` scan replaces the subgroup ops (Firefox path). WebGPU cannot force subgroup size; the port handles that |
| iDWT (`idwt.comp`) | 64 threads; `textureGather` with a MIRRORED_REPEAT nearest sampler on a `sampler2DArray`; ~6.5 KB workgroup memory; `imageStore` | `textureGather` + `mirror-repeat` sampler (core); workgroup memory well under the 16 KB default; storage write r32float (core) or r16float/r8unorm (tier1) | No |
| YCbCr → RGB + present | Fragment shader | Render pass to the canvas texture | No |
| Optional FP16 | `shaderFloat16` | `shader-f16` (Chrome, Safari) | Optional. The port skips it |

None of the decoder stages needs int64, atomics on images, subgroup size control, push constants or large workgroups. WebGPU now has immediates (Chrome 149–150) if per-dispatch constants are wanted.

### 3.2 WebGPU feature availability (October 2026)

| Feature | Chrome / Edge | Firefox | Safari |
|---|---|---|---|
| WebGPU itself | Yes (since 113). Linux NVIDIA/Wayland in 147–148 ([post](https://developer.chrome.com/blog/new-in-webgpu-147-148)) | Yes: Windows 141, Apple Silicon 147 ([release notes](https://www.firefox.com/en-US/firefox/147.0/releasenotes/)), workers 148 | Yes: Safari 26.0 on macOS, iOS and visionOS ([WebKit](https://webkit.org/blog/17333/webkit-features-in-safari-26-0/)) |
| `subgroups` | **Shipped in Chrome 134** ([post](https://developer.chrome.com/blog/new-in-webgpu-134)) | **Not implemented.** `dom/webgpu/Adapter.cpp` returns *unimplemented*, [bug 1955417](https://bugzilla.mozilla.org/show_bug.cgi?id=1955417) | **Landed in WebKit main 2026-07-14**, gated on `MTLGPUFamilyMetal3` (`HardwareCapabilities.mm`). Which Safari release ships it is **unverified** |
| WGSL `subgroup_id` | Chrome 144, emulated on D3D ([post](https://developer.chrome.com/blog/new-in-webgpu-144)) | n/a | **unverified** |
| `texture-formats-tier1` (r16float/r8unorm storage) | Chrome 142 ([post](https://developer.chrome.com/blog/new-in-webgpu-142)) | Not exposed in `Adapter.cpp` | Yes on ARM64 (WebKit `HardwareCapabilities.mm`) |
| `shader-f16` | Yes | Yes | Yes |
| `timestamp-query` | Yes | Yes | Yes (where the counter set exists) |

### 3.3 The existing WebGPU port (MIT): analysis

Location: [`imbcmdth/pyrowave@webgpu/webgpu`](https://github.com/imbcmdth/pyrowave/tree/webgpu/webgpu). It contains 12 WGSL files (~2.4k lines), a `webgpu.h` C API and the Granite-free bitstream code reused from the Metal port.

**What works**

- Hand ports of **all encoder and decoder shaders**.
- **Bit-identical forward transform** to the Vulkan encoder. Streams interoperate in both directions.
- Decoder 8-bit output equals the Vulkan float output rounded to nearest.
- Tested on wgpu-native v29 (Vulkan) and Dawn nightly 20260925 (Vulkan and D3D12).

**Porting tricks to reuse**

- A subgroup-ID "ticket" emulation (`subgroup.wgsl`).
- Byte and half loads extracted from u32 words.
- `atomicOr` byte writes in the encoder.
- Explicit FP16 rounding done with integer math. The WGSL builtins misbehaved: NVIDIA folded `unpack2x16float(pack2x16float(x))`, and D3D12's `pack2x16float` does not round to nearest.
- **All bands of a level run in one dispatch**, because WebGPU inserts barriers between storage-writing dispatches. Upstream's ~40 dispatches per stage made quantize and pack 10–20× slower.

**Gaps for Cha Portal**

- It **requires `subgroups`**, so it does not run in Firefox. It needs a workgroup-scan variant of `wavelet_dequant.wgsl`.
- The pyramid is `r32float` only, twice the bandwidth of upstream's R16F. Add a tier1 `r16float` path.
- **The decoder only reads back to CPU** (`decode_read`). For display we need a GPU output: write planes to textures and add a YCbCr→RGB render pass to the canvas.
- It is C++ against `webgpu.h`. For the browser there are two options:
  - (a) compile with Emscripten plus Dawn's `emdawnwebgpu` port, or
  - (b) port the ~1k lines of host logic to TypeScript and keep the WGSL as is. **Recommended**: smaller bundle, no WASM↔JS GPU handle juggling, easy to profile.
- It has existed for a day and has no users. Expect to own the fork.

### 3.4 Performance estimate in the browser

- **Discrete GPU.** The port measured GPU decode at **0.089 ms** for 1080p 4:2:0 on an RTX 4090 via wgpu-native. That is 1.6× the Vulkan figure, mostly extra barriers and the r32float pyramid. 4K 4:4:4 has ~8× the coefficients, which suggests **≈0.5–1 ms** (estimate).
- **iGPU** (Intel Xe or Apple M-series). Scaling by memory bandwidth (~10–20× less than a 4090) suggests **≈1–2 ms at 1080p and ≈5–15 ms at 4K 4:4:4** (estimate, **unverified**). 4K120 4:4:4 on an iGPU in a browser is unlikely; 1440p60 should be fine.
- **Memory traffic.** 4K 4:4:4 with r32float is ~100 MB of coefficient writes per frame plus reads, roughly 30–40 GB/s at 120 fps. tier1 r16float halves that.
- **Per-frame upload.** `queue.writeBuffer` of the payload: at 500 Mbit/s and 60 fps that is about 1 MB/frame, which is cheap.

### 3.5 Getting the bits into the browser (the real risk)

Packet rates: at 500 Mbit/s with ~1150-byte payloads that is **≈54k datagrams/s**; at 1 Gbit/s, **≈108k/s**. Each WebTransport datagram is one `ReadableStream` chunk in JS.

| Option | Pros | Cons |
|---|---|---|
| **WebTransport datagrams** (Baseline since Safari 26.4, March 2026; [webrtc.ventures](https://webrtc.ventures/2026/04/webtransport-is-now-baseline-what-it-means-for-real-time-media/)) | Unreliable and unordered, which fits PyroWave's per-block independence. `getStats()` exposes RTT and drops | `maxDatagramSize` is ~1200 bytes. QUIC encryption and congestion control apply. **Chrome's per-datagram JS overhead and QUIC receive throughput at 300–1000 Mbit/s are unbenchmarked (unverified).** Blocks larger than one datagram need fragmentation |
| **WebTransport, one uni stream per frame** (or per band group) | Reliable within the frame, with no head-of-line blocking across frames. Cancel a late frame with `RESET_STREAM`. Large writes mean far fewer JS events. Retransmits on a LAN cost under 1 ms | Retransmit delay matters only on lossy links. Same QUIC throughput question |
| WebRTC DataChannel (unordered, `maxRetransmits:0`) | Universal; NAT traversal comes free | SCTP-over-DTLS throughput ceilings (**unverified**). Any lost fragment kills the whole message |
| WebSocket (TCP) | Highest raw throughput in browsers; simple | Head-of-line blocking. Acceptable on clean wired LANs; poor under loss |

**Recommendation.** Build a 1-day **transport benchmark** before committing: a Chrome/Firefox/Safari client receiving 200 / 500 / 900 Mbit/s of synthetic PyroWave-sized traffic over each option, measuring CPU load and loss. Prefer **WebTransport with stream-per-frame on clean LANs**, and datagrams plus critical-shard FEC when loss is non-zero.

### 3.6 Presentation latency

PyroWave's ~0.1 ms is dwarfed by the browser compositor. A WebGPU canvas present usually lands one compositor frame later; there is no `desynchronized` equivalent for WebGPU canvases (**unverified**). The rendering and event loop add jitter on top. Native clients with a MAILBOX or IMMEDIATE swapchain, `present_wait` and fullscreen direct scanout will beat the browser by roughly a frame. This mainly argues for the **native thin client** as the "Moonlight-class" path. The browser should be the convenient path.

### 3.7 WASM SIMD fallback

**Not worth it.** The iDWT is about 20 FLOP per sample. 1080p 4:2:0 at 60 fps (3.1 M samples/frame) needs about 4 GFLOP/s, which is achievable on 2–4 WASM threads with SharedArrayBuffer and COOP/COEP isolation (estimate). 4K 4:4:4 at 120 fps needs about 60 GFLOP/s, which is not. WebGPU is Baseline in 2026, so treat WASM as a last-resort 1080p60 mode at most. A WebGL2 fallback (fragment iDWT with the dequant moved to CPU) is possible but not justified.

### 3.8 Browser verdict

PyroWave in the browser is **feasible for Chrome now**. Safari likely follows once subgroups ship, or immediately with a subgroup-free dequant. **Firefox needs the workgroup-scan path.** Gate it behind measured transport capacity. Even then it is a "LAN tier in the browser", not a latency-record path.

---

## 4. Native thin-client feasibility

| Platform | Decoder | Presentation |
|---|---|---|
| Linux, Windows, Steam Deck, Android | **libpyrowave (Vulkan)**. Use `pyrowave_create_device` to share the client's `VkDevice`; `decode_gpu_buffer` into R8/R16 planes; a YCbCr→RGB pass into the swapchain. Choose the fragment path on Mali; **measure** compute vs fragment on Adreno (compute was faster on Adreno 740) | `VK_PRESENT_MODE_MAILBOX_KHR`, or IMMEDIATE with VRR; `VK_KHR_present_wait`/`present_id` for latency metrics; direct scanout in fullscreen. Use pyrofling-style phase-locked presentation for paced output |
| macOS, iOS | In-tree **Metal port** (API 0.5; Valve-contributed; upstream may drop it), or the Vulkan build on KosmicKrisp/MoltenVK (~2× slower than Metal) | CAMetalLayer with `displaySyncEnabled = NO` for lowest latency |
| Any (single codebase) | Rust with **wgpu** plus the WGSL from §3.3, shared with the browser build | wgpu-native caveats: subgroups only as a native feature, none on D3D12 (port README), so force the Vulkan backend on Windows |

**Recommendation.** Write the native client in Rust (or C++) **linking libpyrowave directly** on Vulkan platforms, using the Metal port on Apple. Keep the wgpu/WGSL decoder as a shared fallback and for parity testing against the browser. Expected decode cost is 0.05–0.2 ms on desktop and 3–6 ms on mobile, so end-to-end latency will be dominated by capture, pacing and the display.

---

## 5. Codec landscape 2026 for low-latency streaming

### 5.1 Hardware encoders

| Vendor | Codecs | 4:4:4 encode | Low-latency and loss tools | Notes |
|---|---|---|---|---|
| **NVIDIA NVENC** (Turing → Blackwell) | H.264, HEVC; **AV1 from Ada (RTX 40)** | **H.264 and HEVC 4:4:4: yes.** AV1 4:4:4: no (**unverified**) | Ultra-low-latency tuning, **intra-refresh**, **LTR**, **reference-picture invalidation** (`NvEncInvalidateRefFrames`), slices, CBR with small VBV | **Blackwell (9th gen):** 4:2:2 H.264/HEVC encode and decode, ~5 % BD-BR gain for HEVC/AV1, AV1 UHQ mode with lookahead (latency-tolerant only) ([NVIDIA Blackwell whitepaper](https://images.nvidia.com/aem-dam/Solutions/geforce/blackwell/nvidia-rtx-blackwell-gpu-architecture.pdf)) |
| **AMD AMF / VCN** (VCN4 = RDNA3, VCN5 = RDNA4) | H.264, HEVC, AV1 (RDNA3+) | **No** hardware 4:4:4 encode (**unverified**; Sunshine's 4:4:4 support lists NVIDIA and Intel only) | Intra-refresh, LTR, slices, pre-analysis off for latency | RDNA4 improved H.264/AV1 quality (**unverified** magnitude) |
| **Intel QSV / VAAPI** (Arc, Xe2/Xe3, Gen12+) | H.264, HEVC, AV1 (Arc+) | **HEVC 4:4:4 yes** (Gen11/12+), which Sunshine uses on Windows | Intra-refresh, LTR, low-power (VDEnc) mode | HEVC SCC encode on Gen12 (**unverified**) |
| **Apple VideoToolbox** | H.264, HEVC; ProRes on M-series | HEVC 4:4:4: **unverified** | Real-time mode | Only relevant for macOS hosts |
| **Vulkan Video encode** | H.264, H.265 (2023), **AV1 (`VK_KHR_video_encode_av1`)**, **`VK_KHR_video_encode_intra_refresh`** (Vulkan 1.4.321, July 2025), quantization maps | Driver-dependent | Intra-refresh: RADV in Mesa 25.3 ([Phoronix](https://www.phoronix.com/news/RADV-Intra-Refresh-Video)), NVIDIA beta drivers ([Khronos](https://www.khronos.org/blog/khronos-announces-vulkan-video-encode-intra-refresh-extension)) | Driver matrix below |

**Vulkan Video driver matrix** (from [Igalia's status page](https://blogs.igalia.com/vjaquez/vulkan-video-status/), updated 2026-05-22):

| Driver | H.264/H.265 encode | AV1 encode |
|---|---|---|
| NVIDIA | Linux 535.43 | Linux 550.40 |
| RADV | Mesa 23.1–24.1 | Mesa 25.2 |
| ANV | Mesa 24.3 | Not listed |

Framework support on the same page:

- FFmpeg: Vulkan H.264/H.265/AV1 encode across 7.1–8.0.
- GStreamer: the Vulkan H.264 encoder is still listed as in development.
- FFmpeg 8.1 (March 2026) added Vulkan ProRes and DPX compute codecs, D3D12 H.264/AV1 encoders, and JPEG-XS via SVT ([Phoronix](https://www.phoronix.com/news/FFmpeg-8.1-Coming-Soon)).
- Themaister's **pyroenc** (MIT, [repo](https://github.com/HansKristian-Work/pyroenc)) is a small Vulkan Video encode library: H.264 and H.265 8/10-bit, RGB input with GPU colour conversion, on-demand IDR, and intra-refresh (marked "not well-tested"). pyrofling uses it as `h264_pyro` and `h265_pyro`.

### 5.2 4:4:4 and text clarity

- For desktop text, **4:4:4 matters more than bitrate**: chroma fringing on coloured text and UI.
  - Sunshine/Moonlight support 4:4:4 on NVIDIA HEVC (and Intel on Windows; NVIDIA on Linux via the CUDA paths) ([Sunshine releases](https://github.com/LizardByte/Sunshine/releases)).
  - GeForce NOW's "Cinematic Quality Streaming" ships **YUV 4:4:4, 10-bit HDR and AV1** at around **100 Mbit/s** on Blackwell servers ([NVIDIA](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)).
- **AV1 screen-content tools** exist in the spec (palette mode, intra block copy, `allow_screen_content_tools`), but hardware encoders rarely expose them (**unverified** per vendor). HEVC SCC is similar.
- PyroWave does 4:4:4 natively at roughly +15–20 % bits for equal luma quality. Vibepollo's guidance is ~1.6×.

### 5.3 Loss recovery techniques (inter codecs)

| Technique | How it works | Who uses it |
|---|---|---|
| IDR on request | Simplest; causes a bitrate spike | Everyone, as a fallback |
| **RFI** (reference frame invalidation) | The client reports the lost frame; the encoder re-references the last good frame | Moonlight/Sunshine on capable encoders |
| **LTR** (long-term references) | Keep a known-good reference to fall back to | Common building block for RFI |
| **Intra-refresh** | A rolling intra column or row; flat bitrate, no IDR spikes, but a visible "crawl" at low bitrate | pyroenc/pyrofling, WiVRn, Steam Remote Play (**unverified**) |
| **FEC** | Moonlight uses Reed-Solomon (default ~20 %, configurable) | Moonlight; Vibepollo's PyroWave mode adds critical-only FEC |
| **Slices** | Limit error propagation and allow parallel decode | Moonlight client-requested slices-per-frame |

PyroWave avoids this entire category: every frame is a keyframe.

### 5.4 Intra and wavelet peers

| Codec | Transform / coding | Latency | Bitrate / quality | License and openness | GPU real-time OSS? | Verdict |
|---|---|---|---|---|---|---|
| **JPEG XS** (ISO/IEC 21122; eds. 2019, 2022, 2024 with **TDC** temporal differential coding) | Le Gall 5/3 DWT; **5 horizontal × 0–2 vertical** levels; Golomb-like significance and bit-plane coding | **1–32 lines** (sub-frame) | Visually lossless at 2:1–10:1; broadcast-style (SMPTE ST 2110-22, RFC 9134) ([Wikipedia](https://en.wikipedia.org/wiki/JPEG_XS)) | **Patent pool, RAND royalties.** [SVT-JPEG-XS](https://github.com/OpenVisualCloud/SVT-JPEG-XS) is BSD+Patent (CPU); FFmpeg 8.1 wraps it | No OSS GPU encoder found (**unverified**) | Inspiration only: line-based sub-frame pipelining and TDC are good ideas. Patents rule out depending on it |
| **HTJ2K** (ISO/IEC 15444-15, ITU-T T.814) | 9/7 or 5/3 DWT plus the FBCOT block coder (much faster than J2K's EBCOT) | Frame-based | Much better compression than PyroWave at the same quality | OpenJPH (BSD-2, CPU); nvJPEG2000 has HTJ2K decode (proprietary) (**unverified**) | Not for real-time encode | Inspiration. The README names it as the path to "proper" entropy coding |
| **VC-2 / Dirac Pro** (SMPTE 2042) | Wavelet, fixed-size slices, exp-Golomb | Low | Needs very high rates. cyanreg: 1080p60 4:2:0 ≈ 250 Mbit/s; Themaister's SF6 1440p 4:4:4 test at 300–700 Mbit/s gave garbage (8 dB) vs PyroWave 35–46 dB ([#2](https://github.com/Themaister/pyrowave/issues/2)) | Royalty-free | Vulkan VC-2 from GSoC 2024, **not merged** | No |
| **APV** (Samsung, open spec) | Intra DCT, entropy coded | Has a sub-frame real-time spec | ~120 Mbit/s for 1080p60 at quality similar to VC-2 at 250 (cyanreg) | Open / royalty-free (**unverified**) | cyanreg wrote a Vulkan APV encoder/decoder at ~1.5k fps on a 6900 XT (upstream status **unverified**) | Interesting future peer: ~2× more efficient than PyroWave, but no streaming stack exists |
| **ProRes** | Intra DCT | Frame | Mezzanine-class rates | Apple licence | FFmpeg 8.1 Vulkan ProRes | No |

### 5.5 What the incumbents use

| Service | Transport | Codecs | Notes |
|---|---|---|---|
| **Moonlight / Sunshine** (and Apollo, Vibepollo) | RTSP + RTP-ish over UDP; Reed-Solomon FEC; optional AES-GCM | H.264 / HEVC / AV1, HDR, 4:4:4 | Vibepollo adds PyroWave (§2.3) |
| **Parsec** | Proprietary "BUD" protocol over UDP | H.264 / H.265; 4:4:4 "color mode" (**unverified** tier) | |
| **GeForce NOW** | WebRTC-based in browsers (**unverified** for native) | H.264 / HEVC / AV1; CQS 4:4:4 10-bit ~100 Mbit/s | L4S-based "Low Latency Streaming"; up to 5K120 and 360 fps at 1080p (NVIDIA press) |
| **Steam Remote Play** | Proprietary | H.264 / HEVC / **AV1**; **PyroWave beta** | AV1 is confirmed by the [#19](https://github.com/Themaister/pyrowave/issues/19) reporter switching to it |

### 5.6 WebCodecs decode matrix (browser, October 2026)

Data from the [WebCodecs Fundamentals 2026 dataset](https://webcodecsfundamentals.org/datasets/codec-analysis-2026/) (2.4 B tests, Jan–Oct 2026), [StaZhu's Chromium HEVC guide](https://github.com/StaZhu/enable-chromium-hevc-hardware-decoding) and the [WebCodecs HEVC notes](https://webcodecsfundamentals.org/codecs/hevc.html).

| | Chrome / Edge | Firefox | Safari |
|---|---|---|---|
| H.264 decode | ~universal (HW, plus SW fallback) | yes | yes |
| HEVC decode | HW only. Chrome non-Windows ~85 %; Edge on Windows ~57 % | Shipped desktop decode in 133–137 (Windows MFT via MS extension, macOS VT, Linux VA-API) **for media playback**; WebCodecs coverage "nearly absent" | ~universal |
| AV1 decode | ~91 % on desktop (dav1d SW plus HW) | ~91 % desktop; **0 % on Android (API gap)** | HW only (M3+/A17 Pro+): ~27 % macOS, ~33 % iOS |
| 10-bit | AV1 and HEVC Main10 yes | AV1 yes | HEVC Main10 yes |
| **4:4:4 decode** | **HEVC Rext 4:4:4 HW**: macOS Apple Silicon (8/10-bit), Windows Intel Gen11+, **NVIDIA from Chrome 137** (full Rext on Blackwell), AMD on newer generations. AV1 High (4:4:4) via dav1d SW (**unverified**). H.264 4:4:4 SW (**unverified**) | **unverified** | HEVC 4:4:4 via VideoToolbox (**unverified**) |
| Zero-copy to WebGPU | `importExternalTexture(VideoFrame)` | yes (**unverified** perf) | yes (**unverified** perf) |

Practical consequences for WAN:

- **AV1 4:2:0 and HEVC Main/Main10** cover ~99.6 % of browsers combined.
- **H.264 is the universal floor.**
- **4:4:4 in the browser** is reliable only as HEVC Rext on Chrome with supported GPUs or on Apple Silicon. Probe with `VideoDecoder.isConfigSupported` and `hardwareAcceleration: 'prefer-hardware'`.

---

## 6. Recommendations

### 6.1 Where PyroWave fits

- **LAN, wired, ≥1 GbE (ideally 2.5/10 GbE) with near-zero loss.** This is PyroWave's home. It gives the best latency (codec ≈0, no reference chains), no IDR spikes, graceful blur on loss, and native 4:4:4 and HDR.
- **LAN Wi-Fi 6/6E/7.** Opt-in "experimental" only, with deadline partial decode and critical FEC. The ALVR and Galaxy XR results show it works at about 400 Mbit/s, but it is fragile.
- **WAN.** Not a fit. Even "low" settings at 100–150 Mbit/s exceed typical uplinks, and loss bursts on the internet make LL-band FEC overhead grow. The exception is a symmetric multi-gigabit site-to-site link, which the README says has been "battle tested over fiber".

### 6.2 Recommended codec ladder

The tier is selected per session from three inputs:

- a **bandwidth probe** (Vibepollo-style, 32 MiB HTTPS, slowest of 3, minus 20 %) plus a short **UDP burst probe** for loss
- **client capability probes**: `navigator.gpu` features, `VideoDecoder.isConfigSupported`, native caps
- a **content profile**: desktop/text (H ≈ 1.25, prefers 4:4:4) vs game/TV (H ≈ 2)

| Tier | When | Codec | Settings | Client |
|---|---|---|---|---|
| **T0 "Portal Direct"** | Wired LAN, probe ≥ model bitrate × 1.3, loss < 0.1 % | **PyroWave** 4:4:4 for desktop, 4:2:0 for games; HDR10 when both ends support it | Bitrate from the regression at 35 dB and the chosen H, capped by the probe. Critical-shard FEC 10–25 %. Deadline decode at ~½ frame | Native (Vulkan/Metal) first. Browser WebGPU when the transport benchmark passes |
| **T1 "LAN-Lite / fast WAN"** | Wi-Fi LAN or WAN ≥ ~80 Mbit/s | **HEVC 4:4:4** (NVENC/QSV) when the client can decode it, otherwise **AV1 10-bit 4:2:0** or HEVC Main10 | 40–150 Mbit/s CBR, no B-frames, intra-refresh, LTR + RFI, RS FEC ~10–20 %, slices | WebCodecs HW decode or native |
| **T2 "WAN"** | 10–80 Mbit/s | **AV1** (Ada/RDNA3/Arc+) or **HEVC** 4:2:0; H.264 where the client lacks them | Adaptive bitrate (GCC/L4S-style), RFI preferred over IDR, FEC adapted to loss | WebCodecs or native |
| **T3 "Compat"** | Weak or unknown clients, Firefox Android | **H.264** High 4:2:0 | Low-latency profile | Universal |

Encoder access on nodes, in order of preference:

1. NVENC, VAAPI or AMF via FFmpeg/GStreamer. This is the most mature path, including for Wolf-style pipelines.
2. **Vulkan Video via pyroenc**, an optional zero-copy path that shares the `VkDevice` with PyroWave. RADV now has intra-refresh.

### 6.3 Plan to make PyroWave first-class

**Phase 0 — spikes (1–2 weeks)**

1. **Transport benchmark** (§3.5): browser receive throughput and CPU at 200 / 500 / 900 Mbit/s for WebTransport datagrams vs stream-per-frame vs DataChannel, in Chrome, Firefox and Safari.
2. Build libpyrowave in the node's container base image (Vulkan 1.3; NVIDIA via the container toolkit with `graphics` capability; AMD/Intel via `/dev/dri` plus Mesa RADV/ANV). Run `pyrowave-c-test` as a GPU qualification step on node registration.

**Phase 1 — node encoder (zero-copy)**

- **Capture.** The environment's compositor (gamescope for Steam, or a KWin/Mutter/wlroots headless session) exports **DMA-BUF RGB(A)/RGB10 with a DRM modifier plus SYNC_FD**. As a fallback, consume a **PipeWire NV12 DMA-BUF** (4:2:0 only).
- **Encode path:**

  ```
  pyrowave_create_device_by_compat2(.., VK_QUEUE_GLOBAL_PRIORITY_REALTIME)   // CAP_SYS_NICE in compose
  pyrowave_image_create(DMA_BUF + VkImageDrmFormatModifierExplicitCreateInfoEXT)
  pyrowave_sync_object_create(SYNC_FD import, TEMPORARY)                     // acquire
  pyrowave_encoder_encode_gpu_scaled_synchronous(view, sRGB|HDR10 -> YCbCr, R16 intermediate,
                                                 crop/scale, &{max_bytes = bitrate/fps/8})
  pyrowave_encoder_compute_num_critical_packets(bands=2..3, boundary, padding)
  pyrowave_encoder_packetize_with_padding(boundary = transport_payload, padding = our header)
  ```

  When the compositor is ours and Vulkan-based, use `pyrowave_create_device` (shared `VkDevice`) and `pyrowave_device_set_command_buffer` to record the encode straight after composition, which saves a submit.
- **Pacing.** Capture-driven, with a heartbeat. Use phase-offset feedback from the client (pyrofling `pyro_phase_offset`). Replace any queued unsent frame with the newest (Vibepollo).
- **Static-desktop efficiency.** Skip encoding when nothing changed (compositor damage), with a refresh every N ms for loss healing. A future R&D item is a "conditional replenishment" extension that sends only changed blocks with a persistent decoder coefficient store. It is non-standard and needs decoder changes, so keep it behind a feature bit.
- **Bitrate selection.** Vendor `pyrowave_regression_results.h`; the user picks a quality (dB) and a viewing mode (H). Clamp to the probe result and NIC speed (Vibepollo reads link speed every 2 s).

**Phase 2 — Cha Portal media framing (transport-agnostic)**

- One **frame message** = sequence header + block records. Use **Vibepollo-compatible record framing**: padding records, critical blocks first, first-fit packing, so the same parser serves the Moonlight interop path.
- **Datagram mode:** a ~12–16 byte Cha header (session, frame id 32-bit — not PyroWave's 3-bit counter —, shard index, shard count, critical-shard count, FEC group), shards aligned to records, RS or HybridLT XOR FEC over critical shards, adaptive detail FEC. Oversized records are fragmented across shards and treated as "all or nothing".
- **Stream mode:** one QUIC or WebTransport uni stream per frame, reset after the deadline.
- **Negotiation:** codec = `pyrowave`, a `bitstream_id` (vendored commit), profile (420/444, SDR/HDR10), max datagram size, FEC capabilities. Clear the decoder per frame to avoid the 3-bit wrap problem.

**Phase 3 — native thin client (first PyroWave consumer)**

- Rust (or C++) app: QUIC/UDP receive, reassembly, `pyrowave_decoder_push_packet`, deadline-driven `decode_is_ready_with_sideband`, `decode_gpu_buffer` into planes, a YCbCr→RGB pass, and present with MAILBOX or IMMEDIATE plus `present_wait`.
- Apple: Metal port. Android: Vulkan, compute or fragment chosen by measurement.
- Expose latency HUD stats: capture→encode→send→recv→decode→present.

**Phase 4 — browser decoder**

- Fork `imbcmdth/pyrowave@webgpu`. Port the decoder host logic to **TypeScript** and keep the WGSL. Then add:
  - (a) `wavelet_dequant` **without subgroups** (workgroup scans) for Firefox and Safari-without-subgroups
  - (b) use of `subgroup_id` when present (Chrome 144+)
  - (c) a **`texture-formats-tier1` r16float pyramid**
  - (d) **GPU output** to the canvas through a YCbCr→RGB render pass, so there is no readback
  - (e) feature-detected `shader-f16`
- Run it in a **Worker with OffscreenCanvas**, with network receive in the same worker to avoid main-thread jank.
- CI: decode the same golden streams with the Vulkan build and the WebGPU build, and assert PSNR ≥ 60 dB between them (the port's own validation method).

**Phase 5 — external endpoints**

- Implement **Vibepollo's PyroWave negotiation and record framing** (§2.3) in the Cha Portal client's Moonlight-protocol adapter, so users can portal into Vibepollo hosts with PyroWave.
- Optionally let Cha Portal nodes **serve** the Moonlight protocol with PyroWave too, so Nonary's Moonlight fork can connect.
- Track Wolf PR #517 and reuse its GStreamer element if it merges.

---

## 7. Risks and gotchas

1. **Bitstream and API instability** (draft spec, API 0.6.0, no version field). Pin a commit, negotiate a `bitstream_id`, and keep a golden-stream conformance suite.
2. **Bandwidth reality for desktops.** H ≈ 1–1.5 at 1440p/4K 4:4:4 needs 0.5–0.9 Gbit/s at 60 Hz and twice that at 120 Hz. 1 GbE homelabs will be capped around 1080p–1440p60. Surface this in the UI as "needs 2.5 GbE".
3. **Intra-only cost on static content.** Without damage-driven skipping, an idle desktop burns hundreds of Mbit/s.
4. **Browser transport ceilings** (QUIC throughput, per-datagram JS overhead) are unproven at these rates. **Browser compositor latency** removes most of PyroWave's advantage.
5. **WebGPU fragmentation.** Firefox has no subgroups and no tier1; Safari's subgroups are recent. A subgroup-free path is mandatory.
6. **Single maintainer.** Themaister has no interest in Moonlight/Sunshine. The Metal port may be removed upstream. Valve's adoption lowers the abandonment risk but does not guarantee API stability.
7. **GPU-specific correctness bugs** (Intel subgroup-16 encoder bug, NVIDIA R8 rounding, FP16 folding in WGSL). Run the self-test on each node and client GPU.
8. **Mobile decode is 3–6 ms**, not 0.1 ms. Budget for it on Android, Quest and Deck-class devices.
9. **Oversized blocks** (up to ~16 KB) break the "one datagram = whole blocks" assumption. Fragment them.
10. **The 3-bit sequence counter** misorders frames after ≥4 consecutive drops. Clear the decoder per frame and carry our own 32-bit frame id.
11. **Encode competes with the game for GPU time.** Use the REALTIME queue (needs `CAP_SYS_NICE`) and cap the game's frame rate below the GPU limit, as Themaister advises.

## 8. Open questions

- Will Themaister or Valve freeze a v1 bitstream with a version field? Will Valve document its transport (FEC, Wi-Fi heuristics) or open the Steam Link client support?
- Which Safari release ships WebGPU `subgroups`, and when will Firefox close bug 1955417?
- What are Chrome's, Firefox's and Safari's **actual** WebTransport datagram and stream receive ceilings at 500 Mbit/s–1 Gbit/s on desktop hardware? (Spike 0.1.)
- Can browsers present a WebGPU canvas with less than one frame of compositor latency (low-latency or desynchronized canvas for WebGPU)?
- Is a conditional-replenishment (changed-blocks-only) extension worth carrying as a Cha-specific bitstream feature for desktops, or should we push it upstream?
- Is HEVC Rext 4:4:4 WebCodecs decode on NVIDIA (Chrome 137+) robust enough to be the WAN text-clarity tier? Does any hardware AV1 encoder ship 4:4:4 (AV1 High profile)?
- APV (an open intra DCT codec with Vulkan implementations, ~2× PyroWave's efficiency per cyanreg) could become a "T0.5" tier for 1 GbE desktops. Who maintains a streaming-ready implementation?

## Sources (primary)

- PyroWave repo, README, bitstream spec, eval docs, issues #2–#19: https://github.com/Themaister/pyrowave
- Blog: https://themaister.net/blog/2025/06/16/i-designed-my-own-ridiculously-fast-game-streaming-video-codec-pyrowave/
- pyrofling: https://github.com/Themaister/pyrofling
- pyroenc: https://github.com/HansKristian-Work/pyroenc
- WebGPU port: https://github.com/imbcmdth/pyrowave/tree/webgpu/webgpu
- ffrwd package: https://github.com/imbcmdth/ffrwd-package-pyrowave
- Vibepollo 2.0.0 and PyroWave protocol: https://github.com/Nonary/Vibepollo (`docs/pyrowave-protocol.md`, `release_notes/2.0.0.md`)
- Steam beta: https://steamcommunity.com/groups/homestream/discussions/0/564794422009744473/
- Steam beta coverage: https://www.phoronix.com/news/Valve-Steam-Beta-Pyrowave, https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/
- Wolf: https://github.com/games-on-whales/wolf/issues/515, https://github.com/games-on-whales/wolf/pull/517
- VR field reports: https://github.com/Terminal-ennui/galaxy-xr-alvr-pyrowave-444, https://github.com/joemossjr16/pyrowave-streaming
- Chrome WebGPU: https://developer.chrome.com/blog/new-in-webgpu-134, …-142, …-144, …-147-148, …-149-150
- WebKit features: https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WebGPU/WebGPU/HardwareCapabilities.mm
- Firefox features: https://raw.githubusercontent.com/mozilla-firefox/firefox/main/dom/webgpu/Adapter.cpp
- Vulkan Video status: https://blogs.igalia.com/vjaquez/vulkan-video-status/
- Vulkan Video intra-refresh: https://www.khronos.org/blog/khronos-announces-vulkan-video-encode-intra-refresh-extension
- WebCodecs data: https://webcodecsfundamentals.org/datasets/codec-analysis-2026/
- Chromium HEVC: https://github.com/StaZhu/enable-chromium-hevc-hardware-decoding
- JPEG XS: https://en.wikipedia.org/wiki/JPEG_XS
- SVT-JPEG-XS: https://github.com/OpenVisualCloud/SVT-JPEG-XS
- GeForce NOW Blackwell: https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now
