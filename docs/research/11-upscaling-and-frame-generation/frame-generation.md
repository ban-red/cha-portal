# Frame generation for latency-critical remote streaming (cloud gaming and remote desktop), state as of 2026-11-09

> Evidence for [research note 11](README.md).

Scope: interpolation vs extrapolation vs input-driven reprojection, where each runs (server or client), what each costs in latency, and how each fits Cha Portal's pipeline. That pipeline is a generic Wayland compositor with nested gamescope, NVENC/VA-API/PyroWave, no B-frames, per-frame IDs and timestamps, client-reported arrival/decode/present, about 5 ms median send→shown on a wired LAN, a Mac client at ProMotion 120 Hz, and browsers. Notation used throughout: T = source (real) frame interval in ms, C = compute time to synthesize one frame, N = output/source frame-rate multiplier, RTT = network round trip.

## 1. Latency arithmetic: interpolation vs extrapolation vs reprojection, client vs server, vs streaming more real frames

### Takeaway
Interpolation must hold back each real frame until the next real frame exists. For N× it delays real frames by about (N−1)/N·T + C, and each generated frame trails its own timestamp by about T + C. That is +8–10 ms at 60→120, +17–19 ms at 30→60 and +4–6 ms at 120→240, and it costs the same whether it runs on the server or the client. Extrapolation and reprojection add no hold (only C has to fit inside the display interval). Input-driven reprojection is the only one of the three that lowers perceived latency, and only for camera rotation. For a GPU-bound game, streaming 120 real fps beats 60 fps + FG→120 by roughly 15–20 ms of input-to-photon. Real frames should be the default whenever the server can render them.

