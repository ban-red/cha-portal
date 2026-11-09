# Codec-level and network-adaptive resolution scaling for low-latency interactive streaming (state as of 2026-11-09)

> Evidence for [research note 11](README.md).

Scope: sending fewer pixels and getting them back on the client, at the bitstream, protocol and codec level, with what encoders and browser or hardware decoders support. Primary sources were checked where possible: the AV1 spec, nv-codec-headers (NVENC API 13.1), libva, intel/media-driver, Mesa, Chromium and libwebrtc `main` source as fetched on 2026-11-09. Items I could not confirm are tagged (unverified).

## 1. AV1 super-resolution (superres + loop restoration): normative status, decoder support, encoder support, measured gains

### Takeaway
Superres is a normative decoder tool in every AV1 profile, but Chrome cannot use it on hardware in 2026. Chromium's shared hardware AV1 decoder, used on macOS (VideoToolbox), Windows (D3D) and Linux, rejects any frame whose coded width differs from its upscaled width ("Super resolution is not supported" leads to a decode error). NVENC exposes no superres control, and Intel's VA-API capability tables advertise `support_superres = 0` even though the driver contains superres HAL code. The measured gains are also small: superres beats out-of-loop downscaling, but it beats full resolution only at very low bitrates. For Cha Portal it is a dead end. The project is right to keep `superres_scale_denominator = 8`.

