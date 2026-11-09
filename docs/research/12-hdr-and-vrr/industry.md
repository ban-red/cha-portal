# HDR and VRR in shipping game-streaming and remote-desktop products (state as of 2026-11-09), and the evidence on VRR vs fixed-refresh presentation

> Evidence for [research note 12](README.md).

Scope: industry slice only. It builds on, and does not repeat, `docs/research/03-moonlight-sunshine-ecosystem.md` (GameStream HDR wire format: `hdrMode`/`dynamicRangeMode`, the 0x010e HDR metadata packet, Vibepollo `clientVrrRequested`), `08-adjacent-platforms-2026.md` (Steam PyroWave beta, Punktfunk, Polaris, Nestri) and `11-upscaling-and-frame-generation/industry.md` (GFN Blackwell/CQS, cloud FG guidance, Nonary VRR client presets, NSDI 2025 headline).

Labels used: **[vendor]** = the company's own page or release notes; **[dev]** = a project's own code, docs or maintainer; **[user]** = a single user's issue or forum post; **[press]** = journalism repeating vendor material; **[independent]** = third-party measurement or peer-reviewed work; **(via search summary)** = I saw only a search-engine extract, not the page; **(unverified)** = not confirmed by a second source.

---

## 1. GeForce NOW: HDR, Cloud G-Sync/VRR, Reflex, 240/360 fps, latency

### Takeaway
GFN is the only service with a full HDR + VRR story, and both are limited to its own native apps. HDR (HDR10, 10-bit, HEVC/AV1) works on the native Windows and macOS apps, SHIELD TV, Windows-on-Arm and LG TVs. Browsers, Chromebooks and Samsung/LG browser clients get 10-bit SDR ("Color precision"), not HDR. Cloud G-Sync (March 2024) works only in the native Windows and macOS apps. The server stops forcing a fixed cadence: the stream rate follows the game's frame rate, and the client's VRR display refreshes when each frame arrives. NVIDIA has published no latency numbers comparing VRR on and off. Its only figure is "as low as 30 ms" click-to-pixel in 360 fps Reflex mode.