### Cited Findings
- GFFE (Intel and UCSB, arXiv May 2024) states the core constraint. Interpolation (DLSS 3, FSR 3) needs the next rendered frame, which adds at least one rendering interval of latency for generated frames; Reflex can reduce that but not remove it. Extrapolation uses only past frames and adds no latency. — [GFFE arXiv HTML](https://arxiv.org/html/2406.18551v1); [abstract](https://arxiv.org/abs/2406.18551)
- Measured cost of in-game interpolation. TechSpot's 2022 benchmark found that DLSS 3 frame generation in Cyberpunk 2077 added about 10 ms. Digital Foundry's early DLSS 4 MFG test (RTX 5080 engineering sample, January 2025) found the 2nd generated frame added about 4 ms and the 3rd about 2 ms on top of 2× FG, so most of the cost is buffering the first extra frame. This is secondhand via TechSpot; DF's own article was not fetched. — [TechSpot, Jan 2025](https://www.techspot.com/news/106265-early-dlss-4-test-showcases-cleaner-images-multiplied.html); [GamersNexus](https://gamersnexus.net/gpus/fake-frames-tested-dlss-40-mfg-4x-nvidias-misleading-review-guide)
- NVIDIA told DF the acceptable *input* frame rate for 3×/4× is about the same as for 2×. — [GamersNexus](https://gamersnexus.net/gpus/fake-frames-tested-dlss-40-mfg-4x-nvidias-misleading-review-guide)
- Measured cost of game-agnostic, post-process interpolation (Lossless Scaling, the closest analogue to what a video-only client can do). At a 40 fps base with 28.47 ms end-to-end, LSFG 3 at 80 fps measured 41.70 ms (+13.2 ms) and LSFG 2 measured 54.31 ms (+25.8 ms). These are community benchmarks by CptTombstone, reported by Tom's Hardware (early 2025). — [Tom's Hardware](https://www.tomshardware.com/video-games/pc-gaming/lossless-scaling-3-update-touts-greatly-improved-latency-and-performance-universal-frame-gen-tool-boasts-24-percent-reduced-latency)
- In PCGH's Starfield end-to-end test (April 2024, about 41 fps base, GTX 1080 Ti, 4K), FSR 3 FG without Reflex reached about 150 ms against about 72 ms with Reflex and no FG. This shows the render-queue interaction compounding the FG hold. The figures are indicative and come from German captions. — [PCGH](https://www.pcgameshardware.de/Starfield-Spiel-61756/Specials/Lossless-Scaling-Test-Frame-Generation-FSR-3-DLSS-Latenz-LSFG-1446068/galerie/3875242/)
- DLSS 4 keeps frame pacing in hardware ("Flip Metering" in the Blackwell display engine). This underlines that generated frames must be presented at exact sub-interval times to deliver smoothness. — [NVIDIA DLSS 4 announcement, Jan 2025](https://www.nvidia.com/en-us/geforce/news/dlss4-multi-frame-generation-ai-innovations/)
- DLSS 4.5 (announced at CES in January 2026, rolled out via an NVIDIA app beta around early April 2026) adds 6× MFG and "Dynamic MFG", which picks 2×/3×/4×/6× moment to moment to match the display's max refresh. It is limited to RTX 50. — [TweakTown](https://www.tweaktown.com/news/110798/nvidia-launches-dlss-4-5-dynamic-multi-frame-generation-and-6x/index.html); [Neowin](https://www.neowin.net/amp/nvidia-introduces-dlss-45-dynamic-multi-frame-generation-and-a-6x-mode/); [PCGH](https://www.pcgameshardware.de/Deep-Learning-Super-Sampling-Software-277618/News/DLSS-4-5-mit-Dynamic-Multi-Frame-Generation-1524105/)
- Reflex 2 Frame Warp (vendor numbers, January 2025): THE FINALS at 4K on an RTX 5070 measured 56 ms native, 27 ms with Reflex Low Latency, and 14 ms with Frame Warp. VALORANT measured under 3 ms PC latency at 800+ fps. Most of the gain is the late camera warp, not extra frames. — [NVIDIA](https://www.nvidia.com/en-us/geforce/news/reflex-2-even-lower-latency-gameplay-with-frame-warp/)

### Inferences (explicit arithmetic)

**A. Interpolation hold, wherever it runs.** To display generated frame I(n + k/N), the synthesizer needs real frame F(n+1). The paced sequence is I(n+1/N), …, I(n+(N−1)/N), then F(n+1).
- Real frame F(n+1) is shown (N−1)/N·T + C later than it would be without FG.
- Each generated frame trails its scene time by T + C (it is built at the moment F(n+1) arrives).

| Case | T (ms) | Real-frame hold (N−1)/N·T | + C (assume 2–5 ms client, 1–3 ms 4090) | Generated frame staleness T + C |
|---|---|---|---|---|
| 30→60 (2×) | 33.3 | 16.7 | ≈19–22 | ≈35–38 |
| 60→120 (2×) | 16.7 | 8.3 | ≈10–13 | ≈19–22 |
| 120→240 (2×) | 8.33 | 4.2 | ≈5–9 | ≈10–13 |
| 60→240 (4×) | 16.7 | 12.5 | ≈14–17 | ≈19–22 |

- Sanity check: 60→120 predicts about 10 ms, matching TechSpot's DLSS 3 figure. MFG increments of (2/3 − 1/2)·16.7 = 2.8 ms and (3/4 − 2/3)·16.7 = 1.4 ms match DF's roughly 4 ms and 2 ms in order of magnitude. LSFG 3 at 40→80 predicts 12.5 + C against the measured +13.2 ms.
- Throughput constraint at the client: C ≤ T/N minus decode, colour conversion and present work. At 120→240 that leaves about 4 ms per generated frame.
- The ProMotion ceiling is 120 Hz, so 120→240 only matters for browsers on external 240 Hz monitors (unverified that macOS drives such panels at 240 Hz from every M-series Mac).

**B. Extrapolation and reprojection.** Synthesized frame E(n + 1/N) is built from F(n) and F(n−1), plus fresh input for reprojection, and shown at its nominal slot.
- Added hold is 0. C only has to fit before the vsync deadline.
- Its *information* is still the same age as F(n); it is a prediction, not new data.
- Exception: input-driven reprojection injects client-local input. Camera rotation response becomes about input poll + warp C + vsync wait, typically 2–6 ms on the client and independent of RTT.

**C. Server-side FG, then encode (in-game DLSS/FSR FG or a Vulkan-layer FG such as lsfg-vk inside the session).**

| Path | Total loop latency vs native 60 fps | Notes |
|---|---|---|
| Native 60 fps, no FG | 0 (baseline) | — |
| 60 fps + server FG→120, encode/stream 120 | +T/2 + C ≈ +9–11 ms | Same hold as client FG |
| Native 120 fps | −(T60 − T120)/2 = −4.2 ms (input sampling wait) − about 8.3 ms render time (if GPU-bound) ≈ −8 to −12 ms | — |
| 60 fps streamed + client interpolation→120 | +8.3 + C_client ≈ +10–14 ms | — |
| 60 fps streamed + client extrapolation/reprojection→120 | ≈ +0 | Rotation latency is cut further with input reprojection |

- Server FG also costs 2× NVENC frames and more bitrate. With P-frame-only codecs, doubling fps raises bitrate less than 2× because inter-frame residuals shrink (unverified rule of thumb; no source fetched). With PyroWave (intra-only), bitrate scales about linearly with fps, so 2× fps costs about 2× bits.
- Upside of server FG: it interpolates *uncompressed* frames with game motion vectors and HUD-less buffers, so quality is best and client cost is zero.
- **Native 120 fps vs 60 fps + FG→120:** about (8–12) + (9–11) ≈ 17–23 ms difference in input-to-photon, in favour of real frames. On a LAN where send→shown is about 5 ms, that FG penalty is 2–4× the entire network/codec path. FG at LAN scale is a smoothness feature, not a latency feature.

**D. Network jitter.**
- Treat the client display schedule as a target latency L after capture. A real frame is on time if its arrival+decode latency is ≤ L.
- With interpolation, F(n+1) must be ready by the time I(n+1/2) is due, which happens T/2 + C earlier. Interpolation therefore *consumes* T/2 + C of whatever jitter margin exists. It does not hide inside an existing jitter buffer for free.
- Extrapolation runs the other way. When F(n+1) misses its slot, showing E(n+1) instead of repeating F(n) lets L be set nearer the median than the P99. Example at 60 fps: a P99 jitter of 12 ms needs L ≈ median + 12 ms; with extrapolate-on-late, L ≈ median + 4 ms with a few percent synthesized frames, saving about 8 ms. (This is an illustrative calculation, not a measurement.)

**E. Display refresh.**
- On a fixed 120 Hz panel, a 60 fps stream shows each frame for 2 refreshes. 2× interpolation fills them, but costs the hold from A.
- 90 fps on 120 Hz is non-integer, giving a 1,1,2 refresh cadence and judder. Extrapolation/warp can render for *any* display timestamp, which fixed-ratio interpolation handles less gracefully.
- With VRR/adaptive ProMotion, presenting on arrival removes the average half-refresh wait (4.2 ms at 120 Hz). That often makes FG for pacing unnecessary.
- Generated frames only look smooth if presentation time is controlled. The native Mac app can time presents; browsers generally cannot control present time beyond rAF/vsync (unverified detail; covered by other researchers).

**F. Cha Portal instrumentation.** Synthesized frames should be flagged in client telemetry (frame ID + "synth" flag + source frame IDs). Otherwise send→shown, present-time and "frames shown" metrics will look better than the real stream is.

### Gaps
- No Digital Foundry or Blur Busters primary article was fetched with exact click-to-photon numbers for 2026 drivers (DLSS 4.5 FG model, FSR Redstone FG). Figures above are from 2022–2025 and secondhand.
- No measured C for optical-flow interpolation on Apple Silicon (M1–M4) or in WebGPU at 1080p/1440p.

## 2. Client-side frame generation from decoded video alone (optical flow, codec motion vectors)

### Takeaway
It works and is shipping in practice, as Lossless Scaling on the client of a stream, a pattern users report with Moonlight. But it is the lowest-quality option: no depth, no game motion vectors, no HUD separation, and it operates on compressed frames. Interpolation-type methods pay the full hold from section 1.
- Cost on a desktop NVIDIA GPU at 1080p: about 2.5–10 ms for RIFE-class models (community numbers).
- Codec motion vectors are a cheap but coarse flow prior: about −3 dB vs RIFE without refinement, and about −0.6 to −0.8 dB with a small refinement net.
- Codec MVs are not exposed by hardware decoders, and PyroWave has none at all.

### Cited Findings
- RIFE throughput on an RTX 4090 at 1080p, community reports on the SVP forum: about 416 fps raw (FP16, model 4.6, vs-rife + TensorRT), about 288 fps (TRT 8.5, FP16, 10 streams), and about 94–115 fps in full real-time playback/transcode pipelines. The ensemble-off variant is fastest, ensemble-on has better quality. — [SVP forum thread](https://www.svp-team.com/forum/viewtopic.php?pid=81473); [SVP RIFE wiki](https://www.svp-team.com/wiki/RIFE_AI_interpolation)
- SVP's guidance for 1080p at 60–72 fps RIFE playback is an RTX 3080 generically, or an RTX 3060 with TensorRT. — [SVP RIFE wiki](https://www.svp-team.com/wiki/RIFE_AI_interpolation)
- GFFE's runtime table (1080p, RTX 4070 Ti Super, TensorRT FP16) for video-frame methods: IFRNet (interpolation) 19.50 ms, UPR-Net (interpolation) 43.04 ms, DMVFN (G-buffer-free video *extrapolation*) 20.57 ms.
  - Quality (PSNR/SSIM×100/LPIPS×100): DMVFN 21.73/76.09/20.78, IFRNet 24.24/84.39/18.01.
  - Pure video extrapolation is markedly worse than interpolation on game footage. — [GFFE](https://arxiv.org/html/2406.18551v1)
- ANVIL (arXiv, March/April 2026) reuses H.264 decoder MVs (FFmpeg `export_mvs`, quarter-pel, macroblock-level) to pre-align frames, then refines them with a small INT8 net on a Snapdragon 8 Gen 3 NPU at 1080p.
  - Runtime: ANVIL-S 12.8 ms NPU-only, 28.4 ms median end-to-end; 94.9% of pairs fit a 33.3 ms budget.
  - PSNR on Vimeo90K / Xiph-1080p: RIFE 34.26/30.04, ANVIL-M 33.66/29.74, ANVIL-S 33.45/29.65, MV blend with no network 31.20/28.98.
  - Limitations: median+Gaussian smoothing of MVs was essential (29.63 → 31.20 dB), LPIPS is worse (0.036 vs 0.019), and the method fails on stochastic textures.
  - Android MediaCodec hardware decode does *not* expose MVs, so software decode was required (about 16% battery over 30 minutes). — [ANVIL](https://arxiv.org/html/2603.26835v2)
- Earlier work found block-based transmitted MVs "disappointing" for interpolation because of blockwise coding of MVs and residuals; they need refinement such as mesh-based estimation. — [Yoon & Ahuja](https://migrate2wp.web.illinois.edu/html-files-to-import/publications/Frame%20Interpolation%20Using%20Transmitted%20Block-Based%20Motion%20Vectors.pdf)
- Mobile research targets GPUs without optical-flow hardware. Mob-FGSR (SIGGRAPH 2024, Zhejiang University + OPPO) reconstructs MVs by splatting with LUT-based warping. Reported numbers on a Snapdragon 8 Gen 3 were about 50 fps with frame generation and more than 110 fps combined with 2× SR. It needs engine data, so it is not video-only; whether it interpolates or extrapolates was not confirmed. — [Mob-FGSR project](https://mob-fgsr.github.io); [paper](https://sites.cs.ucsb.edu/~lingqi/publications/paper_sig24mobfgsr.pdf)
- Users report running Lossless Scaling on the Moonlight *client* to multiply a 60 fps stream, saving bandwidth. A reply notes it only smooths and adds input latency. No official Moonlight client-side FG feature was found. Host-side forks such as Vibepollo instead capture the full output rate of games that already use FG. — [r/losslessscaling thread](https://reddit.birdcat.cafe/r/losslessscaling/comments/1lek51k/lossless_scaling_on_moonlight_client_side); [Vibepollo](https://github.com/Daviex/Vibepollo)
- lsfg-vk is an open-source Vulkan-layer port of Lossless Scaling's frame generation for Linux/Steam Deck by PancakeTAS. It is relevant as a game-agnostic FG that could run inside the server session. — [TechPowerUp forum post](https://www.techpowerup.com/forums/posts/5550681) (secondary)
- Vendor FG pipelines treat optical flow on final colour as a *complement* to engine MVs, not a replacement. FSR frame interpolation blends optical-flow-based and game-MV-based results per pixel, favouring the better colour match. It uses a scene-change detector and optionally a HUD-less colour buffer, with an inpainting pass to clean UI. — [AMD FidelityFX Frame Interpolation 1.1.3 docs](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/frame-interpolation/)
- DLSS 3 used the RTX 40 Optical Flow Accelerator. DLSS 4 replaced it with an AI flow model that is 40% faster, uses 30% less VRAM, and runs once per rendered frame for multiple generated frames. These are vendor claims. — [NVIDIA](https://www.nvidia.com/en-us/geforce/news/dlss4-multi-frame-generation-ai-innovations/)

### Inferences
- **Codec MVs in Cha Portal are mostly unavailable on the client.**
  - VideoToolbox and WebCodecs return decoded surfaces, not MVs. ANVIL's MediaCodec finding is analogous; the VideoToolbox/WebCodecs claim is unverified.
  - PyroWave is intra-only and has no MVs.
  - The practical variant is the reverse direction: the *server* forwards the encoder's motion field (or a cheap low-res motion estimate) as side data, a few kB per frame at 1/16-res. That is cheaper than the client estimating flow. Whether NVENC can export the MVs it used for a given encode without a separate motion-estimation-only pass is unverified.
- **Compression sensitivity.** Flow estimators lock onto block edges and ringing at low bitrate, and encoder MVs follow rate-distortion choices, not true motion. Client FG quality will therefore track bitrate. It is most viable on LAN (high bitrate) and degrades exactly on WAN, where it would be most wanted for bandwidth reasons.
- **Interpolation vs extrapolation, video only.** Interpolation looks clearly better (about 24 vs 22 dB PSNR in GFFE's game scenes) but costs T/2 + C of hold. Extrapolation costs nothing in hold but produces visible errors at motion onsets and stops. For a streaming client the sensible split is:
  - Extrapolation or global/rotation warp for concealment and pacing (sections 4 and 6).
  - Interpolation only as an opt-in smoothness mode for low base rates in non-competitive games.
- **Compute budget on the Mac client.** IFRNet-class cost of about 20 ms at 1080p on a 4070 Ti Super implies such models will not fit a 4–8 ms budget on Apple Silicon without heavy downscaling. Plan on RIFE-lite or flow-at-quarter-resolution designs. This is an inference; no Apple Silicon numbers were found.

### Gaps
- No published evaluation of RIFE/IFRNet/FILM specifically on *compressed game-stream* frames (H.264/HEVC/AV1 at streaming bitrates), and none on UI/text-heavy frames.
- No WebGPU or Core ML/Metal real-time interpolation benchmarks found.
- FILM was not evaluated here. It is known to be heavy (unverified), and no real-time figures were found.

## 3. Server-assisted client frame generation (depth/MV/pose side data; VR split rendering; speculative execution)

### Takeaway
VR split rendering proves the model: the client reprojects the last decoded frame to a fresh pose using depth (CloudXR's depth-mesh reprojection, Meta's positional TimeWarp and AppSW). Outatime showed speculative frames + depth + wider FOV can mask 120–250 ms RTT for about 2× bitrate. For a generic Linux streamer, though, depth and MVs are only realistically available by intercepting the game's graphics API. Options are a Vulkan layer with depth heuristics (fragile) or interception of upscaler/FG API calls (unverified path). Rotation-only reprojection needs no side data beyond FOV and input mapping. That makes it the realistic first step.

### Cited Findings
- Meta Application SpaceWarp (AppSW):
  - The app renders at half rate (for example 36 fps) and submits a *lower-resolution* motion-vector buffer (3D NDC delta, recommended 16-bit signed float) plus a depth buffer. The compositor synthesizes the missing frames (for example to 72 Hz).
  - Rendering MV/depth above the recommended resolution adds no quality.
  - Submitting depth also enables positional TimeWarp, which corrects translation as well as rotation.
  - Translucent objects are not supported. Objects whose materials don't write MVs stutter in synthesized frames. — [Meta AppSW developer guide](https://developers.meta.com/horizon/documentation/native/android/mobile-asw/); [AppSW essentials](https://developers.meta.com/horizon/essentials/app-spacewarp)
- NVIDIA CloudXR (6.x): CloudXR.js uses `XRFrame.predictedDisplayTime` to get the target pose for *depth-mesh reprojection*. On a headset missing that attribute, reprojection stops and frames are shown once at arrival rate. This is a user report on NVIDIA's forum, 2026. — [NVIDIA developer forum](https://forums.developer.nvidia.com/t/androidxr-device-performance-cloudxr-6-2-1/380632)
- Patent literature on cloud XR describes the client reprojecting the server image to the latest pose, reprojecting at a higher rate than the negotiated server frame rate, and continuing to reproject when the stream stalls. A 5G split-rendering filing says client ATW keeps motion-to-photon at about 20 ms while downlink latency is typically 50–60 ms. — [US 12439013](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12439013); [US 12256052](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12256052); [US 11412310](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11412310)
- Microsoft Research Outatime (MobiSys 2015, Best Paper), tested on Doom 3 and Fable 3:
  - Navigation input is predicted with Markov models over supersampled input.
  - The server renders a *clipped cube map plus depth* around the predicted pose. The client runs image-based rendering (IBR) to correct navigation mispredictions.
  - Impulse events (fire) are handled by parallel speculative timelines with state-space subsampling and time-shifting.
  - Masks up to 120 ms RTT; a later version claims 250 ms. — [MSR](https://www.microsoft.com/en-us/research/publication/outatime-using-speculation-to-enable-low-latency-continuous-interaction-for-mobile-cloud-gaming/); [paper PDF](https://www.cs.princeton.edu/courses/archive/fall15/cos518/papers/outatime.pdf)
- Outatime bitrate figures, from the paper text:
  - A full cube map plus depth has a nominal 12× overhead.
  - Clipping helps because 99% of mispredictions are covered by under 80° in Fable 3, and 225° yaw in Doom 3 at 250 ms.
  - With joint encoding of speculative frames: thin client 0.53 Mbps median; Outatime independent encoding 4.01 Mbps; joint 3.33 Mbps; clipped + joint 2.41 Mbps (4.54×); at RTT ≤ 128 ms, 1.04 Mbps (1.97×); navigation-only 0.8 Mbps (1.51×).
  - Disocclusion "smears" were patched by extruding foreground depth onto the background before IBR. — [Outatime PDF](https://www.cs.princeton.edu/courses/archive/fall15/cos518/papers/outatime.pdf)
- Google Stadia's "negative latency" (Majd Bakar, VP engineering, Edge interview, October 2019) meant input prediction plus higher frame rates to offset streaming latency. Developers warned about rollback and desync costs on mispredicts. Commentators compared it to RetroArch run-ahead. No technical publication found; Stadia closed in January 2023 (not re-verified here). — [PC Gamer](https://www.pcgamer.com/google-says-stadia-will-be-faster-than-local-gaming-hardware-in-two-years); [GamesBeat](https://gamesbeat.com/how-google-stadias-negative-latency-might-work/); [Engadget](https://www.engadget.com/2019-10-10-google-stadia-negative-latency.html)
- MetalFX Frame Interpolation (macOS 26, WWDC25 session 211) takes two rendered frames + motion vectors + depth. The app declares motion scale and depth convention. Transparent effects and particles are not in the MV/depth textures. UI can be handled as "Composited", "Offscreen" or "Every-Frame". — [WWDC25 session 211](https://developer.apple.com/videos/play/wwdc2025/211/); [MTLFXFrameInterpolatorDescriptor](https://developer.apple.com/documentation/metalfx/mtlfxframeinterpolatordescriptor)
- Generic depth capture in a layer: ReShade's "Generic Depth" add-on uses heuristics (draw-call counts, aspect ratio, format) to *guess* the main scene depth buffer and has a manual override. Users report Vulkan depth access as inconsistent per game. vkBasalt (the Linux Vulkan post-processing layer) has said its depth support is not ready. — [Generic Depth docs](https://readshade.github.io/ReadShade/software/addons/genericdepth.html); [ReShade forum, RDR2 Vulkan](https://reshade.me/forum/troubleshooting/9269-depth-buffer-in-red-dead-redemption-2-on-vulkan); [vkBasalt summary](https://awesome.ecosyste.ms/projects/github.com%2FDadSchoorse%2FvkBasalt)

### Inferences
- **What each technique needs, in increasing order of difficulty:**
  1. **Rotation-only warp (ATW-style).** Needs camera intrinsics (FOV), the yaw/pitch per input count, and which input sequence number each frame already includes. It needs no depth: a pure rotation maps pixels by the depth-independent homography H = K·R·K⁻¹.
  2. **Translation-aware (positional) warp.** Needs per-pixel depth plus camera translation per frame.
  3. **Object-motion extrapolation (AppSW-like).** Needs per-pixel MVs too.
  4. **Speculative multi-branch (Outatime-like).** Needs control of game state (checkpoint/rollback) and is impossible for an unmodified Proton game.
- **What a generic Cha Portal node can get:**
  - (a) Nothing from the game: video only.
  - (b) Its own input stream and timestamps. It already has these, and only needs to stamp each frame with the last input sequence number consumed. The compositor can approximate this by the last input datagram delivered before capture; exact attribution to the game's simulation tick is not possible generically.
  - (c) A depth buffer via a Vulkan layer in the game process (DXVK/vkd3d-proton games are Vulkan underneath), using ReShade-style heuristics. This is best-effort and breaks per game.
  - (d) Depth + MVs + camera jitter at the point where games already hand them to an upscaler/FG library (DLSS/FSR/XeSS). That is the only place MVs exist in a well-defined form. Intercepting it is technically similar to existing DLSS→FSR wrapper mods (unverified; not researched here).
  - Gamescope composites the game's final swapchain images and does not hand over depth/MVs (no source contradicting this found).
- **Side-data bitrate, order-of-magnitude estimate:**
  - Depth at 1/4 width × 1/4 height (480×270 for 1080p) in 16-bit is about 259 kB/frame raw (about 124 Mbit/s at 60 fps). Encoded as a 10/12-bit monochrome video stream it should compress to a small fraction of the colour stream, because depth is piecewise-smooth. Plausibly 5–15% of colour bitrate (unverified estimate).
  - MVs at 1/8 or 1/16 resolution are smaller still.
  - The only measured end-to-end data point is Outatime's: about 1.5–2× total bitrate for wider FOV + depth + speculation.
- **Guard band / overscan.** Outatime and VR runtimes render beyond the visible FOV. Cha Portal cannot change a game's FOV via gamescope, so edge disocclusion after a warp must be inpainted or clamped (see section 4).

### Gaps
- ALVR's reprojection/pose-prediction implementation details were not found (no primary source retrieved).
- PS VR2 reprojection details were not researched.
- No published bitrate measurements for streaming depth alongside game video in a modern codec (AV1/HEVC) were found.

## 4. Input-driven warping for flat-screen games (Reflex 2 Frame Warp, async-reprojection demos, application to cloud gaming)

### Takeaway
Reflex 2 Frame Warp is the canonical design: sample the newest mouse input, warp the just-rendered frame to the new camera, inpaint holes from prior frames' colour and depth, and composite the HUD unwarped. It was announced in January 2025 and, as of about July–August 2026, had still not shipped in any game. Only an unofficial demo exists, built from leaked binaries. For streaming, client-side rotation warp is unusually valuable because it removes the *network* RTT from mouse-look, not just render latency. The hard problems are input-to-angle calibration, input attribution per frame, HUD/viewmodel masking, and edge fill. Translation/parallax requires depth.

### Cited Findings
- Frame Warp mechanism, per NVIDIA (January 6, 2025):
  - While the GPU renders, the CPU computes the camera position for the next frame from the latest input. The just-rendered frame is warped to that newer camera "as late as possible, just before the rendered frame is sent to the display".
  - The resulting holes are filled by a "latency-optimized predictive rendering algorithm" using camera, colour and depth data from prior frames.
  - It launches first on RTX 50 with THE FINALS and VALORANT "coming soon". — [NVIDIA](https://www.nvidia.com/en-us/geforce/news/reflex-2-even-lower-latency-gameplay-with-frame-warp/)
- A Japanese ASCII article reports the HUD is composited *after* inpainting/warp. NVIDIA documentation confirming this was not found (unverified). — [ASCII.jp](https://weekly.ascii.jp/elem/000/004/245/4245918/3/)
- Status in 2026:
  - As of about July 2026 (VideoCardz/HWBusters, "18 months after announcement"), there was no public release, no driver toggle and no shipping game. NVIDIA's developer page asks studios to sign up for notifications. These are secondhand via search summaries; the VideoCardz page could not be fetched.
  - Modder PureDark built a demo from Reflex 2 files found in ARC Raiders/THE FINALS test builds, reportedly running on RTX 20-series too. One report measured about 7.8 ms → 1–2 ms latency at 240 fps in that demo (single early test).
  - No evidence of a release between August and November 2026 was found. — [VideoCardz](https://videocardz.com/newz/nvidia-reflex-2-remains-unreleased-18-months-after-announcement-rtx-owners-still-waiting); [HWBusters](https://hwbusters.com/news/nvidia-reflex-2-is-18-months-old-and-still-hasnt-shipped/); [VideoCardz on PureDark demo](https://videocardz.com/newz/puredark-releases-free-demo-of-nvidia-reflex-2-frame-warp-works-on-rtx-20-gpus); [Tom's Hardware](https://www.tomshardware.com/pc-components/gpus/intrepid-modder-builds-frame-warp-demo-from-nvidia-reflex-2-binaries-tech-remains-mysteriously-shelved-despite-greatly-reducing-latency)
- Comrade Stinger's async-reprojection demo (circa 2022–23; [video](https://www.youtube.com/watch?v=VvFyOFacljg)), popularised by 2kliksphilip and tested by Linus Tech Tips, brought VR-style async reprojection to flat mouse-look games.
  - An Unreal Engine forum post said no games or engines had implemented it.
  - Community testers reported that looking around at a 30 fps base was "100% convincing" as 144 fps, but moving (translation) exposed the 30 fps base, along with smearing at screen edges and detail (forum reports; exact attribution unverified). — [UE forum](https://forums.unrealengine.com/t/asynchronous-reprojection-support/1688774); [Overclockers UK thread](https://forums.overclockers.co.uk/threads/using-asynchronous-reprojection-to-make-30fps-look-like-240fps.18963465); [Blur Busters thread](https://forums.blurbusters.com/viewtopic.php?p=86441)
- mc-ar-mod (LGPL-3.0, Minecraft) is an open implementation based on Comrade Stinger's shader code. It uses a second rendering context to asynchronously reproject frames with new camera rotation *and player position*. It is self-described as a proof of concept, with camera bobbing disabled and GUI-transition glitches. — [GitHub mt1006/mc-ar-mod](https://github.com/mt1006/mc-ar-mod)
- Blur Busters argues "frame rate amplification" via reprojection can be "lagless" (keeping the base render's lag at much higher output rates). It also argues reprojection artifacts are often less objectionable than stutter and fade at higher base rates. — [Blur Busters forum](https://forums.blurbusters.com/viewtopic.php?p=27715); [p=27720](https://forums.blurbusters.com/viewtopic.php?p=27720)
- GFFE's limitations list the same failure classes for any depth-based warp: UI and particles (no depth) are mispositioned, never-seen disocclusions are not filled, and shading changes in disocclusions are not handled. — [GFFE](https://arxiv.org/html/2406.18551v1)

### Inferences (how a Cha Portal client would do it, and what breaks)

**Pipeline**

| Step | What | Detail |
|---|---|---|
| (1) | Input attribution | Server stamps each frame with `last_input_seq` applied (or delivered before capture) |
| (2) | Residual input | Client keeps a ring of sent mouse deltas, so residual = Σ deltas with seq > `last_input_seq` |
| (3) | Delta → angle | Δyaw = k·Σdx and Δpitch = k·Σdy, where k is degrees per count |
| (4) | Warp | H = K·R(Δyaw, Δpitch)·K⁻¹ with K from horizontal FOV, applied to the last decoded frame on the GPU at present time |
| (5) | Mask | HUD/viewmodel/crosshair mask keeps those pixels unwarped |
| (6) | Edge fill | Fill the uncovered edge (clamp/mirror/blur, or inpaint from the previous warped frame) |

How to obtain k and FOV:
- From a per-game profile, or by online calibration: regress the global image rotation estimated from consecutive real frames against the residual mouse counts.
- Mouse acceleration, in-game smoothing, ADS sensitivity changes and vehicles all break a fixed k. The calibration must be continuous and must disable itself when residuals stop predicting motion.

**Edge-exposure arithmetic.** The warp must cover the rotation accumulated during the whole input→display loop L_loop: Δθ = ω·L_loop. With a 90° horizontal FOV at 1920 px, focal length f = 960 px and edge shift ≈ f·tan(Δθ).

| ω | L_loop | Δθ | Uncovered edge |
|---|---|---|---|
| 300°/s | 30 ms (LAN) | 9° | ≈152 px (8% of width) |
| 300°/s | 70 ms (WAN) | 21° | ≈369 px (19%) |
| 1000°/s flick | 70 ms | 70° | Unusable |

Edge fill is therefore acceptable on LAN at moderate turn rates and degrades fast on WAN. This is exactly where Outatime and VR use overscan, which a generic streamer cannot request from the game.

**What goes wrong**
- (a) **HUD and crosshair are baked into the video in game mode, so they swim.** Masks can come from temporal-stability detection (pixels static while the scene rotates) or per-game masks.
- (b) **The first-person viewmodel moves with the camera.** It should not be warped, but it is not static either (bob/sway).
- (c) **Parallax/translation (WASD) cannot be corrected without depth.** Rotation-only warp is still exact for the background at infinity and good for distant geometry.
- (d) **Moving objects are frozen at their last position** until the next real frame. That is acceptable at 60–120 fps sources.
- (e) **Menus, cutscenes, map screens and RTS/top-down cameras must disable the warp.** A heuristic is "game currently shows a cursor, or mouse residuals don't correlate with image motion".
- (f) **Snap-back.** If the server applied the input differently, for example because of server-side smoothing, the next real frame corrects the image with a visible jump.
- (g) **Server-side cursor.** In game mode the cursor is composited into the video, so it would be warped with the scene. Desktop mode already draws it client-side.

**Comparison with Reflex 2.** On the server node, Reflex 2 only cuts render-queue latency. Client warp cuts uplink + server + encode + network + decode for camera rotation. On WAN that is tens of ms, which is far more valuable for cloud gaming than any FG multiplier.

### Gaps
- No public technical paper from NVIDIA on Frame Warp's inpainting network or its HUD handling was found.
- No shipping cloud-gaming client using input-driven reprojection for flat games was found (none of GeForce NOW, Xbox Cloud or Moonlight forks confirmed; product inventories are covered elsewhere).
- No user study quantifying acceptability of edge-fill artifacts versus latency for flat-screen mouse-look was found.

## 5. Research and industry state of the art (2023–2026): extrapolation, joint SR+FG, real-time VFI, vendor designs

### Takeaway
Research has swung from interpolation (which adds latency) to *extrapolation*:
- ExtraNet (2021), ExtraSS (2023) and GFFE (2024) brought G-buffer-free extrapolation to about 6.6 ms at 1080p.
- PatchEX (2024) and Predictive Sparse Shading (TOG 2025) followed.
- Amulet (August 2026) is a non-neural layered-cache method targeting 60→240 Hz at 4K.

All shipping vendor FG in 2026 is still *interpolation* driven by engine MVs + depth + flow (DLSS 4/4.5 MFG, FSR FG, XeSS-FG, MetalFX). The only vendor extrapolation/warp product, Reflex 2, has not shipped. Every high-quality method needs engine buffers; video-only methods (DMVFN, RIFE/IFRNet, ANVIL) are the only ones a generic streamer can use without a game hook.

### Cited Findings
- **ExtraNet** (ACM TOG 40(6), SIGGRAPH Asia 2021): real-time *extrapolated* rendering with G-buffers for low-latency temporal supersampling. It is the first in the extrapolation line. — [ACM DL](https://dl.acm.org/doi/10.1145/3478513.3480531)
- **ExtraSS** (SIGGRAPH Asia 2023, Intel + UCSB): joint spatial supersampling + frame extrapolation from prior frames + G-buffers. It avoids the latency that forces DLSS 3/FSR 3 to depend on Reflex/Anti-Lag. In GFFE's comparison its extrapolation part (ExtraSS-E) runs in 4.18 ms + 0.35–8.23 ms of G-buffer generation at 1080p. No product announcement under that name was found. — [Intel article](https://www.intel.com/content/www/us/en/developer/articles/technical/extrass-a-framework-for-joint-spatial.html); [paper](https://sites.cs.ucsb.edu/~lingqi/publications/paper_siga23extrass.pdf); [KitGuru](https://www.kitguru.net/gaming/joao-silva/extrass-framework-is-intels-answer-to-dlss-3-and-fsr-3-frame-generation-technologies/); [GFFE](https://arxiv.org/html/2406.18551v1)
- **GFFE** (Wu, Vembar, Sochenov, Panneer, Kim, Kaplanyan, Yan; Intel + UCSB; arXiv May 2024; venue not stated on arXiv):
  - Inputs: rendered colour + depth + motion vectors + camera poses for frames t−1 and t. No G-buffers for the extrapolated frames.
  - Tested at 30→60 fps at 1080p. Runtime is 2.34 ms at 540p, 3.66 ms at 720p and 6.62 ms at 1080p on an RTX 4070 Ti Super (Falcor + TensorRT FP16).
  - Quality averages: PSNR 24.20, SSIM 85.93, LPIPS 9.67, FovVDP 7.65. That beats ExtraSS-E/IFRNet/UPR-Net on LPIPS and FovVDP, and DMVFN on everything.
  - The authors explicitly target cloud gaming / low-latency streaming on low-end clients that lack scene data.
  - Known limits: UI/particles, uncollected disocclusions, dynamic-lighting shading in disocclusions, and a 4-scene training set.
  - No code link found. — [arXiv HTML](https://arxiv.org/html/2406.18551v1); [Intel](https://www.intel.com/content/www/us/en/developer/articles/technical/gffe-g-buffer-free-frame-extrapolation.html); [project page](https://poiw.github.io/publication/gffe/)
- **PatchEX** (arXiv July 2024): patch-based parallel extrapolation splitting foreground and background, aiming for "interpolation quality at extrapolation speed". — [arXiv 2407.17501](https://arxiv.org/pdf/2407.17501)
- **Consecutive Frame Extrapolation with Predictive Sparse Shading** (ACM TOG 44(6), 2025): sparsely shades critical regions and reuses generated pixels elsewhere, enabling multiple consecutive extrapolated frames. The full text was not retrievable (403), so runtime is not captured. — [ACM DL](https://dl.acm.org/doi/10.1145/3763363)
- **Amulet** (University of Stuttgart, arXiv August 11, 2026): non-neural, rasterizes potentially visible geometry into a sparse tiled layered image-space cache. The cache is predictively shaded for future views, with per-tile lifetimes. It explicitly handles disocclusions and runs up to 250 Hz at 4K, typically 60 Hz shading → 240 Hz display. It claims competitiveness with DLSS and neural-flow approaches. It needs scene geometry, so it is an engine-side method. No code link. — [arXiv 2608.10423](https://arxiv.org/abs/2608.10423)
- **Mob-FGSR** (SIGGRAPH 2024, ZJU + OPPO): joint FG + SR for mobile without optical-flow hardware (MV reconstruction by splatting, LUT warping). Snapdragon 8 Gen 3 figures are in section 2. — [project](https://mob-fgsr.github.io)
- **ReFrame** (arXiv June 2025): layer caching for neural real-time rendering networks, giving about a 1.4× average speedup with negligible quality loss. — [arXiv 2506.13814](https://paperswithcode.co/paper/2506.13814)
- **FrameDiffuser** (arXiv December 2025): G-buffer-conditioned diffusion for neural forward frame rendering. Real-time suitability was not confirmed. — [arXiv 2512.16670](https://arxiv.org/pdf/2512.16670)
- **ANVIL** (arXiv March/April 2026): codec-MV-prior real-time VFI on NPU (see section 2). — [arXiv 2603.26835](https://arxiv.org/html/2603.26835v2)
- **NVIDIA DLSS 4 MFG** (January 2025): up to 3 generated frames per rendered frame. An AI optical-flow model replaced the OFA hardware: 40% faster, −30% VRAM, run once per rendered frame. Blackwell hardware flip metering handles pacing. Research page: [NVIDIA ADLR DLSS4](https://research.nvidia.com/labs/adlr/DLSS4). — [NVIDIA](https://www.nvidia.com/en-us/geforce/news/dlss4-multi-frame-generation-ai-innovations/)
- **DLSS 4.5** (CES January 2026, rolled out spring 2026): up to 5 generated frames per rendered frame (6×). Dynamic MFG targets the display's max refresh. RTX 40 gets an improved 2× FG model only. — [TweakTown](https://www.tweaktown.com/news/110798/nvidia-launches-dlss-4-5-dynamic-multi-frame-generation-and-6x/index.html); [Guru3D](https://www.guru3d.com/story/nvidia-dlss-45-expands-ai-frame-generation-to-6x-and-improves-image-sharpness/)
- **AMD FidelityFX Frame Interpolation 1.1.3** (FSR 3 FG; documentation page dated ©2026):
  - Inputs: dilated game MVs, dilated depth, reconstructed previous depth, back buffer, optional HUD-less back buffer, and an `FfxOpticalFlow` vector field + scene-change detection.
  - It blends flow-based and MV-based candidates, and an inpainting pass cleans up holes and UI.
  - Pacing lives in a separate frame-interpolation swapchain.
  - Open source in the FidelityFX SDK. — [GPUOpen](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/frame-interpolation/)
- **Apple MetalFX Frame Interpolation** (macOS 26): requires two frames + MVs + depth, and is optionally combined with the MetalFX upscaler. — [WWDC25 session 211](https://developer.apple.com/videos/play/wwdc2025/211/)
- Not re-verified in this pass (unverified):
  - Intel XeSS 2 (December 2024) added XeSS-FG (interpolation using optical flow + game MVs) and Xe Low Latency.
  - AMD "FSR Redstone" (December 2025) added ML-based frame generation for RX 9000.
  - Both are interpolation designs that need engine MVs + depth.

### Inferences
- **Industry vs research.** Every shipping FG product interpolates, because interpolation is far more robust than extrapolation. Vendors pay for that with Reflex/Anti-Lag queue reduction. The research front (GFFE, Predictive Sparse Shading, Amulet) is extrapolation, with explicit motivation from cloud gaming and high-refresh displays.
- **Fit for Cha Portal**
  - **Methods it could run generically:** DMVFN-class video extrapolation (about 20 ms at 1080p on a 4070 Ti Super, poor quality), RIFE/IFRNet interpolation, codec-MV warps (ANVIL), and rotation-only reprojection.
  - **Methods that need a game hook:** GFFE needs depth + MVs + camera from the game. Amulet needs geometry, so it is infeasible generically.
  - **Most portable high-quality option:** let the game's own DLSS/FSR/XeSS FG run *on the server* and stream the multiplied output (section 1C latency).
  - **Native macOS app:** MetalFX's interpolator is directly usable *if* the server streams MVs + depth. It would still impose the interpolation hold.
- **Open code.**
  - Open: FidelityFX SDK (FG), Practical-RIFE / vs-rife (VFI), lsfg-vk, and mc-ar-mod (reprojection shader).
  - Not found: GFFE, ExtraSS and Amulet code.
  - Not checked: ANVIL code.

### Gaps
- Runtime and quality of the TOG 2025 Predictive Sparse Shading paper.
- Amulet's per-frame cost on specific GPUs.
- Whether GFFE was formally published at a venue.
- XeSS-FG and FSR Redstone ML FG technical details (latency, inputs) were not re-checked in this pass.

## 6. Frame generation as loss and jitter concealment

### Takeaway
VR runtimes are the clearest shipping case of synthesizing frames when the source misses a deadline (ATW/ASW/AppSW, and cloud-XR clients that keep reprojecting when the stream stalls). For flat cloud gaming, no shipping client was found that synthesizes a flat frame instead of repeating one. Published concealment work is mostly MV recovery, frame copy, or codec-level resilience. For Cha Portal, one-slot extrapolation or warp on a late or lost frame is the cheapest FG feature with a positive latency effect: it lets the jitter buffer shrink. It should be capped to 1–2 consecutive slots before falling back to repeat-and-wait-for-recovery.

### Cited Findings
- Cloud-XR patents describe the client reprojecting at a higher rate than the server frame rate and continuing to reproject when the stream stalls. — [US 12439013](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12439013)
- Meta AppSW synthesizes every other frame from depth + MVs, and objects without MVs stutter. This is the same failure mode a concealment frame would have for unmodelled motion. — [Meta AppSW](https://developers.meta.com/horizon/documentation/native/android/mobile-asw/)
- A frame-interpolation concealment method for lost H.265 intra frames in cloud-based mobile streaming recovers the lost frame's MVs in real time. It reports about +2.5 dB over block matching and up to +12 dB over frame copy at 1–5% loss (abstract only). — [core.ac.uk record](https://core.ac.uk/works/16177590)
- A remote-gaming error-concealment patent replaces lost regions with co-located regions from earlier frames. — [US 12720113](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12720113)
- GRACE (NSDI 2024) trains a neural codec under simulated loss, as an alternative to decoder-side heuristic or neural concealment. — [GRACE paper](https://fyy.cs.illinois.edu/files/grace-nsdi24.pdf)
- Tencent's START cloud-gaming platform paper (NSDI 2026) attacks loss at the source: congestion control + RACK-TLP + opportunistic FEC, reaching a 0.049% packet loss rate. This indicates production systems prioritize transport fixes over synthesis. — [NSDI '26](https://www.usenix.org/conference/nsdi26/presentation/wang-jing)

### Inferences
- **Where it fits in Cha Portal.** The pipeline already does FEC + reference-frame invalidation, so a lost frame means a gap until the recovery frame decodes (at least one RTT plus encode). The client has F(n) and F(n−1) and can generate E(n+1) in three ways:
  - (a) a global-motion or rotation warp using mouse residuals;
  - (b) a coarse block-flow extrapolation from F(n−1)→F(n);
  - (c) the server-forwarded motion field of F(n).
- **Policy.** Synthesize for at most 1–2 slots, then hold. The longer the gap, the larger the snap-back when the recovery frame arrives.
- **Jitter (section 1D).** Extrapolate-on-late lets the playout target L sit near median arrival rather than P99, saving roughly (P99 − P50) of jitter. That buys more latency than interpolation could ever cost back.
- **Late frame after a synthetic one.** Either show it at the next slot (a time-consistent "catch-up") or drop it if the next frame is already decoded. Never show both out of order.
- **Partial frames.** Slice/tile-level concealment (warping only missing tiles from the previous frame) is possible with HEVC/AV1 tiles or PyroWave's independently decodable regions (unverified for PyroWave's packetization).

### Gaps
- No public evaluation of frame *extrapolation* as concealment in flat cloud gaming (QoE vs repeat) was found.
- No shipping flat-screen streaming client was confirmed to do this.

## 7. Desktop / remote-desktop content: does FG make sense for text and UI? Where to turn it off

### Takeaway
Mostly no. Every vendor FG design treats UI as a special case that must be separated from the scene. GFFE lists UI/particles as unhandled, FSR wants a HUD-less buffer and inpaints UI, and MetalFX has dedicated UI modes. A remote desktop stream is essentially all UI. Damage-based frame skipping, client-side cursor rendering and real frames at 60–120 fps already give the responsiveness that matters, and FG would only add text ghosting plus, for interpolation, latency. Turn FG off in desktop mode and gate it narrowly in game mode.

### Cited Findings
- GFFE: effects without depth, such as UI and particles, are not positioned correctly in extrapolated frames. — [GFFE](https://arxiv.org/html/2406.18551v1)
- FSR frame interpolation takes an optional HUD-less colour buffer and runs an inpainting pass to clean UI. It also includes scene-change detection to avoid interpolating across cuts. — [AMD FidelityFX FI docs](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/frame-interpolation/)
- MetalFX frame interpolation offers three UI strategies (Composited, Offscreen, Every-Frame UI). This shows vendors avoid interpolating UI pixels. — [WWDC25 session 211](https://developer.apple.com/videos/play/wwdc2025/211/)
- Meta AppSW does not support translucent content; content without MVs stutters. — [Meta AppSW](https://developers.meta.com/horizon/documentation/native/android/mobile-asw/)
- ANVIL's codec-MV smoothing over-smooths fine detail and edges, which is exactly where text lives. — [ANVIL](https://arxiv.org/html/2603.26835v2)

### Inferences
- **Desktop mode: off.**
  - Damage-driven updates mean most desktop "frames" are static or small-region changes.
  - Typing and clicking need latency, not smoothness, and the client-side cursor already decouples pointer motion from the stream.
  - Interpolated or extrapolated text produces doubled glyphs and wobble during scroll and drag.
  - A remote video player inside the desktop has its own native rate (24/30 fps). Interpolating it is TV-style "motion smoothing", which users often reject (unverified preference claim).
- **Possible exception, low priority: kinetic scrolling.** Smooth scrolling is a pure translation over a region. The *compositor* sees surface damage but not app-internal scroll offsets, so a reliable scroll vector is unavailable generically. Higher real fps on scroll is the simpler answer.
- **Game mode: gate FG.** Disable it, or fall back to repeat, when:
  - (1) a scene cut or fade is detected (borrow FSR's scene-change idea);
  - (2) the game shows a cursor or a menu, or mouse residuals don't correlate with global image motion (this also disables input reprojection);
  - (3) the base fps already ≥ display refresh (for example 120 fps stream on ProMotion 120 Hz);
  - (4) the base fps is very low (<30), where motion per frame defeats flow (vendor guidance that the acceptable *input* fps is unchanged for 3×/4×, via [GamersNexus](https://gamersnexus.net/gpus/fake-frames-tested-dlss-40-mfg-4x-nvidias-misleading-review-guide));
  - (5) client compute cannot meet C ≤ T/N (detect from present-time telemetry);
  - (6) competitive shooters, for interpolation specifically, because of the hold;
  - (7) bitrate is low enough that compression artifacts dominate the flow estimate.
- **Recommended priority for Cha Portal**, all inferences:
  - (i) Stream more real frames (120 fps on LAN) whenever the node can.
  - (ii) Add client-side late/lost-frame extrapolation, a 1–2 slot cap, synth flags in telemetry, and zero hold.
  - (iii) Add opt-in rotation-only input reprojection for mouse-look games. It needs `last_input_seq` in frame metadata, k/FOV calibration, a HUD mask and edge fill, and gives the biggest gain on WAN.
  - (iv) Offer opt-in client 2× interpolation (30→60, 60→120) for slow-paced games, accepting +T/2 + C.
  - (v) Pass through server-side in-game FG when the game provides it.
  - (vi) Research-track only: depth/MV side channel via a Vulkan-layer or upscaler-API hook, which would enable translation-aware warp, GFFE-style extrapolation, or MetalFX interpolation on the Mac app.

### Gaps
- No user study on frame generation for remote-desktop text/UI content was found. The recommendation rests on vendor UI-handling designs and known failure modes, not direct measurement.
