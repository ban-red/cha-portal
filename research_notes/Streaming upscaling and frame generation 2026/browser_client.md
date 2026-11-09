# Browser-client upscaling and frame interpolation on live, low-latency video (state as of 2026-11-09)

Scope: in-browser only (Chrome first, Safari 26 and Firefox second) on decoded live video for Cha Portal's three render paths: (1) WebRTC `<video>`, (2) WebTransport + WebCodecs `VideoDecoder` → `VideoTrackGenerator` → `<video>`, (3) PyroWave WebGPU compute → OffscreenCanvas. Baseline client: MacBook Pro M4 driving a 3024×1964 panel from a stream capped at 2560×1440, at 60/90/120 fps, with a ~5 ms median send→shown budget.

Date and confidence labels: [2026] = source dated or measured in 2026. [older: YYYY] = an older source still in effect. (unverified) = claim I could not confirm from a primary source. "Estimate" = my own arithmetic. No source measures it.

---

## 1. Built-in browser/driver video enhancement on `<video>` (NVIDIA RTX VSR, Intel/AMD, Edge Clarity Boost and Edge VSR, macOS)

### Takeaway
Only Windows has built-in video upscaling. On Chrome/Edge, NVIDIA RTX Video Super Resolution (and Intel VSR behind a feature flag) runs inside Chromium's DirectComposition video-overlay path. It is applied in the D3D11 video-processor blit that already scales the overlay, so it should not add a frame of buffering. The user enables it in the driver, a page can neither turn it on nor detect it, and it is off on battery. I found nothing equivalent for Chrome or Safari on macOS, so the M4 baseline gets no free upscaler. No source confirms whether RTX VSR engages on WebRTC or `VideoTrackGenerator`-backed `<video>`. That has to be tested on Windows hardware.

### Cited Findings
**NVIDIA RTX VSR inside Chromium (Windows)**
- Chromium's `ui/gl/swap_chain_presenter.cc` (main branch, read 2026-11) has `ToggleVpSuperResolution`, which picks a path by vendor:
  - NVIDIA (0x10de) → `ToggleNvidiaVpSuperResolution`. It sets a D3D11 stream extension with `kStreamExtensionMethodSuperResolution` and checks no feature flag here.
  - Intel (0x8086) → `ToggleIntelVpSuperResolution` (Intel VPE "super resolution" scaling mode). It requires `features::kIntelVpSuperResolution`.
  - Any other vendor returns `E_NOTIMPL`. AMD has no code path in this file.
  — [Chromium swap_chain_presenter.cc](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/gl/swap_chain_presenter.cc)
- Gating in the same file:
  - `use_vp_super_resolution = enable_vp_super_resolution_ && !is_on_battery_power_`, so VSR is off on battery.
  - `enable_vp_super_resolution_ = !layer_tree_->disable_vp_super_resolution()`.
  - VSR runs only in `VideoProcessorBlt`. `TryPresentToDecodeSwapChain` (the zero-copy decode-swap-chain path) returns early when `ShouldUseVideoProcessorScaling()` is true (`!is_on_battery_power_ && !disable_vp_scaling`). The decode swap chain path never gets VSR.
  - If the toggle fails, VSR is disabled for that swap chain.
  - The file has no WebRTC, frame-rate or content-type gate other than HDR.
  — [Chromium swap_chain_presenter.cc](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/gl/swap_chain_presenter.cc)