### Cited Findings
**HDR and 10-bit: clients and tiers**
- System requirements page (current), Windows: a paid tier (the page says "Premium") is required for "5K/4K/UW streaming resolutions, NVIDIA RTX, HDR, Cloud G-SYNC" on the native app. The macOS native app has the same list. The **Linux native app's list omits HDR** ("5K/4K/UW streaming resolutions, NVIDIA RTX, and 120/240/360 FPS streaming"). SHIELD TV needs Ultimate for "4K resolution and HDR". Regional copies of the page say "Ultimate" instead of "Premium" for HDR/AV1/Cloud G-SYNC, so tier naming is inconsistent across regions. [vendor] — [GFN system requirements](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs); regional wording (via search summary) — [GFN system reqs (it-it)](https://www.nvidia.com/it-it/geforce/products/geforce-now/system-reqs/)
- HDR display requirement: an HDR10 monitor for Ultimate. For SHIELD, a 4K HDR TV on HDMI 2.0a or later is recommended. [vendor] (via search summary) — [GFN system reqs](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs)
- Release notes (undated version entries): 2.0.34 "4K resolution with HDR on SHIELD TV"; 2.0.67 (Windows on Arm, beta) "up to 4K resolution, 120 FPS and HDR"; 2.0.59 "Fixed an issue where some games were launching with HDR disabled on supported devices." [vendor] — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- 10-bit "Color Precision" is separate from HDR. 2.0.64 enabled it for Ultimate on the native Windows, macOS and SHIELD TV apps. 2.0.69 extended it to "Edge and Chrome browsers on Windows", "Chrome browser on Chromebook, Samsung TV, and LG TV". NVIDIA says it "enhances visual quality even if you do not have an HDR display or if a game does not support HDR". The setting is under Settings → Streaming Quality → Color precision, and "an 8-bit setting is available if your device is not supported." **Mac browsers, iOS Safari and Android are not in the list.** [vendor] — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- In the 2.0.43 notes, 1440p120 browser streaming is "not available when streaming from a browser on Mac, ChromeOS, iOS Safari, or Smart TVs". [vendor] — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- LG TVs are "the world's first to stream 4K at 120Hz with HDR through a native app on supported LG TVs" (Blackwell announcement, 2025-08-18). Cinematic-Quality Streaming = "YUV … 4:4:4 chroma sampling and 10-bit high dynamic range", AV1, "up to 100 Mbps". The Steam Deck native app goes "from 60 to 90 fps"; HDR is not mentioned for Deck. [vendor] — [NVIDIA Newsroom, 2025-08-18](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
- Codecs: 2.0.62 added AV1 on macOS for M3-class GPUs. 2.0.83 starts rolling HEVC out to "browser clients on select platforms". 2.0.85 adds manual codec selection; the default is automatic. The Linux app requires a GPU with "H.264 or H.265 Vulkan video codecs". [vendor] — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/), [system reqs](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs)
- 10-bit was greyed out on an AMD Vega iGPU. A moderator said the requirements match HDR's, and that chip had no 10-bit H.264 decode. [user] — [r/GeForceNOW mirror](https://lr.in.psf.lt/r/GeForceNOW/comments/1dtqwwc/is_10bit_working_or_not)
- Known HDR failure: from 2024-11-15 an incident turned colours "gray, washed out, or overexposed" in several games, and NVIDIA advised disabling HDR until it was fixed. [vendor via status aggregator] — [IsDown GFN incident](https://isdown.app/status/geforce-now/incidents/331251-hdr-color-problems)

**Bandwidth (no separate HDR row)**
- Minimum bandwidth by mode: 720p60 15 Mbps; 1080p60 25; QHD/UW-QHD 120 35; 4K120 45; QHD/FHD at 240/360 fps 55; 5K120 65 (Windows, macOS, Linux). SHIELD 4K60 40. Smart TV 4K60 40 and 4K120 45. iOS tops out at FHD60. **HDR and 10-bit have no bandwidth premium in the table.** [vendor] — [GFN system reqs](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs)

**Cloud G-Sync / VRR: how it works**
- CES 2024 preview. Ultimate already worked by "varying the stream rate to the client, driving down total latency on Reflex-enabled games". Cloud G-SYNC adds "varying the display refresh rate for smooth and instantaneous frame updates for variable refresh rate monitors". The only claim is qualitative: "minimized stutter and latency". [vendor] — [NVIDIA G-SYNC CES 2024 article](https://www.nvidia.com/en-us/geforce/news/g-sync-pulsar-gaming-monitor/)
- Launch on 2024-03-07: Cloud G-SYNC plus Reflex in 60 and 120 fps modes in the Windows and macOS apps. VRR monitors "match the streaming rate from GFN servers". [press] — [Wccftech, 2024-03-07](https://wccftech.com/geforce-now-adds-granblue-fantasy-relink-day-passes-and-cloud-g-sync-nvidia-reflex-support-for-60-120-fps/); CES preview of Reflex 60/120 modes — [NVIDIA blog, CES 2024](https://blogs.nvidia.com/blog/geforce-now-thursday-CES-2024)
- Requirements reported at launch (via search summary; press accounts conflict):
  - Native apps only, not browser, mobile or TV.
  - Display above 60 Hz with G-SYNC, G-SYNC Compatible, FreeSync or ProMotion.
  - GeForce driver R545 or later.
  - Windows: one monitor only.
  - Digital Trends: needs a GTX 16/RTX 20 or newer GPU, and Intel/AMD GPUs were not supported at launch. Tom's Hardware: works on Macs with Apple silicon, Intel CPUs and Radeon GPUs.
  - Sources — [Tom's Hardware](https://www.tomshardware.com/video-games/cloud-gaming/geforce-now-gains-cloud-g-sync-variable-refresh-rate-tech-works-with-macs-using-apple-silicon-intel-cpus-and-radeon-gpus), [Digital Trends](https://www.digitaltrends.com/?p=3550421), [PC Gamer](https://pcgamer.com/nvidia-announces-g-sync-is-coming-to-geforce-now-and-it-works-with-any-vrr-monitor)
- Related server-side pacing:
  - 2.0.76: "Streaming at 120 FPS now adapts to follow your game's FPS when it falls below 120 FPS on the native Windows and macOS app."
  - 2.0.28: a new default VSync setting, "Adaptive VSync, helps reduce stutter on poor networks", and lowers macOS latency "by up to 16ms".
  - 2.0.31: "lowered latency by up to 16ms for games that support Adaptive Vsync".
  - 2.0.34 (RTX 3080 era): "NVIDIA Adaptive Sync technology lowers latency below 60ms".
  - 2.0.37: bitrate, VSync and upscaling can be changed live in the overlay.
  - [vendor] — [GFN release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/)
- NVIDIA's recommended client setup for cloud frame generation: a VRR (G-Sync) display, Custom mode at 90 fps or more, VSync = Adaptive, Reflex on. [vendor] — summarised in `docs/research/11-upscaling-and-frame-generation/industry.md` §1

**240/360 fps and latency claims**
- 2.0.48: "competitive mode streaming at up to 240 frames per second, powered by NVIDIA Reflex". 2.0.75: 240 fps on the native Windows app on AMD RX 5000+ GPUs. System requirements list "90/120/240/360 FPS streaming" for the native Windows, macOS and Linux apps, at 55 Mbps for FHD/QHD at 240/360. [vendor] — [release highlights](https://www.nvidia.com/en-us/geforce-now/release-highlights/), [system reqs](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs)
- "Up to 360 fps at 1080p" with Reflex; "click-to-pixel response times as low as 30 milliseconds"; "sub-30-millisecond network latency" for most users in supported regions. There is no VRR on/off breakdown. [vendor] — [NVIDIA Newsroom, 2025-08-18](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)

### Inferences
- GFN's tiering says HDR is a premium, native-app feature, while 10-bit SDR is cheap enough to give every client that can decode Main10. Mac browsers get neither, which matches Chrome-on-Mac being Cha's hardest HDR target.
- Cloud G-Sync is essentially two decisions: (a) the server emits frames when the game produces them, with no fixed encode clock, and (b) the client presents each decoded frame immediately into a VRR swapchain. Neither requires protocol support beyond "don't pad to a fixed rate". This maps directly onto Cha's fixed 60/90/120 Hz composer clock: the composer clock is the thing to make optional.
- That GFN has put no HDR premium in its bandwidth table suggests 10-bit is close to free at its bitrates. It is not evidence that HDR adds no bits; see §5.

### Gaps
- No NVIDIA or independent measurement of GFN latency or judder with Cloud G-Sync on vs off.
- No primary NVIDIA support page for Cloud G-Sync requirements was retrievable. Driver, GPU and single-monitor limits come from 2024 press only.
- HDR status on GFN Android, iOS, Steam Deck, the Linux app and Samsung's native TV app is not stated anywhere I could find.
- Release-highlight entries carry no dates.

---

## 2. Xbox Cloud Gaming, PlayStation, Steam Remote Play/Link, Luna, Boosteroid, Shadow, Parsec, others

### Takeaway
Apart from GFN, consumer cloud services are SDR-first. Xbox Cloud Gaming has no documented HDR and is capped at 1440p60. Sony streams PS5 cloud titles at up to 4K60 with HDR, but only to a PS5 console; the Portal and Remote Play apps are SDR. Luna and Boosteroid claim no HDR, and Shadow publishes nothing about it. Parsec is explicitly 8-bit Rec.709 and tells users to turn HDR off. **Valve is the 2026 mover.** Steam Remote Play has HDR from Windows hosts since late 2024. A 2026-10-22 beta "big revamp" adds Automatic/On/Off HDR modes, HDR capture on Linux (PipeWire) and macOS hosts, Deck OLED and Android 15+ clients, and PyroWave with auto HDR on wired LAN. No service except GFN documents client-display VRR or frame-rate matching.

### Cited Findings
**Xbox Cloud Gaming**
- February 2026 update: Xbox Series X|S, One X and One S consoles can stream cloud games at 1440p. [press] — [Neowin](https://www.neowin.net/news/xbox-february-2026-update-brings-1440p-streaming-new-system-sounds-and-more/); [Xbox Wire (ja-jp), 2026-03-03](https://news.xbox.com/ja-jp/2026/03/03/february-xbox-update-1440p-streaming-rog-xbox-ally-updates-and-more/)
- November 2025: user-selectable resolution, up to 1440p for select titles; no 4K, 60 fps maximum (via search summary). [press] — [Pure Xbox, 2025-11](https://www.purexbox.com/news/2025/11/xbox-cloud-gaming-expands-to-new-regions-and-adds-user-selected-resolution-feature)
- 1440p at the higher bitrate "may consume up to 14GB of data per hour" vs 9 GB/h at 1080p. That is ≈31 vs ≈20 Mbit/s average, by my arithmetic (via search summary). [press] — [Thurrott](https://www.thurrott.com/?p=330104)
- HDR: I found no Microsoft statement either way. One January 2024 guide says HDR is not supported [low-quality secondary] — [Gamer Tweak](https://gamertweak.com/xbox-cloud-gaming-not-working-fix-2023). One 2026 blog claims HDR "falls back to SDR on most client devices" and passes through only on Series X clients (opinion, no testing; unverified) — [cloudgamingexpert blog](https://cloudgamingexpert.vercel.app/blog/hdr-is-mostly-broken-on-cloud)
- 2026-11-04 previews (phone-as-controller on TVs, Wi-Fi Nano Controller) and monthly hour caps from November 2026 include nothing about HDR or VRR. [vendor] — [Xbox Wire, 2026-09-03](https://news.xbox.com/en-us/2026/09/03/xbox-cloud-gaming-changes/)

**PlayStation**
- PS5 cloud streaming for PS Plus Premium (October 2023): 4K, 1440p, 1080p or 720p, all at 60 fps, "with HDR support". 1080p needs 15 Mbit/s and 4K needs 38 Mbit/s, with no separate HDR figure. It launched on PS5 consoles only. [vendor via press] — [PlayStation Blog, 2023-10-11](https://blog.playstation.com/2023/10/11/ps5-cloud-streaming-launches-this-month-for-playstation-plus-premium-members/), [TheFPSReview](https://www.thefpsreview.com/2023/10/11/ps5-cloud-streaming-launches-this-month-for-ps-plus-premium-members-up-to-4k-60-fps/)
- PS Portal update 7.0 (2026-03-18) added a "1080p High Quality" higher-bitrate mode for Remote Play and Cloud Streaming (5 Mbit/s minimum, 15 recommended). HDR is not mentioned; the Portal has an LCD. [vendor] — [PlayStation Blog, 2026-03-17](https://blog.playstation.com/2026/03/17/new-playstation-portal-software-update-adds-1080p-high-quality-mode-and-ux-refinements/)
- PS5 HDR Remote Play to Steam Deck OLED works through chiaki4deck 1.5.0 / chiaki-ng, "unlike PlayStation Portal" (2023-12). It is PS5-only. Non-HDR devices tone-map. [press + dev] — [Tom's Hardware](https://www.tomshardware.com/video-games/handheld-gaming/steam-deck-oled-now-supports-hdr-remote-play-from-ps5-unlike-playstation-portal), [chiaki-ng updates](https://streetpea.github.io/chiaki-ng/updates/done)

**Steam Remote Play / Steam Link / Steam Deck / Steam Machine**
- November 2024: Steam Link gained HDR and AV1. Full HDR streaming from a Windows PC with NVIDIA or AMD hardware encode to a Windows PC or Mac with an HDR display. The receiver must use "Enhanced 1080p" or "4K" quality, not the standard tier (via search summary). [press] — [FlatpanelsHD](https://www.flatpanelshd.com/news.php?subaction=showfull&id=1731479469), [PC Guide](https://www.pcguide.com/news/steam-remote-play-now-supports-hdr-but-steam-deck-users-are-missing-out/)
- 2026 timeline:
  - 2026-07-29 beta: Deck OLED HDR streaming.
  - Early September stable: shipped.
  - 2026-10-09 beta: Deck OLED HDR disabled, no reason given.
  - 2026-10-22 beta: re-enabled.
  - [press; page labelled AI-assisted] — [WindowsForum](https://windowsforum.com/news/steam-deck-oled-hdr-remote-play-returns-in-beta-with-brightness-and-streaming-controls.449804/); see also `08-adjacent-platforms-2026.md`
- 2026-10-22 Steam Deck beta changelog, verbatim: "Big revamp of HDR support:"; "Added an advanced client setting for HDR, with options for Automatic (enabled when HDR content is enabled), On (always on, allowing increased brightness for SDR content)" [and Off]; "In addition to supporting HDR in games and on Windows desktop, HDR capture is now supported when using pipewire on desktop Linux and streaming from macOS."; "HDR is enabled again on Steam Deck OLED."; "HDR is enabled on Android phones running Android 15 and newer, when using the beta Steam Link app". [vendor via press] — [Steam Deck HQ, 2026-10-23](https://steamdeckhq.com/steam-deck-gets-hdr-remote-play-overhaul-in-latest-beta-client-update/)
- A VideoCardz report attributes earlier overexposed graphics to Steam having used "a fixed SDR brightness value where a dynamic one was needed" (developer explanation, secondhand; unverified). — cited by [WindowsForum](https://windowsforum.com/news/steam-deck-oled-hdr-remote-play-returns-in-beta-with-brightness-and-streaming-controls.449804/) → [VideoCardz](https://videocardz.com/newz/steam-deck-oled-gets-hdr-streaming-back-in-latest-steam-beta)
- PyroWave beta (2026-09-21): 100–500 Mbit/s, wired LAN, 4:4:4 and HDR auto-selected. [vendor] — covered in `08-adjacent-platforms-2026.md`
- Steam Machine:
  - Valve lists HDMI 2.0, and says the hardware supports HDMI 2.1 with software "still a work in progress". [press] — [SlashGear](https://www.slashgear.com/2058637/steam-machine-hdmi-2-1-compatibility)
  - Under SteamOS 3.9 it outputs 4K144 10-bit HDR over HDMI FRL, but VRR drops out in that mode because of a kernel check. A prepared Linux fix restores 40–144 Hz VRR plus ALLM (unverified). [press] — [PC Games Hardware](https://www.pcgameshardware.de/Steam-Machine-Hardware-256919/News/SteamOS-39-HDMI-FRL-4K144-VRR-Problem-1553675/)
  - Valve positions it as a streaming host for Deck, Frame and Steam Link. [vendor via press] — [Gematsu, 2025-11](https://www.gematsu.com/2025/11/valve-announces-steam-machine-steam-frame-and-steam-controller)
  - I found nothing confirming HDR Remote Play *from* a Steam Machine host specifically.

**Luna, Boosteroid, Shadow**
- Boosteroid: users were still requesting HDR in April 2025. I found no announcement since. [user] — [r/BoosteroidCommunity mirror](https://redlib.hbubli.cc/r/BoosteroidCommunity/comments/1k34zc5/what_boosteroid_needs_is_hdr_support)
- Luna: in April 2026 it removed channels, Bring-Your-Own-Library and third-party subscriptions. Nothing on HDR. [press] — [Cloudbase](https://cloudbase.gg/best-amazon-luna-alternatives/)
- Shadow: its October 2025 blog covers GPU refreshes, DLSS and Windows 11, with no HDR, codec or fps details (see `11-.../industry.md` §3). Its comparison pages omit HDR. [vendor] — [Shadow vs Luna](https://shadow.tech/gaming/shadowpc-vs-luna)
- Third-party claim: "Boosteroid, Luna, Shadow: no HDR support claimed" (unverified blog). — [cloudgamingexpert blog](https://cloudgamingexpert.vercel.app/blog/hdr-is-mostly-broken-on-cloud)

**Parsec and enterprise remote desktop**
- Parsec "uses 8 bits per pixel YUV pixel format and Rec 709 color space for it's streaming protocol". [vendor] — [Parsec: Color Management Workflow](https://support.parsec.app/hc/en-us/articles/32361382749204-Color-Management-Workflow-In-Parsec)
- Parsec on HDR hosts: HDR "will cause the stream colors to look brighter than usual or washed out. We recommend disabling HDR in your system settings while using Parsec." [vendor] — [Parsec: wrong colors](https://support.parsec.app/hc/en-us/articles/32381130731924-Issues-with-the-video-stream-having-wrong-colors-glitching-flickering-or-blinking)
- Parsec quality options: "Prefer 4:4:4 Color" with "Constant FPS", and a 10-bit option for colour accuracy, which is not HDR. [vendor] — [Parsec: Improve Stream Quality](https://support.parsec.app/hc/en-us/articles/32381785123860-Improve-Stream-Quality-and-Color-Accuracy)
- Citrix Virtual Apps and Desktops ships "10-bit high dynamic range" sessions. They need 10-bit HDR monitors and NVIDIA GPUs with 10-bit HEVC (via search summary). A support article warns that 10-bit HDR sessions "might fallback to 8-bit on Nvidia Blackwell client GPUs". [vendor] — [Citrix docs](https://docs.citrix.com/en-us/citrix-virtual-apps-desktops/graphics/10-bit-high-dynamic-range), [CTX696450](https://support.citrix.com/external/article/CTX696450/10bit-high-dynamic-range-sessions-might.html)

### Inferences
- Among consumer services, HDR is not table stakes: Xbox, Luna, Boosteroid, Shadow, Parsec and the Portal all ship without it. Among LAN/self-hosted streamers, Valve's 2026 moves make it table stakes. Steam has auto HDR, HDR on all three host OSes and HDR in PyroWave, and Moonlight/Sunshine have had HDR since 2022–23 (§3).
- Valve's "Automatic (enabled when HDR content is enabled)" default is the clearest UX precedent. Go HDR only when the host content is HDR, and offer "On" for brighter SDR at the cost of SDR fidelity.
- Neither Sony nor Microsoft documents client-side VRR or rate matching. Both run fixed 60 fps streams to fixed-refresh TVs, consistent with the NSDI finding (§4) that fixed client VSync is the industry default.

### Gaps
- No Microsoft or Sony primary documentation on cloud-stream HDR per client device, or on Xbox/PS Remote Play HDR (one unsourced claim says Xbox Remote Play has no HDR).
- Steam Remote Play's HDR bitrate uplift, Steam Link's macOS/iOS/Apple TV HDR status, and whether Steam matches stream fps to client refresh or VRR: not found.
- Boosteroid, Luna and Shadow 2026 HDR status: no primary source either way.

---

## 3. Moonlight ecosystem (hosts, clients, forks) and other self-hosted streamers

### Takeaway
On the host side, HDR is mature on Windows (Sunshine, Apollo, Vibepollo, Foundation). On Linux it is "experimental": KMS capture only, VA-API on Intel/AMD, and a compositor that supports HDR. On the client side, moonlight-qt does HDR on Windows, macOS (EDR, using 203-nit reference white), Linux (libplacebo Vulkan, including tone-mapping to SDR displays), KMSDRM, Steam Deck and Raspberry Pi. v6.2.0 (October 2026) added VRR on NVIDIA in D3D11 and KMSDRM VRR/ALLM. The recurring complaints across all clients are washed-out, oversaturated or overexposed colour and TVs not switching mode, not missing features. Among Cha's peers, Wolf has no HDR yet (PRs open), Nestri added HDR in 2026-09, Polaris added PyroWave HDR10, Punktfunk's browser carries HDR only via WebGPU, and Selkies has nothing. Vibepollo plus Nonary's client is the only GameStream-family stack with explicit VRR pacing.

### Cited Findings
**Sunshine (LizardByte), docs at master 2026-11-08**
- "Streaming HDR content is officially supported on Windows hosts and experimentally supported for Linux hosts." Further points from the same page:
  - HDR must be active in the host OS, "which may require an HDR-capable display or EDID emulator dongle".
  - The client must enable HDR, "otherwise the stream will be SDR (and probably overexposed if your host is HDR)".
  - "HDR calibration can differ significantly between client and host displays."
  - "Some GPUs video encoders can produce lower image quality or encoding performance when streaming in HDR".
  - Windows: Intel/AMD/NVIDIA with HEVC Main10 or AV1 10-bit encode. They recommend "streaming the Windows HDR Calibration app to your client device and saving an HDR calibration profile". Games using NVAPI HDR "may not display properly".
  - Linux: Intel/AMD via VA-API only. "The KMS capture backend is required for HDR capture. Other capture methods, like NvFBC or X11, do not support HDR." It needs a compositor that renders HDR, such as Gamescope or KDE Plasma 6.
  - [dev] — [Sunshine docs/getting_started.md](https://github.com/LizardByte/Sunshine/blob/master/docs/getting_started.md)
- Windows display options:
  - `dd_hdr_option=auto` "Change HDR to the requested state from the client".
  - `dd_wa_hdr_toggle_delay`: for a virtual display "it might incorrectly display HDR color", so Sunshine toggles HDR off and on (about 500 ms; "directly impacts stream start time").
  - `dd_refresh_rate_option=auto` "Change refresh rate to the requested FPS value from the client".
  - `fp16_sdr_gamma_encoded` is a fix for FP16 SDR captures looking "washed out".
  - `minimum_fps_target` (default: half the stream fps): "many clients expect a constant stream of video frames, a certain amount of duplicate frames are sent". The docs never mention VRR.
  - [dev] — [Sunshine docs/configuration.md](https://github.com/LizardByte/Sunshine/blob/master/docs/configuration.md)
- The GNOME `displayconfig-mutter` example passes `--hdr ${SUNSHINE_CLIENT_HDR}` and the refresh rate per client. Older tools "do not support newer Mutter features such as HDR and VRR". [dev] — [Sunshine docs/app_examples.md](https://github.com/LizardByte/Sunshine/blob/master/docs/app_examples.md)
- Open Linux HDR bugs:
  - #5894 (2026-10-10): kmsgrab HDR metadata blob-ID changes cause repeated capture reinitialisation and black or one-frame output.
  - #5967: the Linux CUDA/NvFBC converter rejects 10-bit HEVC surfaces on Pascal.
  - [user/dev] — [Sunshine #5894](https://github.com/LizardByte/Sunshine/issues/5894), [Sunshine #5967](https://github.com/LizardByte/Sunshine/issues/5967)

**Apollo / Vibepollo / Foundation**
- Apollo:
  - The virtual display has "HDR support that matches the resolution/framerate config of your client automatically". HDR needs Windows 11 23H2, and is "generally supported on 24H2".
  - "It's your client's job to get HDR content displayed right, not the host."
  - Dark or yellow images come from clients tone-mapping SDR to peak brightness, leaving "no headroom" (issue #164).
  - "usually Apple products that have HDR capability can be trusted to have good results."
  - "Enabling HDR is generally not recommended with ANY streaming solutions at this moment", because of "semi-incompatible standards" and "Game support for HDR is still choppy". The virtual display is Windows-only.
  - [dev] — [Apollo README](https://github.com/ClassicOldSong/Apollo)
- Vibepollo:
  - Linux beta: "Managed private virtual displays support HDR". PyroWave "supports SDR/HDR, 8/10-bit, and 4:2:0/4:4:4 where the GPU allows" (wired LAN plus the VRR Moonlight fork). SteamOS has an optional patched-gamescope HDR10 path.
  - VRR: with the VRR client, Windows switches the virtual display to a **fixed 1000 Hz mode**, "so new frames are captured promptly without raising the stream frame rate". Linux uses presentation-driven capture.
  - RTSS frame cap matched to client fps, with V-Sync disabled.
  - [dev] — [Vibepollo README](https://github.com/Nonary/Vibepollo)
- Vibepollo bugs: #672 (2026-10-24), "PyroWave HDR10 rejected: enable HDR on the host capture display". #707 (2026-11-03), RDNA4 Linux VA-API "HDR streams have badly wrong colors" while SDR is fine. [user] — [Vibepollo #672](https://github.com/Nonary/Vibepollo/issues/672), [Vibepollo #707](https://github.com/Nonary/Vibepollo/issues/707)
- Nonary VRR moonlight-qt fork (vrr18, 2026-09-30) presets: Low Latency (up to half a frame time, ≈4.17 ms at 120 fps), Balanced (one frame time), Smooth (up to 24 ms). Steam Deck present-to-flip at 4K 4:4:4 fell from 3.9 to 2.0 ms. [dev] — per `11-.../industry.md` and `03-...md`
- Foundation Sunshine:
  - "Scene-adaptive HDR with HDR10+, HDR Vivid, and Dolby Vision extensions, calibrated to screen brightness" (translated). ZakoDisplay provides an HDR virtual screen. Full features need its companion client.
  - It advises starting in SDR ("先关闭") and enabling HDR once the basic stream works.
  - [dev] — [foundation-sunshine README](https://github.com/AlkaidLab/foundation-sunshine)
  - Issue #1200 (2026-10-31): the per-client HDR calibration flow (Windows HDR Calibration) cannot complete. #1201: per-client Windows colour profiles don't switch on connect. Both point to a shipped per-client HDR calibration/ICC feature. [user] — [#1200](https://github.com/AlkaidLab/foundation-sunshine/issues/1200), [#1201](https://github.com/AlkaidLab/foundation-sunshine/issues/1201)
  - Moonlight V+ adds HLG and 144/165 Hz. [dev] — per `03-...md`

**moonlight-qt (master 4113dd2, 2026-11-05; v6.2.0)**
- v6.2.0 (GitHub shows "04 Oct", so 2026-10-04; `11-.../industry.md` says 2026-10-06):
  - "HDR metadata is now used for tonemapping on macOS. You may need to tweak your HDR brightness in the OS".
  - "D3D11 renderer has been updated to enable VRR support on Nvidia GPUs".
  - "KMSDRM renderer is rewritten to support atomic modesetting, overlays, VRR, ALLM"; "V-sync off with atomic async flip".
  - "The default renderer on macOS is now libplacebo (using MoltenVK)."
  - "Fixed displays not being switched to HDR mode correctly on Raspberry Pi".
  - [dev] — [moonlight-qt v6.2.0](https://github.com/moonlight-stream/moonlight-qt/releases/tag/v6.2.0)
- macOS Metal renderer: `CAEDRMetadata HDR10MetadataWithDisplayInfo:… contentInfo:… opticalOutputScale:203.0` "to enable OS tonemapping". In other words, 203 cd/m² (BT.2408 reference white) is mapped to EDR 1.0. [dev] — [vt_metal.mm](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/vt_metal.mm)
- libplacebo/Vulkan renderer:
  - "This renderer supports HDR (including tone mapping to SDR displays)".
  - It requires `VK_COLOR_SPACE_HDR10_ST2084_EXT` for HDR output, and falls back to another device without HDR if needed.
  - `disable_10bit_sdr = true // Some drivers don't dither 10-bit SDR output correctly`.
  - It substitutes `PL_COLOR_HDR_BLACK` when the host's min luminance is 0.
  - On macOS it must set an scRGB colourspace before the first drawable, "otherwise our Metal layer ends up stuck in SDR mode".
  - [dev] — [plvk.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/plvk.cpp)
- VRR mechanics:
  - KMSDRM: "Enable VRR if V-sync is off by default" (`VRR_ENABLED = !vsync`).
  - D3D11: flip model with `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING` and present with `syncInterval 0` when V-Sync is off.
  - UI tooltips: "Disabling V-Sync allows sub-frame rendering latency, but it can display visible tearing"; "Frame pacing reduces micro-stutter by delaying frames that come in too early".
  - Defaults: V-Sync on, frame pacing off, 60 fps.
  - [dev] — [drm.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/drm.cpp), [d3d11va.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/d3d11va.cpp), [SettingsView.qml](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/gui/SettingsView.qml), [streamingpreferences.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/settings/streamingpreferences.cpp)
- Rate matching: in fullscreen, moonlight-qt picks "the highest refresh rate that our stream FPS evenly divides". If none divides, as with "a 120 FPS stream onto a 60 Hz monitor", it keeps the default. If the stream fps exceeds display Hz + 5 it forcibly disables V-Sync. The pacer keeps a rolling 500 ms queue history and drops frames when it is consistently more than one frame queued. [dev] — [session.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/session.cpp), [pacer.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/pacer/pacer.cpp)
- Default bitrate has no HDR term. It scales by resolution and fps (√ above 60 fps) and doubles for 4:4:4. [dev] — [streamingpreferences.cpp `getDefaultBitrate`](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/settings/streamingpreferences.cpp)
- Open HDR and VRR issues [user]:
  - #1306: HDR oversaturated until HDR is toggled off and on in Windows (IDD virtual display).
  - #1878 (closed 2026-05): HDR shows correctly only on KMSDRM or gamescope, so users asked for GNOME/KDE Wayland colour-management support.
  - #1875: overexposed on Wayland and fine on X11.
  - #2081 (2026-10-28): ARM Linux V4L2 decoder offers Main10/P010, but Moonlight streams 8-bit.
  - #1512: HDR and 4:4:4 can't be used together on Intel Arc.
  - #1545: a FreeSync TV's refresh "fluctuates wildly … between 50 Hz and 120 Hz" on a steady 89–90 fps stream, causing tearing.
  - #1509: macOS VRR "only seems to work when using the 'Fullscreen' display mode with V-sync enabled".
  - #1674: heavy stutter with V-Sync, HDR and VRR on GNOME.
  - #1424: enabling 4:4:4 unexpectedly made a Samsung 144 Hz OLED follow stream fps (VRR) and tear less.
  - #2021: 4K120 stutters while 4K144 is smooth on the same TV.
  - #2053: macOS libplacebo renders about 97 of 163 fps at 5120×2160 165 Hz, while Metal renders all.
  - Links: [#1306](https://github.com/moonlight-stream/moonlight-qt/issues/1306), [#1878](https://github.com/moonlight-stream/moonlight-qt/issues/1878), [#1875](https://github.com/moonlight-stream/moonlight-qt/issues/1875), [#2081](https://github.com/moonlight-stream/moonlight-qt/issues/2081), [#1512](https://github.com/moonlight-stream/moonlight-qt/issues/1512), [#1545](https://github.com/moonlight-stream/moonlight-qt/issues/1545), [#1509](https://github.com/moonlight-stream/moonlight-qt/issues/1509), [#1674](https://github.com/moonlight-stream/moonlight-qt/issues/1674), [#1424](https://github.com/moonlight-stream/moonlight-qt/issues/1424), [#2021](https://github.com/moonlight-stream/moonlight-qt/issues/2021), [#2053](https://github.com/moonlight-stream/moonlight-qt/issues/2053)

**Moonlight Android / iOS / tvOS / webOS, Artemis**
- moonlight-ios [user]:
  - #667: "Elevated blacks on iPad M4 when HDR (Beta) turned on". HDR is still labelled Beta on iOS.
  - #661: Apple TV's 50/60 Hz default changes on HDR activation.
  - #582: "Oversaturated HDR on tvOS", while iOS is fine.
  - #497: a Sony TV doesn't auto-switch to HDR, unlike PS5 or Xbox.
  - Links: [#667](https://github.com/moonlight-stream/moonlight-ios/issues/667), [#661](https://github.com/moonlight-stream/moonlight-ios/issues/661), [#582](https://github.com/moonlight-stream/moonlight-ios/issues/582), [#497](https://github.com/moonlight-stream/moonlight-ios/issues/497)
- moonlight-android [user]:
  - #1054: request for "an HDR Brightness/Luminance slider" because some TVs are too dark in HDR.
  - #1340: HDR at resolutions below 4K "looks greyed out, overbrighten".
  - #1456: false "Your PC GPU does not support streaming HDR".
  - #1406: MediaTek Pentonic TV hwcomposer "deliberately dropping 1/2 of all frames".
  - #945: Sony TVs can't enable HDR in Android's "isGame" mode.
  - Links: [#1054](https://github.com/moonlight-stream/moonlight-android/issues/1054), [#1340](https://github.com/moonlight-stream/moonlight-android/issues/1340), [#1456](https://github.com/moonlight-stream/moonlight-android/issues/1456), [#1406](https://github.com/moonlight-stream/moonlight-android/issues/1406), [#945](https://github.com/moonlight-stream/moonlight-android/issues/945)
- Artemis (Apollo's Android client) [user]:
  - #372: HDR "loses saturation" on a RedMagic Astra.
  - #431: users ask for the iOS-style HDR and 8/10-bit indicator in the overlay.
  - #194: HDR fails on Shield TV Pro, with a nod to the docs not recommending HDR.
  - Links: [#372](https://github.com/ClassicOldSong/moonlight-android/issues/372), [#431](https://github.com/ClassicOldSong/moonlight-android/issues/431), [#194](https://github.com/ClassicOldSong/moonlight-android/issues/194)
- moonlight-tv (webOS): HDR depends on the decoder backend. "NDL will always have better performance than SMP. Although sometimes it doesn't have H265, or HDR." [dev] — [moonlight-tv FAQ](https://github.com/mariotaku/moonlight-tv/wiki/FAQs)
- Steam Deck gamescope: with HDR on, SDR Moonlight streams shifted SteamOS UI colours. Setting colourimetry to BT.709 fixed it. The same report says Apple TV Moonlight showed SDR games washed out while HDR was active. [user] — [gamescope #1887](https://github.com/ValveSoftware/gamescope/issues/1887)

**Other self-hosted streamers**
- Wolf / Games on Whales:
  - Stable is NV12, 8-bit only (`configTOML.cpp`: "HDR isn't supported by Wolf yet"), per `01-wolf-games-on-whales.md`.
  - Open work: #469 "complete end-to-end HDR transport" (draft, updated 2026-10-31); #555 "per-client SDR/HDR" (P010 only for HDR clients, NV12/BT.709 for SDR); #567 Vulkan encode "No HDR support — deliberately left for a follow-up"; #222 is the FR.
  - [dev] — [wolf #469](https://github.com/games-on-whales/wolf/pull/469), [#555](https://github.com/games-on-whales/wolf/pull/555), [#567](https://github.com/games-on-whales/wolf/pull/567), [#222](https://github.com/games-on-whales/wolf/issues/222)
- Nestri:
  - #314 (2026-09-04) "make HDR reachable": it starts XWayland and advertises the opaque FourCCs. #315 verified the 10-bit HDR capture path "end to end".
  - #346 "media bitrate control, HDR" was promoted to production in #352 (2026-09-28).
  - [dev] — [nestri #314](https://github.com/nestrilabs/nestri/pull/314), [#315](https://github.com/nestrilabs/nestri/pull/315), [#346](https://github.com/nestrilabs/nestri/pull/346)
- Polaris:
  - #771 (merged 2026-09-25) "stream a Vulkan compute wavelet codec, with no copy and HDR10".
  - #803 (open): a labwc headless stream delivers "~58.85 fps instead of 60" because of wlroots' integer-ms frame timer, "a regular micro-stutter (roughly one missing frame per second)".
  - #688: owned displays capped at a 120 fps refresh ceiling.
  - [dev/user] — [polaris #771](https://github.com/papi-ux/polaris/pull/771), [#803](https://github.com/papi-ux/polaris/issues/803), [#688](https://github.com/papi-ux/polaris/pull/688)
- Punktfunk browser client: WebCodecs → WebGL2, or WebGPU, "the only route that carries HDR". [dev] — per `08-adjacent-platforms-2026.md`
- Selkies: no HDR issue, roadmap entry or feature found in the v2.0.0 META or codec META (searched 2026-11-09). — [selkies #227](https://github.com/selkies-project/selkies/issues/227)

### Inferences
- In the GameStream world, HDR is a capability the client requests and the host can honour on Windows (and Linux KMS/VA-API). An adopted Sunshine/Apollo host will send HEVC/AV1 Main10 PQ plus 0x010e metadata if Cha asks for it. Stock Moonlight clients will request HDR from Cha-as-host. So the protocol cost of HDR is low; the cost is all in capture, colour conversion and display.
- A 203-nit-to-SDR-white mapping (moonlight-qt's `opticalOutputScale:203.0`) is the de facto rule for presenting HDR10 on Apple EDR. A native macOS player should copy it exactly.
- VRR in Moonlight is "V-Sync off + tearing-allowed flip": the OS/driver does the rest. The pitfalls are platform-specific: macOS only in true fullscreen, NVIDIA D3D11 only since v6.2.0, GNOME stutter, and TVs fluctuating on jittery arrivals. For Cha this means VRR presentation must be a tested per-platform path, not a checkbox.
- Linux HDR hosting is fragile across the board: KMS-only capture, blob-ID churn, wrong VA-API colours on RDNA4, and Wolf not yet having it. This matters for Cha's containerised Linux apps, which render through its own compositor rather than KMS.

### Gaps
- moonlight-android and moonlight-ios release notes for HDR/VRR in 2026 were not checked directly. Apple TV/iOS behaviour comes from issues only.
- Nonary's VRR pacing algorithm internals (e.g., how it decides when to hold a frame) are not documented in its README. Only the preset budgets are known.
- No first-hand check of Moonlight Android's or Apple TV's frame-rate matching (Android `setFrameRate`, tvOS match-frame-rate).

---

## 4. Evidence on VRR and streaming: fixed V-Sync vs V-Sync off vs VRR; average cost of a fixed refresh; judder; LFC; how services pick stream fps

### Takeaway
The best streaming-specific evidence is NSDI 2025 (Li et al., independent and peer-reviewed). Each uncoordinated VSync stage in a cloud-gaming loop adds a delay "nearly uniform[ly] distribut[ed] between 0 and 16.7 ms" at 60 Hz, which averages about half a refresh interval. The client-side display VSync alone averaged 7.9 ms. Five chained stages made VSync 35.7% of non-network latency (mean 43.9 ms, range 4.5–82.9 ms) and the main source of latency jitter. Disabling VSync matched their optimised latency but caused "unstable frame rate and screen tearing". Desktop measurements (Blur Busters, RTINGS) agree on the mechanics:
- Fixed-refresh V-Sync costs up to one refresh (half on average) plus any queued frames.
- VRR approaches V-Sync-off latency if fps stays below the ceiling.
- A fixed refresh cannot show a non-divisor fps without judder.

I found no published measurement of VRR vs fixed refresh inside a game-streaming client.

### Cited Findings
- NSDI 2025, "Dissecting and Streamlining the Interactive Loop of Mobile Cloud Gaming", Li et al. [independent]:
  - Measured mobile cloud-gaming platforms show 112–403 ms interactive latency.
  - The instrumented X-MCG (Android cloud with Sunshine/Moonlight modules) has five VSync events: in-game, guest SurfaceFlinger, HWComposer, encoder frame-rate, and client SurfaceFlinger.
  - Table 2 means: VSync 43.9 ms (median 44.0, min 4.5, max 82.9), network 15.7, game rendering 27.9, video processing 27.2.
  - "the VSync mechanism (5 VSync events) contributes the most (35.7%) to the non-network latency".
  - "the latencies of 5 VSync events all exhibit a nearly uniform distribution between 0 and 16.7 ms".
  - Table 3: the client-side "Layer Composition II" VSync averages 7.9 ms.
  - The "butterfly effect": network jitter makes later stages miss VSyncs.
  - LoopTailor (bypass plus remote VSync alignment) cut the client VSync wait "from 8 ms to 3 ms" and total latency by about 35%, to 91 ms average. "Disable VSync" had similar latency "but … introduces severe side effects including unstable frame rate and screen tearing."
  - Android VRR "switch[es] between multiple discrete refresh rates rather than continuously adapting … (as of Sep. 2024)", so the client VSync always exists.
  - Source: [USENIX NSDI '25 PDF](https://www.usenix.org/system/files/nsdi25-li-yang.pdf), [presentation page](https://www.usenix.org/conference/nsdi25/presentation/li-yang)
- RTINGS methodology [independent]:
  - The photodiode sits mid-screen, so the input-lag floor is half a refresh: "4.17 ms on a 120Hz TV, and 8.33 ms for a 60Hz TV".
  - TVs are tested at 1080p, 1440p, 4K and 60/120/144 Hz.
  - Monitors are now tested "with VRR enabled", with refresh and fps set equal and multiple readings averaged.
  - Sources (via search summary): [RTINGS TV input lag](https://www.rtings.com/tv/tests/inputs/input-lag), [RTINGS monitor input lag](https://www.rtings.com/monitor/tests/inputs/input-lag)
- Blur Busters G-SYNC 101 (via search summary; the page itself returned HTTP 403) [independent, single lab]:
  - Uses high-speed camera plus modified mouse.
  - First-on-screen readings with V-Sync off "can appear to have up to twice the input lag reduction of middle screen readings".
  - With double-buffered V-Sync, a missed frame window locks fps "to half the refresh rate by a repeated frame". G-SYNC instead "adjusts the refresh rate to the framerate".
  - "just 2 FPS below the refresh rate is indeed still enough to avoid the G-SYNC ceiling and prevent V-SYNC-level input lag", regardless of max refresh.
  - At 240 Hz, "the input lag of G-SYNC becomes similarly low as VSYNC OFF (unlike at 60Hz where the difference is much bigger)".
  - Their 2014 preview measured CS:GO averages of 53/55 ms with V-Sync off vs 59/52 ms with G-SYNC, "below the noise floor".
  - Sources: [G-SYNC 101](https://blurbusters.com/gsync/gsync101-input-lag-tests-and-settings/), [G-SYNC preview part 2](https://blurbusters.com/gsync/preview2/)
- A forum estimate puts the gap between G-SYNC + V-SYNC capped at 138 fps @144 Hz and V-Sync off uncapped at "1–3 ms" (another poster says "1-2ms max"). [user] — [Blur Busters forum](https://forums.blurbusters.com/viewtopic.php?t=10449)
- "Perception-Aware Frame Display Scheduling for Low-Latency Cloud Gaming" (ACM, 2026; full text 403): targets irregular arrival vs a fixed 60 Hz client display. Reported effective rate for PASync is 53.47 fps (via search summary; unverified). — [ACM DL PDF](https://dl.acm.org/doi/pdf/10.1145/3798065.3798067)
- Low Framerate Compensation: below the VRR floor "frames are duplicated and displayed multiple times so that they can sync to a refresh rate that is within the displays refresh rate range". FreeSync Premium and Premium Pro mandate LFC. [vendor] — [AMD FreeSync](https://amd.com/en/products/graphics/technologies/freesync.html). The required max:min ratio is reported as 2.5× [press] — [PC Gamer](https://www.pcgamer.com/amd-rtg-summit-hdr-displays-freesync-and-more/2), [Igor's Lab](https://www.igorslab.de/en/amd-graphics-cards-freesync-latency-hdr/). An AMD document quoted on a forum says 2× (sources conflict). — [Guru3D](https://www.guru3d.com/articles-pages/radeon-technology-group-tech-update-december-2015,1.html)
- VRR follows uneven delivery rather than fixing it. One source notes that if the console delivers frames unevenly, VRR follows that timing; VRR flicker on OLEDs is worst at roughly 40–60 fps with uneven pacing and "nearly invisible at 90-120 fps" (via search summary). [press/vendor blog] — [KTC support blog](https://us.ktcplay.com/blogs/support-tips/console-vrr-stutter-30fps-games)
- Streaming-specific VRR failure reports [user]:
  - A FreeSync TV swinging 50–120 Hz on a 90 fps stream: [moonlight-qt #1545](https://github.com/moonlight-stream/moonlight-qt/issues/1545)
  - A wlroots timer producing 58.85 fps on 60 Hz, one hitch per second: [polaris #803](https://github.com/papi-ux/polaris/issues/803)
  - A TV hwcomposer halving frames: [moonlight-android #1406](https://github.com/moonlight-stream/moonlight-android/issues/1406)
  - A 4K120 stutter vs smooth 4K144: [moonlight-qt #2021](https://github.com/moonlight-stream/moonlight-qt/issues/2021)
- How products choose the stream rate relative to the client:
  - GFN: server stream rate follows the game (2.0.76); Cloud G-Sync on VRR displays; Adaptive VSync default. [vendor] — §1
  - Moonlight: user picks fps (default 60). Fullscreen chooses the highest refresh divisible by the fps, and forces V-Sync off if fps > Hz + 5. [dev] — §3
  - Sunshine: changes the host display refresh to the requested fps, and duplicates frames down to half the stream fps for static content. [dev] — §3
  - Apollo/Punktfunk: per-client virtual display at the client's resolution and refresh. [dev] — §3, `11-.../industry.md`
  - Vibepollo with VRR: host virtual display at a fixed 1000 Hz, so capture is presentation-driven, with an RTSS cap at client fps. [dev] — §3
  - PS5 cloud and Xbox cloud: fixed 60 fps. [vendor/press] — §2

### Inferences
- Arithmetic from the uniform-delay finding (my calculation): an uncoordinated fixed-refresh present stage adds U(0, T). That is a mean of T/2 and a worst case of T: 8.3/16.7 ms at 60 Hz, 5.6/11.1 ms at 90 Hz, 4.2/8.3 ms at 120 Hz. Its standard deviation is T/√12 (≈2.4 ms at 120 Hz).
  - Cha has both a fixed host composer clock and a fixed client display. If they are unaligned, that is two such stages: about +8.3 ms mean at 120 Hz, more than Cha's whole ~5 ms send→shown budget.
  - Aligning or eliminating these stages (VRR on the client, phase-locking the composer to the client's vblank, or presenting on arrival) is the largest latency lever left on a wired LAN.
- Judder arithmetic (my calculation):
  - 90 fps on a fixed 120 Hz display shows 3 frames per 4 refreshes, with holds of 8.3, 8.3 and 16.7 ms. That is a 30 Hz repeating cadence error of 8.3 ms.
  - 50 fps on 60 Hz repeats one frame every 6 refreshes, a 10 Hz beat with 16.7 ms hitches.
  - 60 fps on 120 Hz is clean, with every frame held 2 refreshes.
  - This is why moonlight-qt picks a refresh divisible by the stream fps. Cha's 90 fps mode is only judder-free on 90 Hz, 180 Hz or VRR displays. A MacBook Pro ProMotion panel is VRR, and so are many 120/144 Hz monitors.
- VRR's benefit in streaming exists only if frames reach the client at even intervals. Network and decode jitter go straight to the screen. A small, adaptive "hold until expected time" buffer (Nonary's presets: ½ frame / 1 frame / 24 ms) trades a few ms for evenness. On wired LAN with ~5 ms total latency, the ½-frame budget (≈4 ms at 120 fps) is the relevant one.
- LFC matters for streams that drop below the panel's VRR floor (e.g., 40–48 Hz). Below it, the client or OS must repeat frames, so a streamer should not rely on VRR for very low-fps content such as idle desktops. Sunshine's "duplicate frames down to half fps" behaviour does the same job from the host side.

### Gaps
- No independent measurement of VRR vs fixed V-Sync vs V-Sync off *in a streaming client* (Moonlight, GFN, Steam), whether latency or judder. NVIDIA published none for Cloud G-Sync.
- Blur Busters' and RTINGS' actual numeric tables could not be retrieved (403); only their methodology and conclusions via search summaries.
- The PASync (ACM 2026) paper's results could not be read.
- No source on Apple ProMotion/adaptive-sync behaviour for externally paced video in Chrome or native macOS players (likely covered by the browser/native-player slices).

---

## 5. Evidence on HDR streaming in practice: bandwidth uplift, latency cost, user complaints

### Takeaway
Nobody publishes an HDR bitrate premium for HEVC/AV1. GFN's bandwidth table, Sony's 38 Mbit/s 4K figure and Moonlight's default-bitrate formula all treat HDR as free. Only PyroWave guidance prices 10-bit at about ×1.15. I found no measured HDR latency cost; Sunshine warns only that some encoders lose quality or speed in HDR. The dominant real-world problems are colour and brightness, not bandwidth:
- SDR content washed out or overexposed on HDR hosts.
- Oversaturated or desaturated HDR on specific clients.
- Elevated blacks.
- TVs not switching into HDR mode.
- Dim HDR with no client brightness control.
- Virtual displays needing HDR toggles.
- Games needing an HDR host display or an in-game toggle.

### Cited Findings
**Bandwidth**
- GFN's minimum-bandwidth table has no HDR/10-bit row (4K120 45 Mbit/s, 5K120 65; CQS "up to 100 Mbps"). [vendor] — [GFN system reqs](https://www.nvidia.com/en-us/geforce/products/geforce-now/system-reqs), [NVIDIA Newsroom](https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now)
- PS5 cloud: 1080p 15 Mbit/s, 4K 38 Mbit/s, "with HDR support" and no separate HDR figure. [vendor via press] — [TheFPSReview](https://www.thefpsreview.com/2023/10/11/ps5-cloud-streaming-launches-this-month-for-ps-plus-premium-members-up-to-4k-60-fps/)
- moonlight-qt's `getDefaultBitrate()` has no HDR factor; 4:4:4 doubles the resolution factor. [dev] — [streamingpreferences.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/settings/streamingpreferences.cpp)
- PyroWave guidance: about 1.6 bits/pixel for clean 4:2:0 SDR; 4:4:4 ×1.6; 10-bit ×1.15 (Vibepollo). [dev] — per `03-moonlight-sunshine-ecosystem.md`, `02-pyrowave-and-codecs.md`

**Latency and quality cost**
- "Some GPUs video encoders can produce lower image quality or encoding performance when streaming in HDR compared to SDR." [dev] — [Sunshine getting_started.md](https://github.com/LizardByte/Sunshine/blob/master/docs/getting_started.md)
- From `11-.../industry.md`: NVIDIA RTX VSR is disabled under HDR and AMD's HDR upscaling path is degraded, so HDR closes off some client super-resolution paths. [dev] — [moonlight-qt PR #1557](https://github.com/moonlight-stream/moonlight-qt/pull/1557)

**Common complaints, by type**
- *SDR on an HDR host looks washed out or overexposed:*
  - Parsec, which tells users to disable HDR. [vendor] — [Parsec](https://support.parsec.app/hc/en-us/articles/32381130731924-Issues-with-the-video-stream-having-wrong-colors-glitching-flickering-or-blinking)
  - Sunshine: "probably overexposed if your host is HDR". [dev] — [Sunshine docs](https://github.com/LizardByte/Sunshine/blob/master/docs/getting_started.md)
  - Steam: fixed SDR brightness caused overexposure (unverified). — [WindowsForum](https://windowsforum.com/news/steam-deck-oled-hdr-remote-play-returns-in-beta-with-brightness-and-streaming-controls.449804/)
  - Microsoft: "SDR content appears too bright and desaturated, try restarting the app"; "Some SDR apps appear darker on an HDR display compared to other SDR apps". [vendor] — [Microsoft: HDR settings in Windows](https://support.microsoft.com/en-US/Windows/2d767185-38ec-7fdc-6f97-bbc6c5ef24e6)
  - KDE's developer: Windows in HDR uses the sRGB piecewise curve rather than gamma 2.2, which makes dark SDR areas "washed out". [dev] — [Xaver Hugl, KDE, 2025-03-31](https://planet.kde.org/xavers-blog-2025-03-31-on-brightness-and-calibrating-your-displays)
- *Client-side HDR rendering wrong:*
  - tvOS oversaturated: [moonlight-ios #582](https://github.com/moonlight-stream/moonlight-ios/issues/582)
  - iPad M4 elevated blacks: [#667](https://github.com/moonlight-stream/moonlight-ios/issues/667)
  - Android desaturated: [Artemis #372](https://github.com/ClassicOldSong/moonlight-android/issues/372)
  - Android greyed out below 4K: [moonlight-android #1340](https://github.com/moonlight-stream/moonlight-android/issues/1340)
  - Windows with an IDD virtual display: oversaturated until HDR is toggled ([moonlight-qt #1306](https://github.com/moonlight-stream/moonlight-qt/issues/1306)), with the Sunshine workaround `dd_wa_hdr_toggle_delay`
  - Linux Wayland overexposed: [#1875](https://github.com/moonlight-stream/moonlight-qt/issues/1875)
  - RDNA4 host colours wrong: [Vibepollo #707](https://github.com/Nonary/Vibepollo/issues/707)
  - All [user]
- *Brightness/peak mismatch:*
  - Apollo: yellow or dark images because the client maps SDR to peak and leaves "no headroom". [dev] — [Apollo README](https://github.com/ClassicOldSong/Apollo)
  - moonlight-qt v6.2.0: "You may need to tweak your HDR brightness in the OS". [dev] — [v6.2.0](https://github.com/moonlight-stream/moonlight-qt/releases/tag/v6.2.0)
  - Request for a client HDR brightness slider: [moonlight-android #1054](https://github.com/moonlight-stream/moonlight-android/issues/1054) [user]
  - Sunshine: "HDR calibration can differ significantly between client and host displays", hence streaming the Windows HDR Calibration app to the client. [dev] — [Sunshine docs](https://github.com/LizardByte/Sunshine/blob/master/docs/getting_started.md)
  - Foundation built per-client HDR calibration profiles, and users report them not completing or not switching. [user] — [#1200](https://github.com/AlkaidLab/foundation-sunshine/issues/1200), [#1201](https://github.com/AlkaidLab/foundation-sunshine/issues/1201)
- *Display doesn't enter HDR or mode changes:*
  - Sony TVs don't switch: [moonlight-ios #497](https://github.com/moonlight-stream/moonlight-ios/issues/497), [moonlight-android #945](https://github.com/moonlight-stream/moonlight-android/issues/945)
  - Apple TV 50/60 Hz flips on HDR: [moonlight-ios #661](https://github.com/moonlight-stream/moonlight-ios/issues/661)
  - Raspberry Pi not switching (fixed in v6.2.0) [user/dev]
- *Games need host-side HDR:*
  - moonlight-qt's HDR tooltip: "some games may require an HDR monitor on your host PC to enable HDR mode." [dev] — [SettingsView.qml](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/gui/SettingsView.qml)
  - Sunshine: NVAPI-HDR games "may not display properly". [dev]
  - Vibepollo: PyroWave HDR10 is refused unless the host capture display is in HDR. [user] — [#672](https://github.com/Nonary/Vibepollo/issues/672)
  - GFN 2.0.59: games launching with HDR disabled. [vendor]
- *Vendor scepticism:* Apollo says HDR "is generally not recommended with ANY streaming solutions" and "SDR actually provides much more stable color accuracy". Foundation advises starting in SDR. [dev] — [Apollo README](https://github.com/ClassicOldSong/Apollo), [Foundation README](https://github.com/AlkaidLab/foundation-sunshine)
- *Visibility:* users ask for an HDR and 8/10-bit indicator in the stats overlay (iOS and PC have it, Android doesn't). [user] — [moonlight-android #1431](https://github.com/moonlight-stream/moonlight-android/issues/1431), [Artemis #431](https://github.com/ClassicOldSong/moonlight-android/issues/431)

### Inferences
- For a wired-LAN product running at hundreds of Mbit/s, HDR's bit cost is negligible (≈15% at most on PyroWave, probably less on HEVC/AV1). Its real costs are engineering: per-platform display paths, metadata, SDR-in-HDR mapping and calibration.
- The complaint list is effectively a test plan:
  1. SDR desktop and UI on an HDR client must not wash out. Map SDR white to the OS's SDR white, or to 203 nits.
  2. HDR must not be enabled when content is SDR (Steam's "Automatic").
  3. Mode switching must be explicit and reported.
  4. Users need a client HDR brightness or peak control.
  5. Show HDR and bit depth in stats.
  6. Handle a host in HDR with an SDR client by tone-mapping (moonlight-qt libplacebo does this).
- 10-bit SDR (GFN's "Color precision") is a cheap, separate win that reduces banding on any client that can decode Main10. It is worth considering before full HDR on the browser path.

### Gaps
- No controlled measurement of HDR vs SDR encode or decode latency for NVENC/AMF/QSV/VideoToolbox in a streaming context.
- No service publishes HDR-specific bitrate ladders.
- No quantitative survey of how often streamed games actually run in HDR.

---

## 6. Guidelines a streamer should honour: HGIG, ITU-R BT.2408, Windows SDR brightness/Auto HDR, KDE/Wayland, SteamOS/Deck OLED

### Takeaway
The working consensus:
- (a) **One tone-mapping pass.** The game (or the app producing HDR) tone-maps to the display's real limits, and the display passes through. That is HGIG.
- (b) **SDR/graphics white at 203 cd/m² in PQ.** That is BT.2408, adopted by KDE for games and by moonlight-qt on macOS. The user's OS "SDR brightness" setting overrides it locally.
- (c) **The client decides final presentation.** It knows the panel; the host does not.

For a streamer this means:
- forward the host's HDR10 static metadata unchanged;
- carry the client's display capabilities to the host so games can calibrate;
- map SDR content to the client OS's SDR white rather than to peak;
- never tone-map twice.

### Cited Findings
- **HGIG:**
  - A Sony/Microsoft-organised volunteer group publishing HDR gaming best practice. The game tone-maps to the display's limits, set through console calibration screens: the PS5 "Adjust HDR" screen and the Xbox HDR Game Calibration app. The TV's HGIG mode disables its own tone mapping and clips above panel peak.
  - Enable HGIG *before* calibrating. HGIG "can make the screen look darker" than TV tone mapping. [press/vendor] — [What Hi-Fi](https://www.whathifi.com/advice/hgig-explained-what-is-hgig-how-do-you-get-it-and-should-you-use-it), [LG support: enable HGIG](https://www.lg.com/us/support/help-library/lg-tv-how-to-enable-the-hgig-setting--20153301892284), [Display Daily](https://displaydaily.com/hdr-gaming-interest-group-to-propose-production-and-distribution-guidelines/)
  - Parameter names (MaxTML / MinTML / MaxFFTML exchanged once and used by the renderer) come from a single secondary site. [secondary; unverified] — [cinemaconfig HGIG](https://cinemaconfig.com/reference/hgig)
  - HGIG "isn't really a format … but rather a set of fairly loose guidelines" (via search summary). — [What Hi-Fi](https://www.whathifi.com/advice/hgig-explained-what-is-hgig-how-do-you-get-it-and-should-you-use-it)
- **ITU-R BT.2408** (latest revision BT.2408-9, 03/2026): HDR Reference White (100%), diffuse white and **Graphics White are all 203 cd/m²** (PQ signal 58%, HLG 75%). "Graphics should be inserted based on this level." [standard] — [ITU-R BT.2408-9 (2026) PDF](https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2408-9-2026-PDF-E.pdf), [BT.2408-9 TOC](https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2408-9-2026-TOC-HTM-E.htm)
  - W3C CSS Color HDR (Working Draft 2026-07-28) uses 203 cd/m² as the standard HDR reference white, but says it "may result in a HDR Reference White being displayed at a value lower or higher than 203". [standard draft] — [CSS Color HDR Module Level 1](https://www.w3.org/TR/css-color-hdr/)
  - Critics note most episodic and theatrical HDR does not grade diffuse white at 203. [independent blog] — [Daejeon Chronicles](https://daejeonchronicles.com/2023/07/05/diffuse-white-level-of-most-episodic-hdr-content-nowhere-near-203-nits/)
- **Apple EDR in practice:** moonlight-qt passes HDR10 mastering and content-light metadata to `CAEDRMetadata` with `opticalOutputScale:203.0`, so macOS performs the tone mapping. [dev] — [vt_metal.mm](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/vt_metal.mm)
- **Windows:**
  - "SDR content brightness": "On an external HDR display, this setting will change the brightness of SDR content relative to HDR content". On built-in displays SDR brightness "is controlled by a separate brightness setting, or it might be controlled automatically".
  - "Use HDR video streaming" is a separate capability, with a battery opt-out.
  - HDR10 support is checked in dxdiag via BT2020RGB/YCC and Eotf2084Supported.
  - [vendor] — [Microsoft: HDR settings in Windows](https://support.microsoft.com/en-US/Windows/2d767185-38ec-7fdc-6f97-bbc6c5ef24e6)
  - One user's measurements of the slider: 0% ≈ 81 nits (Microsoft states the minimum is 80), 25% ≈ 181, 50% ≈ 287, 75% ≈ 401, 100% ≈ 498. The slider sets the SDR reference (paper) white, including for Auto HDR. [user] — [HardForum](https://hardforum.com/threads/windows-11-sdr-content-brightness.2027311/)
  - Auto HDR got a per-game intensity slider in Game Bar (5.721+). [press] — [TechSpot](https://www.techspot.com/community/topics/windows-11-users-can-now-adjust-auto-hdr-on-a-per-game-basis.275141)
- **KDE Plasma / Wayland color management** (KWin developer):
  - Each transfer function carries a reference luminance ("HDR reference white, graphics white, or SDR white"). The compositor scales linearly by `output_reference / input_reference`. "Maximum SDR Brightness" sets the reference luminance at 100% brightness.
  - In Plasma 6.3, games should set "the HDR paper white in games to 203cd/m²" and then the maximum.
  - Native apps should follow the compositor's preferred image description, "render SDR UI at the reference luminance", and "should drop all HDR settings".
  - Min/max luminance is configured via `kscreen-doctor`.
  - [dev] — [Xaver Hugl, KDE blog, 2025-03-31](https://planet.kde.org/xavers-blog-2025-03-31-on-brightness-and-calibrating-your-displays)
- **SteamOS / Deck OLED:**
  - HDR display settings arrived in SteamOS 3.5 (external displays). SteamOS 3.5.5 shipped Deck OLED support and HDR. [press] — [Phoronix: SteamOS 3.5.5](https://www.phoronix.com/news/SteamOS-3.5.5)
  - HDR on the Deck is a gamescope feature. [press] — [Phoronix XDC 2023](https://Phoronix.com/news/XDC-2023-AMD-Colors-HDR)
  - Steam Remote Play's 2026-10-22 HDR modes: Automatic, "On (always on, allowing increased brightness for SDR content)", Off. [vendor] — [Steam Deck HQ](https://steamdeckhq.com/steam-deck-gets-hdr-remote-play-overhaul-in-latest-beta-client-update/)
  - User workarounds: BT.709 colourimetry for SDR Moonlight streams with HDR on [gamescope #1887](https://github.com/ValveSoftware/gamescope/issues/1887); `DXVK_HDR=0` for games without a toggle (2023 community guide). [user] — [r/SteamDeck mirror](https://reddit.birdcat.cafe/r/SteamDeck/comments/1868qyv/steam_deck_oled_hdr_games_guide/kb8fnsf/?context=3)
- **Streamer-specific practice:**
  - Sunshine: calibrate by streaming the Windows HDR Calibration app to the client and saving a profile; in-game brightness sliders may need retuning per client display. [dev] — [Sunshine docs](https://github.com/LizardByte/Sunshine/blob/master/docs/getting_started.md)
  - Apollo: "It's your client's job to get HDR content displayed right, not the host"; ICC correction is "totally useless while streaming HDR". [dev] — [Apollo README](https://github.com/ClassicOldSong/Apollo)
  - GameStream already carries the host's mastering primaries, white point, min/max luminance, MaxCLL and MaxFALL in packet 0x010e, re-sent on HDR state changes (`03-moonlight-sunshine-ecosystem.md` §4.9).

### Inferences
- For Cha's own compositor, which renders containerised Linux apps:
  - SDR app content should be composed at a reference white tied to the *client's* SDR white: macOS EDR 1.0, browser SDR white, or 203 nits when encoding PQ.
  - HDR games should get the client's peak and min luminance as their "display", since games running in Cha's containers calibrate against whatever the virtual output advertises. This is the virtual-display equivalent of HGIG.
  - That implies sending the client's display capabilities (peak, min, SDR white, gamut) to the node at session start, as Apollo/Vibepollo do for resolution and refresh.
- When Cha adopts a Sunshine/Apollo host, it should pass the 0x010e metadata through to the macOS player's `CAEDRMetadata`, or to the browser's equivalent, and not tone-map again. When serving a stock Moonlight client, it should send correct static metadata and rely on the client.
- Don't build HDR calibration screens into Cha; KDE explicitly advises against it. A per-client "SDR brightness" and "peak brightness" override, defaulting to the OS values, covers the brightness complaints in §5.

### Gaps
- The official HGIG guideline document (hgig.org) was not retrieved, so the exact parameter names and recommended defaults are secondary.
- Windows' SDR-brightness default value and its relationship to the 80-nit DirectX reference are only documented by users.
- I found no Valve documentation of gamescope's SDR-in-HDR mapping (SDR nits / ITM defaults) for Deck OLED, and no SteamOS guidance for streamers.
- No guideline body (HGIG, ITU, Microsoft, Apple) addresses *streamed* HDR specifically: who tone-maps, and whose display metadata a remote game should calibrate against.
