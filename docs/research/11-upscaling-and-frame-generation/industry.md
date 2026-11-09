# Industry practice: upscaling and frame generation in shipping game/desktop streaming products (state as of 2026-11-09)

> Evidence for [research note 11](README.md).

Scope note: product and practice inventory only. Each finding is dated. "(unverified)" marks claims from secondary sites, forum posts, or search snippets I could not open in full. Reddit/Steam forum material is a community-practice signal, not evidence of product behaviour. GitHub's API was not reachable from this session, so PR/release states come from rendered GitHub pages read on 2026-11-09.

## 1. NVIDIA GeForce NOW (2025–2026): Blackwell tier, DLSS 4/4.5 frame generation in the cloud, streamed frame rates, Cloud G-Sync, Reflex, Cinematic Quality Streaming, client-side upscaling

### Takeaway
GeForce NOW is the only major service that runs frame generation on the server and streams the generated frames. It does so as a premium feature, gated by tier: RTX 5080-class servers since September 2025, and DLSS 4.5 overrides with up to 4x dynamic MFG since about October 2026. NVIDIA deliberately capped streaming MFG at 4x. Client-side "AI-Enhanced" upscaling is an older (2022), NVIDIA-GPU-only feature that has seen little attention since. In 2025–2026 the visible client work has been codec choice, 4:4:4, HDR, VRR and new platforms, not new client SR.