- The same file implements NVIDIA RTX Video HDR (`ToggleNvidiaVpTrueHDR`, "VpAutoHDR"). It is used only for SDR content on an HDR-enabled monitor, not on battery, and only with driver support. — [Chromium swap_chain_presenter.cc](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/gl/swap_chain_presenter.cc)
- A third-party renderer project confirms that Chromium calls the NVIDIA and Intel VSR toggles through the D3D11 video context output/stream extensions. — [Aleksoid1978/VideoRenderer #77](https://github.com/Aleksoid1978/VideoRenderer/issues/77)
- Intel's driver uses a different extension GUID and payload (VPE). Chromium earlier removed concurrent video-processor blits because they hung an Intel HD 620. — [frame-flow #562](https://github.com/charles8051/frame-flow/issues/562)
- User report (unverified): RTX VSR never activates on RTX 50-series in Chromium browsers, blamed on forced AV1 decode. A failed `VideoProcessorSetStreamExtension` HRESULT clears `enable_vp_super_resolution_` until the swap chain is reallocated. — [NVIDIA forum](https://www.nvidia.com/en-us/geforce/forums/game-ready-drivers/13/580594/rtx-video-super-resolution-never-activates-on-rtx-/?topicPage=5%2F)

**RTX VSR product facts**
- Supported in Chrome 110+, Edge 110+, Firefox 126+ and VLC 3.0+ on RTX GPUs. It infers higher-resolution frames from the current low-resolution frame alone, with no temporal input. — [Wikipedia: Video Super Resolution](https://en.wikipedia.org/wiki/Video_Super_Resolution)
- [older: 2023] NVIDIA offers four quality levels in the NVIDIA Control Panel and upscales "up to 4K, matching their display resolution". Levels 3–4 "require more GPU usage". — [NVIDIA blog 2023-02-28](https://blogs.nvidia.com/blog/2023/02/28/rtx-video-super-resolution/); [Windows Central](https://www.windowscentral.com/software-apps/nvidia-makes-videos-sharper-with-more-clarity-thanks-to-its-new-rtx-video-super-resolution)
- A later model update uses up to 30% fewer GPU resources at the highest quality. It also adds an "Auto" intensity mode. — [Wccftech](https://wccftech.com/nvidia-rolls-out-enhanced-rtx-video-super-resolution/)
- Measured cost is reported only as GPU load and power, never as ms per frame:
  - About +10% GPU utilisation on an RTX 3080 at the top tier. — [PC Gamer](https://www.pcgamer.com/nvidia-rtx-vsr-tested-streaming-performance)
  - 20 W / 25% load (quality 1) vs 30 W / 36% (quality 4) on a 4070 Ti with a 480p clip. — [TechSpot forum](https://www.techspot.com/community/topics/nvidia-launches-rtx-video-super-resolution-to-boost-streaming-video-quality.279341)
- Input range of 360p–1440p: stated only by a third-party guide. NVIDIA's own text says only "up to 4K" (unverified). — [UniFab guide](https://unifab.ai/resource/how-to-use-nvidia-rtx-video-super-resolution)
- One aggregated search summary said VSR "doesn't work on streamed video". I could not trace this to a primary source (unverified, possibly about DRM). — [Medium/Geek Culture](https://medium.com/geekculture/nvidia-has-released-the-rtx-video-super-resolution-web-video-upscaler-61be4385e549)

**AMD**
- [older: 2024] AMD "Video Upscale" arrived in Adrenalin 24.1.1 (January 2024):
  - RX 7000 desktop only.
  - DX11 apps including Chrome and Edge, up to 4K, with a sharpness slider.
  - Chrome and Edge need the "Media Foundation for Clear" flag in chrome://flags.
  - A commenter said Chromium had not implemented it at launch (unverified).
  — [Tom's Hardware](https://www.tomshardware.com/pc-components/gpus/amd-video-upscaler-arrives-for-rx-7000-gpu-owners-yearning-for-nvidias-rtx-video); [TechPowerUp](https://techpowerup.com/318217/amd-software-adrenalin-24-1-1-whql-released-with-amd-fluid-motion-frames-support)

**Microsoft Edge Video Super Resolution**
- [older: 2023-03-08] How it works:
  - ML models from Microsoft Research Asia, compressed by DeepSpeed, running on ONNX Runtime + DirectML. A DX12 pipeline converts DX11 video textures to DirectML tensors.
  - Requires RTX 20/30/40 or AMD RX 5700–7800.
  - Video must be below 720p ("480p and below" in the settings text), with both dimensions above 192 px.
  - Not used for DRM content or on battery.
  - 20 ms per 360p frame on an RTX 2060.
  - User-controlled through the address-bar "HD" icon and edge://settings/system. The post mentions no WebRTC/MediaStream support and no page API.
  — [Microsoft Edge blog](https://blogs.windows.com/msedgedev/2023/03/08/video-super-resolution-in-microsoft-edge/)

**Edge Clarity Boost (Xbox Cloud Gaming)**
- [older: 2021–2022] A client-side spatial upscaling/sharpening feature for the Xbox Cloud Gaming stream, Edge only. The user toggles it from the xCloud stream's actions menu. It shipped in Edge 103 alongside an "efficiency mode". Coverage disagrees on whether it is upscaling or sharpening. — [Xbox Wire 2021-11-29](https://news.xbox.com/en-us/2021/11/29/clarity-boost-with-xbox-cloud-gaming-on-edge-browser/); [TechRadar](https://www.techradar.com/news/xbox-cloud-gaming-just-got-a-lovely-upgrade-on-microsoft-edge); [Notebookcheck](https://www.notebookcheck.net/Microsoft-announces-an-increase-in-clarity-with-Xbox-Cloud-Gaming-on-Microsoft-Edge.582673.0.html)

**macOS**
- Every 2026 Mac-targeted video-SR project I found is a third-party WebGPU extension (webgpu-vsr, Crisp/webvsr, Framegen; see §2–§4). — [wyanzhao/webgpu-vsr](https://github.com/wyanzhao/webgpu-vsr)
  - webgpu-vsr calls itself "a WebGPU reconstruction of RTX VSR" for Apple Silicon. That suggests no native equivalent exists in Chrome on macOS (inference from the project's framing).

### Inferences
- **Windows:** RTX VSR is the only "free" option, and it applies only when the stream is smaller than the displayed `<video>` rectangle and the video is promoted to a DComp overlay that takes the VideoProcessorBlt path.
  - A 1440p stream full-screened on a 4K Windows monitor is the right case. Because VSR runs inside the existing overlay blit, it should add GPU time but no extra queued frame.
  - Whether WebRTC or `VideoTrackGenerator` frames reach that path depends on Chromium promoting them to overlays (NV12 hardware frames). No source covers this, so it needs a test: chrome://gpu overlay info plus NVIDIA's VSR status indicator.
- **No page control or detection:** no Web API exposes VSR. A page can only influence it indirectly: send a smaller stream than the element, keep the `<video>` overlay-eligible (no CSS filters/transforms or clip-paths), and expect it off on laptop battery.
- **AMD's path:** it rides the Media Foundation renderer for "clear" (non-DRM) media playback. WebRTC and MediaStream-backed `<video>` do not use that renderer, so AMD Video Upscale most likely does not apply to Cha's paths 1–2 (inference, unverified).
- **Edge VSR:** below 720p only, with 20 ms/frame inference in 2023. It is irrelevant for a 1440p, 5 ms-budget stream.
- **Clarity Boost:** appears tied to xbox.com in Edge, and I found no public API for it (see Gaps).

### Gaps
- No primary source says whether RTX VSR or Intel VSR engages on WebRTC tracks or on `<video>` fed by `MediaStreamTrackGenerator`/`VideoTrackGenerator`, or adds measurable latency there. NVIDIA's RTX Video FAQ returned HTTP 403.
- No ms-per-frame cost of RTX VSR at 1440p→4K is published. Only GPU-load and power anecdotes exist.
- Whether `disable_vp_super_resolution()` is set for any content class (for example low-latency or WebRTC) is not visible from swap_chain_presenter.cc alone. The setter lives elsewhere and I did not trace it.
- How Clarity Boost is exposed to xbox.com (a private Edge API, allow-listed origins) is undocumented. I found no technical description.
- I found no Apple/Safari or Chrome-on-macOS built-in video super resolution. Treat it as absent unless shown otherwise.

---

## 2. WebGPU shader upscalers and sharpeners for live frames (FSR 1, CAS, NIS, SGSR, Anime4K, bicubic/Lanczos, 2025–2026 WebGPU projects)

### Takeaway
Cheap, single-frame spatial shaders are the only upscalers that fit a ~1 ms slice on an M4:
- FSR 1 EASU+RCAS: MIT, with community WGSL ports.
- SGSR 1: BSD-3, single pass.
- NIS: MIT.
- Catmull-Rom/Lanczos plus CAS.

The native FSR 1 cost is 0.4–1.0 ms at 4K output on 2019–2021 desktop GPUs. A 2026 WebGPU measurement on Apple Silicon puts plain Catmull-Rom at 0.26 ms (M5 Max) to 0.72 ms (12-core base chip) at 1080p output. Expect roughly 1–4 ms for EASU+RCAS at 3024×1964 on a base M4 (estimate). Temporal game upscalers (FSR 2+, SGSR 2) need engine motion vectors and depth, which the client lacks.

### Cited Findings
**FSR 1**
- Published per-frame cost (upper bounds, native DX/Vulkan, 2021 drivers) — [AMD GPUOpen FSR 1](https://gpuopen.com/fidelityfx-superresolution/):

  | GPU class | 4K output | 1440p output |
  |---|---|---|
  | RX 6800 XT / RTX 3080 | 0.40 ms | 0.20 ms |
  | RX 6700 XT / RTX 3060 Ti | 0.60 ms | 0.30 ms |
  | RX 5700 XT / RTX 2060 SUPER | 1.0 ms | 0.50 ms |

- FSR 1 is MIT-licensed. It is spatial only (no history or motion vectors), expects an anti-aliased input, and should run after tone mapping in perceptual space. Noise or grain passes go after upscaling. — [AMD GPUOpen FSR 1](https://gpuopen.com/fidelityfx-superresolution/)
- Ports and variants:
  - A WGSL FSR 1 reimplementation (MIT) ships in the `upscaler-for-anime4k` Chrome extension (`fsr.js`, Chrome 113+). — [MtyldZ/upscaler-for-anime4k](https://github.com/MtyldZ/upscaler-for-anime4k)
  - An RCAS WGSL port is used in the "Shade" WebGPU renderer (no timings published). — [three.js forum: Shade](https://discourse.threejs.org/t/shade-webgpu-graphics/66969/175)
  - A mobile-optimised FSR 1 fuses EASU+RCAS into one pass. — [atyuwen gist](https://gist.github.com/atyuwen/78d6e810e6d0f7fd4aa6207d416f2eeb)
  - The mpv/GLSL port offers lossy EASU speedups such as disabling deringing. — [agyild gist](https://gist.github.com/agyild/82219c545228d70c5604f865ce0b0ce5)

**SGSR, NIS, Anime4K**
- Snapdragon Game Super Resolution 1 is a single-pass spatial upscaler under BSD-3-Clause. SGSR 2 is Qualcomm's temporal upscaler. I found no WebGPU port of either. — [SnapdragonStudios/snapdragon-gsr](https://github.com/SnapdragonStudios/snapdragon-gsr); [Neowin on SGSR 2](https://www.neowin.net/news/qualcomm-announces-snapdragon-game-super-resolution-2-with-several-significant-improvements/)
- [older: 2021–2022] The NVIDIA Image Scaling (NIS) SDK is MIT-licensed on GitHub (NVIDIAGameWorks) with HLSL/GLSL shaders that also run on AMD and Intel. I found no WGSL port. — [ecosyste.ms NIS entry](https://awesome.ecosyste.ms/projects/github.com%2Fnvidiagameworks%2Fnvidiaimagescaling); [Phoronix](https://www.phoronix.com/news/NVIDIA-Image-Scaling-SDK-1.0)
- Anime4K on WebGPU:
  - `Anime4K-WebGPU` (npm package) chains a CNN 2× upscaler and a GAN restore pass for drop-in use in a WebGPU pipeline. — [Anime4KWebBoost/Anime4K-WebGPU](https://github.com/Anime4KWebBoost/Anime4K-WebGPU)
  - `anime4k-wgpu` (MIT) is a WGSL port of all Anime4K CNN/GAN shaders (modes A/AA/B/BB/C/CA, presets Light→Extreme). It requires the `float32-filterable` WebGPU feature, with no plan to support GPUs without it. Its web demo needs Chrome 116+, Firefox Nightly or Safari 26+. It publishes no timings. — [SegaraRai/anime4k-wgpu](https://github.com/SegaraRai/anime4k-wgpu)

**Measured WebGPU resampling and SR on Apple Silicon, from webgpu-vsr** — [wyanzhao/webgpu-vsr](https://github.com/wyanzhao/webgpu-vsr)
- [2026, measurements dated 2026-11-01/02] Median GPU compute-pass time, which excludes decode→display latency.
- Catmull-Rom bicubic ("Quality 0"), any source, 1080p output:
  - 0.255 ms (p95 0.256) on an "Apple M5 Max 40-core GPU" in Playwright Chromium 156.
  - 0.718–0.720 ms on an "Apple M6 base 12-core GPU" Mac mini in Chrome 156.
- Frames are pulled via `requestVideoFrameCallback` → `VideoFrame` → `importExternalTexture`. One compute pass records all dispatches. There are no CPU readbacks, and the canvas sits under the `<video>`.
- The author notes "paced playback can show higher GPU times as the GPU clock drops." This matches Cha's observation that the Apple GPU drops to low clocks at a 60 fps duty cycle.
- MIT code. The weights are byte-identical NVIDIA RTX Video SDK parameters, for personal use only and not redistributable.

**Other 2025–2026 WebGPU-native projects**
- WebSR (MIT):
  - Hand-written WebGPU compute CNNs (Anime4K-derived `cnn-2x-s/m/l`, retrained on real-life/3D content), plus an optional MobileNet v3 content classifier. It supports OffscreenCanvas/worker.
  - The README says it is "very new… shouldn't be used (yet) in production".
  — [sb2702/websr](https://github.com/sb2702/websr)
- Crisp/webvsr Chrome extension:
  - SPAN-Lite 2× (16-channel, reparameterised 3×3 convs), a lite 8,956-parameter model, then Catmull-Rom to display size, then adaptive sharpening.
  - Zero-copy `importExternalTexture` from `<video>`.
  - A frame-time governor falls back full model → lite → sharpen only → original.
  — [fishy-ops/webvsr](https://github.com/fishy-ops/webvsr)

### Inferences
**Cost estimates at Cha's output size (3024×1964 ≈ 5.9 MP, about 72% of 4K)**

| Option | Licence | Est. cost, base M4 | Est. cost, M4 Pro | Est. cost, RTX 3060 Ti-class | Text | Games |
|---|---|---|---|---|---|---|
| Bilinear (today, compositor) | n/a | 0 (compositor) | 0 | 0 | soft | soft |
| Catmull-Rom / Lanczos-2 | n/a | ~1–2 ms (from 0.72 ms at 1080p on a 12-core chip ×2.9 pixels) | ~0.5–1 ms | <0.5 ms | slightly sharper, mild ringing | OK |
| + CAS/RCAS sharpen | MIT | +0.3–1 ms | +0.2–0.5 ms | +0.1–0.2 ms | risk of halos on text | good |
| FSR 1 EASU+RCAS | MIT | ~1–4 ms | ~0.5–2 ms | ~0.45 ms (0.6 ms × 0.72) | edge-adaptive but tuned for AA'd 3D; can ring on 1-px glyphs | good |
| SGSR 1 (1 pass) | BSD-3 | likely ≤ FSR 1 (unverified) | — | — | similar | good |
| NIS | MIT | similar to FSR 1 (unverified) | — | — | similar | good |
| Anime4K CNN (M/L) | MIT | several ms to tens of ms (no measurements found) | — | — | strong line art; odd on UI text (unverified) | stylised |

- **How the estimates were made:** they scale published native numbers and webgpu-vsr's Apple measurements by pixel count. They assume the base M4 sits near the "12-core base chip" class. Low-clock behaviour at 60 fps could push the costs to the top of each range.
- **Path 3 (PyroWave):**
  - FSR 1, SGSR 1 or Catmull-Rom+RCAS is one more render/compute pass on a GPUTexture that is already resident. Size the OffscreenCanvas to the element's device pixels (3024×1964) so the compositor does no second bilinear scale.
  - The upscale could be fused into PyroWave's last inverse-wavelet/colour-conversion pass to avoid writing and re-reading the full-res intermediate.
  - Expect +1–3 ms on a base M4 (estimate). That fits only if the PyroWave decode sits at the low end of its 2–6 ms range.
- **Path 2 (WebTransport/WebCodecs):** the same shaders apply to `importExternalTexture(VideoFrame)`. The catch is the presentation path, not the shader (see §5).
- **Path 1 (WebRTC):** shaders need the frames first: `importExternalTexture(HTMLVideoElement)` on rVFC, or `MediaStreamTrackProcessor` → `VideoFrame`. Either way, display moves from the fast `<video>` overlay to a canvas.
- **Content type:**
  - For desktop text at a non-integer 1.36× scale, no spatial upscaler creates missing glyph detail, and sharpeners add halos. Raising the 1440p cap (codec-level) beats any client shader.
  - For games, FSR 1 or SGSR 1 plus mild RCAS is the standard choice.
  - A practical policy is per-session: sharpen-only or bicubic for desktops, EASU+RCAS for games.
- **Temporal shaders:** FSR 2/3 and SGSR 2 need per-pixel motion vectors and depth from the engine. The browser client has neither, and codec motion vectors are not exposed (§4). Temporal shader upscalers are not usable client-side unless the host sends motion vectors as side data.

### Gaps
- I found no published ms-per-frame figure for any WGSL FSR 1, CAS, NIS or SGSR port on Apple Silicon or Windows in a browser. All shader costs above are estimates.
- No Anime4K-WebGPU timings were found at 1440p or 4K.
- I did not check the licences of the `upscaler-for-anime4k` and Anime4K-WebGPU repos in detail, apart from the FSR code's stated MIT origin.
- webgpu-vsr labels its hardware "Apple M5 Max 40-core", "Apple M6 base 12-core" and "macOS 27.2/27.0.1" with "Chrome 156". I could not independently verify that these chips and OS builds were publicly available on 2026-11-01, so treat the hardware labels with caution even though the numbers look internally consistent.

---

## 3. ML super-resolution in the browser (WebNN, ONNX Runtime Web, LiteRT.js/TF.js, fastest real-time SR models)

### Takeaway
WebNN is not shipped anywhere as of November 2026:
- Chrome's origin trial (M147–149) was pulled on 2026-03-27 and expired on 2026-08-11.
- Stable builds keep it off by default (M155 and main), and a new trial is pencilled in for "Chrome 160 (TBD)".
- I found no Safari WebNN status.

So browser ML SR today means hand-written WGSL or ORT-Web/LiteRT.js on WebGPU. Measured 2026 numbers on Apple Silicon:
- The smallest useful networks reach about 8 ms per 720p frame on an M4 MacBook Pro.
- An RTX-VSR-like "low" network takes 6.7 ms for 1080p→4K on a 40-core M5 Max GPU.

At 1440p input that is about 12 ms on a 40-core part and roughly 30 ms or more on a base M4 (estimate). ML SR does not fit Cha's 5 ms budget at 60–120 fps on the M4 baseline. It is plausible only on fast Windows dGPUs or at lower stream resolutions.

### Cited Findings
**WebNN status**
- [2026-10-20] Chrome status:
  - Chromestatus feature 5176273954144256 is "Proposed", behind flag `web-machine-learning-neural-network`.
  - The origin trial (Chrome 147→149) is "Closed — Complete", ended 2026-08-11. It was re-enabled for M147 on 2026-03-10 and disabled 2026-03-27 "because more release-blocking issues were reported" (CL 7702513).
  - `kWebMachineLearningNeuralNetwork` is `default_state = false` on the M155 branch and main.
  - The Edge OT also expired on 2026-08-11.
  - webnn.io lists a new OT as "Chrome 160 (TBD) to 164 (TBD)".
  — [webmachinelearning/webnn #964](https://github.com/webmachinelearning/webnn/issues/964)
- The original OT was announced for Chrome 146 beta and excluded Android. — [Phoronix: Chrome 146 beta](https://www.phoronix.com/news/Chrome-146-Beta); [blink-dev Intent to Experiment: WebNN](https://groups.google.com/a/chromium.org/g/blink-dev/c/5CWKSChYo98)
- webnn.io tells developers to use Canary plus the flag. — [webnn.io installation](https://webnn.io/en/learn/get-started/installation)
- [2026-10-26] TPAC 2026 WebML WG agenda — [webmachinelearning/meetings #39](https://github.com/webmachinelearning/meetings/issues/39):
  - Backend status for LiteRT, Windows ML and Core ML is an open item. The baseline is the "WebNN API 22 January 2026 CR Snapshot" plus Chrome/Edge Developer Trials.
  - "Firefox implementation hits a key milestone" (no details).
  - WebGPU interop: initial PR #942, `interopDevice` for zero-copy (#941), exported `GPUBuffer` usage flags (#954). Real-time workloads (#898) and latency/compute-policy hints (#934) are under discussion.
  - No Safari or WebKit status was given.

**Runtimes**
- [2026-08-18] Google announced LiteRT.js, a JS binding for LiteRT.
  - Backends: CPU (XNNPACK), WebGPU, and "WebNN (via Apple Core ML)".
  - Claims: up to 3× faster than an existing web runtime on an M4 MacBook Pro, and 5–60× over CPU for GPU/NPU real-time use. Real-ESRGAN upscaling is among the demos.
  — [@IT (ITmedia), 2026-08-18](https://atmarkit.itmedia.co.jp/ait/spv/2608/18/news049_2.html)
- [2026-10] Single-user run of ONNX Runtime Web (WebGPU EP) image upscaling on an M5 Max: ~154 ms/frame for an "AnimeVideo" model and 271–308 ms for "General" (Real-ESRGAN-class), versus 1.64 s on WASM CPU. Neither the browser nor the ORT version is stated. — [ComfyICU node page](https://comfy.icu/node/GeneretiCoreBrowserUpscale)
- WebGPU dispatch overhead: 32–71 µs per dispatch on Metal and 24–36 µs on Vulkan for API overhead alone. Implementation choice varies it by 2.2× on Metal. The data come from an LLM batch-1 study across Chrome, Safari and Firefox. Note the paper is withdrawn and superseded by arXiv:2608.08730. — [arXiv 2604.02344](https://arxiv.org/abs/2604.02344)

**Measured browser SR on Apple Silicon and others**
- WebSR / free.upscaler.video, WebGPU, 720p input, 2× (article dated 2025-01-20, with metrics as of January 2026) — [S. Bhattacharyya, "Efficient, practical super resolution in the browser"](https://free.upscaler.video/technical/architecture/):

  | Model | Size | M4 MacBook Pro | Other hardware |
  |---|---|---|---|
  | Small | 8 ch, 7 layers, ~10k params | 120 FPS | 40–60 FPS on mid-range iGPUs; 40 FPS on a $200 Chromebook; 15–30 FPS on mobile |
  | Medium | 16 ch, ~30k params | 40 FPS | — |
  | Large | 28 ch, ~100k params | 15 FPS | — |

  - WebGPU ran 10–30% faster than the WebGL versions.
  - Design lessons: "buffer reads and writes, not FLOPs, dominate runtime"; use channel counts that are multiples of 4 and CReLU; pack weights into mat4 uniforms by hand.
- webgpu-vsr (RTX-VSR-like CNN in WGSL, FP16 weights with f32 accumulation) — [wyanzhao/webgpu-vsr](https://github.com/wyanzhao/webgpu-vsr):

  | Machine | Model | Source → output | Median GPU time |
  |---|---|---|---|
  | M5 Max 40-core, Chromium 156 | low | 960×540 → 1080p | 2.15 ms |
  | M5 Max | medium | 960×540 → 1080p | 4.85 ms |
  | M5 Max | low | 720p → 1080p | 3.41 ms |
  | M5 Max | medium | 720p → 1080p | 7.97 ms |
  | M5 Max | low | 1080p → 4K | 6.69 ms |
  | M5 Max | medium | 1080p → 4K | 17.23 ms |
  | M5 Max | low | 720p → 1440p | 3.34 ms |
  | M5 Max | medium | 720p → 1440p | 7.86 ms |
  | M6 base 12-core, Chrome 156 | low | 540p → 1080p | 5.05 ms |
  | M6 base | medium | 540p → 1080p | 13.13 ms |
  | M6 base | low | 720p → 1080p | 8.70 ms |
  | M6 base | medium | 720p → 1080p | 22.30 ms |

  - On the base chip, medium is recommended only for 30 fps content, and low sits near the deadline threshold at 720p60.
  - A deadline controller steps medium → low → bicubic → native on sustained late frames, with no automatic upgrade.
- Crisp (webvsr): 16-channel SPAN-Lite 2× takes about 22 ms (~46 fps) for 720p→1440p on an RTX 2070 SUPER. No Apple numbers. — [fishy-ops/webvsr](https://github.com/fishy-ops/webvsr)

**Efficient and real-time SR challenge baselines**
- [2026] NTIRE 2026 Efficient SR (×4) — [arXiv 2604.03198](https://arxiv.org/abs/2604.03198); [NTIRE2026_ESR repo](https://github.com/Amazingren/NTIRE2026_ESR):
  - 95 registrants and 15 valid teams, targeting PSNR ≥26.90 dB (DIV2K_LSDIR valid) while cutting runtime, params or FLOPs versus the SPAN baseline.
  - SPAN baseline: 0.151 M params, 9.83 GFLOPs at 256×256 input, 41.68 M activations, 7.74 ms average on an RTX A6000.
- [2026] NTIRE 2026 also ran a first "Mobile Real-World Image SR" challenge (not examined in detail). — [arXiv 2604.17306](https://arxiv.org/pdf/2604.17306)
- [older: 2024] AIS 2024 Real-Time 4K SR of compressed AVIF images: 540p→4K (×4). The report says all proposed methods upsample under 8 ms (another mirror says under 10 ms) on commercial GPUs. 160 participants, 25 submitted. — [arXiv 2404.16484](https://arxiv.org/html/2404.16484v1)
- [older: 2025] Mobile AI 2025 quantized SR on mobile NPUs: winner AntSR, Full-HD reconstruction under 50 ms. — [CVPRW 2025 MAI report](https://openaccess.thecvf.com/content/CVPR2025W/MAI/papers/Ignatov_Quantized_Image_Super-Resolution_on_Mobile_NPUs_Mobile_AI_2025_Challenge_CVPRW_2025_paper.pdf)
- [older: 2021] MAI 2021 real-time video SR on smartphones: the best entries upscale to HD at up to 80 FPS. — [arXiv 2105.08826](https://arxiv.org/pdf/2105.08826)

### Inferences
**Compute budget at 1440p input (estimates)**
- SPAN's ×4 cost is about 150 kFLOPs per input pixel (9.83 GFLOPs / 65,536 px). That would be about 550 GFLOPs per 1440p frame and about 33 TFLOP/s at 60 fps, far beyond any M4 GPU. NTIRE "efficient" winners are about 2× cheaper at best, so challenge-grade models are out of reach.
- The WebSR small net (~10k MACs/px) is about 75 GFLOPs per 1440p frame. That matches its measured 720p performance scaling to roughly 30 ms per 1440p frame on an M4 MacBook Pro (4× the pixels of its 120 FPS@720p result).

**Scaling webgpu-vsr "low" to Cha's input size**
- Its cost tracks input pixels: 2.07 MP at 1080p input → 6.7 ms on M5 Max.
- A 1440p input (3.69 MP) is then about 12 ms on a 40-core GPU.
- Using the repo's own base-chip/Max ratio (~2.5× on the low model), that is about 30 ms on a base-class Apple GPU, before the extra downsample from 2× (5120×2880) to 3024×1964.
- This is 2–6× over Cha's entire latency budget. ML SR is only viable on the Mac if the stream is sent at about 720p–1080p and upscaled, which trades bandwidth for GPU time and is the opposite of Cha's priority.

**Windows dGPUs**
- Scaling Crisp's 22 ms@720p on a 2070S gives about 90 ms at 1440p. Even assuming a 3–4× faster RTX 4080/4090, a 16-channel SPAN-Lite at 1440p input is about 20–30 ms.
- A webgpu-vsr-"low"-class net would be several ms on a high-end dGPU. Feasible at 60 fps only with a 1080p-or-lower source (estimate).

**Runtimes and dispatch overhead**
- WebNN with a Core ML/ANE backend (the only route to the M4 Neural Engine from a page) is not available to users. LiteRT.js's "WebNN via Core ML" backend therefore inherits the flag gate (inference).
- ORT-Web/LiteRT.js on WebGPU add graph and dispatch overhead, which hand-written fused WGSL avoids. WebSR's and webgpu-vsr's results suggest hand-written WGSL is what reaches real time.
- At ~30–70 µs per dispatch on Metal (LLM-context figure), a 7–10-layer CNN costs well under 1 ms in dispatch overhead. Multi-pass Anime4K-L/UL chains (tens of passes) could add 1–2 ms.

**Hooks per path**
- Path 3: an SR network would be compute passes after PyroWave's decode.
- Path 2: SR runs on `importExternalTexture(VideoFrame)`.
- Path 1: needs rVFC/`importExternalTexture(video)` or `MediaStreamTrackProcessor`.
- All three end in a WebGPU canvas present (§5).

### Gaps
- No measured ML-SR number exists at 1440p input or 3K/4K output in a browser on an M4 or M4 Pro specifically. The figures above are extrapolations.
- The AIS 2024 report's runtime hardware and per-team numbers were not retrieved.
- NTIRE 2026 per-team runtimes and the fastest entry were not retrieved (`results.json` not opened).
- No Safari WebNN status and no evidence of WebNN in WebKit as of 2026-11.
- The LiteRT.js browser support matrix and its SR benchmarks were not found. I saw only the press summary.
- No source covers WebNN↔WebGPU zero-copy (`interopDevice`, `MLTensor`) performance for per-frame video use.

---

## 4. Temporal approaches without game motion vectors: decoded-video optical flow, codec motion vectors, browser frame interpolation (RIFE/IFRNet/FILM on WebGPU)

### Takeaway
WebCodecs does not expose codec motion vectors. The VideoFrame Metadata Registry needs a W3C spec per entry, and I found no motion-vector proposal. Browser frame interpolation does exist in 2026:
- A distilled RIFE-family WGSL runtime (Framegen) reports about 2 ms per generated 720p frame on an RTX 4060 Ti.
- RIFE 4.x through ORT-Web is used for offline slow-motion.

Interpolation needs the next real frame, which adds at least one source-frame interval (8.3 ms at 120 fps, 16.7 ms at 60 fps). That alone breaks a 5 ms budget. Extrapolation and other "no-wait" schemes are unproven in the browser.

### Cited Findings
- **Codec motion vectors:**
  - The registry enumerates the fields allowed in `VideoFrameMetadata`. Each new entry must be announced in a WebCodecs GitHub issue and defined by a W3C spec that reached consensus. — [W3C VideoFrame Metadata Registry](https://w3c.github.io/webcodecs/video_frame_metadata_registry.html)
  - Precedent: in May 2024 the Media WG ran a call for consensus to register Human Face Segmentation (from the WebRTC WG). — [public-media-wg 2024-05](https://lists.w3.org/Archives/Public/public-media-wg/2024May/0003.html)
  - [2026] Chrome 145 added `VideoFrame.metadata()`. An MDN PR documenting it was open as of March 2026. — [mdn/content PR #43506 (mirror)](https://gh.r-mj.com/mdn/content/pull/43506)
  - My searches turned up no proposal to expose decoder motion vectors (see Gaps).
- **Framegen** (Chrome/Edge 121+ on Windows, macOS Apple Silicon and Linux; Firefox 141+ on Windows) — [NimaJafariComp/Framegen](https://github.com/NimaJafariComp/Framegen):
  - [2026] Model: the FlyHighFG model (0.78 M params, 2.9 MB) on a hand-written WGSL runtime over raw WebGPU, with no ML framework.
  - Speed: "2×–6× any `<video>` at 3–4 ms per frame". The author reports 2.0 ms per generated frame at 720p on an RTX 4060 Ti, and "about a millisecond on a mid-range GPU". No Apple numbers.
  - A Pacer picks what to show on each display tick.
  - Options: optional 2× neural upscale, "static protection" (copies unchanged pixels such as subtitles or logos from the source), and Auto/motion-adaptive modes that back off under load or in fast scenes. Its "anime cadence smoothing" needs one source frame of lookahead.
  - Licences: code MIT, FlyHighFG weights CC BY-NC 4.0.
- **RIFE on ORT-Web:**
  - RIFE v4.9 ONNX export (MIT) runs client-side on the WASM or WebGPU EP. — [edgetools/rife](https://huggingface.co/edgetools/rife)
  - `webtween` uses RIFE 4.25 on WebGPU for 1–16× slow motion, streaming two frames at a time. — [bvdmitri/webtween](https://github.com/bvdmitri/webtween)
  - cacheflowe/media-converter runs RIFE 4.9 through ORT-Web WebGPU with WebCodecs/MediaBunny decode. These are offline or transcode tools, not live. — [cacheflowe/media-converter](https://github.com/cacheflowe/media-converter)
  - No independent ORT-Web RIFE benchmark at real-time frame rates was found (search summary).
- **Crisp/webvsr:** interpolation ("motion smoothing") is not implemented. It is the "big" roadmap item, with RIFE-lite training scripts present. — [fishy-ops/webvsr](https://github.com/fishy-ops/webvsr)
- **Edge VSR and RTX VSR are spatial only**, inferring from the current frame alone. — [Wikipedia: VSR](https://en.wikipedia.org/wiki/Video_Super_Resolution)

### Inferences
**Latency and cost**
- Interpolating 60→120 fps means holding frame N until frame N+1 arrives. Added latency is at least 16.7 ms at a 60 fps source, plus the inference time. A 120 fps source needs no interpolation on a 120 Hz ProMotion panel anyway.
- Interpolation is therefore only defensible for low-bandwidth or 30–60 fps sessions where smoothness matters more than latency. It does not fit Cha's LAN profile.
- Framegen's 2 ms@720p on a 4060 Ti suggests about 6–8 ms at 1440p on that GPU if cost scales with pixels; flow networks often run at reduced internal resolution, so it could be less. On a base M4, expect several times that (estimate). It is not real-time at 120 fps on the Mac baseline.

**Motion data for temporal reconstruction**
- Codec motion vectors are not available, so per-pixel motion would have to come from:
  - (a) a client optical-flow pass (cost close to an interpolation network), or
  - (b) host-side side data: game-engine or encoder motion vectors sent alongside the stream, for example in a WebTransport datagram or a PyroWave side channel. That is the only cheap route to FSR 2/SGSR 2-style temporal upscaling in the client, and it is a protocol change.

**Static protection**
- Framegen's "static protection" (copy unchanged pixels from the source) is the right idea for desktops: text and UI should never be synthesised. Any interpolation or extrapolation mode for Cha would need this, or a damage-rect mask from the host.

### Gaps
- I found no evidence of a W3C/WebCodecs proposal to expose decoder motion vectors or macroblock data, and no browser that exposes them.
- No browser-side real-time frame interpolation has been measured on Apple Silicon. Framegen gives no Apple numbers, and its timings are self-reported.
- I found no in-browser frame extrapolation (no-lookahead, ASW-style) implementation or measurement.
- IFRNet and FILM WebGPU ports: none found.

---

## 5. Getting a hardware-decoded `VideoFrame` into WebGPU (Chrome/macOS, Safari 26), presentation cost, and keeping the fast `<video>` path while upscaling

### Takeaway
- `importExternalTexture` accepts `VideoFrame` (Chrome since 116, Safari 26, Firefox 147 on Apple Silicon). The spec only promises it is *potentially* zero-copy. Import throughput is not the bottleneck: one M4 MacBook Pro measurement shows about 1230 fps on that path.
- Cha's extra 4–5 ms on the WebGPU path is the canvas→compositor present path, not the import. `desynchronized`/low-latency canvas is still unavailable on macOS, and a Chromium engineer argued against it in 2025.
- The only way to upscale and keep the `<video>` presenter is WebGPU → OffscreenCanvas → `new VideoFrame(canvas)` → `VideoTrackGenerator` → `<video>`. It is spec-intended to be cheap, but Chromium waits on GPU work and WebKit had a CPU-cost bug as of October 2025. It is unmeasured for latency and must be prototyped.

### Cited Findings
**Import APIs**
- [older: 2023] The initial WebGPU spec could not create `GPUExternalTexture` from WebCodecs `VideoFrame`. Chrome 116 added `VideoFrame` as a source for `importExternalTexture` and `copyExternalImageToTexture`. External textures sample "potentially in a 0-copy way directly from the source YUV". — [Chrome: What's New in WebGPU 116](https://developer.chrome.com/blog/new-in-webgpu-116)
- The spec is "potentially zero-copy". An implementation may copy depending on its internals. — [vercel-labs/vgpu #12](https://github.com/vercel-labs/vgpu/issues/12)
- External-texture lifetime: an `HTMLVideoElement`-sourced texture expires in a task after import, and a `VideoFrame`-sourced one when the frame is closed. — [gpuweb #2124](https://github.com/gpuweb/gpuweb/issues/2124)
- [2025–2026] Chrome on an M4 MacBook Pro renders about 1230 fps via `importExternalTexture`, against about 1120 fps via `ImageBitmapRenderingContext` (single-blog measurement). — [WebCodecs Fundamentals: Rendering](https://webcodecsfundamentals.org/basics/rendering/)

**Safari and Firefox support**
- WebGPU ships in Safari 26 on macOS, iOS, iPadOS and visionOS (WWDC25). — [WebKit WWDC25 post (mirror)](https://discuss.privacyguides.net/t/news-from-wwdc25-webkit-in-safari-26-beta/28226)
- Safari 26.1 fixed "video playback using the WebGPU renderer in WebCodecs could display a black screen" and "WebGPU video textures failed to load in Three.js panoramas". — [WebKit Features for Safari 26.1](https://webkit.org/blog/17541/webkit-features-for-safari-26-1/)
- [2026] WebKit's `importExternalTexture` misreads 10-bit hardware-decoded frames as 8-bit. One project copies those frames instead of importing them. — [Silo-Server PR #2544](https://github.com/Silo-Server/silo-server/pull/2544)
- [2026-01-13] Firefox 147 enabled WebGPU on Apple Silicon Macs on all supported macOS versions. Earlier: Firefox 141 on Windows (July 2025) and 145 on macOS (partial). Intel Macs are not yet covered. — [AlternativeTo](https://alternativeto.net/news/2026/1/firefox-147-brings-apple-silicon-webgpu-support-improved-video-playback-and-pip-and-more); [The Register](https://www.theregister.com/2026/01/14/firefox_and_tbird_147/)

**Low-latency canvas on macOS**
- [older: 2019–2021] `desynchronized` is a hint the UA may ignore. Apps must read back the context attributes. — [Chrome blog: desynchronized](https://developer.chrome.google.cn/blog/desynchronized); [blink-dev Intent to Ship lowLatency](https://groups.google.com/a/chromium.org/g/blink-dev/c/nxjWgMIeC1Q/m/GfwjbzeVAwAJ)
- Support for the WebGL variant: Chrome 81+ and Edge 79+. Not supported in Firefox or Safari. — [web-features explorer](https://web-platform-dx.github.io/web-features-explorer/features/webgl-desynchronized/)
- [2025-02-26/27] A Chromium graphics-dev proposal to enable canvas low-latency mode on Mac — [graphics-dev thread](https://groups.google.com/a/chromium.org/g/graphics-dev/c/20qDm3ZD2f8):
  - Blockers: a Metal `IOSurfaceImageBacking` cannot do concurrent read/write, which single-buffer mode needs. Because most macOS frames are composited through overlays, reusing one IOSurface can stop the UI from refreshing.
  - Ken Russell recommended against pursuing `desynchronized` on macOS (tearing; the mode was built for ChromeOS pen input).
  - The thread ended unresolved. WebGPU and video were not discussed.
- Chromium issue 41449024 ("Enable Mac LowLatency (aka desynchronized) 2d/WebGL canvas") is P3, Available, tagged "Rendering Core 2026 Fixit", with no resolution seen. — [crbug 41449024](https://issues.chromium.org/issues/41449024)

**WebGPU output back into `<video>`**
- W3C WebCodecs #83: going through OffscreenCanvas should have "no significant memory or performance cost" because canvas and GPUTexture share memory. Direct GPUTexture→VideoFrame interop was postponed in favour of the canvas path. — [w3c/webcodecs #83](https://github.com/w3c/webcodecs/issues/83)
- In Chromium the `VideoFrame` constructor reads the canvas's GPU output and waits for processing if needed. One demo measured about 1 ms of GPU processing per frame. — [tidoust/media-tests](https://www.github.com/tidoust/media-tests)
- [2025-10] WebKit bug 300579: constructing a `VideoFrame` from an OffscreenCanvas costs far more CPU than from a regular canvas in a demo feeding `VideoTrackGenerator`. A cheaper constructor was proposed. A related bug tracks the same path. — [WebKit bug 300579](https://bugs.webkit.org/show_bug.cgi?id=300579); [WebKit bug 267291](https://www2.webkit.org/show_bug.cgi?id=267291)
- `VideoTrackGenerator` is the spec name. `MediaStreamTrackGenerator` is Chromium's older, non-standard name. — [OpenReplay](https://blog.openreplay.com/real-time-video-processing-webcodecs-api/)

**rVFC-driven capture**
- webgpu-vsr and Crisp both use `requestVideoFrameCallback` + `importExternalTexture` from a `<video>`, drawing to a canvas placed under or over the video. — [wyanzhao/webgpu-vsr](https://github.com/wyanzhao/webgpu-vsr); [fishy-ops/webvsr](https://github.com/fishy-ops/webvsr)

### Inferences
- **Root cause of the 4–5 ms gap:**
  - Import is cheap: ~1230 fps on M4 means under 1 ms per frame including the draw.
  - The likely penalty is that a WebGPU canvas goes through Chrome's canvas→viz compositor→CoreAnimation path and is latched at the next BeginFrame. `<video>` with `VideoTrackGenerator` can be promoted straight to a CALayer overlay.
  - Without a macOS low-latency canvas mode (blocked as of 2025–2026), any upscaled WebGPU-canvas path on Chrome/macOS probably inherits that cost, before the shader's own 0.5–4 ms.
- **Option A: keep `<video>` and upscale.** Worker: `VideoDecoder` → `importExternalTexture(frame)` → EASU/RCAS into an OffscreenCanvas sized to device pixels → `new VideoFrame(offscreenCanvas, {timestamp})` → `VideoTrackGenerator.writable` → `<video>` (now 1:1, no compositor scaling).
  - Cost: a GPU-sync wait inside the `VideoFrame` constructor plus a possible copy, plus a 5.9 MP frame through the media pipeline instead of 3.7 MP.
  - It may preserve most of the 2.7–3.4 ms present path. Prototype it on Chrome and Safari (WebKit bug 300579) before committing. This is the most promising "upscale but keep the fast presenter" route.
- **Option B: canvas present (today's measured 7.3–8.9 ms).** Add the shader cost on top. That costs about 5–7 ms over the `<video>` path, beyond the 5 ms budget.
- **Option C: driver VSR (Windows/NVIDIA only).** Keeps the `<video>` path untouched (§1).
- **Option D (PyroWave):** it already pays the canvas present cost, so a fused upscale pass is the cheapest marginal addition there.
- **Path 1 (WebRTC):** shader upscaling means rVFC → `importExternalTexture(videoElement)` → a canvas over the hidden or under-laid `<video>`. rVFC fires after the frame is handed to the compositor, so the upscaled copy likely appears at least one vsync after the raw frame would have (inference). An alternative is `MediaStreamTrackProcessor` on the receive track → VideoFrames in a worker → Option A, at the cost of extra queueing.
- **HDR/10-bit:** on Safari, 10-bit sources may need a `copyExternalImageToTexture` copy instead of external import (Silo PR). For an SDR 8-bit stream this does not matter.

### Gaps
- No public 2026 measurement of WebGPU canvas present latency vs `<video>` on Chrome/macOS or Safari, besides Cha's own 7.3–8.9 ms vs 2.7–3.4 ms.
- No measurement of the OffscreenCanvas → `VideoFrame` → `VideoTrackGenerator` round-trip latency in Chrome 2026. It is the key unknown for Option A.
- Whether Chrome's macOS `importExternalTexture` from a VideoToolbox-decoded `VideoFrame` is truly zero-copy (multi-planar IOSurface) in 2026 is not confirmed by a primary source.
- A search summary claimed that Chrome 133 made external textures use the `VideoFrame`'s display size (unverified; primary Chrome 133 blog not fetched).
- I found no new 2026 API for direct `GPUTexture` → `VideoFrame`, or for WebGPU "present at next vsync" or presentation feedback.
- Whether Safari 26 supports `VideoTrackGenerator` in a worker and `MediaStreamTrackProcessor` was not verified in this pass.

---

## 6. Frame pacing and presentation for an upscaled path at 60/120 Hz (ProMotion); does the extra pass force an extra frame of latency?

### Takeaway
An extra pass costs a whole display frame (8.3 ms at 120 Hz) only when it pushes the GPU work past the compositor's latch deadline for that vsync. The pass itself does not inherently add buffering. On Apple Silicon at a 60 fps duty cycle, GPU clocks drop and stretch every pass, so a 1–2 ms shader can measure 2–4 ms. Existing WebGPU video-SR projects all use deadline-based controllers that step down quality on late frames. Cha should do the same, and should treat the canvas present path's own extra vsync as the larger risk.

### Cited Findings
- [2026] webgpu-vsr — [wyanzhao/webgpu-vsr](https://github.com/wyanzhao/webgpu-vsr):
  - "Paced playback can show higher GPU times as the GPU clock drops."
  - A deadline controller steps medium → low → bicubic → native on sustained late frames, watchdog timeouts or a full frame ring. GPU mean time alone does not trigger a downgrade, and it never upgrades automatically.
  - On a base 12-core chip, 720p60 with the low model "sits near the controller threshold".
- [2026] Crisp/webvsr: a "frame-time governor" keeps the model within the video's frame budget and passes the original video through when it can't keep up. Step-downs are based on frames actually delivered. — [fishy-ops/webvsr](https://github.com/fishy-ops/webvsr)
- [2026] Framegen: a "Pacer" decides what to show on each display tick, with Match-display, Custom-FPS and fixed 2×–6× modes. Auto/motion-adaptive modes reduce generation based on GPU headroom. — [NimaJafariComp/Framegen](https://github.com/NimaJafariComp/Framegen)
- [older: 2021, opinion] HN commenter: Chrome canvas rendering is fast but "latency is high: Chrome has 1 to 2 frames" of compositor latency. `desynchronized` canvas avoids it on ChromeOS and Windows only. — [Hacker News](https://news.ycombinator.com/item?id=28783719); [Hacker News](https://news.ycombinator.com/item?id=28785069)
- macOS low-latency canvas remains blocked by IOSurface/overlay constraints. A Chromium engineer recommended against it in February 2025, and the bug was still open in its 2026 fixit tag. — [graphics-dev thread](https://groups.google.com/a/chromium.org/g/graphics-dev/c/20qDm3ZD2f8); [crbug 41449024](https://issues.chromium.org/issues/41449024)
- [2026] Native-app contrast (out of browser scope, for calibration): near-zero input-to-display latency on macOS needs a CAMetalLayer with 2 drawables, `presentsWithTransaction` and rendering as late as possible. The browser exposes none of these controls. — [Daniel Hooper on X](https://x.com/DanielcHooper/status/2067320915545686158)

### Inferences
**Budget arithmetic for 120 Hz**
- At 120 Hz the compositor latches every 8.33 ms.
- PyroWave decode (2–6 ms) plus an upscale (1–3 ms on a base M4) gives 3–9 ms of GPU time. A slow frame can therefore miss the latch and slip a full 8.3 ms. At 60 Hz there is more slack, but the GPU clocks down.
- Mitigations:
  - Fuse the upscale into the last decode pass.
  - Use bicubic+RCAS rather than EASU on the base M4.
  - Adopt a webgpu-vsr-style deadline controller: EASU+RCAS → bicubic → bilinear compositor scale, measured on presented frames (rVFC `presentationTime`/`expectedDisplayTime` for `<video>`; GPU timestamp queries for canvas).

**Interpolation versus native rate**
- Frame interpolation as a way to reach 120 Hz would add a whole source frame (§4).
- At 90/120 fps sources the panel is already fed at or near native rate. Spending GPU on interpolation is strictly worse than streaming at the higher rate on a LAN.

**Priority by path**
1. On the `<video>` paths (1–2), the best upscaled-latency candidate is Option A (§5): WebGPU → `VideoFrame` → `VideoTrackGenerator`, which keeps `<video>`'s overlay presentation. It must be measured.
2. On the PyroWave path, upscaling is cheap marginally because the canvas present cost is already paid.
3. On Windows/NVIDIA, driver VSR on `<video>` costs zero development and likely no added queueing (§1).

### Gaps
- No source confirms whether Chrome on macOS runs WebGPU canvas presentation and rAF at 120 Hz on ProMotion panels, or how the canvas latch deadline is computed in 2026.
- No published measurement exists of how much one extra compute pass moves the canvas present time in Chrome or Safari on Apple Silicon.
- No source covers the Apple GPU's DVFS behaviour under WebGPU (clock ramp latency, or whether `powerPreference: "high-performance"` changes it on Apple Silicon).
