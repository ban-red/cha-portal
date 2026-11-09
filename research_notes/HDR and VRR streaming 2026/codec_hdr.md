# Codec-side HDR for Cha Portal: 10-bit encode, colour signalling and tone mapping on NVENC, VA-API, SVT-AV1 and PyroWave (as of 2026-11-09)

Scope: low-delay real-time encoding only (no B-frames, no lookahead, one-frame VBV). VOD figures are context only. Labels used: **[vendor]** = vendor docs or headers, **[source]** = read directly in open-source code, **[single-dev]** = one developer's report, **[independent]** = third-party measurement, **[arith]** = my own arithmetic, **(unverified)** = not confirmed from a primary source. Code was read on 2026-11-09 from each project's default branch, so line numbers may drift.

## 1. NVENC (Video Codec SDK 12.x/13.x, Ada RTX 4090): 10-bit input, RGB→YUV matrix, latency, HDR metadata, 4:4:4

### Takeaway
NVENC accepts 10-bit packed RGB (ARGB10/ABGR10) and 10-bit YUV (P010-style YUV420_10BIT, YUV444_10BIT). Its built-in RGB→YUV conversion runs as a CUDA kernel, and FFmpeg hard-tags it as BT.601 ("bt470bg"), limited range, whatever the requested colourspace. So for HDR10, Cha should do its own BT.2020-NCL/PQ conversion to P010 or YUV444_10BIT in CUDA or Vulkan, as Sunshine does, and must not rely on ARGB10. The same evidence suggests Cha's current SDR path (ARGB input tagged matrix BT.709) may be mis-tagged. HDR10 MDCV/CLLI emission is native in SDK 13.0+ for HEVC and AV1 only. AV1 4:4:4 is not supported even in SDK 13.1, and HEVC 4:4:4 10-bit is available (FREXT). NVIDIA publishes no 10-bit latency or throughput numbers.

### Cited Findings
- **SDK version.** The current nv-codec-headers master is `NVENCAPI_MAJOR_VERSION 13`, `MINOR_VERSION 1` [vendor] — [nvEncodeAPI.h (FFmpeg/nv-codec-headers master)](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h)
- **Input buffer formats** [vendor] — [nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h):
  - `YUV420_10BIT` (0x10000) is 10-bit semi-planar Y + interleaved UV (P010 layout), 2 bytes per sample, data in the top 10 bits.
  - `YUV444_10BIT` (0x100000) is 10-bit planar 4:4:4.
  - `ARGB10` (0x2000000) is packed A2R10G10B10, and `ABGR10` (0x20000000) is packed A2B10G10R10.
  - `NV16` and `P210` (4:2:2) are new in SDK 13.0. Diffing tags shows P210 is absent from n12.1.14.0 and n12.2.72.0 and present from n13.0.19.0 [source/arith].