### Cited Findings
**Blackwell / RTX 5080 tier (announced 2025-08-18, rollout from September 2025)**
- The Ultimate tier moved to "GeForce RTX 5080-class" Blackwell servers: 62 TFLOPS, a 48 GB frame buffer, AMD Zen 5 CPUs and ConnectX-7 SmartNICs. Pricing stayed at $19.99/month; Performance is $9.99/month. — [NVIDIA Newsroom, 2025-08-18](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
- Claimed stream modes: up to 360 fps at 1080p with Reflex, and click-to-pixel latency "as low as 30 milliseconds". Also up to 5K (5120×2880) at 120 fps on Windows and macOS, and 4K120 HDR on supported LG TVs. The native Steam Deck app gets 60–90 fps. NVIDIA says DLSS 4 Multi-Frame Generation is what enables 5K at 120 fps. — [NVIDIA Newsroom, 2025-08-18](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
- The "up to 2.8x faster" headline compares MFG 4x on the 5080 tier with FG 2x on the old 4080 tier, so much of the gain is generated frames rather than raw GPU speed. — [HWCooling, Aug 2025](https://www.hwcooling.net/en/blackwell-is-coming-to-geforce-now-also-relying-on-multi-frame-gen/); [PCGamesN, Aug 2025](https://www.pcgamesn.com/nvidia/geforce-now-rtx-5080)
- NVIDIA's 30 ms figure for the 360 fps mode was measured in Overwatch 2 on a 10 ms network. NVIDIA positions it as lower than a PS5 Pro rendering locally at 120 Hz (vendor claim, via secondary summary). — [search summary of TechPowerUp, Aug 2025](https://www.techpowerup.com/340035/nvidia-updates-geforce-now-with-rtx-5080-cinematic-streaming-new-low-latency-tech-and-more) (page returned 403; unverified)

**Cinematic-Quality Streaming (CQS) (announced Aug 2025)**
- CQS includes YUV 4:4:4 chroma, 10-bit HDR, "advanced AV1 encoders" that adapt to network speed, "new heads-up display sharpness" and "AI-powered video filters on select games". It also auto-detects laptop display resolution and streams at up to 100 Mbps. The release does not say whether the HUD sharpness and AI filters run on the server or the client. — [NVIDIA Newsroom, 2025-08-18](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)

**DLSS 4.5 overrides and MFG in the cloud (announced Gamescom, Aug 2026; rolled out around Oct 2026)**
- Club386 (2026-10-16) reports that DLSS 4.5 override controls now reach all GFN users, with more options for paid tiers. The overrides cover Super Resolution, Ray Reconstruction and Frame Generation. The SR and RR overrides cannot be active at the same time. RR and FG are limited to Performance (4080-class) and Ultimate (5080-class). Ultimate adds MFG, including "4x dynamic multi frame generation" that targets a set frame rate. — [Club386, 2026-10-16](https://www.club386.com/nvidia-4x-multi-frame-gen-geforce-now/)
- NVIDIA chose not to offer 5x/6x dynamic MFG on any tier. Club386 reports that NVIDIA's internal testing "showed no benefit for streaming players". On local PCs, DLSS 4.5 (CES 2026) supports up to 6x dynamic MFG for "smooth 240+ FPS". — [Club386, 2026-10-16](https://www.club386.com/nvidia-4x-multi-frame-gen-geforce-now/); [NVIDIA CES 2026 DLSS news](https://www.nvidia.com/en-gb/geforce/news/dlss-4-rtx-path-tracing-game-announcements-ces-2026)
- NVIDIA's recommended client setup for cloud FG: a G-Sync/VRR-capable GPU and monitor, Custom streaming mode at 90 fps or more, VSync set to Adaptive, Reflex on, and a game that supports both Reflex and DLSS FG. The status shows "Off" in menus and cutscenes. FG "can appear inactive if GeForce Now judges the current frame rate high enough". — [Club386, 2026-10-16](https://www.club386.com/nvidia-4x-multi-frame-gen-geforce-now/)
- Launch titles for the overrides include 007 First Light, Control Resonant and Resident Evil Requiem. — [Club386, 2026-10-16](https://www.club386.com/nvidia-4x-multi-frame-gen-geforce-now/)
- Reports conflicted at Gamescom about whether the DLSS 4.5 controls were shipping or coming "this fall". A later TweakTown piece says they shipped, with MFG limited to RTX 5080 Ultimate and capped at 4x dynamic. — [Pause Hardware, Aug 2026](https://pausehardware.com/geforce-now-gamescom-2026/); [TweakTown 114119](https://www.tweaktown.com/news/114119/dlss-4-5-and-dlss-overrides-roll-out-to-geforce-now-including-multi-frame-generation-for-rtx-5080-ultimate-subscribers/index.html) (403 on fetch; via search summary). NVIDIA's own Gamescom post: [NVIDIA blog, Aug 2026](https://blogs.nvidia.com/blog/geforce-now-thursday-gamescom-2026/)

**Cloud G-Sync and Reflex (2024)**
- Cloud G-Sync (announced CES 2024) varies the client display's refresh rate to match the streaming rate. NVIDIA describes it as reducing tearing and stutter, and lowering latency in Reflex games. It works on Windows and Mac with VRR displays, including G-Sync, VESA AdaptiveSync/FreeSync, HDMI 2.1 VRR and Apple ProMotion. — [NVIDIA blog, CES 2024](https://blogs.nvidia.com/blog/CES-2024-geforce-now-activision-blizzard-day-passes-g-sync); [AnandTech, 2024](https://www.anandtech.com/show/21292/variable-refresh-rate-support-comes-to-nvidias-geforce-now-cloud-streaming-service)
- Sources conflict on Reflex at 240 fps. NVIDIA described a "Reflex 240-fps mode". TweakTown reported that Reflex applied at 60/120 fps but not 240 fps (2023–2024 coverage). — [TweakTown 96723](https://www.tweaktown.com/news/96723/nvidia-geforce-now-gets-sync-and-reflex-so-its-nearly-indistinguishable-from-local-pc/index.html)

**Client-side upscaling (older; still present)**
- In January 2022 (client v2.0.37), GFN added three upscaling modes: Standard, Enhanced and AI-Enhanced. NVIDIA's partner blog says Enhanced "provides a higher quality upscale, but may cause some latency depending on your system specifications". AI-Enhanced uses a trained network and needs select NVIDIA GPUs or a SHIELD TV. — [NVIDIA GFN Thursday, Jan 2022](https://blogs.nvidia.com/blog/geforce-now-thursday-january-27/); [cloud.gg partner blog, Jan 2022](https://cloud.gg/blog/this-week-on-gfn-28-january-2022/); [Softonic](https://en.softonic.com/articles/nvidia-geforce-now-ai-upscaling)
- Reception: Tom's Hardware headlined it "GeForce Now's New AI Image Upscaler Is No DLSS Replacement" (2022). The search summary says the reviewer found it worse-looking than DLSS in all modes; I could not open the article body. — [Tom's Hardware](https://www.tomshardware.com/news/geforce-now-ai-image-upscaler)
- Release notes (2.0.43) say the Enhanced mode later gained NVIDIA Image Scaling (NIS) and sharpening. I found no entry for RTX Video Super Resolution in the GFN client or browser. — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)

**2025–2026 client changes (GFN release highlights; dates partly inferred)**
- 2.0.62: AV1 streaming on macOS with M3-series GPUs. 2.0.75: 240 fps mode for Windows clients with AMD GPUs. 2.0.76: 120 fps streaming "now adapts to follow your game's FPS" on the Windows and macOS apps. 2.0.81 (CES 2026): native Linux app beta. 2.0.83: "Optimal Latency" network-adaptation setting, Vision Pro at up to 4K/90 fps, and H.265 decoding "rolling out to browser clients on select platforms". 2.0.85: "Members can now manually select the streaming codec". — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- Other 2026 additions reported: a native Fire TV app (Feb 2026), Linux beta (Ubuntu 24.04+), Firefox support, GOG integration, and Steam Machine/Steam Controller support (Gamescom 2026). — [GamingOnLinux, Aug 2026](https://www.gamingonlinux.com/2026/08/steam-machine-and-steam-controller-support-coming-to-nvidia-geforce-now-plus-dlss-4-5-ray-reconstruction-out-now-for-desktop-gamers/); search summary of [aitoolly, 2026-08-28](https://aitoolly.com/en/ai-news/article/2026-08-28-nvidia-geforce-now-expands-cloud-gaming-ecosystem-at-gamescom-2026-with-dlss-45-and-new-device-suppo) (secondary)

**Reception of cloud MFG**
- Digitec (Swiss retailer editorial) reports that "RTX 5080 streaming with MFG 4× brings significantly more lag than the older cloud gaming", while MFG 2× was "still acceptable". — [Digitec](https://www.digitec.ch/en/page/trying-out-geforce-now-with-rtx-5080-performance-39272) (page returned 503 twice; quotes come from the search snippet; unverified)
- A Tom's Guide reviewer (reposted on NeoGAF) found that fears of MFG latency stacking on network delay were "quickly extinguished". This is an anecdotal impression. — [NeoGAF repost of Tom's Guide](https://www.neogaf.com/threads/toms-guide-i-tested-nvidia-geforce-now%E2%80%99s-rtx-5080-upgrade-%E2%80%94-it-made-my-750-ps5-pro-feel-old.1687518/)

### Inferences
- NVIDIA treats cloud FG as a host-side, engine-integrated feature: DLSS FG uses game motion vectors and Reflex. The client stays a plain decoder; the levers it does use are VRR (Cloud G-Sync) and high stream fps. NVIDIA's client-side SR (AI-Enhanced/NIS) is legacy and limited to NVIDIA hardware. Mac and browser clients get no NVIDIA SR, consistent with NVIDIA not shipping client SR on non-NVIDIA silicon.
- Three choices together say that streaming more real or generated frames only pays off when the client can display them at VRR: the 4x cap, the 90 fps minimum and the VRR/Reflex recommendation. Above a certain multiplier, extra generated frames add bandwidth and latency without perceived benefit.
- "HUD sharpness" and "AI-powered video filters" in CQS suggest some image processing aimed at text and HUD legibility, but NVIDIA does not say where it runs.

### Gaps
- No independent measured latency (for example Digital Foundry) of GFN cloud MFG 2x/4x versus FG off was found. I could not find a Digital Foundry GFN 5080 cloud test.
- I could not confirm whether CQS "AI-powered video filters" run on the server before encoding or in the client, or which games they cover.
- I found no confirmation of Reflex at 360 fps beyond NVIDIA's claim. Many release-note entries are undated.

## 2. Xbox Cloud Gaming: Clarity Boost and 2025–2026 changes (1440p, bitrate, codecs, client-side SR)

### Takeaway
Microsoft's 2025–2026 story is "more bits and more pixels from the server": 1440p streams and higher bitrate, which reached consoles in February 2026. No new upscaling or frame generation was announced. Clarity Boost remains a 2021 Edge-only, client-side spatial enhancement with no evidence of change since.

### Cited Findings
- On 2026-02-25 Microsoft said Game Pass Ultimate members can stream "at up to 1440p resolution" with "higher bitrate streaming" on Xbox Series X|S, One X and One S. It was "joining the Xbox PC app, handhelds, supported browsers, select Fire TV devices, LG TVs, and Samsung TVs", which already had it. No bitrate figures, codecs, upscaling or frame rates were stated. — [Xbox Wire, 2026-02-25](https://news.xbox.com/en-us/2026/02/25/february-xbox-update-1440p-streaming-rog-xbox-ally-updates-and-more/)
- Wikipedia says the Xbox Cloud Gaming beta ended on 2025-10-01, alongside a Game Pass plan restructure. — [Wikipedia: Xbox Cloud Gaming](https://en.wikipedia.org/wiki/Xbox_Cloud_Gaming)
- A secondary report says peak bitrate rose from 10 to 27 Mbps. Microsoft does not state this. — [Gadget Hacks](https://cord-cutters.gadgethacks.com/news/xbox-game-pass-adds-1440p-streaming-to-consoles/) (unverified)
- Clarity Boost launched in Edge Canary 96 in late 2021 and reached general Edge in 2022. It is a client-side image enhancement available only in Edge; Chrome and Safari stream without it. — [PC Gamer, 2021](https://pcgamer.com/xbox-cloud-gaming-gets-an-image-quality-boostbut-only-on-edge); [TechRadar](https://www.techradar.com/news/xbox-cloud-gaming-update-gives-us-a-great-reason-to-use-microsoft-edge); [Tom's Guide](https://www.tomsguide.com/news/theres-finally-a-reason-to-use-microsoft-edge-instead-of-chrome)

### Inferences
- Microsoft's server fleet runs console titles, and console titles do their own in-engine upscaling and frame pacing. Microsoft's lever is stream resolution and bitrate, not service-level FG. Clarity Boost shows that a browser-vendor-specific client SR path is feasible, but Microsoft never extended it to Chrome, Safari or consoles.

### Gaps
- I found no 2025–2026 primary source on Clarity Boost's status (still shipping, changed or removed), on Xbox Cloud codecs (H.264 vs HEVC/AV1), or on any client SR on consoles or TVs.
- I found no measured latency or quality comparison of 1080p versus 1440p xCloud.

## 3. Other commercial services: Sony (PS5 Pro PSSR, PS Portal, PS Plus cloud), Valve Steam Remote Play/Steam Link (PyroWave), Shadow, Parsec, Boosteroid, Amazon Luna, others

### Takeaway
Apart from GFN, none of these services ships upscaling or frame generation as a streaming feature. Sony keeps PSSR for local PS5 Pro rendering and improves the Portal stream with bitrate. Valve's 2026 move is the opposite of upscaling: PyroWave, a high-bandwidth (100 Mbit/s–1 Gbit/s) intra-only wavelet codec with optional 4:4:4, for wired LANs. Shadow exposes DLSS inside a full Windows VM, so users run FG themselves; Parsec offers 4:4:4/10-bit.

### Cited Findings
**Sony**
- PlayStation Portal software update (rolling out from 2026-03-18) added a "1080p High Quality" mode. It keeps 1080p and raises bitrate for Remote Play and Cloud Streaming. Sony recommends at least 15 Mbps; its support page lists 13 Mbps minimum for 1080p. Cloud streaming requires PS Plus Premium. Sony reported cloud-streaming monthly users up 162% year-on-year (January 2026). — [PlayStation Blog, 2026-03-17](https://blog.playstation.com/2026/03/17/new-playstation-portal-software-update-adds-1080p-high-quality-mode-and-ux-refinements/); [PlayStation support](https://www.playstation.com/en-us/support/subscriptions/psportal-cloud-game-streaming/)
- PS5 Pro received an upgraded PSSR built on AMD FSR 4 technology. It debuted with Resident Evil Requiem on 2026-02-27, and a March 2026 system update added a global "Enhance PSSR Image Quality" toggle for 100+ PSSR games. Sony says it is an updated PSSR, not FSR 4 itself, and it is PS5 Pro-only. — [TweakTown 107703](https://www.tweaktown.com/news/107703/ps5-pro-gets-new-pssr-upgrade-in-2026-not-amd-fsr-4-support/index.html); [HotHardware](https://hothardware.com/news/sony-upgrades-ps5-pros-pssr-upscaler-in-time-for-resident-evil-requiem)
- I found no source showing PSSR applied to Remote Play or cloud streams, on the server or the Portal client.

**Valve Steam Remote Play / Steam Link**
- On 2026-09-21 the Steam Client Beta added "an experimental video codec, Pyrowave, which allows high bandwidth, low latency video streaming". It is off by default and covers Windows and macOS beta clients, Linux via the experimental SteamRT3 client, and SteamOS. The Steam Link mobile app is "coming soon". It is not for Steam Link VR (flat-screen Remote Play only). — [GamingOnLinux, 2026-09-22](https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/)
- PyroWave uses about 5–10x the bandwidth of other codecs. Valve recommends wired Gigabit Ethernet. Fixed bitrate is 100–500 Mbit/s, and a 2026-10-01 beta raised the manual ceiling to 1 Gbit/s with the note that "more bandwidth increases latency". YUV 4:4:4 is optional and off by default, suggested for desktop and text use. Encode and decode run on the GPU. Valve warns that some setups won't see lower latency because network and display latency still count. — [GamingOnLinux, 2026-09-22](https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/); [Mixed News, Oct 2026](https://mixed-news.com/en/steam-client-beta-pyrowave-1-gbit-manual-bitrate-reset-latency/); [VideoCardz](https://videocardz.com/newz/steam-remote-play-gets-pyrowave-codec-with-up-to-500-mbit-s-streaming-and-yuv-444)
- The PyroWave beta coverage mentions no upscaling or frame generation in Remote Play. — [GamingOnLinux, 2026-09-22](https://www.gamingonlinux.com/2026/09/steam-beta-adds-experimental-new-pyrowave-video-codec-for-remote-play/)

**Shadow (full cloud Windows PC)**
- Shadow's blog (2025-10-25) says Shadow "refreshed all Shadow's GPUs in recent months" and that "every Shadow plan now supports DLSS!" and ray tracing. New PCs ship with Windows 11 from 2025-10-14. The post gives no GPU models, codec, fps caps or client upscaling. — [Shadow blog, Oct 2025](https://shadow.tech/blog/windows-11-ray-tracing-and-dlss-whats-new-at-shadow-october-2025/)
- Third-party listing: Neo is $37.99/month and Power is $54.99/month. Shadow says it does not cap resolution (4K possible). — [tech-insider (secondary)](https://tech-insider.org/shadow-pc-vs-geforce-now-2026/) (unverified)

**Parsec**
- Client-side 4:4:4 requires enabling H.265, then "Prefer 4:4:4 Color" and "Constant FPS"; 10-bit color is available for higher accuracy. The latest changelog entry is 2026-10-08 (bugfixes). No upscaling or FG feature was found. — [Parsec support](https://support.parsec.app/hc/en-us/articles/32381785123860-Improve-Stream-Quality-and-Color-Accuracy); [Parsec changelog](https://parsec.app/changelog)

**Boosteroid**
- In February 2026 Boosteroid added a streaming-resolution picker to its Windows app, according to a search snippet (the Boosteroid blog pages redirect-looped). No upscaling or FG was found. — [Boosteroid blog](https://boosteroid.com/blog/?p=8932) (unverified)

### Inferences
- Console and handheld vendors (Sony, Valve) put upscaling in the game renderer (PSSR, FSR/gamescope) and improve the stream with bitrate or codec, not stream SR. Valve's PyroWave bets that on wired LANs, more bits per frame beats reconstruction. That matches Cha Portal's own PyroWave use.
- "Full VM" services (Shadow, Parsec, Cha Portal-like desktops) leave in-game DLSS/FSR FG to the user. The service's job then becomes capturing and streaming the generated frames correctly (see Vibepollo in section 4).

### Gaps
- Amazon Luna: no 2025–2026 primary source found on resolution, codec, upscaling or FG. One forum anecdote says a Luna stream was detected as 60 fps (section 5).
- Blacknut, Utomik and other smaller services: not researched within budget; no findings.
- No Boosteroid or Shadow primary specs for encoder settings or client post-processing.

## 4. Open-source hosts and clients (Sunshine, Apollo, Vibepollo, Punktfunk, LuminalShine; Moonlight-qt/iOS/Android, forks, moonlight-web-stream; Selkies, Nestri, Wolf)

### Takeaway
As of November 2026, no official Moonlight client ships upscaling or frame interpolation. A cross-platform "Video Super Resolution" PR series by linckosz has been open upstream since March 2025, covering RTX VSR, AMF, FSR1, MetalFX, SGSR and iOS MetalFX, with no merge. Client SR and FG exist only in forks; one fork does MetalFX frame interpolation on macOS 26. Host-side, the frontier is virtual displays at the client's native resolution plus correct capture and pacing of game-generated frames (Vibepollo), not server-side upscaling of the stream.

### Cited Findings
**Moonlight-qt (official)**
- v6.2.0 was released 2026-10-06, the first release since v6.1.0 (2024-09-17). Its changes: D3D11 VRR on NVIDIA; a KMSDRM rewrite with atomic modesetting, overlays, VRR and ALLM; AV1 hardware decode on macOS (M3+); HDR metadata tonemapping on macOS; YUV 4:4:4 on DXVA2; and Vulkan rendering outside a desktop environment. Upscaling and super resolution are not mentioned. v6.1.0 added "Experimental YUV 4:4:4 support for improved text clarity during remote desktop usage". — [moonlight-qt releases](https://github.com/moonlight-stream/moonlight-qt/releases); [releases.atom](https://github.com/moonlight-stream/moonlight-qt/releases.atom)
- PR #1557 "Add Video Super Resolution for Windows x64 (AMD, Intel and NVIDIA), Windows ARM (Snapdragon), Linux and MacOS" (linckosz) has been open since 2025-03-15 and replaces the earlier #1180. Backends: NVIDIA VSR via a D3D12 video processor, AMD AMF Video Upscaling, an FSR1 shader on Intel (oneVPL didn't work), and the MetalFX spatial scaler on macOS (tested M1 Pro/M3 Pro), with no sharpening control. The author reports added latency under 1 ms typical and 2 ms at most, at 1440p120 and 150 Mbps on discrete GPUs. An Intel N95 iGPU tops out at 1440p60. NVIDIA disables the enhancement under HDR, and AMD in HDR falls back to FSR 1.0 without sharpening. No maintainer merge decision is visible. — [moonlight-qt PR #1557](https://github.com/moonlight-stream/moonlight-qt/pull/1557); [PR #1180](https://github.com/moonlight-stream/moonlight-qt/pull/1180)
- An FSR request in issue #647 dates to the earlier era. — [moonlight-qt issue #647](https://github.com/moonlight-stream/moonlight-qt/issues/647)

**Moonlight iOS/tvOS, Android, Xbox (official repos)**
- moonlight-ios PR #704 "Video Super Resolution for Moonlight iOS/tvOS" has been open since 2026-04-07. It implements a MetalFX spatial upscaler via Metal and Core Image, preserving HDR and color attachments. The author measured about +20% power on an iPhone 13 Pro at 720p60: battery use 15% without VSR versus 18% with it. There are no maintainer reviews. A fork commit (bartleman, 2026-08-17) ports it to tvOS. — [moonlight-ios PR #704](https://github.com/moonlight-stream/moonlight-ios/pull/704)
- moonlight-android PR #1567 "Add Video Super Resolution" has been open since 2026-04-16. It uses Snapdragon Game Super Resolution v1 on GLES 3.1 with a two-pass OES→2D blit, and sets `eglSwapInterval(0)`. Author-measured decode-to-display latency: Mali-G57 phone 9→12 ms (+3 ms); Mali-G31 TV 12→26 ms (about 2x). The author suggests TV-class devices may need a fallback. There are no maintainer reviews. moonlight-xbox PR #267 (VSR) is closed. — [moonlight-android PR #1567](https://github.com/moonlight-stream/moonlight-android/pull/1567)

**Client forks with SR/FG**
- AyberkA0/moonlight-macos-framegen, release v6.1.0-metalfx.1 (published 20 Sep; year not shown, but it requires macOS 26, so 2026): MetalFX spatial upscaling with presets of 0.4x, 0.5x and 0.75x, where the host streams lower resolution and the Mac upscales. Frame generation uses `MTLFXFrameInterpolator` on macOS 26+: "the host sends half the frame rate" and the Mac interpolates. Motion comes from VideoToolbox motion estimation plus a vector-median filter and a UI mask to protect the HUD, since it "works from 2D video rather than engine motion vectors". An alternative Apple VT low-latency frame-interpolation engine is limited to a 720p pixel budget and gives softer output. Known issues: wrong edge pixels in fast motion, and on 60 Hz displays it halves real frames, so it only helps if the host cannot reach 60. Builds are unsigned and not notarized. No latency figures are given. — [GitHub release](https://github.com/AyberkA0/moonlight-macos-framegen/releases/tag/v6.1.0-metalfx.1)
- Nonary/moonlight-qt "VRR" fork, v6.1.0-vrr18 (about October 2026): a PyroWave decoder on Windows (D3D11) and Linux (libplacebo Vulkan), requiring a matching "Vibeshine" host. Damaged frames are shown with blur rather than dropped. VRR pacing presets: Low Latency up to half a frame time (about 4.17 ms at 120 fps), Balanced up to one frame time, Smooth up to 24 ms. On Steam Deck at 4K 4:4:4, median present-to-flip fell from 3.9 to 2.0 ms. No upscaling or FG. — [newreleases.io mirror of v6.1.0-vrr18](https://newreleases.io/project/github/Nonary/moonlight-qt/release/v6.1.0-vrr18)
- MoonCastDev/moonlight-android PR #89 adds FSR 1.0 with sharpening for PC, PS5 and Xbox targets (shaders tested only off-device). — [PR #89](https://github.com/MoonCastDev/moonlight-android/pull/89) (status unverified)
- derflacco's "Artemide" Android fork added FSR (improved Balanced/Performance scaling), then a later entry "Removed FSR EASU upscaling". — [derflacco/moonlight-android releases](https://github.com/derflacco/moonlight-android/releases) (ordering and current state unverified)

**Hosts**
- Apollo (Sunshine fork, about 11.5k stars) has a built-in virtual display that "matches the resolution/framerate config of your client automatically" (Windows only; Linux planned). Its Artemis Android client controls the virtual display. No upscaling or frame interpolation. The README warns HDR "is generally not recommended with any streaming solution right now". — [Apollo README](https://github.com/ClassicOldSong/Apollo)
- Vibepollo (Apollo fork, Nonary): "DLSS/FSR game-provided frame generation requires Vibepollo's virtual screen for reliable capture", and WGC in Service Mode "captures the full frame rate of frame-generated titles". It "can automatically apply optimal Lossless Scaling settings to generate frames for any application", can enable NVIDIA Smooth Motion on RTX 40+, manages RTSS so "the applied frame cap matches the client device's requested FPS", and "targets 4x virtual refresh for pacing" of frame-generated capture. With the VRR Moonlight fork, the virtual display switches to a fixed 1000 Hz mode. A Linux beta needs kernel 6.16+, with HDR virtual displays, and supports PyroWave except on the NvFBC path. Up to 8 clients each get their own virtual screen. No upscaling. — [Vibepollo README](https://github.com/Nonary/Vibepollo)
- Punktfunk (Rust, Linux/Windows host with its own protocol plus GameStream for Moonlight) gives each device "a virtual display at its own resolution and refresh rate". Its config has a game frame limit of 0–240 fps and a display refresh multiple of 0–4x. The Linux client is Vulkan (Vulkan Video/VAAPI). No FG or upscaling setting was found. — [Punktfunk docs](https://docs.punktfunk.unom.io/docs); [config](https://docs.punktfunk.unom.io/docs/configuration); [Punktfunk vs Sunshine](https://punktfunk.unom.io/en/blog/punktfunk-vs-sunshine-moonlight)
- LuminalShine 26.08.3 (NortheBridge, about Aug–Sep 2026) reports "3456×2160@120 YUV 4:4:4 10-bit HDR: 119 fps sustained" with bitrate-aware wire pacing. No upscaling or FG. — [newreleases.io mirror](https://newreleases.io/project/github/NortheBridge/luminalshine/release/26.08.3)
- moonlight-web-stream (a browser Moonlight client) bridges Sunshine to the browser over WebRTC, or over WebSocket with WebCodecs `VideoDecoder` (HTTPS required). No upscaling, sharpening or interpolation. — [moonlight-web-stream README](https://github.com/MrCreativ3001/moonlight-web-stream)

### Inferences
- Upstream Moonlight's lack of action on SR PRs (about 20 months for #1557, about 7 months for iOS/Android, xbox closed) suggests a policy or bandwidth decision rather than a technical block. The likely reasons, inferred from the PRs: vendor-specific code paths, HDR incompatibility (RTX VSR off in HDR), battery cost on mobile, and doubled latency on weak TV SoCs. It is not confirmed by maintainers.
- The only shipping client-side FG on Apple (the framegen fork) has to estimate motion from decoded video and mask the UI. That is the HUD/text artifact problem, built in, and the reason engine-side FG on the host is preferred when available.
- Host projects have converged on "render at the client's native resolution via per-client virtual displays" (Apollo, Vibepollo, Punktfunk). That removes most of the need for client upscaling and for host-side "render low and let the client scale" tricks. The remaining open problem they solve is pacing of FG output (Vibepollo's 4x virtual refresh, the 1000 Hz VRR virtual display).

### Gaps
- Sunshine (upstream LizardByte) 2025–2026 release notes were not checked within budget; no evidence either way of SR/FG work.
- Selkies, Nestri and Games on Whales/Wolf: searches returned nothing on upscaling or FG; I could not confirm their 2026 feature state.
- Polaris (host) and the Switch and webOS Moonlight ports: not researched; no findings.
- No independent measurements of any of the fork features above; all numbers are author-reported.

## 5. Community practice: Lossless Scaling with Moonlight, host-side DLSS/FSR FG streamed at high fps, Steam Deck/Linux (gamescope FSR, lsfg-vk)

### Takeaway
Two community patterns exist. (a) Run FG on the host (DLSS/FSR FG, Lossless Scaling, Smooth Motion) and stream at high fps; this needs specific capture modes (WGC plus VSync, or virtual displays) for generated frames to reach the encoder at all. (b) Run Lossless Scaling or lsfg-vk on the client; users report added latency and a need for VSync. Neither pattern has rigorous published measurements; evidence is forum-level.

### Cited Findings
- Steam forum (community signal): with Apollo→Moonlight, generated frames "do not get sent over unless capture mode is set to WGC AND sync mode is set to Vsync". Others report Moonlight stats rising from 60 to 120 fps with LS on the host, or doubt that LS applies at all. — [Steam Community: Lossless Scaling](https://steamcommunity.com/app/993090/discussions/0/4355625227127211665) (anecdotal)
- Steam forum (community signal): running LS on the Moonlight client "will add LS latency to network latency", and stutter is bad unless client VSync is used, which adds more latency. — [Steam Community: Lossless Scaling](https://steamcommunity.com/app/993090/discussions/0/4355625227127211665) (anecdotal)
- Steam forum (community signal): on Amazon Luna, the stream "gets recognized as 60 FPS", so client LS could not make a 30 fps game smoother. — [Steam Community: Lossless Scaling](https://steamcommunity.com/app/993090/discussions/0/4334231144975059487) (anecdotal)
- LSFG 3 average latency improved about 24% over LSFG 2, per community benchmarks by CptTombstone. The developer recommends unlocked multipliers only with a locked base framerate. — [Yahoo Tech](https://tech.yahoo.com/general/articles/lossless-scaling-3-touts-greatly-132457190.html)
- lsfg-vk 2.0 on Steam Deck (plugin 0.14.1, Sept 2026): Cyberpunk 2077 held 60 fps at 3x "with little noticeable lag", while Crimson Desert showed missed frames, lag and stutter ("hit and miss"). An experimental build reduced ghosting but not latency. — [GamingOnSteam, 2026-09-13](https://gamingonsteam.com/2026/09/13/lsfg-vk-2-0-on-steam-deck-official-plugin-update-and-install-guide/); [WindowsForum](https://windowsforum.com/news/steam-deck-lsfg-vk-experimental-fixes-ghosting-not-latency.442030/)
- Steam Deck Moonlight tips (community signal): match the stream to the Deck's 1280×800 panel, or stream higher (1920×1200) for downsampled clarity. MoonDeck's wiki says gamescope's Moonlight display resolution should be at least native. — [MoonDeck Buddy wiki](https://github.com/FrogTheFrog/moondeck-buddy/wiki/Troubleshooting); [Steam Community](https://steamcommunity.com/app/1675200/discussions/0/4695657408659687063)
- Productised host-side FG: Vibepollo automates LS settings, Smooth Motion and RTSS caps matched to client fps, and requires its virtual display for reliable capture of DLSS/FSR FG. — [Vibepollo README](https://github.com/Nonary/Vibepollo)

### Inferences
- Host-side FG streamed as real frames is the more viable pattern. The client stays a dumb decoder, and the FG gets engine motion vectors (DLSS/FSR) or at least full-resolution, pre-compression frames (LS). The catch is capture correctness: generated frames bypass some capture paths, such as DXGI duplication without VSync. Encoder bitrate also scales with fps.
- Client-side FG on decoded video stacks the interpolation delay on top of network jitter buffering, and needs VSync for even pacing. That is why community reports converge on "adds lag".

### Gaps
- No controlled latency measurements (photodiode or click-to-photon) of Moonlight with host-side versus client-side LSFG.
- No data on battery cost of client-side LSFG or lsfg-vk on Deck while streaming.

## 6. Published measurements comparing "render native + stream" vs "render low + upscale on host" vs "stream low + upscale on client", and "stream more real frames" vs "frame-generate"

### Takeaway
I found no published, controlled study that directly compares these pipelines for game streaming. The only numbers are vendor claims (NVIDIA's 30 ms click-to-pixel at 360 fps), PR-author microbenchmarks for client SR (+<1–3 ms on capable GPUs, about +14 ms on a weak TV SoC, about +20% power on iPhone), and general cloud-gaming latency studies. Those studies show that client-side display and VSync stages, not decode, dominate non-network latency.

### Cited Findings
- An NSDI 2025 month-long measurement of eight cloud gaming platforms found VSync waits the largest contributor (35.7% on average) to non-network latency. — [Li et al., NSDI 2025](https://www.usenix.org/system/files/nsdi25-li-yang.pdf) (figure via search summary)
- VR cloud gaming (EyeNexus, 2025): average latency rose with resolution, from 69 to 74 to 87 ms on WiFi, attributed mainly to the headset's limited client resources. — [arXiv 2509.11807](https://arxiv.org/pdf/2509.11807)
- Client SR microbenchmarks (author-reported): RTX/AMF/MetalFX on desktop at <1 ms typical and 2 ms max at 1440p120 [moonlight-qt #1557](https://github.com/moonlight-stream/moonlight-qt/pull/1557); SGSR on Android at +3 ms on a phone and 12→26 ms on a TV SoC [moonlight-android #1567](https://github.com/moonlight-stream/moonlight-android/pull/1567); MetalFX on iPhone 13 Pro at +20% power [moonlight-ios #704](https://github.com/moonlight-stream/moonlight-ios/pull/704).
- Server FG versus more real frames: NVIDIA's own comparison is MFG 4x (5080 tier) against FG 2x (4080 tier). No FG-off baseline for latency is published. — [HWCooling](https://www.hwcooling.net/en/blackwell-is-coming-to-geforce-now-also-relying-on-multi-frame-gen/)
- Bandwidth versus latency at the extreme: Valve says raising PyroWave bandwidth to 1 Gbit/s "increases latency", which signals a sweet spot rather than "more bits is always better". — [Mixed News, Oct 2026](https://mixed-news.com/en/steam-client-beta-pyrowave-1-gbit-manual-bitrate-reset-latency/)

### Inferences
- From the cited numbers, client SR on capable hardware costs latency within a few ms, small next to network and VSync. The real costs are power on mobile, HDR incompatibility, and inconsistent quality across vendors. "Stream low + upscale on client" mainly saves bandwidth, which matters on WAN and Wi-Fi but little on the wired-LAN PyroWave path.
- "Stream more real frames" with VRR is what both NVIDIA (90 fps+ VRR guidance, 4x cap) and Valve (PyroWave, high bandwidth) invest in. FG is offered on top only where engine motion vectors exist.

### Gaps
- No Digital Foundry or academic photon-latency comparison of these pipelines was found. This is a real evidence gap, not a search miss I could close within budget.

## 7. Lessons: user complaints and what services chose not to do, and why

### Takeaway
The recurring problems are added input lag from FG, especially at high multipliers and stacked on network latency. Others are artifacts on HUD and text when interpolating from video, the battery and power cost of client SR on mobile, HDR conflicts, and weak TV SoCs. Vendors' explicit "no" decisions are NVIDIA capping cloud MFG at 4x and gating FG behind high-fps VRR setups. Microsoft and Sony chose not to add stream SR at all. Upstream Moonlight has not merged client SR.

### Cited Findings
- NVIDIA skipped 5x/6x dynamic MFG on GFN because internal testing "showed no benefit for streaming players", and FG can auto-disable when the base fps is high enough. — [Club386, 2026-10-16](https://www.club386.com/nvidia-4x-multi-frame-gen-geforce-now/)
- Reviewer complaint: MFG 4x in the cloud has "significantly more lag"; 2x is acceptable. — [Digitec](https://www.digitec.ch/en/page/trying-out-geforce-now-with-rtx-5080-performance-39272) (unverified, snippet only)
- HUD and text: the macOS framegen fork needs a UI mask and still shows edge errors in fast motion. NVIDIA's CQS advertises "new heads-up display sharpness", implying HUD legibility was a known issue. — [moonlight-macos-framegen release](https://github.com/AyberkA0/moonlight-macos-framegen/releases/tag/v6.1.0-metalfx.1); [NVIDIA Newsroom](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
- Battery: about +20% power for MetalFX SR on iPhone. — [moonlight-ios #704](https://github.com/moonlight-stream/moonlight-ios/pull/704)
- HDR: NVIDIA RTX VSR is disabled under HDR, and AMD's HDR path is degraded. — [moonlight-qt #1557](https://github.com/moonlight-stream/moonlight-qt/pull/1557). Apollo advises against HDR streaming generally. — [Apollo README](https://github.com/ClassicOldSong/Apollo)
- Weak clients: SGSR roughly doubles latency on a Mali-G31 TV. — [moonlight-android #1567](https://github.com/moonlight-stream/moonlight-android/pull/1567)
- Client FG on 60 Hz displays halves real frames, so it only helps when the host is below 60 fps. — [moonlight-macos-framegen release](https://github.com/AyberkA0/moonlight-macos-framegen/releases/tag/v6.1.0-metalfx.1)
- GFN's 2022 client SR was criticised as "no DLSS replacement". — [Tom's Hardware](https://www.tomshardware.com/news/geforce-now-ai-image-upscaler)

### Inferences
- For Cha Portal, the industry pattern maps to these placements:
  - FG goes on the host, inside the game via DLSS/FSR FG in Proton where available, with capture and pacing that delivers generated frames (Vibepollo-style).
  - SR is an optional client filter: MetalFX on the native macOS app, cheap shaders in the browser. It is off by default and disabled for HDR and desktop/text sessions.
  - On wired LAN with PyroWave, prefer native-resolution virtual displays and more real frames over either technique.
- Nobody among the major services ships browser-side SR except the legacy Edge-only Clarity Boost. A Chrome/Safari client SR would be ahead of the industry, not catching up. Justify it by bandwidth savings on WAN, not latency.

### Gaps
- No systematic user-survey or telemetry data on FG adoption or complaints in GFN. Reddit threads were not reachable via search results.
- No statement from Moonlight maintainers explaining the non-merge of SR PRs.