### Cited Findings
- **Bitstream:** superres is horizontal only. If `enable_superres` is set in the sequence header, each frame may set `use_superres`. A 3-bit `coded_denom` gives `SuperresDenom = coded_denom + 9` (9..16), with `SUPERRES_NUM = 8`, so the coded width is `UpscaledWidth*8/SuperresDenom`, between 8/9 and 1/2 of full width. Height is never scaled. The "Upscaling process" takes a FrameWidth×FrameHeight frame and outputs a "horizontally upscaled frame of width UpscaledWidth and height FrameHeight". In the decoding process it runs after CDEF and before loop restoration, so loop restoration runs at full resolution. — [AV1 Bitstream & Decoding Process Specification (aomediacodec.github.io; working-draft HTML last modified 2023-05-25, which links the approved PDF)](https://aomediacodec.github.io/av1-spec/)
- **Sequence flag:** "It is allowed to set enable_superres equal to 1 even when use_superres is not equal to 1 for any frame." — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **Screen-content conflict:** on intra frames, `allow_intrabc` is only read `if (allow_screen_content_tools && UpscaledWidth == FrameWidth)`. A superres frame therefore cannot use intra block copy. — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **Design intent:** superres is meant for low bitrates. Deblocking and CDEF run at low resolution, the upscale is normative, and loop restoration (Wiener or self-guided) then runs at full resolution to recover detail. — [AOM "Tool Description for AV1 and libaom"](https://aomedia.org/docs/AV1_ToolDescription_v11-clean.pdf)
- **Quality, older study (APSIPA 2020):** superres "is almost always observed to be much better than upscaling lower resolution frames outside the codec in objective metrics", and "at very low bit-rates, it is sometimes observed to be better than full resolution too" perceptually. — [Chen et al., "An Overview of Coding Tools in AV1", APSIPA Trans. 2020](https://www.cambridge.org/core/journals/apsipa-transactions-on-signal-and-information-processing/article/an-overview-of-coding-tools-in-av1-the-first-video-codec-from-the-alliance-for-open-media/5972E00494363BE37E3439FAE382DB10)
- **Quality, SVT-AV1 docs:** "for most natural videos and common bit rates, enabling super-resolution actually leads to a drop in compression efficiency. According to internal tests, coding gain is achieved only at very low bit rates with the Auto mode." The modes are Fixed, Random, QThreshold and Auto (Auto-Solo, Auto-Dual, Auto-All). The Auto modes re-run the coding loop per denominator, which costs extra encoder time. — [SVT-AV1 Docs/Appendix-Super-Resolution.md](https://gitlab.com/AOMediaCodec/SVT-AV1/-/blob/master/Docs/Appendix-Super-Resolution.md)
- **libaom options:** `--superres-mode` ("0: disabled (default), 1: fixed, 2: random, 3: qthresh, 4: auto") and, separately, `--resize-mode` ("0: off (default), 1: fixed, 2: random, 3: dynamic"). — [libaom av1/arg_defs.c (main)](https://aomedia.googlesource.com/aom/+/refs/heads/main/av1/arg_defs.c)
- **NVIDIA decode:** NVDEC's AV1 picture parameters carry `enable_superres` ("superres in seq level"), `use_superres` ("superres enabled for frame") and `coded_denom`, and `width` is documented as "coded width, if superres enabled then it is upscaled width". The NVDEC hardware path therefore handles superres. — [nv-codec-headers dynlink_cuviddec.h (master, 2026)](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/dynlink_cuviddec.h)
- **NVIDIA encode:** the NVENC API 13.1 header (`NVENCAPI_MAJOR_VERSION 13`, `MINOR 1`, copyright 2010–2026) has no superres field. The only AV1 size-related knob is `darWidth/darHeight`, which "Specifies … the render width (AV1)". — [nv-codec-headers nvEncodeAPI.h](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h); the [NVENC Programming Guide (SDK 13.0)](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html) also never mentions superres.
- **VA-API encode interface:** libva defines `support_superres` in `VAConfigAttribValEncAV1` ("Allows setting enable_superres in the SPS"), plus `enable_superres`, `use_superres` and `superres_scale_denominator` ("value range [9..16]"). Comment: "For SupRes, underline encoder is responsible to do downscaling", meaning the driver downscales and the app supplies a full-size source. — [libva va/va_enc_av1.h](https://raw.githubusercontent.com/intel/libva/master/va/va_enc_av1.h)
- **Intel media-driver HAL:** `encode_av1_superres.cpp` implements superres encode. It checks the denominator range 9–16 and notes a "HW restriction of SuperRes denominator for SuperRes + LoopRestoration": "only valid SuperRes denominator values are 10, 12, 14 and 16". Feature managers for Xe_LPM+, Xe2_HPM/LPM, Xe3_LPM and Xe3P reference it. — [intel/media-driver encode_av1_superres.cpp (master)](https://raw.githubusercontent.com/intel/media-driver/master/media_softlet/agnostic/common/codec/hal/enc/av1/features/encode_av1_superres.cpp)
- **Intel media-driver Linux VA-API caps:** the caps tables for Xe2_HPM, Xe2_LPM, Xe3_LPM and Xe3P_HPM (Profile0 and Profile1) all set `VAConfigAttribValEncAV1 = {0,0,0,0,0,0,1,0,0,0,0,0,1,1,0}`. In field order that is palette = 1, superres = 0, restoration = 0, intrabc = 1, cdef_channel_strength = 1. They also set `VAConfigAttribEncDynamicScaling = 0`. — [capstable_data_av1_encode_xe2_hpm_r0_specific.h](https://raw.githubusercontent.com/intel/media-driver/master/media_softlet/linux/xe2_hpm_r0/encode/av1/ddi/capstable_data_av1_encode_xe2_hpm_r0_specific.h) (field order from [va_enc_av1.h](https://raw.githubusercontent.com/intel/libva/master/va/va_enc_av1.h))
- **Mesa:** the VA frontend parses `use_superres` from packed headers and forwards `superres_scale_denominator`. `VAConfigAttribEncAV1` reports whatever the gallium driver returns for `PIPE_VIDEO_CAP_ENC_AV1_FEATURE`. — [Mesa src/gallium/frontends/va/picture_av1_enc.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/gallium/frontends/va/picture_av1_enc.c), [config.c](https://gitlab.freedesktop.org/mesa/mesa/-/raw/main/src/gallium/frontends/va/config.c). I could not confirm whether radeonsi (VCN 4/5) sets the superres bit (unverified).
- **Chromium hardware decode:** `media/gpu/av1_decoder.cc` contains `if (current_frame_size.width() != frame_header.upscaled_width) { DVLOG(1) << "Super resolution is not supported"; return kDecodeError; }`. A change of `enable_superres` between sequence headers forces a hardware context reset. — [Chromium media/gpu/av1_decoder.cc (main)](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/av1_decoder.cc)
- **Who uses that decoder:** the macOS VideoToolbox AV1 path (`media/gpu/mac/video_toolbox_av1_accelerator.h` includes `media/gpu/av1_decoder.h` and implements `AV1Decoder::AV1Accelerator`) and the Windows path (`media/gpu/windows/d3d_av1_accelerator.h`, `class D3DAV1Accelerator : public AV1Decoder::AV1Accelerator`). — [Chromium media/gpu/mac](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/mac/), [media/gpu/windows](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/windows/). The Linux VA-API path is assumed to use the same class but I did not re-check it in this snapshot (unverified).
- **Safari:** AV1 decodes only on devices with an AV1 hardware decoder (A17 Pro, M3 and later). Apple ships no software AV1 decoder in VideoToolbox, although Safari bundles dav1d for AVIF. — [Bitmovin, Apple AV1 support](https://bitmovin.com/blog/apple-av1-support/); [FFmpeg trac #10642](https://trac.ffmpeg.org/ticket/10642). I found no source on whether Apple's AV1 hardware or VideoToolbox handles superres (unverified).
- **Real-time stacks avoid superres:** libwebrtc's libaom AV1 wrapper sets no superres control at all (no `AV1E_SET_ENABLE_SUPERRES`). — [webrtc libaom_av1_encoder.cc (main)](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/video_coding/codecs/av1/libaom_av1_encoder.cc). Meta's 2023 AV1 RTC write-up does not mention superres either. — [@Scale, "AV1: Improving RTC Video Quality at Scale", 2023-11-29](https://atscaleconference.com/av1-improving-rtc-video-quality-at-scale/)

### Inferences
- Chrome hardware-decodes Cha Portal's main client (Apple Silicon) through VideoToolboxAV1Accelerator plus the shared `AV1Decoder`. Any frame with `use_superres = 1` would hit `kDecodeError`. With WebCodecs, a decode error closes the decoder, and I found no mid-stream software fallback (unverified). On Safari, superres support is unknown and there is no software fallback. Superres is therefore not deployable to Cha Portal's browsers, whatever the encoder can do.
- Setting only the sequence-level `enable_superres = 1` while every frame keeps `use_superres = 0` appears harmless to Chromium: the check compares per-frame widths. But changing `enable_superres` between sequence headers forces a context reset. Keeping it 0 costs nothing.
- None of Cha Portal's encoders can produce superres today. NVENC has no knob. Intel Xe2/Xe3 VA-API advertises `support_superres = 0` and `support_restoration = 0`, and superres without loop restoration loses most of its point. AMD is unconfirmed. Even where possible, SVT-AV1 and AOM data say it pays only at very low bits per pixel, below the 5–50 Mbit/s 1440p range.

### Gaps
- No primary source on superres support in Apple M3/M4/M5 VideoToolbox, Qualcomm Adreno or AMD VCN AV1 decoders. The AV1 spec makes it mandatory for conformance, but I could not verify real hardware or driver exposure, and Chrome's own parser blocks it before the driver sees it.
- No 2024–2026 BD-rate table for libaom superres alone at low delay. Neither the APSIPA overview nor the SVT-AV1 doc gives numbers.
- Whether radeonsi (VCN 4/5) advertises superres in `PIPE_VIDEO_CAP_ENC_AV1_FEATURE` was not checked: the file has moved and I did not locate it.

## 2. Dynamic resolution without keyframes: AV1 reference scaling, VVC RPR, HEVC/H.264 limits, NVENC/VA-API, browsers, libwebrtc

### Takeaway
AV1, VP9 and VVC can change coded size on an inter frame by predicting from scaled references. AV1 allows the reference to be at most 2× larger and at most 16× smaller. HEVC and H.264 need a new SPS and an IRAP/IDR. Chromium's hardware AV1 decoder accepts mid-sequence size changes and outputs frames at the new coded size, ignoring `render_size`, but a Chromium bug reports intermittent freezes with such streams. libwebrtc itself does not use the feature: any width or height change makes `VideoStreamEncoder` release and re-init the encoder, which yields a keyframe. Whether NVENC AV1 emits a reference-scaled inter frame after `NvEncReconfigureEncoder` without `forceIDR` is undocumented. Intel's VA-API driver reports `VAConfigAttribEncDynamicScaling = 0`.

### Cited Findings
- **AV1 frame size per frame:** "frame_size_override_flag equal to 1 specifies that the frame size will either be specified as the size of one of the reference frames, or computed from the frame_width_minus_1 and frame_height_minus_1 syntax elements." For inter frames, `frame_size_with_refs` can copy a reference's size (`found_ref`) or code it explicitly. — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **AV1 scaling limits:** for every reference, `2*FrameWidth >= RefUpscaledWidth`, `2*FrameHeight >= RefFrameHeight`, `FrameWidth <= 16*RefUpscaledWidth` and `FrameHeight <= 16*RefFrameHeight`, "even if all the blocks in an inter frame are coded using intra prediction". Motion compensation computes `xScale/yScale` (`REF_SCALE_SHIFT` fixed point) from the reference and current sizes. — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **AV1 render size:** "The render size is provided as a hint to the application about the desired display size. It has no effect on the decoding process." — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **Chromium handling:** on a mid-sequence size change Chromium logs "Resolution change in the middle of video sequence. Frames encoded using reference frame scaling." and keeps decoding. The comment cites SVT-AV1's Appendix-Reference-Scaling. The visible rect is set to the current frame's coded width and height, because render_width/height "has no effect on the decoding process", so the output VideoFrame shrinks. `kConfigChange`, which reallocates surfaces, fires only when the sequence header's max frame size, profile, bit depth, chroma format, colour space or visible rect changes. — [Chromium media/gpu/av1_decoder.cc](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/av1_decoder.cc)
- **Chromium bug:** issue 338251332 is titled "AV1 videos encoded using reference frame scaling freeze intermittently when played in chromium". I could read only the title (sign-in wall); status unknown. — [Chromium issue 338251332](https://issues.chromium.org/issues/338251332)
- **VA-API decode:** in libva issue #634 (opened 2022-09-28, still open), VP9 and AV1 allow size changes on non-keyframes. Apps traditionally recreate the context on a keyframe size change, which may drop needed reference state. Intel iHD can decode frames larger than the `vaCreateContext` size. AMD Mesa "cannot handle a frame whose size is larger than the size configured in vaCreateContext()" but handles smaller ones. A DRC capability attribute was proposed in PR #636. — [intel/libva issue #634](https://github.com/intel/libva/issues/634)
- **libwebrtc always re-inits:** `RequiresEncoderReset()` returns true if `new_send_codec.width != prev_send_codec.width || height != …`, and `ReconfigureEncoder` then calls `ReleaseEncoder()` and `encoder_->InitEncode()`. — [webrtc video/video_stream_encoder.cc (main)](https://webrtc.googlesource.com/src/+/refs/heads/main/video/video_stream_encoder.cc)
- **libwebrtc AV1 wrapper:** no `rc_resize_mode`. Quality scaling uses QP thresholds (`kLowQindex/kHighQindex`) when `automatic_resize_on` is set. `kf_mode = AOM_KF_DISABLED`, and keyframes come only on request. — [webrtc libaom_av1_encoder.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/video_coding/codecs/av1/libaom_av1_encoder.cc)
- **Inference from the two files above:** each quality-scaler step in Chrome's WebRTC sender is a fresh encoder and therefore a keyframe.
- **Why it matters, Meta (2023-11-29):** "Reference Picture Resampling (RPR), which allows resolution changes without generating a key frame". H.264 calling generated "about 1.5 key frames per minute just for resolution switches caused by network changes". — [@Scale/Meta](https://atscaleconference.com/av1-improving-rtc-video-quality-at-scale/)
- **libaom/SVT-AV1 dynamic resize (2026):** SVT-AV1 PR #3369 (GitLab !2693, June 2026; GitHub closed 2026-10-26) found `--resize-mode 3` (dynamic) "a silent no-op" under `--rtc`. The fix aligns with libaom's `ratectrl.c`: 1/2-only resize for real time, a scene-change gate, a 1.5× overshoot nudge. Low-bitrate CBR BD-rate: vidyo1 720p −17.7% (SSIM) / −7.4% (PSNR); foreman CIF −8.7% / −3.2%. Without resize vidyo1 "floors at about 46 kbps", with resize it reaches 30 kbps. It also notes that "frame-size-override resize decodes to variable-resolution frames that decoders do not upscale". — [AOMediaCodec/SVT-AV1 PR #3369](https://github.com/AOMediaCodec/SVT-AV1/pull/3369)
- **NVENC:** `NV_ENC_CAPS_SUPPORT_DYN_RES_CHANGE` ("Dynamic Encode Resolution Change Support"). `maxEncodeWidth/Height`: "If set to 0, Encoder will not allow dynamic resolution change." The reconfigure API cannot change GOP structure, MaxWidth/MaxHeight or PTD mode. `resetEncoder` "should be used only with an IDR frame". — [nvEncodeAPI.h 13.1](https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h)
- **NVENC guide:** "it is advisable to force the next frame following the reconfiguration as an IDR frame" (forceIDR). "In case there in a resolution reconfiguration in the middle of an intra refresh wave, the ongoing wave will be terminated immediately." There are no codec-specific statements on AV1 reference scaling. — [NVENC Programming Guide, SDK 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- **VA-API encode:** "AV1 encoder may support SupRes and dynamic scaling function … For dynamic scaling, app should provide the scaled raw source." — [va_enc_av1.h](https://raw.githubusercontent.com/intel/libva/master/va/va_enc_av1.h). Intel Xe2/Xe3/Xe3P caps report `VAConfigAttribEncDynamicScaling = 0`. — [intel/media-driver caps table](https://raw.githubusercontent.com/intel/media-driver/master/media_softlet/linux/xe2_hpm_r0/encode/av1/ddi/capstable_data_av1_encode_xe2_hpm_r0_specific.h)
- **VVC RPR:** VVC "enables picture resolution change within a sequence at a position without encoding an IRAP picture, which is always intra-coded", something HEVC and AVC cannot do. The scaling ratio is limited to ≥ 1/2 (2× downsampling from reference to current) and ≤ 8 (8× upsampling). Three resampling filter sets cover 1/2–1/1.75, 1/1.75–1/1.25 and 1/1.25–8. Picture size and conformance window move into the PPS, with only the maximum size in the SPS. — [USPTO "Reference picture resampling" (describes VVC; not the ITU-T text)](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11917208)

### Inferences
- **WebCodecs path (Chrome):** an NVENC or VA-API AV1 stream with `frame_size_override_flag = 1` on a P-frame (references ≤ 2× larger) should decode on Chrome's hardware path without a keyframe. `VideoFrame.codedWidth/visibleRect` would follow the new size, and `render_size` would be ignored, so the Cha Portal renderer must scale every frame to the window. The freeze bug means this needs testing on Apple Silicon (VideoToolbox) and Windows before shipping.
- Safari's behaviour with reference-scaled AV1 through VideoToolbox is unknown. Untested (unverified).
- **WebRTC path:** Cha Portal's own sender controls the bitstream, so a reference-scaled frame can go over a WebRTC transport. On the receiving side, Chrome's WebRTC video receive stream and `<video>` element follow in-band size changes (they already handle keyframe size changes). Whether its jitter buffer or hardware decoder handles a size change on a delta frame was not verified (unverified).
- **NVENC:** set `maxEncodeWidth/Height` to 2560×1440 at init. Then test whether `NvEncReconfigureEncoder` with a smaller `encodeWidth` and `forceIDR = 0` yields an AV1 inter frame with `frame_size_override_flag = 1` and valid scaled references. The header and guide are silent. For HEVC/H.264 a new SPS forces an IDR, though RFI and an intra-refresh wave could soften its cost; any wave in progress is terminated.
- **VA-API AV1:** Cha Portal writes its own sequence and frame headers, so it can signal the override. But with `EncDynamicScaling = 0` the Intel hardware probably cannot motion-compensate from a reference of a different size. Expect that to require an intra frame (inference). AMD Mesa decoders would also choke on frames larger than the context size, which matters for any Linux client.
- **PyroWave:** intra-only, so it has no reference-scaling problem. Resolution can change on any frame, and only the decoder's texture allocation must follow.
- **Practical pattern:** cap the sequence max size at 2560×1440, step down by ratios ≤ 2× per frame (the AV1 rule), and use a two-step "bridge" for larger drops, as AV2 formalizes (see §4).

### Gaps
- The NVENC behaviour after an AV1 reconfigure without forceIDR: no NVIDIA statement found. It needs an empirical test, parsing the OBUs for `frame_type` and `frame_size_override_flag`.
- The status and root cause of Chromium issue 338251332 (sign-in wall).
- Whether VideoToolbox (macOS) and D3D12 (Windows) drivers correctly decode AV1 or VP9 reference-scaled frames. No primary data found.

## 3. LCEVC (MPEG-5 Part 2): mechanism, latency, browser decoder, licensing, cloud-gaming evidence, hardware, open-source encoder

### Takeaway
LCEVC adds up to two residual enhancement layers and a normative upsampler on top of any base codec, which is usually coded at a quarter of the pixels. On the web it is decoded in JavaScript, WASM and WebGL on top of an MSE `<video>` base. There is no WebCodecs, WebRTC or native browser path. The decoder SDK is BSD-3-Clause-Clear, which explicitly grants no patents. Usage needs V-Nova patent licences: in August 2025 V-Nova announced a service royalty by size or active users and a device royalty from about $0.20. No open-source encoder was found. The cloud-gaming claims (about 30% savings, about 5 ms per frame on NVENC) are vendor-sourced.

### Cited Findings
- **Mechanism and timeline:** LCEVC is an enhancement layer for a base stream from AVC, HEVC, VP9, AV1, EVC or VVC, adding "up to two layers of encoded residuals" and "normative signalled up-sampling methods". The base layer is hardware-decodable and the enhancement is designed for software. Timeline: CfP 2018, Final Draft October 2020, verification test validated April 2021 ("tended to indicate an overall benefit" for AVC, HEVC, EVC and VVC), conformance January 2022, adopted for Brazil's SBTVD TV 3.0 in January 2022. — [Wikipedia: LCEVC](https://en.wikipedia.org/wiki/LCEVC)
- **Base resolution:** the base is typically coded at a quarter of the target resolution, and the enhancement carries the residual between the upscaled base and the source. — [V-Nova "LCEVC in depth"](https://campaign.v-nova.com/lcevc-in-depth) (via search excerpt; page not fully read)
- **Web decoder (LCEVCdecJS):** the base is decoded by the HTML5 video element via MSE. A WebWorker demuxes segments and extracts LCEVC data, `liblcevc_dpi.wasm` decodes the enhancement, and WebGL composites onto a canvas each `requestAnimationFrame`. It requires WebGL 1.0 with framebuffers, WebWorkers, WebAssembly and WebAudio with a Delay node. Integration targets MSE players (hls.js patch files). The page describes the decoder as "source available" and does not mention WebRTC, WebCodecs or WebGPU. — [V-Nova docs: LCEVCdecJS](https://docs.v-nova.com/technologies/mpeg-5.lcevc/decoder/sdk/lcevc-decoder-for-web-lcevcdecjs)
- **Shaka Player:** supports LCEVC for DASH and HLS through lcevcdec-js hooked into MSE. — [V-Nova blog: GStreamer and Shaka integration](https://v-nova.com/insights/blog/lcevc-gstreamer-shaka-player-open-source-integration/)
- **W3C Media WG (Nov 2025):** discussed the lack of a native browser path. "Base layer is decoded by browser MSE. LCEVC is decoded in software." — [W3C minutes 2025-11-10](https://www.w3.org/2025/11/10-me-minutes.html) (via search excerpt)
- **Native decoder SDK:** LCEVCdec is "licensed under the BSD-3-CLAUSE-CLEAR License" and is decode-only. Onward distribution "REMAINS SUBJECT TO THE EXCLUSION OF PATENT LICENSES PROVISION", and patent enquiries go to legal@v-nova.com. — [github.com/v-novaltd/LCEVCdec](https://github.com/v-novaltd/LCEVCdec)
- **FFmpeg and GStreamer:** FFmpeg 7.1 decodes LCEVC through external LCEVCdec. GStreamer has LCEVC elements from 1.26 (March 2025). — [Wikipedia: LCEVC](https://en.wikipedia.org/wiki/LCEVC)
- **Licensing (2025-08-08):** two programs, one for distribution services (royalties by size of service, waived for free-to-air) and one for device makers (volume-based). The device rate reportedly starts at $0.20 per unit, with an "open-access" discount. — [V-Nova press release](https://v-nova.com/press-releases/v-nova-announces-licensing-programs-for-mpeg-5-lcevc-to-support-tv-3-0-and-streaming-growth/); [Broadband TV News, 2025-08-08](https://www.broadbandtvnews.com/2025/08/08/v-nova-announces-licensing-programs-for-mpeg-5-lcevc/); [Wikipedia](https://en.wikipedia.org/wiki/LCEVC)
- **NVENC integration (NVIDIA blog, 2023-05-24, SDK 12.1):** LCEVC is "particularly amenable to cloud gaming and VR/XR". It runs with "1080p60 low-latency decoding on mobile phones and 4Kp90 on XR headsets", uses Tensor Cores, and claims a 10× speedup versus CPU. The aim was to encode 3680×1920 stereo within one frame time. NVENC ReCon (access to the reconstructed frame plus per-block QP and bit statistics) removes the NVDEC stage, "with an encoding time of about 5 ms per frame". No quality numbers are given for the XR comparison at 25 Mbit/s. — [NVIDIA Technical Blog](https://developer.nvidia.com/blog/new-video-creation-and-streaming-features-accelerated-by-the-nvidia-video-codec-sdk/)
- **NAB 2024 (vendor demo):** LCEVC on NVENC HEVC reportedly cut an XR app's bitrate by about 55% (about 55 to 25 Mbit/s) and gave about 30% gains in low-latency cloud gaming. — [SVG, NAB 2024 LCEVC showcase](https://www.sportsvideo.org/2024/04/15/mpeg-5-lcevc-showcase-at-nab-2024-demonstrates-ecosystem-growth-and-deployment-readiness/) (via search excerpt; vendor claim, unverified)
- **Software overhead:** about 10–20% CPU on top of hardware base decode, from a third-party article citing V-Nova measurements (unverified). — [forasoft LCEVC explainer](https://www.forasoft.com/learn/video-encoding/articles/lcevc-explained)
- **Independent comparison:** an academic comparison of the V-Nova LCEVC SDK against open-source codecs exists (UMH). I did not read it. — [Comparing V-Nova LCEVC SDK with Practical Open-Source Video Codecs (PDF)](https://dspace.umh.es/bitstream/11000/39590/1/Comparing_V-Nova_LCEVC_SDK_with_Practical_Open-Source_Video_Codecs.pdf)

### Inferences
- For Cha Portal, LCEVC fails on two counts. (a) Licensing: an AGPL self-hosted project cannot pass on patent rights. BSD-3-Clear excludes them, and royalties are per service or device. (b) Pipeline: LCEVCdecJS assumes MSE and `<video>` segments with a canvas overlay. Cha Portal uses WebCodecs or WebRTC frames and would have to port the WASM residual decoder and the WebGL compositor onto per-frame `VideoFrame`s. There is also no encoder Cha Portal can ship.
- Architecturally, LCEVC resembles "downscale plus a learned upsampler plus a residual" with no extra frame of latency, since the enhancement is per frame and needs no lookahead. The roughly 5 ms NVENC-assisted encode would fit a 60 fps budget, but this rests on vendor statements.

### Gaps
- No independent, peer-reviewed latency or quality numbers for LCEVC in low-delay cloud gaming.
- No confirmed open-source encoder. An informal GitHub note claims MPEG's reference model is obtainable (unverified).
- Hardware or SoC LCEVC decode (V-Nova says "driver level in certain SoCs"): no specific vendor list found.

## 4. AV2 (AOM): status in Nov 2026 and tools relevant to super-resolution, screen content and low delay

### Takeaway
AV2 v1.0.0 (spec plus AVM reference software) is dated 2026-05-28 and was announced 2026-06-09. It claims about 30% bitrate savings over AV1, more for screen content. AV2 removes superres but keeps and extends reference scaling, now with a 2× limit in both directions and "bridge frames". It strengthens screen-content tools: IBC usable with in-loop filters, palette, forward skip coding, Backward Reference Update, and a Main 4:4:4 10-bit profile. No browser or hardware decode exists in 2026, and hardware is expected from 2027 at the earliest (unverified).

### Cited Findings
- **Release:** AOM announced the AV2 release, with the specification public at av2.aomedia.org. The page lists "AV2 specification and reference code, v1.0.0" dated 2026-05-28. — [AOM press release](http://aomedia.org/press%20releases/Alliance-for-Open-Media-Releases-AV2-Codec/); [GIGAZINE 2026-06-10](https://gigazine.net/gsc_news/en/20260610-aomedia-av2-codec/)
- **Draft:** a draft spec and AVM were released in early 2026. — [CNX Software, 2026-02-03](https://www.cnx-software.com/2026/02/03/aomedia-av2-video-codec-draft-specification-release-and-a-quick-try-at-the-reference-implementation/)
- **AOM implementers post (2026-07-27):** "the coding tools are complete". About 30% better compression than AV1 on VMAF and PSNR, with visual gains "closer to 40%". Reference software is available and optimized implementations are "well underway". Hardware follows software, with no timeline. — [AOM blog](http://aomedia.org/blog%20posts/AV2-What-the-Final-Specification-Means-for-Implementers/)
- **Adoption claim:** in October 2026 AOM reported rapid AV2 adoption four months after release (title only; not read). — [AOM press release](http://aomedia.org/press%20releases/AOMedia-Rapid-AV2-Adoption-Across-the-Video-Ecosystem/)
- **Tool changes (Andrey Norkin overview, 2026-05-31):**
  - Super-resolution is removed and loop restoration reworked.
  - Reference scaling is kept "from 2x down to 1/16" of the current frame size. AV2 "disallows prediction from a reference more than twice the current resolution, so bridge frames handle large downscaling".
  - IBC has local and global variants and, unlike AV1, works with in-loop filters. Palette is kept. "Backwards Reference Update allows partial frame updates for mostly static content such as screencasting".
  - S-frames act as switching points.
  - Profiles: Main_420_10 (IP0/1/2), Main_422_10_IP1 and Main_444_10_IP1.
  - New in-loop filters (CCSO, GDF) and TCQ, superblocks up to 256×256, and mandatory film-grain synthesis.
  - About 30% gain over AV1 from 270p to 4K, "screen content and HDR show larger gains".
  — [Norkin, AV2 overview](https://norkin.org/research/av2_overview/index.html)
- **Transform and entropy tools:** IST, TCQ, ATC, PARA, Forward Skip Coding (FSC, "screen-content gains comparable to the Palette tool"), CCTX, parity hiding, improved lossless coding. — [arXiv 2601.02712, "Transform and Entropy Coding in AV2"](https://arxiv.org/html/2601.02712v1)
- **Low delay:** AVM CTC includes a Low Delay configuration. AVM v11 added 0.7% (LD) and 0.3% (RA) over v10. — [Streaming Learning Center, "Inside AV2"](https://streaminglearningcenter.com/codecs/inside-av2-architecture-performance-and-adoption-outlook.html) (via search excerpt)
- **Software and hardware outlook:** libavm and dav2d are in development, and first hardware decoders are expected in 2027 chips (secondary source, unverified). — [Wikipedia: AV2](https://en.wikipedia.org/wiki/AV2)

### Inferences
- Within AOM, AV2 confirms the direction of travel: decoder-side super-resolution is abandoned in favour of reference scaling, with display-side scaling left to the application. A design built on AV1 reference scaling plus client upscaling carries forward to AV2.
- AV2 is irrelevant for Cha Portal's 2026–2027 browser clients: no Chrome or Safari decoder, no NVENC, VA-API or VCN encoder. Its screen-content features (IBC with loop filters, Backward Reference Update, 4:4:4 Main profile) are the ones that would matter for desktop streaming later.

### Gaps
- No primary source for AV2 browser plans (Chrome dav2d integration) or NVIDIA, Intel or AMD AV2 hardware roadmaps.
- No published AV2 low-delay BD-rate for screen content against AV1 in a real-time configuration.

## 5. H.266/VVC and other codecs: browser or hardware decode in 2026

### Takeaway
VVC has the cleanest normative mechanism for keyframe-free resolution change (RPR). It decodes in hardware on Intel Lunar Lake (Xe2 LPG) via VA-API and FFmpeg, but no mainstream browser decodes it in 2026. It is irrelevant to Cha Portal's browser clients.

### Cited Findings
- Intel added VVC decode to its VA-API driver stack and to FFmpeg (VA-API and QSV) in 2024. — [Phoronix: Intel FFmpeg 2024Q2 VVC](https://www.phoronix.com/news/Intel-FFmpeg-H266-VVC); [Phoronix: VVC VA-API decoding lands in FFmpeg](https://www.phoronix.com/news/VVC-VA-API-FFmpeg-Decode)
- Lunar Lake (Xe2) supports VVC decode. Arc B-series (Battlemage) does not. — [Intel Community thread](https://community.intel.com/t5/Graphics/Arc-B-series-support-VVC-H-266-decoding/td-p/1699558)
- Chrome, Firefox, Edge and Safari had no VVC decoder as of 2026, according to secondary sources only. I found no Google or Apple primary statement (unverified). — [forasoft VVC explainer](https://www.forasoft.com/learn/video-encoding/articles/h266-vvc-explained); [Ant Media VVC guide](https://antmedia.io/versatile-video-coding-vvc-h266-codec-guide/)
- The RPR mechanism (see §2): resolution changes without IRAP, ratio from 1/2 to 8, size signalled in the PPS. — [USPTO RPR patent text](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11917208)
- VP9 also supports non-keyframe resolution change via frame scaling. — [intel/libva issue #634](https://github.com/intel/libva/issues/634)

### Inferences
- VP9 is the only other browser-decodable codec with keyframe-free scaling. It has no NVENC encoder, so for Cha Portal it is AV1 or nothing.
- For a future native macOS client, VVC is moot as well: Apple has no VVC hardware (unverified).

### Gaps
- No primary source on MediaTek or Qualcomm VVC decode, or on any Chrome or WebKit VVC plans.

## 6. Neural-enhanced and neural codecs for real-time streaming (2020–2026)

### Takeaway
The content-aware SR line (NAS to NEMO to LiveNAS to NeuroScaler, then SRVC) shows large bandwidth savings by sending low-resolution video plus per-content models or model deltas. SRVC sends about 1% of the parameters of a roughly 2.2M-parameter model per segment and needs 16% of H.265's bits at equal PSNR. Most of this work targets VOD or live ingest at 30 fps on server GPUs. The game-specific work (GameStreamSR, River) uses codec structure, such as reference versus non-reference frames, to cut client compute, but only reaches 720p-class real time on mobile. DCVC-RT (CVPR 2025, open code) is the first neural codec in real time at 1080p: 113 fps decode on A100, about 34 fps on an RTX 2080 Ti, 1080p30 on an RTX 4090 in deterministic int16. It needs a CUDA-class GPU on both ends and does not run in browsers or on Apple ANE/Metal today (unverified).

### Cited Findings
- **NeuroScaler (SIGCOMM 2022):** versus per-frame and selective neural enhancement, it "improves the processing throughput by 10× and 2.5–5× and reduces the overall cost by 22.3× and 3.0–11.1×". It and NEMO (MobiCom 2020) apply heavy SR only to "anchor" frames and reuse results for non-anchor frames using codec information. — [KAIST NeuroScaler project](https://ina.kaist.ac.kr/projects/neuroscaler/); [Northwestern copy of the SIGCOMM'22 paper](https://networks.cs.northwestern.edu/CS497-s23/papers/sigcomm22-neural-video.pdf)
- **LiveNAS (SIGCOMM 2020):** online-trained SR at the ingest server "enables 4K streaming without requiring real-time 4K encoding or imposing a high-bandwidth requirement on ingest clients". — [KAIST LiveNAS project](https://ina.kaist.ac.kr/projects/livenas/)
- **SRVC (ICCV 2021, MIT):**
  - Design: a low-resolution content stream from a standard codec plus a model stream of per-segment updates to a lightweight SR network.
  - Update cost: it updates about 1% of the parameters (gradient-guided) because re-sending a roughly 2.22M-parameter model every few seconds would erase the gains.
  - Results: 16% of the bits per pixel of H.265 slow and 2% of DVC at equal PSNR, running at 90 fps on a V100.
  — [arXiv 2104.02322](https://arxiv.org/pdf/2104.02322.pdf)
- **GameStreamSR (ISCA 2024):** neural-augmented game streaming on commodity mobile SoCs. It reports a 13× frame-rate speedup for reference frames and 1.6× for non-reference frames, about 2× FPS overall, and 26–33% energy savings versus state-of-the-art DNN SR. — [ACM DL](https://dl.acm.org/doi/10.1109/ISCA59077.2024.00097); [Penn State listing](https://pure.psu.edu/en/publications/gamestreamsr-enabling-neural-augmented-game-streaming-on-commodit/)
- **River (arXiv 2501.06880, Jan 2025):** cloud gaming with retrieval of pre-trained, content-matched SR models plus prefetching, which avoids streaming weights. It reports about 720p at 20 fps on mobile. — [arXiv 2501.06880](https://arxiv.org/abs/2501.06880)
- **iSR (cloud VR gaming):** sends one eye in colour and the other in monochrome, then reconstructs full-colour high-resolution stereo on the client. — [ACM DOI 10.1145/3793853.3795746](https://doi.org/10.1145/3793853.3795746) (abstract only)
- **Real-time neural recovery for cloud gaming on mobile:** arXiv 2307.07847 exists; not read. — [arXiv 2307.07847](https://arxiv.org/pdf/2307.07847)
- **DCVC-RT (CVPR 2025, Microsoft):**
  - Compression: about 21% average savings versus H.266/VTM.
  - Speed (fp16, A100, 1080p): 125.2 fps encode, 112.8 fps decode; 39.5 / 34.1 fps on an RTX 2080 Ti.
  - int16 (for consistent cross-device coding): 1080p 30 fps on an RTX 4090, and 28.3 / 20.9 fps on A100.
  - Code: [github.com/microsoft/DCVC](https://github.com/microsoft/DCVC).
  — [arXiv 2502.20762](https://arxiv.org/abs/2502.20762); [CVPR 2025 paper](https://openaccess.thecvf.com/content/CVPR2025/papers/Jia_Towards_Practical_Real-Time_Neural_Video_Compression_CVPR_2025_paper.pdf); independent evaluation (not read): [Streaming Learning Center](https://streaminglearningcenter.com/articles/evaluating-dcvc-rt-a-real-time-neural-video-codec-that-delivers-on-speed-and-compression.html)
- **MobileNVC (2023):** real-time 1080p neural video compression on a mobile device. — [arXiv 2310.01258](https://arxiv.org/pdf/2310.01258) (not read; numbers unverified)
- **Visor (arXiv 2402.03513):** a per-bitrate resolution ladder with client FSRCNN gives 24.65% (PSNR) and 32.70% (VMAF) bitrate reductions versus a standard HLS ladder, for VOD. — [arXiv 2402.03513](https://arxiv.org/abs/2402.03513)
- **Server-sent side information:** NEMO and NeuroScaler reuse codec motion vectors and residuals to propagate anchor-frame SR to non-anchor frames. — [NeuroScaler paper](https://networks.cs.northwestern.edu/CS497-s23/papers/sigcomm22-neural-video.pdf). GameStreamSR splits SR effort by codec reference structure. — [ACM DL](https://dl.acm.org/doi/10.1109/ISCA59077.2024.00097)
- **Palantír (2024):** SR for UHD live streaming. — [arXiv 2408.06152](https://ar5iv.arxiv.org/html/2408.06152) (not read)

### Inferences
- The low-risk pattern for Cha Portal is "standard codec at reduced resolution plus a client upscaler". That keeps WebCodecs hardware decode and RFI, and optional side information can travel out of band (a data channel, or AV1 metadata OBUs and SEI).
- Per-content model deltas (SRVC-style) are viable for games: content is stable within a title, so models can be fetched per game ahead of time, as River does. They need a client inference runtime (WebGPU, Core ML) that this note does not cover.
- Neural codecs (DCVC-RT) lose WebCodecs and hardware decode and RFI semantics. They need about RTX-4090-class compute for 1080p30 deterministic decoding, and 1440p60 is out of reach on client GPUs in 2026. They are not a fit for Apple Silicon browsers.

### Gaps
- No 2026 successor to DCVC-RT with published real-time 1440p or 4K numbers was found.
- No published results on MV or edge-map side information specifically for game streaming at 1440p, and none with an open browser implementation.
- The NEMO and NAS bandwidth numbers were not retrieved.

## 7. Empirical bitrate trade-offs: downscale plus client upscale versus native resolution (5–50 Mbit/s)

### Takeaway
The published evidence says downscaling plus a good upscaler wins at low bits per pixel and loses or ties at higher bitrates. A 2026 VVC study found 1080p plus VSR often matching or beating native 2160p VMAF within a 0.145–16.8 Mbit/s range (12–28% savings, up to 40% on a GPU client). The older LevelUp game study found about 70% less bandwidth for a 3–20% VMAF loss. Encoder-internal resize (libaom or SVT-AV1 dynamic) gains 7–18% BD-rate only in starvation regimes. I found no low-delay study at 1440p or 4K in the 20–50 Mbit/s range, and none on game or desktop content with modern hardware encoders.

### Cited Findings
- **Premkumar, Menon, Herglotz (Frontiers in Signal Processing, 2026-06-24):**
  - Setup: VVenC (RA, 1 s intra period) and VVdeC on Inter-4K 60 fps natural content; encodes at 540p, 1080p and 2160p, displayed at 2160p; 12 rate points from 0.145 to 16.8 Mbit/s.
  - Results: "1080p plus VSR often matches or exceeds native 2160p VMAF at low-to-mid bitrates", with 12–28% bitrate savings (up to 40% on a GPU client).
  - Client cost: on a CPU-only client, learned SR (FSRCNN, ESPCN) raises decode-plus-VSR energy by up to 85% and misses real-time deadlines. On an RTX 5070 client most configurations meet real time, and total energy falls 25–60%.
  - No gaming or screen content.
  — [Frontiers, DOI 10.3389/frsip.2026.1802483](https://www.frontiersin.org/journals/signal-processing/articles/10.3389/frsip.2026.1802483/full)
- **LevelUp (ACM SEC 2020, game livestreaming):**
  - Slides: VMAF gain of 61–88% for 270p+SR and 15–20% for 480p+SR over plain upscaling; "medium bitrate + SR" gives 70% lower bandwidth at a 3–20% VMAF loss versus high bitrate.
  - A different summary describes the same work as SR frames scoring about 3–20% below medium-resolution H.264.
  - The exact codec and per-game numbers were not confirmed.
  — [LevelUp paper (MSR)](https://www.microsoft.com/en-us/research/wp-content/uploads/2020/08/levelup-sec20.pdf); [SEC'20 slides](https://acm-ieee-sec.org/2020/slides/session3/sec20-paper81-presentation_charts.pdf)
- **SVT-AV1 RTC dynamic resize (2026):** −17.7% SSIM / −7.4% PSNR BD-rate (720p talking head) and −8.7% / −3.2% (CIF), at low-bitrate CBR only. — [SVT-AV1 PR #3369](https://github.com/AOMediaCodec/SVT-AV1/pull/3369)
- **AV1 superres:** a gain only at "very low bit rates" in Auto mode. — [SVT-AV1 superres appendix](https://gitlab.com/AOMediaCodec/SVT-AV1/-/blob/master/Docs/Appendix-Super-Resolution.md)
- **Patents (server picks resolution by client upscaler):** two filings describe choosing a lower encode resolution when the client has a strong upscaler and a higher one when it has only Lanczos or bilinear. — [USPTO 11818192](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11818192); [USPTO 12219173](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12219173)
- **Keyframe overhead of resolution switching:** about 1.5 keyframes per minute in H.264 calls from resolution switches alone. — [Meta @Scale 2023](https://atscaleconference.com/av1-improving-rtc-video-quality-at-scale/)

### Inferences
- **Bits-per-pixel arithmetic (my calculation):**
  - 1440p60 is 2560×1440×60 ≈ 221 Mpx/s: 0.045 bpp at 10 Mbit/s, 0.09 bpp at 20, 0.23 bpp at 50.
  - 2160p60 is about 498 Mpx/s: the Frontiers range tops out at 16.8 Mbit/s, about 0.034 bpp.
  - 1080p60 is about 124 Mpx/s, 56% of 1440p's pixel rate and 25% of 4K's.
- **What that means:** the published wins sit at or below about 0.03–0.05 bpp at the display resolution, which for 1440p60 is roughly 7–11 Mbit/s. That points to the 5–15 Mbit/s WAN tier as where streaming 1080p (or 1706×960) into a 1440p window should pay off. At 20–50 Mbit/s, native 1440p is likely better for low-delay AV1 or HEVC, but that is unmeasured for low-delay hardware encoders (inference).
- **Low-delay penalty:** Cha Portal's encoders (no B-frames, about one frame of VBV, hardware presets) are less efficient than VVenC RA or SVT-AV1. A less efficient encoder reaches the "starved" regime at higher bitrates, which probably moves the crossover up (inference).
- **Encode and decode time:** NVENC and VA-API encode time and hardware decode time scale roughly with pixel count, so 1080p cuts per-frame codec latency by about 40% versus 1440p. Client SR adds its own cost; see the client-upscaling notes (inference).

### Gaps
- No published VMAF or PSNR study on low-delay hardware AV1 or HEVC (NVENC, QSV, VCN) of 1080p plus upscale versus native 1440p or 4K at 5–50 Mbit/s for game content.
- An MMSys 2026 "GameLab" paper (SFU) may cover game-streaming quality but was not read. — [mmsys26_GameLab.pdf](https://www.cs.sfu.ca/~mhefeeda/Papers/mmsys26_GameLab.pdf)

## 8. Screen-content specifics: text and UI under downscale and upscale; why desktops need native resolution

### Takeaway
At the bitstream level, AV1's screen-content tools are incompatible with or unused by the resolution-reduction paths. IntraBC is disabled whenever superres is active (spec). Chrome's WebRTC AV1 always disables IntraBC and enables palette only in screenshare mode. Intel Xe2/Xe3 hardware exposes palette and IntraBC but not superres or restoration. AV1 screen-content tools are worth up to 80% BD-BR versus H.264 (Meta), far more than any SR gain. Learned SR for screen content shows only about 20–26% savings in still-image studies. Desktops should therefore stay at native resolution with the screen-content tools, preferably 4:4:4 where decoders allow. Games tolerate downscale plus SR.

### Cited Findings
- **Spec:** `allow_intrabc` is coded only when `allow_screen_content_tools && UpscaledWidth == FrameWidth`, so IntraBC is impossible on superres frames. — [AV1 spec](https://aomediacodec.github.io/av1-spec/)
- **Meta (2023):** AV1 screen content saves "up to 80%" (about one fifth of the bitrate) versus H.264, through palette mode (colour clusters) and intra block copy (repeated patterns). — [@Scale/Meta 2023-11-29](https://atscaleconference.com/av1-improving-rtc-video-quality-at-scale/)
- **libwebrtc AV1:** in `kScreensharing` mode it sets `AV1E_SET_TUNE_CONTENT = AOM_CONTENT_SCREEN`, enables palette and uses speed 11. `AV1E_SET_ENABLE_INTRABC` is always 0. — [webrtc libaom_av1_encoder.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/video_coding/codecs/av1/libaom_av1_encoder.cc)
- **Intel Xe2/Xe3/Xe3P VA-API AV1 encode caps:** palette = 1, intrabc = 1, superres = 0, restoration = 0. — [intel/media-driver caps table](https://raw.githubusercontent.com/intel/media-driver/master/media_softlet/linux/xe2_hpm_r0/encode/av1/ddi/capstable_data_av1_encode_xe2_hpm_r0_specific.h). The media-driver also has an `encode_av1_scc.cpp` feature. — [intel/media-driver](https://github.com/intel/media-driver)
- **Chromium's hardware AV1Decoder** accepts only "YUV 4:2:0 and YUV 4:4:4" chroma formats; actual 4:4:4 decode depends on the platform accelerator. — [Chromium av1_decoder.cc](https://chromium.googlesource.com/chromium/src/+/main/media/gpu/av1_decoder.cc)
- **AV2:** IBC usable with in-loop filters, "Backwards Reference Update … for mostly static content such as screencasting", larger gains on screen content, and a Main_444_10_IP1 profile. — [Norkin AV2 overview](https://norkin.org/research/av2_overview/index.html). Forward Skip Coding gives screen-content gains comparable to palette. — [arXiv 2601.02712](https://arxiv.org/html/2601.02712v1)
- **ICIP 2023, SR for low-bitrate screen content:** downsample, code, then SR, giving 19.77% (SCID) and 26.18% (SIQAD) bitrate reduction versus HEVC-SCC "with similar subjective quality". These are still-image datasets. — [Feature fusion enhanced SR for low bitrate screen content compression](https://www.aminer.org/pub/65681888939a5f4082ee5a95/feature-fusion-enhanced-super-resolution-for-low-bitrate-screen-content-compression)
- **DCC 2021:** SR for compressed screen-content video, with a dataset and a temporal SR network. — [Lingnan Univ. listing](https://scholars.ln.edu.hk/en/publications/super-resolution-for-compressed-screen-content-video/)
- **ICME 2025 Grand Challenge (VSR for video conferencing):** includes a screen-content track (1080p desktop recordings, H.265 at fixed QPs) whose annotators judged text readability at the target resolution. This shows text legibility is now an explicit SR evaluation criterion. — [arXiv 2506.12269](https://arxiv.org/html/2506.12269v2)
- **Older work:** standard codecs handle sharp edges and high frequencies poorly, which hurts text readability; the paper proposes encoder control instead of extra modules. — [PCS 2012 text readability paper (index)](https://search.isc.ac/inventory/49/2279129.htm)

### Inferences
- For Cha Portal's desktop mode, keep native resolution (up to the 2560×1440 cap) and spend bits on palette and IntraBC in the VA-API AV1 path (supported on Xe2/Xe3), and on 4:4:4 where client decoders allow. Use frame-rate reduction or dirty-region skipping instead of spatial downscale under congestion.
- Downscale with reference scaling plus client SR suits game mode, especially below about 15 Mbit/s at 1440p.
- If a single stream mixes UI and game content, resolution choice should follow content classification. Neither AV1 nor AV2 allows per-region resolution within a frame (inference from the spec structure), and AV2's tiles and layers do not change that.

### Gaps
- No video-level study measures text legibility or OCR accuracy against bitrate for downscale-plus-SR versus native screen-content coding with AV1 SCC tools at real-time settings.
- Hardware decoder support for AV1 4:4:4 (High profile) on Apple M3+ and in Chrome WebCodecs was not verified.