- **RGB→YUV conversion is a CUDA kernel.** The NVENC Programming Guide (SDK 13.0) §6.4 lists "Encoding of RGB contents" among the features that use CUDA internally. Neither the guide nor the header documents which matrix the RGB→YUV conversion uses, or whether VUI `colourMatrix` affects it [vendor] — [NVENC Programming Guide 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- **FFmpeg hard-codes the matrix for RGB input.**
  - For any packed RGB input (`0RGB32`, `RGB32`, `0BGR32`, `BGR32`, `X2RGB10`, `X2BGR10`), FFmpeg's `nvenc.c` sets `colourMatrix = AVCOL_SPC_BT470BG` (BT.601) and `videoFullRangeFlag = 0` for H.264 and HEVC, and `matrixCoefficients = BT470BG` with `colorRange = 0` for AV1. Primaries and transfer pass through from the user [source] — [FFmpeg libavcodec/nvenc.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/nvenc.c) (lines ~1169–1173, ~1348–1352, ~1536–1540).
  - The 2022 commit that introduced this ("avcodec/nvenc: hardcode color characteristics for internal RGB2YUV conversion") is reported to say that NVENC does not use the VUI values as inputs to its built-in conversion, and that testing showed the input is expected to be sRGB and the output ends up limited-range BT.470. I could not open the commit text (bot wall), so that wording comes from a search-engine summary [single-dev, (unverified wording)] — [ffmpeg-cvslog 2022-08](https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2022-August/133327.html)
- **A developer report agrees.** On NVIDIA's forum, a developer said the built-in RGB formats "convert to YUV420 internally… and give me no control over the chromaticity coordinates (BT.601, BT.709)", and moved the conversion into their own compute shaders [single-dev] — [NVIDIA forum: DXGI/NvEnc YUV444 10bit](https://forums.developer.nvidia.com/t/dxgi-nvenc-yuv444-10bit-format-compatability/189782)
- **FFmpeg's `rgb_mode` option.** For HEVC, `rgb_mode` = `yuv420` (default), `yuv444` or `disabled` decides whether packed RGB becomes 4:2:0 or 4:4:4 [source] — [FFmpeg nvenc_hevc.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/nvenc_hevc.c)
- **Sunshine never feeds RGB to NVENC.**
  - Its standalone NVENC backend uses only `NV12`, `YUV420_10BIT`, `AYUV`, `YUV444` and `YUV444_10BIT`. It checks `NV_ENC_CAPS_SUPPORT_10BIT_ENCODE` and `NV_ENC_CAPS_SUPPORT_YUV444_ENCODE`, and writes VUI primaries, transfer, matrix and range from its own colourspace struct [source] — [Sunshine src/nvenc/nvenc_base.cpp](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/nvenc/nvenc_base.cpp)
  - On Linux it converts RGB→YUV in its own CUDA kernel, using per-colourspace `color_vec_y/u/v` and `range_y/uv` matrices [source] — [Sunshine src/platform/linux/cuda.cu](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/linux/cuda.cu)
- **HDR10 metadata in the NVENC API (SDK 13.0+).**
  - Structs: `MASTERING_DISPLAY_INFO {CHROMA_POINTS g, b, r, whitePoint; uint32 maxLuma, minLuma}` (cites AV1 §6.7.4 / HEVC D.2.28) and `CONTENT_LIGHT_LEVEL {uint16 maxContentLightLevel, maxPicAverageLightLevel}` (AV1 §6.7.3 / HEVC D.2.35).
  - Config flags: `NV_ENC_CONFIG_HEVC.outputMaxCll` / `outputMasteringDisplay` ("write … SEI message for HEVC") and `NV_ENC_CONFIG_AV1.outputMaxCll` / `outputMasteringDisplay` ("metadata for Av1").
  - Per-picture pointers: `NV_ENC_PIC_PARAMS_HEVC.pMaxCll` / `pMasteringDisplay` and `NV_ENC_PIC_PARAMS_AV1.pMaxCll` / `pMasteringDisplay` [vendor] — [nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h)
  - The `outputMasteringDisplay` field is absent from the n12.1.14.0 and n12.2.72.0 headers and present in n13.0.19.0 [source/arith] — [n12.2.72.0 header](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/n12.2.72.0/include/ffnvcodec/nvEncodeAPI.h), [n13.0.19.0 header](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/n13.0.19.0/include/ffnvcodec/nvEncodeAPI.h)
  - FFmpeg gates this as `NVENC_HAVE_HEVC_AND_AV1_MASTERING_METADATA` under `NVENCAPI_CHECK_VERSION(13, 0)`, alongside `NVENC_HAVE_H264_10BIT_SUPPORT`, `NVENC_HAVE_422_SUPPORT` and `NVENC_HAVE_MVHEVC` [source] — [FFmpeg libavcodec/nvenc.h](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/nvenc.h)
  - H.264 gets no NVENC metadata flag, so H.264 HDR SEI must be injected by hand.
- **10-bit and bit-depth conversion.**
  - `NV_ENC_CONFIG_HEVC` has `inputBitDepth` / `outputBitDepth`, with the comment "SW will do the bitdepth conversion internally from inputBitDepth -> outputBitDepth".
  - `NV_ENC_CONFIG_AV1` says "HW will do the bitdepth conversion internally… Support for 8 bit input to 10 bit encode conversion only" [vendor] — [nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h)
  - The guide §8.6 says: "prior to Blackwell, NVENC performs an internal CUDA 8 to 10-bit conversion" (HEVC), and H.264 High10 encode exists only on Blackwell [vendor] — [NVENC Programming Guide 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- **4:4:4 support.**
  - `NV_ENC_CONFIG_AV1.chromaFormatIDC`: "Should be set to 1 for yuv420 input (yuv444 input currently not supported)". The comment is present in the 12.1, 12.2, 13.0 and 13.1 headers, so **no AV1 High (4:4:4) on NVENC** [vendor/source].
  - `NV_ENC_HEVC_PROFILE_FREXT_GUID` is "For HEVC Main 422/444 8 bit and HEVC Main 422/444 10 bit profiles only" [vendor] — [nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h)
- **Capability matrix** (application note, SDK 13.0) [vendor] — [NVENC Application Note 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-application-note/index.html):
  - HEVC Main10 from Pascal; HEVC 4:4:4 from Pascal.
  - AV1 Main 4:2:0 8/10-bit on Ada and Blackwell.
  - 4:2:2 and H.264 10-bit on Blackwell only.
- **Throughput.** The application note's only performance table is 1080p **8-bit** 4:2:0, per NVENC engine. Ada figures: HEVC P1 CBR low-latency 1055 fps, AV1 P1 CBR LL 1090 fps, H.264 P1 CBR LL 910 fps. **No 10-bit numbers are published** [vendor] — [NVENC Application Note 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-application-note/index.html)
- **Independent measurements are 8-bit only.** A Dec 2025 latency study (RTX 5070 Ti, Arc A770, Radeon 780M) converted its HDR sources to 8-bit SDR and measured no 10-bit encodes; it lists HDR as future work [independent] — [arXiv 2511.18688](https://arxiv.org/html/2511.18688v2)
- **Qualitative HDR cost claim.** The Sunshine/Apollo docs say some GPUs give lower quality or encode performance in HDR, with no numbers [single-dev/vendor doc] — [Sunshine docs](https://docs.lizardbyte.dev/projects/sunshine)

### Inferences
- **Cha's current SDR NVENC path is probably mis-tagged** (not verified on Cha hardware). `crates/cha-nvenc/src/nvenc.rs` feeds `NV_ENC_BUFFER_FORMAT_ARGB` and tags `colourMatrix = BT709` (lines ~736–759).
  - If FFmpeg's finding holds, NVENC applies BT.601 coefficients and the decoder then inverts with BT.709.
  - Greys are unaffected, because the matrices agree on neutral axes. That fits the reported midtone issue being a transfer problem.
  - Saturated reds and greens would shift in hue and saturation.
  - Test: encode solid sRGB red/green/blue/yellow patches and compare the decoded RGB under both matrix tags. Or follow FFmpeg and tag `matrix = 5/6` (BT.470BG/SMPTE170M) for RGB input.
- **For HDR10 on NVENC, convert yourself.**
  - Run a CUDA (or Vulkan→CUDA interop) kernel: ARGB2101010 PQ BT.2020, or FP16 scRGB → BT.2020 NCL Y′CbCr (limited range, or full range) → `YUV420_10BIT` (P010) or `YUV444_10BIT`.
  - Then tag VUI 9/16/9. Do not rely on ARGB10's internal path, which (per FFmpeg) is tagged BT.601 and has no evidence of a BT.2020 matrix.
  - The extra kernel replaces NVENC's own CUDA RGB→YUV kernel, so the net SM cost should be similar (this is my own reasoning; no measurement exists).
- **Prefer NVENC's own metadata path.** On a driver and SDK that is 13.0 or newer, set `outputMasteringDisplay` / `outputMaxCll` and the per-picture pointers for HEVC and AV1. Keep Cha's `cha-gamestream/src/hdr.rs` injector for H.264 and older SDKs.
  - NVENC presumably repeats the metadata where it repeats SPS/sequence headers (unverified).
  - With infinite GOP and intra refresh there are few IDRs. Make sure the metadata is present on the first IDR and on every RFI-forced IDR.
- **10-bit latency cost on Ada is unmeasured publicly; Cha must benchmark it.**
  - Two NVIDIA statements are relevant: the HEVC 8→10 conversion is CUDA ("SW") before Blackwell, and the AV1 8→10 conversion is "HW".
  - So 10-bit AV1 from 8-bit input should add near-zero host work, while HEVC Main10 from 8-bit input adds a CUDA pass.
  - Feeding P010 directly avoids both.
- **Desktop 4:4:4 HDR on NVENC means HEVC Rext.** It needs the FREXT GUID, `chromaFormatIDC=3` and `YUV444_10BIT`, because AV1 4:4:4 is unavailable.

### Gaps
- No NVIDIA or independent measurement of HEVC Main10 or AV1 10-bit P1/ULL encode latency versus 8-bit on Ada.
- No NVIDIA statement of the RGB→YUV matrix NVENC uses for ARGB, ARGB10 or ABGR10. The BT.601 conclusion rests on FFmpeg's empirical fix.
- The minimum driver branch for SDK 13.0 metadata on Linux was not confirmed.
- Whether NVENC writes the MDCV/CLLI on every IDR or only on the first was not confirmed.

## 2. VA-API: HEVC Main10 and AV1 10-bit on Intel iHD and AMD radeonsi, VPP to P010 with BT.2020/PQ, HDR tone mapping, metadata, driver bugs

### Takeaway
- **Intel UHD 630 (Coffee Lake, the "KBLx" family) cannot do HDR encode.** It has no HEVC 10-bit encode, no AV1 encode, and its VPP cannot output P010.
- **Intel HEVC 10-bit encode** starts at ICL (Gen11), and VDEnc accepts RGB input from TGL. **AV1 10-bit encode** starts at DG2/Arc and covers MTL (Xe-LPG), LNL/BMG (Xe2), PTL (Xe3) and later.
- **Intel's VPP does not accept A2R10G10B10 input on any platform.** Its composition path cannot do BT.2020 conversions, so 10-bit RGB→P010 on Intel needs VEBOX/SFC, VDEnc direct RGB input, or Cha's own shader.
- **AMD:** HEVC Main10 encode on VCN ≥ 2.0 and AV1 Main 10-bit encode on VCN ≥ 4.0, which includes the 780M.
- **AMD's VCN "EFC" does 10-bit RGB → P010 with a BT.2020-NCL matrix inside the encoder**, under strict conditions: no scaling, matching in/out primaries and transfer, and no DCC.
- **Cha's current VPP colour settings fail the EFC transfer check on Mesa.** EFC is therefore probably not used today; that reading comes from my source analysis.
- **Metadata:** FFmpeg's VA-API HEVC encoder writes MDCV/CLLI SEI and its AV1 encoder writes metadata OBUs. Neither driver takes HDR metadata as an encode parameter, so Cha's own SEI/OBU injection is the right path.

### Cited Findings
- **Intel media-driver feature tables** [vendor] — [intel/media-driver docs/media_features.md](https://raw.githubusercontent.com/intel/media-driver/master/docs/media_features.md):
  - VDEnc/HuC (low-power) encode: "HEVC 10bit Input: P010/Y210/Y410/RGB" on TGLx, DG1, DG2/ATSM, MTLx, LNL, BMG, PTL, NVL and CRI. ICL lists "P010/Y410". KBLx, BXTx and SKL are blank.
  - PAK+VME (shader) encode lists HEVC 10-bit for ICL, TGLx and DG1 only. KBLx is blank, so Kaby/Coffee Lake (UHD 630) has **no HEVC Main10 encode**.
  - "AV1 10bit Input: P010" on DG2/ATSM, MTLx, LNL, BMG, PTL, NVL and CRI. AV1 encode is absent on TGL and earlier.
  - VPP CSC/scaling: SKL/BXT/KBL can **input** P010 but **not output** it. ICL and later output P010. "A2R10G10B10/A2B10G10R10" is **Input N on every platform**, and Output Y from ICL.
  - Note: "Composition path does not support conversion from BT2020 RGB to BT2020 YUV, BT2020->BT601/BT709, BT601/BT709 -> BT2020."
  - "HDR10 TM" is a VEBOX feature, "not compatible with denoise, deinterlace, STD/E, TCC". The usual order is VEBOX first, then SFC or EU.
- **libva API** [vendor] — [intel/libva va/va_vpp.h](https://raw.githubusercontent.com/intel/libva/master/va/va_vpp.h):
  - `VAProcColorStandardBT2020` is documented as primaries 9, transfer 14 and matrix 9 (not PQ).
  - `VAProcColorStandardExplicit` uses `VAProcColorProperties {chroma_sample_location, color_range, colour_primaries, transfer_characteristics, matrix_coefficients}`. The header's example is HDR10 = 9 / 16 / 9.
  - "RGB data always use the identity matrix (matrix_coefficients = 0)".
  - `VAProcFilterHighDynamicRangeToneMapping` uses `VAProcHighDynamicRangeMetadataHDR10` and the `VA_TONE_MAPPING_HDR_TO_HDR | HDR_TO_SDR | HDR_TO_EDR | SDR_TO_HDR` caps. `VAProcPipelineParameterBuffer.output_hdr_metadata` carries the output metadata.
- **Mesa VA frontend** [source] — [Mesa src/gallium/frontends/va/postproc.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/gallium/frontends/va/postproc.c):
  - It advertises colour standards BT601, BT709, BT2020 and Explicit.
  - It maps **`VAProcColorStandardBT2020` → primaries BT.2020, transfer SMPTE2084 (PQ), matrix BT2020_NCL**. That differs from libva's documented transfer of 14.
  - Any other standard, including `VAProcColorStandardSRGB`, becomes "unspecified", which then defaults to primaries BT.709 and **transfer GAMMA22**, and matrix BT.709.
  - Explicit uses the `input_color_properties` / `output_color_properties` codes as given.
  - It exposes `VAProcFilterHighDynamicRangeToneMapping` only when the screen reports `hdr_tone_mapping_modes`.
- **radeonsi tone mapping requires VPE.** Tone-mapping caps (`HDR_TO_HDR | HDR_TO_SDR`) are set only when the AMD Video Processing Engine (VPE) is present (`proc->supported`). VPE 2.0+ accepts r10g10b10a2/b10g10r10a2 input and outputs nv12/p010/p012 [source] — [Mesa radeonsi/mm/si_mm_screen.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/gallium/drivers/radeonsi/mm/si_mm_screen.c), [Mesa src/amd/common/ac_video.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/amd/common/ac_video.c)
- **AMD encode caps** [source] — [Mesa src/amd/common/ac_video.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/amd/common/ac_video.c):
  - HEVC encode `main10 = vcn_ip_version >= VCN_2_0_0`, with p010 input when main10. No HEVC 4:4:4 encode cap is set.
  - AV1 encode is `supported = vcn_ip_version >= VCN_4_0_0`, with `formats.p010 = 1`. The encode profile list contains only `AV1_MAIN`, i.e. 4:2:0 8/10-bit.
  - Quirk: AV1 `min_qp = 8` on VCN 4.0.2/4.0.5/4.0.6. I believe the 780M (Phoenix) is VCN 4.0.2 but did not confirm it from a source (unverified).
  - EFC: `efc = VCN >= 2.0 && != VCN 2.2`, and `efc_range_bug = VCN >= 5.0 && firmware < 9.13.38`.
- **radeonsi EFC conditions** [source] — [Mesa radeonsi/mm/radeon_vcn_enc.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/gallium/drivers/radeonsi/mm/radeon_vcn_enc.c), `radeon_vcn_enc_efc_supported`. EFC is used only if all of these hold:
  - no rotation or blend;
  - no crop, offset or scale (src region == dst region);
  - `in_color_primaries == out_color_primaries`;
  - `in_transfer == out_transfer`;
  - output matrix is BT.709 or BT.2020-NCL;
  - no chroma siting of bottom or horizontal-centre;
  - **no DCC** (`tex->surface.meta_offset == 0`);
  - input is 8-bit RGBA/BGRA (→ NV12 only), or 10-bit `B10G10R10A2/X2` or `R10G10B10A2/X2` (→ NV12 or **P010**).
  - The matrix maps to `RENCODE_COLOR_VOLUME_G22_BT709` or `RENCODE_COLOR_VOLUME_G2084_BT2020`.
  - On the VCN5 range bug, BT.709 must be limited range and BT.2020 must be full range.
- **Known bugs and pitfalls:**
  - On an RX 9070 XT (RDNA4) with radeonsi VA-API, all VA-API encoders failed first init ("No usable encoding entrypoint"), and after the fallback **HDR colours were wrong** while SDR was correct. The stream was still signalled BT.2020/PQ [single-dev] — [Vibepollo #707](https://github.com/Nonary/Vibepollo/issues/707)
  - A Mesa maintainer stated that "only P010 format is supported for 10bit encoding"; the fix was to request P010, not YUV420P10 [single-dev] — [Sunshine #1811](https://github.com/LizardByte/Sunshine/issues/1811)
  - Fedora's Mesa ships no VA-API encode entry points; RPM Fusion's `mesa-va-drivers-freeworld` restores them [single-dev] — [lowlat #10](https://github.com/nomi-san/lowlat/issues/10)
- **FFmpeg VA-API metadata:**
  - `vaapi_encode_h265.c` has `SEI_MASTERING_DISPLAY = 0x08` and `SEI_CONTENT_LIGHT_LEVEL = 0x10`, and inserts `SEI_TYPE_MASTERING_DISPLAY_COLOUR_VOLUME` and `SEI_TYPE_CONTENT_LIGHT_LEVEL_INFO` through CBS as packed headers [source] — [FFmpeg vaapi_encode_h265.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/vaapi_encode_h265.c)
  - `vaapi_encode_av1.c` sets `color_config.high_bitdepth` from the pixel depth and emits `AV1_OBU_METADATA` of type `HDR_MDCV` (plus CLL) from frame side data [source] — [FFmpeg vaapi_encode_av1.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/vaapi_encode_av1.c)
- **Cha's current VPP setup.** `crates/cha-streamer/src/encoder/vaapi/ffi.rs` sets `surface_color_standard = VAProcColorStandardSRGB`, `output_color_standard = BT709`, input range full and output range reduced (lines ~332–338). Cha's AV1 sequence header writes `high_bitdepth = 0` and primaries, transfer and matrix all = 1 (`crates/cha-streamer/src/encoder/bitstream/av1.rs` ~165–171) [source, local].

### Inferences
- **Cha's VPP on the 780M probably does not use EFC today** (my reading of Mesa source).
  - With surface `SRGB` and output `BT709`, Mesa resolves the input transfer to GAMMA22 and the output to BT709. EFC requires them to be equal, so the conversion falls back to the shader compositor (or VPE).
  - GBM buffers with a DCC modifier also disqualify EFC.
  - For both the SDR and HDR paths on radeonsi, use `VAProcColorStandardExplicit` with identical primaries and transfer in and out. HDR: in = 9/16/0 (RGB), out = 9/16/9. SDR: in = 1/13/0, out = 1/13/1.
  - Also use a non-DCC modifier for the buffer handed to VA, and keep the same size (no VPP scale).
  - Whether Mesa's shader path then applies a GAMMA22→BT709 transfer conversion that changes midtones is unconfirmed (see Gaps).
- **HDR10 on Intel needs a platform split.**
  - UHD 630: HDR is impossible (no Main10 or AV1 encode, no P010 VPP output). Stream SDR.
  - ICL (Gen11): HEVC Main10 via P010 only. VPP cannot take A2R10G10B10, and the composition path refuses BT.2020, so convert in Cha's own Vulkan/GL compute shader to P010.
  - TGL and later: try VDEnc direct "RGB" input for HEVC 10-bit (the format is likely A2R10G10B10; unverified) or the VEBOX/SFC path with Explicit 9/16/9 colour properties. Otherwise use Cha's own shader.
  - Arc, MTL, LNL, BMG, PTL: AV1 10-bit through P010 as well.
- **Use VA-API HDR tone mapping for HDR→SDR on the node only where it exists**: Intel VEBOX (P010/HDR10 input) and AMD VPE. The 780M (Phoenix) probably lacks VPE (unverified). A Cha shader is more portable and is a single pass anyway.
- **Metadata on VA-API must be in-band and hand-written.** Neither driver path takes MDCV/CLL as an encode parameter. Keep generating SEI and OBUs in `bitstream/h265.rs` and `bitstream/av1.rs` (or reuse `cha-gamestream/src/hdr.rs`).
- **Cha's own HEVC headers need Main10 changes** (from spec knowledge, not checked against Cha's writer):
  - SPS `general_profile_idc = 2` with compatibility flag 2;
  - `bit_depth_luma_minus8 = bit_depth_chroma_minus8 = 2`;
  - VUI `colour_description_present_flag = 1` with 9/16/9 and the range flag.
- **Cha's AV1 headers need 10-bit changes:**
  - `seq_profile = 0`, `high_bitdepth = 1`;
  - `color_description_present_flag = 1` with 9/16/9 and `color_range`;
  - `chroma_sample_position` set.

### Gaps
- Whether Mesa's non-EFC VPP path (vl_compositor/compute) implements BT.2020/PQ transfer and primaries conversion (rather than matrix only), and whether it does GAMMA22→BT709 re-encoding.
- Which AMD APUs or dGPUs have VPE (Phoenix/780M vs Strix and later).
- Intel's exact FourCC for "RGB" 10-bit VDEnc input, and whether iHD honours Explicit 9/16/9 on the VEBOX path for RGB→P010.
- No measured encode-latency deltas for 10-bit vs 8-bit on iHD or radeonsi.
- Xe3 (PTL) is in the tables, but I found no Xe3-specific HDR encode bug reports.

## 3. SVT-AV1 10-bit low-delay cost; does x264 10-bit matter?

### Takeaway
There is no published low-delay SVT-AV1 8-bit vs 10-bit benchmark. The one hard data point is a regression issue where 10-bit throughput fell to roughly a third of 8-bit at fast presets on Windows. A claim of "negligible" cost is unbenchmarked. Cha must benchmark on its CPU nodes. x264 10-bit (High 10) has no browser hardware-decode path, and NVENC encodes H.264 10-bit only on Blackwell, so it is not worth supporting for streaming.

### Cited Findings
- **SVT-AV1 10-bit regression report** [single-dev measurement] — [SVT-AV1 GitLab #1979](https://gitlab.com/AOMediaCodec/SVT-AV1/-/issues/1979):
  - 8-bit throughput stayed flat across commits while 10-bit throughput fell by up to about two-thirds at some presets on Windows (e.g. preset 10: ~100 fps → ~34 fps).
  - The test used an 8-bit source encoded to 10-bit, in a non-realtime random-access configuration.
- **"Negligible speed impact" claim.** One article says encoding 8-bit sources as 10-bit improves quality at the same bitrate with negligible speed impact. It offers no benchmark (unverified) — [libsvtav1.web.app](https://libsvtav1.web.app/article/does-svt-av1-support-10-bit-encoding)
- **Low-delay release notes have no bit-depth split.** SVT-AV1 2.2.0 improved low-delay trade-offs across presets, and later "fast-decode 2" claims 25–50% fewer cycles, without separating bit depths [vendor] — [VideoHelp SVT-AV1 version history](https://videohelp.com/software/SVT-AV1/version-history)
- **H.264 High10 encode on NVENC is Blackwell-only** [vendor] — [NVENC Programming Guide 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- **WebCodecs H.264 registration is optional.** The W3C AVC registration does not require WebCodecs implementers to support H.264 at all; I found no statement that Chrome or Safari decode High 10 in hardware — [W3C AVC WebCodecs registration](https://www.w3.org/TR/2021/WD-webcodecs-avc-codec-registration-20210813)

### Inferences
- **Treat SVT-AV1 10-bit as a separately benchmarked mode.**
  - On CPU nodes the AV1 hot paths switch to 16-bit sample buffers, so expect a meaningful (possibly 1.3–3×) slowdown at the fast realtime presets Cha would use. The range comes from #1979's random-access numbers; there are no low-delay figures.
  - Enable it only where the measured headroom covers the frame time at the target fps.
- **For HDR from a CPU node, SVT-AV1 10-bit 4:2:0 (profile 0) is the only sensible software path.** x264 High 10 produces streams that browsers decode, if at all, in software only (unverified). x265 Main10 is too slow for low-delay at high resolutions (unverified; not researched).

### Gaps
- No low-delay (`--pred-struct 1` / RTC) SVT-AV1 8-bit vs 10-bit fps or latency figures at presets 10–13.
- No source on Chrome or Safari decoding H.264 High 10 (`avc1.6e…`) in WebCodecs, hardware or software.

## 4. PyroWave HDR: what Themaister's pyrowave supports, how Vibepollo, Polaris and Steam use it, extra bandwidth at 10 bits

### Takeaway
pyrowave's GPU encoder has three input colour spaces and two outputs, and the decoder is floating-point.
- **Inputs:** sRGB (non-linear), extended-sRGB *linear* (scRGB, 80-nit normalised) and HDR10 ST2084.
- **Outputs:** sRGB or HDR10 PQ. A PQ output uses the BT.2020-NCL matrix and is *not* tone-mapped.
- **Intermediate:** R16 is "more or less required" for HDR10, while R8 gets dithered.
- **Cost:** Vibepollo's guidance puts 10-bit at about ×1.15 the bitrate, i.e. ~1.84 bits/pixel for clean 4:2:0 HDR.

So the Cha node can hand PyroWave the compositor's PQ (or FP16 scRGB) buffer, with no separate CSC pass.

### Cited Findings
- **`pyrowave_scaled_encode_info`** [source] — [Themaister/pyrowave pyrowave.h](https://raw.githubusercontent.com/Themaister/pyrowave/master/pyrowave.h):
  - `input_color_space`: "For SDR, use VK_COLOR_SPACE_SRGB_NONLINEAR. SPACE_EXTENDED_SRGB_LINEAR_EXT (80 nits normalized) and HDR10_ST2084 is supported as well."
  - `output_color_space`: "VK_COLOR_SPACE_SRGB_NONLINEAR or HDR10_ST2084. HDR10 is *not* tonemapped."
  - "If output color space is HDR10_ST2084, BT.2020 NCL transform is used, otherwise, BT.701 [sic] coefficients are used."
  - The YCbCr output is full-range by default, with centre chroma siting.
  - `intermediate_plane_format` is R8_UNORM or R16_UNORM: "R16_UNORM is more or less required for HDR10… R8_UNORM intermediate format will receive dithering."
  - `ycbcr_range` = NARROW gives H.273 narrow range, with `ycbcr_range_bit_depth` from 8 to 16.
  - `ycbcr_chroma_midpoint` can be 0.5 (the default) or 512/1023 (10-bit BT).
  - The input view must be an RGB(A) UNORM format, or, as a special case, NV12 or **P010** through the COLOR aspect ("intended to be used with pipewire dmabuf screen capture"). NV12 and P010 input is scaled without colour conversion.
  - The CPU path has `PYROWAVE_CPU_BUFFER_FORMAT_YUV420P10` / `YUV444P10`, and `pyrowave_encoder_set_ycbcr_scaling_factor` handles 10-bit LSB data in UNORM16 (factor 0xffff/0x3ff).
- **Cha's parser already carries the flags it needs.** The local PyroWave sequence header has one bit each for primaries (709/2020), transfer (709/PQ), YCbCr transform (709/2020), range and chroma siting (`crates/cha-pyrowave-wgpu/src/parser.rs`) [source, local]. There is no bit-depth field: the decoder output precision is a presentation choice (per `docs/research/02-pyrowave-and-codecs.md` in this repo).
- **Vibepollo** [single-dev project docs, via the repo's earlier research] — [Vibepollo docs/pyrowave-protocol.md](https://github.com/Nonary/Vibepollo/blob/master/docs/pyrowave-protocol.md), summarised in `docs/research/03-moonlight-sunshine-ecosystem.md`:
  - Capability bits: `SCM_PYROWAVE_HDR10 = 0x02000000` (10-bit 4:2:0) and `SCM_PYROWAVE_HDR10_444 = 0x04000000`.
  - Guidance: "~1.6 bits/pixel for clean 4:2:0 SDR… 4:4:4 costs ×1.6 and 10-bit ×1.15". Wired LAN only.
  - Output planes are R8 for 8-bit and R16 for 10-bit.
  - An open HDR PR adds client peak luminance — [Vibepollo PR #559](https://github.com/Nonary/Vibepollo/pull/559)
- **Steam Remote Play** has a beta "Pyrowave Video" toggle (100–500 Mbit/s, YUV 4:4:4 and HDR (auto)) since 2026-09-21 [vendor/press] — [Phoronix](https://www.phoronix.com/news/Valve-Steam-Beta-Pyrowave), [GamingOnLinux](https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/)
- **[arith] bitrates from Vibepollo's factors** (1.6 bpp × 1.15 = **1.84 bpp** for 10-bit 4:2:0; × 1.6 × 1.15 = **2.94 bpp** for 10-bit 4:4:4):

  | Resolution and rate | Pixel rate | 8-bit 4:2:0 (1.6 bpp) | 10-bit 4:2:0 (1.84 bpp) | 10-bit 4:4:4 (2.94 bpp) |
  |---|---|---|---|---|
  | 1080p60 | 124.4 Mpx/s | ≈199 Mbit/s | ≈229 Mbit/s | — |
  | 1440p120 | 442.4 Mpx/s | — | ≈814 Mbit/s | — |
  | 2160p60 | 497.7 Mpx/s | ≈796 Mbit/s | ≈916 Mbit/s | ≈1.47 Gbit/s |

### Inferences
- **PyroWave is the cheapest HDR path for Cha.**
  - It has no reference chain, and 10-bit costs about 15% more bits.
  - The encoder does the PQ/BT.2020 CSC and the R16 planes itself, from an A2R10G10B10 PQ or FP16 scRGB compositor buffer.
  - The "RGB(A) UNORM" wording leaves FP16 scRGB as an *SFLOAT* view unclear (see Gaps).
  - Cha's `cha-pyrowave` crate currently imports "one plane of 8-bit RGB(A)" (`crates/cha-pyrowave/src/lib.rs` ~229), so it needs a 10-bit/FP16 import path.
- **For a Vibepollo-compatible host, Cha must request `SCM_PYROWAVE_HDR10*` and handle the HDR metadata control message.** PyroWave's own header has no MDCV/CLL fields, so static metadata must travel out of band (inference from the header's flags).
- **Bandwidth gates HDR on PyroWave.** 4K60 10-bit 4:2:0 at ~0.9 Gbit/s needs more than 1 GbE. In practice 1080p/1440p HDR fits 1–2.5 GbE, and 4K HDR needs 2.5–10 GbE.

### Gaps
- Whether `input_color_space = EXTENDED_SRGB_LINEAR` accepts an `R16G16B16A16_SFLOAT` view, given that the comment says "RGB(A) UNORM".
- How Steam's Pyrowave HDR (auto) or Polaris signal metadata or tone mapping. I found no Valve technical documentation, and nothing on Polaris.
- No objective measurement of PyroWave's 10-bit vs 8-bit rate-distortion. The ×1.15 is a guideline.

## 5. Transfer choice (PQ vs HLG vs scRGB), what Sunshine/Moonlight, GeForce NOW, Xbox Cloud and Steam send, static vs dynamic metadata, absent or zero MaxCLL

### Takeaway
- **Every game-streaming stack with documented HDR sends HDR10:** BT.2020 primaries, PQ transfer, BT.2020-NCL matrix, 10-bit, plus static metadata. None documents HLG or linear scRGB on the wire.
- **Sunshine/Moonlight carry static metadata twice:** out of band in a control message (`SS_HDR_METADATA`) and in-band as SEI when FFmpeg-based encoders are used. The metadata comes from the *host display*, not from the content.
- **Dynamic metadata** (HDR10+, Dolby Vision) is not used by any streaming service I could document. Chrome ignores HDR10+ SEI and plays HDR10.
- **Missing static metadata** makes some decoders and displays fall back to their own defaults; Chrome 108 could clamp MaxCLL low.

### Cited Findings
- **Sunshine colourspace selection** [source] — [Sunshine src/video_colorspace.cpp](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video_colorspace.cpp):
  - HDR (`bt2020`: "Rec. 2020 + SMPTE 2084 PQ", primaries BT2020, transfer SMPTE2084, matrix BT2020_NCL, 10-bit asserted) is selected only when `config.dynamicRange > 0 && hdr_display`, i.e. the client asks *and* the host display is in HDR mode.
  - There is also `bt2020sdr`: BT.2020 primaries with the BT2020_10 transfer, 10-bit SDR.
  - Range comes from `encoderCscMode & 0x1`.
  - No HLG.
- **Moonlight's HDR control message.**
  - moonlight-common-c defines `SS_HDR_METADATA`: display primaries (RGB order, normalised to 50,000), white point, `maxDisplayLuminance` (nits), `minDisplayLuminance` (1/10000 nit), `maxContentLightLevel`, `maxFrameAverageLightLevel` and `maxFullFrameLuminance`.
  - It is retrieved by `LiGetHdrMetadata` ("a Sunshine protocol extension"), and the HDR mode is pushed through `ConnListenerSetHdrMode` [source] — [moonlight-common-c src/Limelight.h](https://raw.githubusercontent.com/moonlight-stream/moonlight-common-c/master/src/Limelight.h)
  - Sunshine sends it as control message `IDX_HDR_MODE = 11` with `SS_HDR_METADATA` [source] — [Sunshine src/stream.cpp](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/stream.cpp)
  - It also attaches `disp->get_hdr_metadata()` as FFmpeg mastering-display side data on frames, so FFmpeg encoders that support it emit SEI or OBUs [source] — [Sunshine src/video.cpp](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video.cpp) (~2290–2304)
- **GeForce NOW.**
  - HDR streaming needs an Ultimate membership (some regional pages say Premium) and an HDR10-capable display [vendor] — [NVIDIA GFN system requirements](https://www.nvidia.com/en-gb/geforce-now/system-reqs/)
  - "Cinematic Quality Streaming" ships YUV 4:4:4, 10-bit HDR and AV1 at around 100 Mbit/s on Blackwell servers [vendor] — [NVIDIA newsroom](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
  - A review reports CQS capped at 60 fps by default, at 40–58 Mbit/s [independent press] — [The Shortcut review](https://theshortcut.com/p/nvidia-geforce-now-review-blackwell-rtx-5080-game-streaming)
- **Xbox Cloud Gaming.** I found no Microsoft documentation. A blog claims HDR passes through only on Xbox console clients and is stripped on browser, TV and tablet clients (unverified, unsourced) — [cloudgamingexpert blog](https://cloudgamingexpert.vercel.app/blog/hdr-is-mostly-broken-on-cloud)
- **Steam Remote Play** (no Valve technical spec found):
  - It gained HDR streaming to the Steam Deck OLED in the client — [Linuxiac](https://linuxiac.com/steam-client-update-adds-hdr-remote-play-streaming-on-steam-deck-oled/)
  - Betas in October 2026 toggled HDR on the Deck OLED and fall back to H.264 when HEVC is unavailable, which removes HDR — [GamingOnLinux 2026-10](https://www.gamingonlinux.com/2026/10/steam-client-beta-october-9th-brings-more-remote-play-improvements/page=1/)
- **Dynamic metadata.**
  - HDR10+ Technologies says HDR10+ ADVANCED (a 2025 spec) is "supported in cloud gaming" on "compatible HDR10+ GAMING platforms", using "real-time ambient light data and dynamic tone mapping". It names no services [vendor] — [HDR10+ FAQ](https://hdr10plus.org/hdr10_faq/is-hdr10-advanced-supported-in-cloud-gaming/)
  - Chrome on Windows: "HDR10+ SEI dynamic metadata will be ignored while decoding and playback will downgrade to HDR10". Chrome tone-maps automatically from static metadata when present [single-dev, well-maintained] — [StaZhu enable-chromium-hevc-hardware-decoding](https://raw.githubusercontent.com/StaZhu/enable-chromium-hevc-hardware-decoding/main/README.md)
- **Absent metadata in Chrome.**
  - Chrome 108 could not extract metadata that was missing from the container, which "downgraded to PQ only mode, and the max content light level maybe cut to a low value".
  - Chrome 110 extracts static metadata from both the bitstream and the container [single-dev] — [StaZhu README](https://raw.githubusercontent.com/StaZhu/enable-chromium-hevc-hardware-decoding/main/README.md)
- **Absent or zero metadata on displays.** Displays fall back to manufacturer defaults when MaxCLL/MaxFALL are absent or zero, and some ignore the metadata [weak secondary] — [cinemaconfig MaxCLL](https://cinemaconfig.com/reference/maxcll)
  - Whether zero means "unknown" was still an open standards question in PNG WG minutes (Nov 2022) — [W3C PNG WG minutes](https://lists.w3.org/Archives/Public/public-png/2022Nov/att-0003/PNG_WG_Meeting_Minutes_-_Nov_14th__2022.pdf)
- **HLG vs PQ.** HLG is backward compatible with SDR BT.1886 displays and PQ is not. BT.2100 defines both, at 10- or 12-bit — [Wikipedia: Rec. 2100](https://en.wikipedia.org/wiki/Rec._2100)

### Inferences
- **Cha should send HDR10 (PQ, 9/16/9) with static MDCV + CLLI and no dynamic metadata.** That is what Moonlight-protocol peers, browsers and GFN-style clients handle.
- **HLG has no advantage for interactive streaming.** Clients are always negotiated, so backward compatibility is moot. Game and desktop compositors render in absolute (PQ) luminance, and HLG's scene-referred OOTF would need a peak-dependent conversion on the node.
- **scRGB is fine as the node-internal format** (PyroWave accepts it) but never on the wire.
- **What to put in the metadata.**
  - MDCV: the compositor's mastering or target display (Sunshine uses the host monitor's), or BT.2020/D65 with the game's configured peak.
  - CLLI: a real MaxCLL/MaxFALL is unknowable in real time. Either send the target peak (e.g. 1000 / 400) or omit CLLI. Do not send zeros, since behaviour on zero is inconsistent across sinks.
  - Re-send on every IDR.
  - For the Moonlight host (`cha-gamestream`, which currently reports `hdr=false`), fill `SS_HDR_METADATA` from the same values.

### Gaps
- No primary source on Xbox Cloud Gaming's HDR format or clients.
- No Valve documentation of Steam Remote Play HDR signalling (PQ vs HLG, metadata).
- CTA-861.3's exact normative text on MaxCLL/MaxFALL = 0 ("unknown") was not obtained (paywalled).
- No measurement of how specific TVs or monitors behave with absent CLLI in a streamed HEVC/AV1 HDR10 signal.

## 6. Bits and quality: 10-bit vs 8-bit on SDR content, banding, HDR10 bitrate vs SDR at low delay

### Takeaway
The best available evidence is old and from VOD encoders:
- 10-bit coding of 8-bit content buys about 5% luma BD-rate (HM-era HEVC) and reduces banding and contouring.
- Apple's HLS ladder budgets HDR10/HLG HEVC at about 1.2× the SDR bitrate at every rung (my ratio from Apple's table).
- Apple's own SDR HEVC examples use Main10.
- PQ at 10-bit is the HDR10 floor. BT.2100 allows only 10 or 12-bit, and PQ is said to be banding-free only at 12-bit.

There are no low-delay measurements for game or desktop content.

### Cited Findings
- **~5% from 10-bit internal coding.** A Tampere University VCIP paper reports that Main10 coding of 8-bit content gives "around 5% BD-rate savings in luma component over MP", and says more evaluation is needed. The wording comes from a search summary, and the paper itself cites prior work [independent, VOD/HM] — [Vanne et al., VCIP (trepo.tuni.fi)](https://trepo.tuni.fi/bitstream/10024/128707/1/vanne_vcip_v115.pdf)
- **NVENC's newer HEVC gains.** A 2026 longitudinal NVENC paper attributes part of the gains in newer HEVC encoders to native 10-bit internal processing, even with 8-bit input. It also finds UHQ tuning raises end-to-end latency by more than 400%, which rules UHQ out for real-time use [independent] — [arXiv 2605.01187](https://arxiv.org/abs/2605.01187)
- **Apple HLS HEVC ladder** (kbit/s, SDR vs HDR) [vendor] — [Apple HLS Authoring Specification](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices). HDR ÷ SDR is ≈1.19–1.22 at 540p and above; the lowest rung, 360p, is 1.10 (160/145) [arith].

  | Resolution | SDR | HDR | HDR ÷ SDR [arith] |
  |---|---|---|---|
  | 1080p | 4500 | 5400 | 1.20 |
  | 1080p | 5800 | 7000 | 1.21 |
  | 1440p | 8100 | 9700 | 1.20 |
  | 2160p | 11600 | 13900 | 1.20 |
  | 2160p | 16800 | 20000 | 1.19 |

- **Other Apple HLS rules:**
  - Apple's SDR example variant uses `CODECS="hvc1.2.4.L123.B0"` (Main10), i.e. 10-bit SDR.
  - "HDR HEVC video MUST be HDR10, HLG, or Dolby Vision".
  - "For HDR10 content, the Mastering Display Color Volume and Content Light Level Information SEI messages SHOULD be present".
  - "SDR streams MUST be provided" for backward compatibility [vendor] — [Apple HLS Authoring Specification](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices)
- **HDR bitrate claim from a blog.** HDR streams "require 30–40% more bitrate than SDR for the same perceived quality" (unverified, unsourced) — [cloudgamingexpert blog](https://cloudgamingexpert.vercel.app/blog/hdr-is-mostly-broken-on-cloud)
- **PQ and banding.**
  - PQ is derived from Barten CSF peak sensitivities. It is described as banding-free at 12 bits, where a power law spanning 10,000 nits would need about 15 bits.
  - HDR10 is defined at 10-bit.
  - BT.2100's 10-bit narrow range runs from black 64 to peak 940 [secondary] — [Wikipedia: Perceptual quantizer](https://en.wikipedia.org/wiki/Perceptual_quantizer), [Wikipedia: Rec. 2100](https://en.wikipedia.org/wiki/Rec._2100)
  - Broadcast production and distribution "is in general known to be based around signal flows with 10bit precision" [vendor/standards] — [ITU-R BT.2111-3](https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.2111-3-202505-I!!MSW-E.docx)
- **Vendor claims of larger savings.** Fora Soft blogs put 10-bit savings at 5–15% for HEVC and AV1, with no primary data (unverified) — [Fora Soft 8-bit vs 10-bit](https://www.forasoft.com/learn/video-encoding/articles/8bit-vs-10bit-encoding)

### Inferences
- **Default SDR streams to 10-bit wherever it is free.**
  - Free cases: AV1 on NVENC (hardware 8→10 conversion), VA-API from P010, and PyroWave R16.
  - The expected gain is a few percent BD-rate plus visibly less banding in dark gradients, sky boxes and desktop wallpapers, given the evidence above.
  - On HEVC NVENC pre-Blackwell, 10-bit from 8-bit input costs a CUDA pass. Measure it.
  - Tag 10-bit SDR as BT.709 / transfer 1 or 13 / matrix 1 with Main10; it is a legal and widely decoded stream.
- **Budget HDR10 at ×1.15–1.25 the SDR bitrate.** The range takes Apple's VOD ladder (~1.2×) and PyroWave's ×1.15 as anchors. The 30–40% claim is unsupported.
  - At low delay with one-frame VBV, there is no lookahead to absorb highlight detail, so start near ×1.25 and tune (my inference).
- **Never stream PQ at 8 bits.** BT.2100 only defines 10 and 12 bits, and 8-bit PQ steps are far coarser than the Barten threshold.

### Gaps
- No published low-delay (P1/ULL, CBR, one-frame VBV) BD-rate comparison of 8-bit vs 10-bit on game or desktop content for NVENC, QSV, VCN or SVT-AV1.
- No published bitrate-for-equal-quality figures for HDR10 *game* content versus its SDR render.
- No published desktop or text HDR measurements.

## 7. SDR clients and SDR content: where to tone-map, its cost, operators, SDR UI inside HDR (203 nits), sRGB piecewise vs gamma 2.2, should desktops be HDR

### Takeaway
- **SDR client:** have the node render or encode SDR and never ship HDR to it. Sunshine only encodes HDR when the client asks and the host display is HDR; Apple requires SDR variants.
- **HDR client:** the client display adapts the content. libplacebo's `spline` operator is the de facto default and BT.2390 the standards option. Chrome tone-maps HDR10 automatically from static metadata.
- **SDR UI inside an HDR stream:** composite at 203 cd/m² (BT.2408 HDR reference white, and the W3C CSS HDR anchor).
- **SDR decoding curve:** KDE decodes SDR "sRGB" with pure gamma 2.2, while Windows uses the piecewise curve. The difference shows as raised blacks.
- **Desktops** should stay SDR unless HDR content is actually on screen and the client display is HDR.

### Cited Findings
- **Sunshine's HDR gate.** Sunshine selects the BT.2020/PQ colourspace only when `config.dynamicRange > 0 && hdr_display`, i.e. client request plus host HDR display [source] — [Sunshine src/video_colorspace.cpp](https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video_colorspace.cpp)
- **libplacebo tone-mapping functions** [source docs] — [libplacebo options](https://libplacebo.org/options/):
  - `spline` (default) is "Simple spline consisting of two polynomials, joined by a single pivot point", tuned to scene average, forward and inverse.
  - Standards operators: `bt2390` ("EETF from the ITU-R Report BT.2390", Hermite-spline roll-off with a linear segment), `bt2446a` (BT.2446 method A, forward and inverse), `st2094-40` and `st2094-10`.
  - Simple operators: `reinhard`.
  - Legacy, low quality: `mobius`, `hable` ("shifts average brightness noticeably") and `gamma`.
  - Also `linear`, `linearlight` and `clip`.
  - `peak_detect` defaults to yes ("can greatly improve HDR tone mapping when dynamic metadata is absent", but may be slow); `gamut_mapping` defaults to `perceptual`; `contrast_recovery` defaults to 0.0 (0.3 in the high_quality preset); `inverse_tone_mapping` defaults to no.
- **HDR reference white.**
  - The W3C CSS Color HDR draft: HDR Reference White ("diffuse white") is 203 cd/m², per BT.2408-9 §2.1. When compositing SDR and HDR, "the HDR Reference White for SDR content should be anchored at 203 cd/m²" — [W3C CSS Color HDR Level 1](https://www.w3.org/TR/css-color-hdr/)
  - BT.2408 defines graphics white at the same level as HDR reference white; the current edition is BT.2408-9 (03/2026) — [ITU-R BT.2408-9](https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2408-9-2026-TOC-HTM-E.htm), [BT.2408-0 text](https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2408-2017-MSW-E.docx)
  - A FilmLight engineer, quoted in a blog, argues that simply scaling SDR to 203 nits does not preserve the SDR look [secondary] — [HDR Reference White blog](https://daejeonchronicles.com/2021/02/13/hdr-reference-white/)
- **sRGB piecewise vs gamma 2.2.**
  - KWin now uses gamma 2.2 rather than the piecewise sRGB function for sRGB applications, "as that more closely matches what displays actually do". Windows' HDR mode uses the piecewise curve [single-dev, compositor author] — [Xaver Hugl, KWin HDR update (2023-12-17)](https://planet.kde.org/xavers-blog-2023-12-17-an-update-on-hdr-and-color-management-in-kwin)
  - An mpv PR linearises sRGB as pure 2.2 "to avoid raised blacks", noting that the spec reading is disputed — [mpv PR 16915](https://github.com/mpv-player/mpv/pull/16915)
  - KDE bug 496308 reports raised blacks with HDR enabled (closed WORKSFORME) — [KDE bug 496308](https://bugs.kde.org/show_bug.cgi?id=496308)
- **Chrome's HDR handling** [single-dev] — [StaZhu README](https://raw.githubusercontent.com/StaZhu/enable-chromium-hevc-hardware-decoding/main/README.md):
  - On Windows and macOS, Chrome tone-maps PQ/HDR10/HLG automatically, based on static metadata when present.
  - Chrome 109 made the HDR→SDR path 16-bit with zero-copy.
  - From Chrome 123, PQ renders at absolute brightness when Windows HDR mode is on.
- **Cha's current SDR tag.** `crates/cha-nvenc/src/nvenc.rs` already tags SDR with transfer 13 (sRGB), because a BT.709 tag made Chrome on a Mac render midtones about 11 levels too light [source, local].

### Inferences
- **Where to tone-map.**
  - **SDR client:** the node produces SDR (the compositor renders SDR, or the node tone-maps HDR→SDR before encode in one shader pass). It is cheaper on bits (8- or 10-bit SDR at ~1/1.2 the HDR bitrate) and avoids depending on client tone-mapping quality, which is weak in browsers and absent on many TVs and phones.
  - **HDR client with lower peak than the node's target:** send HDR10 with honest MDCV/CLLI. Let the client tone-map (libplacebo `spline` or `bt2390` natively; the browser compositor for WebCodecs). Or tone-map on the node to the client's reported peak, which is what Vibepollo PR #559's "client peak luminance" points toward.
- **Cost of node-side tone mapping.** A per-pixel spline or BT.2390 curve in PQ or linear space is one compute pass. That is negligible next to encode, and it can fuse with the CSC kernel needed anyway for NVENC (§1) and PyroWave. libplacebo's peak detection is the expensive part; skip it because the compositor knows the content peak.
- **SDR UI inside HDR.**
  - Composite SDR windows and cursor at 203 cd/m² reference white in PQ: PQ code ≈ 58% of full scale, about 10-bit code 592 in full range. That is my arithmetic from the ST 2084 curve, (unverified) here.
  - Decode SDR sources with gamma 2.2 to match KDE/Wayland and avoid raised blacks; flag the mismatch with Windows hosts.
  - Make the SDR white level a per-session setting, since users expect "SDR brightness" control as in Windows and KDE.
- **Desktops should be SDR (sRGB/BT.709, ideally 10-bit 4:4:4) by default.** Switch to HDR10 only when an HDR surface is visible and the client display is HDR. That costs about ×1.2 bits and risks client tone-mapping and black-level differences, for no gain on SDR text.

### Gaps
- BT.2408-9's exact SDR→HDR mapping procedures (§ on SDR graphics, up/down mapping) were not read in full.
- BT.2446 methods B and C, and BT.2390's EETF formula, were not fetched as primary text.
- No measured GPU cost for tone mapping at 4K120 on the RTX 4090, 780M or UHD 630.
- Browser tone-mapping quality for WebCodecs to canvas or WebGPU was not researched here; it is client-side.

## 8. End-to-end signalling: VUI/colour_config codes, codec strings, browser 10-bit decode profiles

### Takeaway
- **HDR10 codes:** primaries 9, transfer 16 (PQ, 18 for HLG), matrix 9 (BT.2020-NCL), usually limited range. Write them in HEVC VUI or AV1 `color_config`, and mirror them in MDCV/CLLI and any out-of-band control message.
- **Codec strings:**
  - HEVC Main10: `hev1.2.4.L153.B0` / `hvc1.2.4.L153.B0`.
  - AV1 10-bit 4:2:0: `av01.0.13M.10`, optionally fully qualified as `av01.0.13M.10.0.110.09.16.09.0`.
  - HEVC Rext 4:4:4 10-bit: `hvc1.4.10.L153.B0` (pattern `hvc1.4.10…`).
- **Chrome** hardware-decodes HEVC Main10 on Windows, macOS, Android, ChromeOS and Linux-VAAPI (Chrome 108+). Rext 4:4:4 10-bit works on Intel Gen10+ Windows, NVIDIA Turing–Ada from Chrome 137 and Apple Silicon.
- **Safari** handles PQ, HDR10, HDR10+, HLG and Dolby Vision on macOS.

### Cited Findings
- **NVENC VUI enums match H.273** [vendor] — [nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h):
  - primaries: `BT2020 = 9`;
  - transfer: `BT2020_10 = 14`, `SMPTE2084 = 16` (PQ), `ARIB_STD_B67 = 18` (HLG);
  - matrix: `BT2020_NCL = 9`, `BT2020_CL = 10`.
- **libva's HDR10 example** is primaries 9, transfer 16 (SMPTE ST2084), matrix 9 [vendor] — [libva va_vpp.h](https://raw.githubusercontent.com/intel/libva/master/va/va_vpp.h)
- **AV1 codecs parameter** [standard] — [AV1 ISOBMFF binding](https://aomediacodec.github.io/av1-isobmff/):
  - Format: `<4CC>.<profile>.<level><tier>.<bitDepth>.<monochrome>.<chromaSubsampling>.<colorPrimaries>.<transferCharacteristics>.<matrixCoefficients>.<videoFullRangeFlag>`.
  - Profile, level, tier and bitDepth are mandatory; the rest are all-or-none. Defaults are `.0.110.01.01.01.0`, which SHOULD be omitted when present.
  - Examples: `av01.0.04M.10.0.112.09.16.09.0` and `av01.0.01M.08`.
- **HEVC codec-string patterns.**
  - Main `hev1.1.6.L93.B0`, **Main 10 `hev1.2.4.L93.B0`**, Main Still `hvc1.3.E.L93.B0`, Range Extensions `hvc1.4.10.L93.B0` [single-dev] — [StaZhu README](https://raw.githubusercontent.com/StaZhu/enable-chromium-hevc-hardware-decoding/main/README.md)
  - Apple uses `hvc1.2.4.L123.B0` for HDR10, HLG and SDR Main10 [vendor] — [Apple HLS Authoring Spec](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices)
- **Chrome HEVC hardware decode** [single-dev, maintained by a Chromium HEVC contributor] — [StaZhu README](https://raw.githubusercontent.com/StaZhu/enable-chromium-hevc-hardware-decoding/main/README.md):
  - Main and Main 10 up to 8192×8192.
  - Platforms: macOS 11+, Windows 8+, Android 5+, ChromeOS (VAAPI), and Linux Chrome ≥ 108.0.5354.0 with VAAPI GPUs only.
  - Rext 10-bit 4:4:4: Intel ICL–TGL+ on Windows; NVIDIA Turing–Ada on Windows from Chrome 137 with driver ≥ 572.16 (no 4:2:2); NVIDIA Blackwell all formats; Apple Silicon hardware 8–10-bit 4:0:0/4:2:0/4:2:2/4:4:4.
  - Retaining 4:2:2/4:4:4 chroma requires Chrome ≥ 125.
  - HDR support: Chrome and Edge on Windows and macOS handle PQ, HDR10, HDR10+ (as HDR10), HLG, and DV P8.1/P8.4 but not P5. Safari on macOS handles all of these. Firefox on Windows handles none; it decodes HEVC from 133 but "doesn't seem able to render HLG/PQ correctly".
  - WebCodecs HEVC *encode* needs Chrome ≥ 130.
- **Recent Chromium fixes:** an HDR HEVC `VideoFrame` drawn to canvas came out fully transparent (regression, fixed in Chrome 148), and over-strict HEVC keyframe detection in `VideoDecoder` was fixed in 149 (unverified details, from a tracker summary) — [Remotion WebCodecs bugs](https://www.remotion.dev/docs/mediabunny/webcodecs-bugs)

### Inferences
- **Codec strings for Cha's HDR modes (my construction from the patterns above):**
  - HEVC Main10 at Cha's current level 5.1 (L153): `hev1.2.4.L153.B0`, or `hvc1.2.4.L153.B0` for out-of-band parameter sets.
  - HEVC 4:4:4 10-bit Rext: `hvc1.4.10.L153.B0`. The constraint-byte value for 4:4:4 10-bit should be checked against the SPS (unverified).
  - AV1 Main 10-bit, level 5.1 Main tier: `av01.0.13M.10`, or `av01.0.13M.10.0.110.09.16.09.0` to let `isConfigSupported` or MediaCapabilities see the HDR colour.
  - The 4:2:0 subsampling field is `110` for the default chroma position (0 = unknown) per the ISOBMFF defaults. Cha's AV1 should state `chroma_sample_position` and keep the string consistent.
- **Signal consistently at every layer.** The VUI/`color_config`, the MDCV/CLLI SEI or OBU, the WebCodecs `VideoDecoderConfig.colorSpace` (`{primaries: "bt2020", transfer: "pq", matrix: "bt2020-ncl", fullRange: false}`) and the Moonlight `SS_HDR_METADATA` must agree.
  - A mismatch produces the washed-out symptom of Vibepollo #707.
  - With infinite GOP, SPS/sequence headers and metadata must be repeated on every IDR and on RFI recovery.
- **The Linux browser client is the weak spot.** HEVC Main10 hardware decode in Chrome on Linux requires VAAPI, so NVIDIA Linux clients get software decode or none (inference). AV1 10-bit (dav1d software fallback, or hardware) is the safer universal HDR codec for browsers.

### Gaps
- An authoritative Chromium source for AV1 10-bit *hardware* decode coverage per OS/GPU, and Safari's AV1 10-bit hardware (M3/A17 Pro+), was not fetched.
- Whether WebCodecs `VideoFrame`s from HEVC/AV1 PQ streams keep PQ through `importExternalTexture` and WebGPU HDR canvases (`toneMapping: extended`) was not researched; it is client-side.
- I did not compute the exact general_constraint_indicator bytes for HEVC Rext 4:4:4 10-bit codec strings.
