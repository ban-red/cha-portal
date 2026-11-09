# Node-side (render-side) upscaling and frame generation on Linux GPU hosts, late 2026

> Evidence for [research note 11](README.md).

Scope: what can upscale or generate frames on the GPU node, inside a headless container, for games running under Proton inside gamescope (`--backend wayland`, nested in Cha Portal's Smithay compositor), before frames reach NVENC / VA-API / PyroWave. Research date: 2026-11-09. Test node: RTX 4090 (Ada, AD102), driver >=580.

Note on dates: GitHub release pages for dxvk-nvapi and vkd3d-proton show day/month without a year. Years below come from release order plus the driver/header versions each release cites, so they are inferred.

## 1. In-game upscalers and frame generation under Proton on Linux (late 2026)

### Takeaway
On NVIDIA, DLSS Super Resolution (including the DLSS 4 transformer and DLSS 4.5 second-generation transformer models), Ray Reconstruction, 2x Frame Generation, and Reflex for D3D all work under Proton through dxvk-nvapi. Multi Frame Generation (MFG) and Dynamic MFG / 6x are RTX 50-only, and on Linux they are supported only partly: dxvk-nvapi 0.9.2 (May 2026) calls its Dynamic MFG support "limited/incomplete". On AMD, FSR 4 runs natively on RDNA 4 (FP8 through vkd3d-proton 3.0+). On RDNA 3 dGPUs it runs as the FSR 4.1 INT8 model in Proton Experimental (June 2026). FSR "Redstone" ML frame generation and Intel XeSS-FG/XeSS 3 MFG have no official Linux support; community hacks exist for both. For the RTX 4090 test node, the usable set is DLSS SR/RR (any preset, including the newest), DLSS FG 2x, Reflex, and Smooth Motion. MFG, Dynamic MFG, 6x and Reflex 2 Frame Warp are not available on Ada, and Frame Warp has not shipped anywhere.

### Cited Findings
**NVIDIA DLSS / Reflex under Proton**
- NVIDIA's own Linux gaming doc says: "On Linux, Proton must expose NVIDIA's APIs (NVAPI) for DLSS to function." DXVK-NVAPI ships with standard Proton, and "For games that do not support NVAPI, DLSS options are missing in-game." It lists the game-integrated DLSS features as SR, FG, RR and DLAA. Doc versions exist for drivers 595, 610 and 615 — [NVIDIA Driver Installation Guide: DLSS / Smooth Motion / Reflex](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/gaming.html); [615 version](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/615/gaming.html)
- The R615 version of that doc still does not mention Multi Frame Generation, Dynamic MFG, multi-frame count, or a dynamic target frame rate. It covers SR/FG/RR/DLAA, DLL updating and preset overrides — [NVIDIA doc, 615](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/615/gaming.html)
- DLSS 3 Frame Generation support reached Proton Experimental in November 2024 — [Tom's Hardware (Nov 2024)](https://www.tomshardware.com/software/linux/latest-valve-proton-experimental-update-enables-dlss-3-frame-generation-and-fixes-performance-in-dozens-of-games); [TheFPSReview (2024-11-20)](https://www.thefpsreview.com/2024/11/20/proton-adds-support-for-nvidia-dlss-3-frame-generation-multiplying-performance-for-linux-gamers/). This is old, but later sources confirm FG still works.
- dxvk-nvapi v0.9 (16 Mar 2025) added Reflex for Vulkan titles through a separate layer (`VkLayer_DXVK_NVAPI_reflex.json` / `libdxvk_nvapi_vkreflex_layer.so`). It also added reporting of driver (DRS) settings to titles, used for overriding DLSS presets, plus `DXVK_NVAPI_SET_NGX_DEBUG_OPTIONS`, and it reports the Blackwell architecture — [dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)
- dxvk-nvapi v0.9.1 (24 Jan 2026, year inferred) added "DRS environment variables for overriding DLSS SR/RR scaling ratio" — [dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)
- dxvk-nvapi v0.9.2 (12 May 2026, year inferred) did the following ([dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)):
  - "Implement several NVAPI entrypoints needed for limited/incomplete support for Dynamic Multi Frame Generation." This needs updated DLSS snippets and Streamline.
  - Added R595 DRS keys `NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX`, `NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE`, `NGX_DLSSG_MODE` and `NGX_DLSS_FG_OVERRIDE_RENDER_PRESET_SELECTION`.
  - Updated vkroots "so the Vulkan Reflex layer works with the gamescope WSI layer". This was a crash fix.
  - Requires vkd3d-proton 3.0.1+.
- dxvk-nvapi v0.9.3 (30 Oct 2026, year inferred) did the following ([dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)):
  - Added "an alternative Reflex implementation for Vulkan titles using the VK_NV_low_latency revision 2 extension". This needs driver R615+, "doesn't require a Vulkan layer anymore", and makes the layer obsolete.
  - Added Opacity Micromap support for DXR. This needs vkd3d-proton 3.1+, `VK_KHR_opacity_micromap` and driver R615+.
  - Added R610 DRS keys `NGX_DLSS_NR_OVERRIDE*` and the R615 key `SL_DLSS_OVERRIDE`.
- NVIDIA's doc says: "Reflex is supported out of the box in DirectX games via DXVK-NVAPI." Vulkan Reflex under Proton needs the DXVK-NVAPI layer, which is "disabled by default even when installed" (`DXVK_NVAPI_VKREFLEX=1`). Native Vulkan games use `VK_NV_low_latency2` — [NVIDIA doc](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/gaming.html)
- vkd3d-proton v3.0.1 (6 May 2026, inferred) has "Various fixes for NVIDIA Reflex"; present IDs "can now be decoupled", which "helps frame-gen in particular". It also implements `VK_EXT_present_timing` "allowing for smooth frame pacing for SyncInterval > 1" — [vkd3d-proton releases](https://github.com/HansKristian-Work/vkd3d-proton/releases)
- vkd3d-proton v3.1 (28 Oct 2026, inferred) ([vkd3d-proton releases](https://github.com/HansKristian-Work/vkd3d-proton/releases)):
  - Fixes `low_latency2` paths "submitted by NVIDIA to match spec version 3 update".
  - Adds `KHR_opacity_micromap`.
  - Avoids "some compositor roundtrips in IDXGISwapChain::Present() related to occlusion checks".
- DLSS 4.5 was announced at CES (January 2026) ([NVIDIA GeForce News](https://www.nvidia.com/en-us/geforce/news/dlss-4-5-dynamic-multi-frame-gen-6x-2nd-gen-transformer-super-res/); [Tom's Hardware](https://www.tomshardware.com/pc-components/cpus/nvidia-introduces-dlss-4-5-and-multi-frame-generation-6x-at-ces-2026-updated-models-can-generate-higher-quality-upscaled-frames-and-more-of-them-dynamically)):
  - It brings a second-generation transformer model for SR, Dynamic MFG, and a 6x MFG mode that generates up to 5 frames per rendered frame.
  - 6x and Dynamic MFG are RTX 50-only.
  - The 4.5 SR model works on all RTX GPUs (20-series and up).
  - Dynamic mode adjusts the multiplier toward a target frame rate set in the NVIDIA app.
  - The features ship to developers through the Streamline MFG plugin ("this spring").
- GamingOnLinux (Jan 2026) found no confirmation that DLSS 4.5 or MFG works on Linux. It says Linux support depends on Proton hooks and NVIDIA Linux drivers — [GamingOnLinux (Jan 2026)](https://www.gamingonlinux.com/2026/01/nvidia-announce-dlss-4-5-with-dynamic-multi-frame-generation-plus-dlss-updater-gets-linux-support/)
- A Steam user with an RTX 5080 on driver 580.126.09 reported that MFG needed an NVAPI env var plus GE-Proton. A 2026 article said FG "on Linux is still unreliable" and that the FG option can disappear after Proton or driver updates (both anecdotal) — [Steam for Linux discussion](https://steamcommunity.com/app/221410/discussions/0/732532230755229905/); [Yahoo Tech / XDA-style article](https://tech.yahoo.com/gaming/articles/nvidia-keeps-neglecting-frame-gen-130031610.html)
- Unofficial projects:
  - `dlssg-open` runs DLSS FG on RTX 30-series under Proton. It was tested on driver 615.71, Proton Hotfix (wine 11) and DLSS-G runtime 310.6, and ">2x MFG untested" — [ProfessorTee/dlssg-open](https://github.com/ProfessorTee/dlssg-open)
  - `dlssg-amd` targets RDNA4 — [itchyfeetleech/dlssg-amd](https://github.com/itchyfeetleech/dlssg-amd)
  - DLSS-Unlocked claims MFG on any RTX GPU under Proton with `WINEDLLOVERRIDES="dxgi=n,b" PROTON_ENABLE_NVAPI=1 PROTON_NVIDIA_NVCUDA=1` (unverified third-party mod) — [ShyVortex/dlss-unlocked](https://github.com/ShyVortex/dlss-unlocked)
- DLSS does not work on NVK, the open Mesa NVIDIA driver; DLSS support for NVK was "in progress" as of Oct 2025 — [GamingOnLinux (Oct 2025)](https://www.gamingonlinux.com/2025/10/nvidia-dlss-support-in-progress-for-nvk-the-open-source-vulkan-driver-for-nvidia-gpus/)
- Reflex 2 Frame Warp is announced for RTX 50 (launch titles THE FINALS and VALORANT), but as of late July 2026 it had "no public release, no driver toggle, and no shipping game" — [hwbusters (Jul 2026)](https://hwbusters.com/news/nvidia-reflex-2-is-18-months-old-and-still-hasnt-shipped/); [NVIDIA Reflex 2 announcement (Jan 2025)](https://www.nvidia.com/en-us/geforce/news/reflex-2-even-lower-latency-gameplay-with-frame-warp). One NVIDIA regional page cites VALORANT numbers with Frame Warp, which conflicts with the July 2026 report; this is unresolved.

**AMD FSR 4 / Redstone**
- vkd3d-proton v3.0 (17 Nov 2025, inferred) added "support for AGS WMMA intrinsics through `VK_KHR_cooperative_matrix` and `VK_KHR_shader_float8`", which is "enough to support FSR4" ([vkd3d-proton releases](https://github.com/HansKristian-Work/vkd3d-proton/releases)):
  - The official build exposes this only with native `VK_KHR_shader_float8`, "i.e. RDNA4+ only".
  - An emulation path that "relies on int8 and float16 cooperative matrix support" runs on older GPUs "at significant performance cost".
  - v3.0 also added `AMD_anti_lag`, but "does not take frame-gen into account". v3.1 "massively simplif[ies]" it.
- vkd3d-proton v3.0.1 added "new AGS WMMA ops required for FSR4 Ray Reconstruction and Denoiser, as used in Crimson Desert" — [vkd3d-proton releases](https://github.com/HansKristian-Work/vkd3d-proton/releases)
- FSR 4 for RDNA 3 dGPUs and the Steam Machine arrived in Proton Experimental on about 22–24 June 2026 ([GamingOnLinux (24 Jun 2026)](https://www.gamingonlinux.com/2026/06/amd-fsr-4-lands-on-rdna3-dgpus-for-linux-steamos-and-steam-machine-via-proton-experimental/); [TechPowerUp](https://www.techpowerup.com/350267/proton-experimental-adds-fsr-4-support-for-rdna-3-gpus-on-linux)):
  - It is the FSR 4.1 INT8 model, because RDNA 3 lacks FP8.
  - Commenters report dGPU-only for now and no effect on RDNA4. RDNA 3.5 iGPU coverage is disputed between sources.
- FSR Redstone (ML frame generation, ray regeneration) launched with FSR SDK 2.1 as "Windows-only for now". AMD documents ML FG for RX 9000 on Windows 11 — [Phoronix](https://www.phoronix.com/news/AMD-FSR-SDK-2.1); [AMD FSR page](https://www.amd.com/en/products/graphics/technologies/fidelityfx/super-resolution.html)
- A modder ran Redstone ML FG on RDNA 3 (RX 7800 XT), Linux only. The method used OptiScaler, patched vkd3d-proton DLLs with an RDNA3 WMMA/FP8 workaround, and the INT8 upscaler. It reported "about twice as much latency" as FSR 3.1.6 FG (approx. Dec 2025) — [TechPowerUp](https://www.techpowerup.com/344284/modder-runs-amd-fsr-4-redstone-frame-generation-on-rdna-3-but-only-on-linux); [VideoCardz](https://videocardz.com/newz/modder-gets-redstone-fsr-4-frame-generation-running-on-rdna-3-gpus)

**Intel XeSS**
- XeSS 3 (multi-frame generation 2x/3x/4x through Intel Graphics Software) is "still locked to Windows" — [TechSpot](https://www.techspot.com/news/111637-intel-rolls-out-xess-30-multi-frame-generation.html)
- Under Proton, XeSS has been limited to the non-XMX fallback path, which "does not support XeSS frame generation". Intel says it is working on XMX acceleration for Proton — [TweakTown](https://www.tweaktown.com/news/114176/intel-is-working-on-adding-xmx-acceleration-for-xess-to-valves-proton/index.html)
- The community project `xess-xmx-linux` runs Intel's real XeSS libraries on XMX under Proton, including XeSS-FG and MFG. It was tested on an Arc B390 SteamOS handheld (experimental) — [Phoronix](https://phoronix.com/news/Intel-XeSS-On-XMX-Proton); [PC Gamer](https://www.pcgamer.com/hardware/handheld-gaming-pcs/experimental-project-beats-intel-in-bringing-real-intel-xess-super-resolution-to-steamos-and-proton-games-on-linux/); [GameGPU](https://en.gamegpu.com/news/zhelezo/steamos-poluchila-eksperimentalnuyu-podderzhku-xess-xmx-i-multi-frame-generation)

### Inferences
- **Status matrix for the test node (RTX 4090, Ada, driver >=580, Proton 10/Experimental/GE):**
  - DLSS SR (CNN, transformer K, 4.5 models L/M): yes.
  - DLAA and RR: yes.
  - DLSS FG 2x: yes, with occasional regressions after updates.
  - MFG 3x/4x, 6x and Dynamic MFG: no. These are RTX 50-only, and even on RTX 50 under Linux they are partial.
  - Reflex (D3D): yes. Reflex (Vulkan games): yes, through the layer or, on R615+, the layer-less path in dxvk-nvapi 0.9.3.
  - Reflex 2 Frame Warp: no, not shipped anywhere.
  - FSR 3.1 FG (vendor-agnostic) and XeSS SR (DP4a): likely yes on NVIDIA through vkd3d-proton (not directly verified in 2026 sources).
  - FSR 4 / Redstone and XeSS-FG: not on NVIDIA. The vkd3d-proton FSR4 path is AMD-AGS-intrinsic specific.
- **Upgrading node drivers:** the newest Linux features (layer-less Vulkan Reflex, OMM, newer DRS keys) are gated on R615. A node fleet pinned to the 580 series misses them. R595 headers carry the dynamic MFG DRS keys.
- **Preset and model selection for a headless container:** every lever for picking SR preset or model and FG mode is an env var on the game process (DRS through dxvk-nvapi). That suits Cha Portal's container launch path, because no NVIDIA app or GUI is needed.
- **Ownership of the FG decision:** in-game FG is a per-game, user-menu decision unless forced with `DXVK_NVAPI_DRS_NGX_DLSS_FG_OVERRIDE=on`. Forcing it fleet-wide would add latency to every game, so expose it as an opt-in.

### Gaps
- No primary source found that states the minimum driver/Proton versions for DLSS FG 2x on Linux in 2026 terms. The Nov 2024 Proton Experimental enablement is the last dated "turn-on" event found.
- I could not confirm whether DLSS 4.5 SR models (L/M) are selected by `render_preset_latest` with the DLSS DLL versions that Proton's NGX updater pulls in late 2026 (unverified).
- I found no Linux benchmark of DLSS FG or MFG latency or quality versus Windows.
- No status found for AMD Anti-Lag 2 or FSR 3.1 FG frame pacing under vkd3d-proton in 2026, beyond the `AMD_anti_lag` notes above.

## 2. Injection / override layers (OptiScaler, DLSS/FSR/XeSS upgrade env vars, dxvk-nvapi DRS)

### Takeaway
Everything an NVIDIA-app "DLSS override" does on Windows can be done headlessly on Linux with env vars:
- Valve Proton: `PROTON_ENABLE_NGX_UPDATER=1` swaps in the newest DLSS SR/FG/RR DLLs.
- dxvk-nvapi: `DXVK_NVAPI_DRS_*` forces SR/RR/FG on, sets presets, scaling ratios and (from 0.9.2) dynamic-MFG keys.
- Proton-CachyOS / GE-Proton: `PROTON_DLSS_UPGRADE`, `PROTON_FSR4_UPGRADE`, `PROTON_FSR4_RDNA3_UPGRADE` and `PROTON_XESS_UPGRADE` fetch newer DLLs.

OptiScaler works under Proton and can translate between DLSS, FSR and XeSS inputs (for example DLSS-only game to FSR 4). Its Linux FG paths are more fragile and version-sensitive.

### Cited Findings
- NVIDIA documents these env vars ([NVIDIA doc](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/gaming.html); [615 doc](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/615/gaming.html)):
  - `PROTON_ENABLE_NGX_UPDATER=1` overrides the game's shipped DLSS DLLs (SR, FG, RR). Files land under `ProgramData/NVIDIA/NGX` in the prefix. Examples: Doom Eternal's DLSS 2.3 is replaced by 310.5.2, and Cyberpunk updates `dlss`, `dlssg` and `dlssd`.
  - `DXVK_NVAPI_DRS_NGX_DLSS_{SR,RR,FG}_OVERRIDE=on`
  - `DXVK_NVAPI_DRS_NGX_DLSS_{SR,RR}_OVERRIDE_RENDER_PRESET_SELECTION=render_preset_latest`
  - `DXVK_NVAPI_SET_NGX_DEBUG_OPTIONS=DLSSIndicator=1024,DLSSGIndicator=2` turns on the on-screen indicators. They persist in the prefix registry, so set them to 0 to remove.
  - `__NGX_SHOW_INDICATOR=1024` shows the indicator for native games.
  - Caveats: "Overriding presets is currently not supported on Linux" for native games, available presets "depend on the age of the Proton release", and some titles (Deep Rock Galactic) block DLL updates.
- Proton-CachyOS env vars ([proton-cachyos README mirror](https://github.com/DevL0rd/proton-cachyos); [cachyos-10.0-20250905 release notes](https://newreleases.io/project/github/CachyOS/proton-cachyos/release/cachyos-10.0-20250905-slr); [cachyos-10.0-20260408 release notes](https://newreleases.io/project/github/CachyOS/proton-cachyos/release/cachyos-10.0-20260408-slr)):
  - `PROTON_DLSS_UPGRADE` fetches newer `nvngx_dlss(d|g).dll`. A version can be pinned, for example `310.2`. 310.4.0.0 was the latest in Sep 2025.
  - `PROTON_FSR4_UPGRADE` fetches `amdxcffx64.dll` and upgrades FSR 3.1 games to FSR 4 (default 4.0.2 as of Sep 2025).
  - `PROTON_FSR4_RDNA3_UPGRADE` is the RDNA3 variant (default 4.0.0 in an older README) — [Lemmy repost of README](https://lemmy.korz.dev/post/2215071)
  - `PROTON_XESS_UPGRADE` updates the XeSS / XeSS 2 DLLs — [PCGH](https://www.pcgameshardware.de/Linux-Software-26761/News/FSR-4-und-DLSS-4-profitieren-von-GE-Proton-1488622/)
  - An April 2026 release fixed these variables "not applying the upgrade properly".
- Proton-CachyOS can now download and generate the DLSS input files so that OptiScaler can redirect DLSS-only games to FSR 4 (date unclear, 2026) — [Wccftech](https://wccftech.com/cachyos-proton-can-now-download-dlss-files-to-enable-fsr-4-in-more-linux-games/)
- OptiScaler had a "huge new release" in April 2026 with more upscaling and FG options. The release notes call for the latest Proton with recent vkd3d-proton commits — [GamingOnLinux (Apr 2026)](https://www.gamingonlinux.com/2026/04/optiscaler-tool-gets-a-huge-new-release-with-more-upscaling-and-frame-generation-goodies/)
- Secondary guides report the following (unverified against the repo) — [iTechGuides](https://www.itechguides.com/optiscaler-explained-use-dlss-fsr-xess-and-frame-generation-in-more-pc-games/); [optiscaler.com](https://www.optiscaler.com/):
  - OptiScaler v0.9.4 (about 18 Jul 2026) targets FSR 4.1.1 / FFX 2.3.
  - Linux FSR 4 Vulkan/DX12 paths need Proton 11+.
  - FSR4-FG may need Mesa 25.2+ and a Windows 11 prefix.
  - XeSS MFG stays Intel-Arc-only.
- DLSS Updater (Recol) added Linux support in v3.3.0 (Jan 2026). By v4.3.0 (Jul 2026) it had "grown into a full Proton upscalers panel" — [GamingOnLinux (Jan 2026)](https://www.gamingonlinux.com/2026/01/nvidia-announce-dlss-4-5-with-dynamic-multi-frame-generation-plus-dlss-updater-gets-linux-support/); [GamingOnLinux (Jul 2026)](https://www.gamingonlinux.com/2026/07/dlss-updater-4-3-0-for-linux-has-grown-into-a-full-proton-upscalers-panel/page=1/)
- On non-NVIDIA GPUs, a CachyOS guide maps DLSS FG requests to FSR 3 FG (OptiScaler-style redirection) — [thenets.org](https://thenets.org/posts/pragmata-amd-dlss-frame-generation-cachyos/)

### Inferences
- **NVIDIA nodes:** for Cha Portal, the minimal zero-UI recipe is `PROTON_ENABLE_NGX_UPDATER=1` plus `DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE=on` and `..._RENDER_PRESET_SELECTION=render_preset_latest`. Optionally add the SR scaling-ratio DRS override from dxvk-nvapi 0.9.1 to pin the internal resolution. All of these can be set per game in the container env.
- **AMD nodes:** the matching recipe is `PROTON_FSR4_UPGRADE=1` (Proton-CachyOS/GE) or Proton Experimental's built-in FSR 4.1 (INT8 on RDNA3).
- **OptiScaler is a per-game DLL drop-in** (dxgi/winmm proxy) that users choose and that changes often. Treat it as user-supplied rather than something the platform bundles. Its license and redistribution terms were not checked.

### Gaps
- I did not verify the current Valve Proton 10/11 (non-Experimental) env vars for FSR4, or whether Valve exposes a `PROTON_FSR4_*` style switch.
- GE-Proton's exact env var names in late 2026 are unconfirmed. They likely mirror CachyOS (unverified).
- I did not confirm OptiScaler's repo, license, or exact Linux requirements from the primary source.

## 3. Driver-level / layer-level frame generation with no game integration

### Takeaway
There are two working options on Linux in late 2026:
- **NVIDIA Smooth Motion:** an implicit Vulkan layer `VK_LAYER_NV_present`, enabled with `NVPRESENT_ENABLE_SMOOTH_MOTION=1`, on RTX 40 and newer from driver 580.82.07 (RTX 50 from 575.51.02). It is Vulkan-only, so DXVK and vkd3d-proton games qualify. It is available on the RTX 4090 node today.
- **lsfg-vk:** the Lossless Scaling FG Vulkan layer, at v2.0 since Sep 2026. It needs a purchased Lossless Scaling, and its license is now CC-BY-NC-ND-4.0, so Cha Portal can't bundle it.

AMD AFMF has no Linux equivalent. I found no Mesa/RADV or gamescope-native frame generation. Neither layer is documented to work nested inside gamescope's WSI layer or headless, so this needs testing.

### Cited Findings
- Smooth Motion arrived in the Linux 575.51.02 beta (Apr 2025) for RTX 50 "Blackwell" — [Phoronix](https://www.phoronix.com/news/NVIDIA-575.51.02-Linux-Driver); [GamingOnLinux (Apr 2025)](https://www.gamingonlinux.com/2025/04/nvidia-575-51-02-beta-driver-brings-nvidia-smooth-motion-support-to-linux-geforce-rtx-5060-series-announced/)
- Driver 580.82.07 added Smooth Motion for RTX 40 Series. The same release fixed a 580.65.06 regression where Vulkan apps could hang on Wayland — [Phoronix](https://www.phoronix.com/news/NVIDIA-580.82.07-Linux-Driver); [9to5Linux](https://9to5linux.com/nvidia-580-82-07-supports-nvidia-smooth-motion-on-geforce-rtx-40-series-gpus)
- NVIDIA README (590.48.01), chapter "NVIDIA Smooth Motion" — [NVIDIA README nvpresent (590.48.01)](https://download.nvidia.com/XFree86/Linux-x86_64/590.48.01/README/nvpresent.html):
  - Supports "GeForce RTX 40 Series and newer", Vulkan only.
  - Enabled with `NVPRESENT_ENABLE_SMOOTH_MOTION=1`, which activates implicit layer `VK_LAYER_NV_present`. That layer "overrides the application's presentation to inject additional frames".
  - Presents from an async compute queue, which "can cause issues with third party overlays". `NVPRESENT_QUEUE_FAMILY=1` uses the graphics queue at some performance cost.
  - Logging: `NVPRESENT_LOG_LEVEL=4`, `NVPRESENT_LOG_FILE`. Debug the layer with `VK_LOADER_DEBUG=layer`.
- NVIDIA's gaming doc says: "Smooth Motion has no requirement, it can be enabled for any title without native DLSS support". It also says native DLSS FG and Smooth Motion "should not be used together", because it "results in lower performance and visual artifacts" — [NVIDIA doc](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/gaming.html)
- Field reports (anecdotal):
  - An NVIDIA forum thread is titled "Smooth motion with 40 series has really bad input lag" — [NVIDIA forums](https://forums.developer.nvidia.com/t/smooth-motion-with-40-series-has-really-bad-input-lag/343915)
  - HDR plus Smooth Motion over-saturates colors on 590.48 under Wayland (unresolved) — [NVIDIA forums](https://forums.developer.nvidia.com/t/smooth-motion-conflicting-with-hdr/357210)
  - Early reports of gamescope hanging with Smooth Motion. 575.51.02 also fixed a `VK_KHR_present_wait` issue relevant to gamescope — [Reddit r/linux_gaming via redlib](https://redlib.ssps.io/r/linux_gaming/s/p8QG5oNGPd)
  - Using `DXVK_FRAME_RATE` instead of a MangoHud limiter avoids the extra lag — [search summary of GamingOnLinux comments](https://www.gamingonlinux.com/2025/04/nvidia-575-51-02-beta-driver-brings-nvidia-smooth-motion-support-to-linux-geforce-rtx-5060-series-announced/comment_id=278053)
- A secondary guide claims that as of Oct 2026 Smooth Motion works for x86_64 Vulkan apps per docs for driver 595.45.04 (unverified) — [tech-insider.org](https://tech-insider.org/nvidia-smooth-motion-setup-2026/)
- lsfg-vk 2.0 shipped 5–7 Sep 2026 ([GamingOnLinux (Sep 2026)](https://www.gamingonlinux.com/2026/09/lossless-scalings-frame-generation-for-linux-lsfg-vk-2-0-is-out-now/); [NetGuide](https://netguide.io/news/en/2026/09/10/lossless-scaling-frame-generation-linux-lsfg-vk-2-steam-deck-2/)):
  - New bindless pipeline with FP16 acceleration, claimed 2–3x faster on 2:1 FP16 hardware.
  - About 30% of the previous memory use.
  - Minimum Vulkan lowered from 1.3 to 1.2.
  - Process matching by Linux binary, Windows exe or Steam AppID.
  - New `lsfg-vk-cli` (benchmark/validate/healthcheck) and a Qt6 UI.
  - Ships a 32-bit layer.
  - Requires owning Lossless Scaling on Steam. NetGuide says to use its "lsfg-vk" branch.
  - The project changed license and moved off GitHub. Its home is now lsfg-vk.dev.
  - Adaptive FG and dual-GPU support are still to come.
- The CachyOS package lists the lsfg-vk license as CC-BY-NC-ND-4.0 — [CachyOS package dashboard](https://dashboard.cachyos.org/package/cachyos/x86_64/lsfg-vk)
- lsfg-vk was first released in 2025 (v1, by PancakeTAS) and was later updated to Lossless Scaling's v3.1 FG model (Jul 2025) — [TechPowerUp (2025)](https://www.techpowerup.com/338745/lossless-scalings-frame-generation-lands-on-linux-works-on-steam-deck); [GamingOnLinux (Jul 2025)](https://www.gamingonlinux.com/2025/07/lossless-scalings-frame-generation-for-linux-gets-upgraded-to-the-latest-v3-1)
- A downstream Android/Wine fork (GameNative PR #2220, Oct 2026) bundles gamescope with "support for Mali and LSFG" and FSR/NIS/SGSR scaling. This shows gamescope+lsfg is being combined, but it is a fork, not upstream gamescope — [GameNative PR #2220](https://github.com/utkarshdalal/GameNative/pull/2220)

### Inferences
- **Where Smooth Motion and lsfg-vk sit in Cha Portal's stack:** both are swapchain-level Vulkan layers that hook the game's `vkQueuePresentKHR`. Under Cha Portal the game's swapchain is gamescope's (Xwayland or Wayland surface, through gamescope's WSI layer). So the layer would sit between DXVK/vkd3d-proton and the gamescope WSI layer. Layer ordering with gamescope's WSI layer is a known source of crashes: dxvk-nvapi needed a vkroots fix for its Reflex layer. Test FG with gamescope first.
- **Applying FG to gamescope itself probably won't work.** With `--backend wayland`, gamescope probably hands buffers to the parent as dmabuf `wl_buffer`s rather than through a Vulkan swapchain, so these layers would see nothing (unverified).
- **Containers need the layer manifest.** Smooth Motion is an implicit layer, so the container must carry the NVIDIA layer manifest under `.../vulkan/implicit_layer.d/` and the matching driver userspace. Check that the container runtime's driver injection includes it.
- **Licensing:** lsfg-vk's CC-BY-NC-ND-4.0 license (NonCommercial, NoDerivatives) plus the paid DLL requirement make it incompatible with shipping inside an AGPL-3.0 image. At most, Cha Portal could allow users to mount their own install.
- **Quality:** neither Smooth Motion nor lsfg-vk receives game motion vectors or a UI mask. HUD and UI ghosting will be worse than with in-game DLSS FG, and compression will magnify it (see section 6).

### Gaps
- No primary source on Smooth Motion's added latency, its GPU cost, or its behaviour with FIFO/mailbox, VRR or frame caps on Linux.
- No confirmation that Smooth Motion or lsfg-vk work inside nested gamescope with `--backend wayland`, or with no DRM/KMS display at all (headless). I found only anecdotal gamescope hang reports from 2025.
- AMD AFMF on Linux: no source found. As far as these results show, AMD has no Linux driver-level FG.
- Mesa/RADV-native or gamescope-native frame generation: no evidence found in 2025–2026 sources.
- I could not access lsfg-vk.dev directly to confirm the 2.0 license text, the gamescope compatibility notes, or latency figures.

## 4. gamescope's own scaling in 2026

### Takeaway
Upstream gamescope still offers only spatial filters: `-F fsr` (FSR 1), `-F nis`, plus linear/nearest/pixel-style options and integer, fit, fill and stretch scalers. They apply only when the nested size (`-w/-h`) is smaller than the output size (`-W/-H`). Cha Portal sets `-w = -W`, so today no filter runs. No upstream frame generation, ML upscaler or Reflex-like feature turned up in 2025–2026 sources. gamescope is now at 3.17.0 (23 Oct 2026) after a long 3.16.x series.

### Cited Findings
- gamescope tags list 3.17.0 (Oct 23, 2026), 3.16.30.1 (Oct 18), 3.16.31 (Sep 27), 3.16.30 (Sep 24), 3.16.29 (Sep 16), 3.16.28/3.16.27/3.16.23.6 (Aug 27), 3.16.26 (Aug 26) and 3.16.25 (Jul 21). The GitHub Releases page is empty (no release notes). Visible notes mention SteamOS 3.9/3.10 targets and a "no visible window regression" hotfix in 3.16.31 — [gamescope tags](https://github.com/ValveSoftware/gamescope/tags); [gamescope releases](https://github.com/ValveSoftware/gamescope/releases)
- ProtonShift's launch-options reference ([ProtonShift](https://protonshift.i4c.studio/tools/gamescope-launch-options)):
  - `-F fsr` (FSR 1.0) and `-F nis` apply only when the game resolution is below the output resolution.
  - FSR/NIS sharpness runs 0 (sharpest) to 20, default 2.
  - `--backend auto` picks Wayland in a Wayland session.
- Only one filter is active at a time. Except for NIS, the last `-F` passed wins (dated, about 2024) — [SteamTinkerLaunch wiki](https://github.com/sonic2kk/steamtinkerlaunch/wiki/GameScope)
- gamescope does not accept Wayland clients by default; `--expose-wayland` enables that. Proton games normally connect as X11 clients through gamescope's Xwayland — [ArchWiki: Gamescope](https://wiki.archlinux.org/title/Gamescope); [ProtonShift](https://protonshift.i4c.studio/tools/gamescope-launch-options)
- dxvk-nvapi 0.9.2 had to update vkroots "so the Vulkan Reflex layer works with the gamescope WSI layer". Layering under gamescope is therefore a real compatibility surface — [dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)
- In a fork (GameNative, Oct 2026), gamescope offers FSR, NIS, SGSR, integer, nearest and linear scaling. SGSR is not documented in upstream sources found — [GameNative PR #2220](https://github.com/utkarshdalal/GameNative/pull/2220)

### Inferences
- **How `-w/-h` and `-W/-H` behave in Cha Portal's nested setup:** `-w/-h` is the X11/Wayland screen size the game sees. `-W/-H` is the size of the surface gamescope submits to the Smithay compositor.
  - Setting `-w 1280 -h 720 -W 1920 -H 1080 -F fsr` would render the game at 720p, run FSR1 or NIS in gamescope's compute compositing pass, and deliver 1080p to the compositor and NVENC.
  - This upscaler is cheap and spatial only. Its quality is clearly below in-game temporal upscalers (DLSS/FSR4/XeSS), which have motion vectors and history. It does have the advantage of working for every game, including Vulkan/OpenGL titles and games without DLSS.
- **Recommended pattern:** keep `-w = -W` (as Cha Portal does now) and let the game's own DLSS/FSR 4 upscale internally. Add `-w < -W` plus `-F fsr|nis` only as a fallback for low-end GPUs or games without temporal upscalers.
- **gamescope pacing, nested:** in nested mode, gamescope's frame pacing follows the parent compositor's frame callbacks at `-r`. Any FG layer under it can only usefully deliver up to `-r` frames per second (see section 5).
- **Using upstream gamescope's own FSR1 with lsfg-vk:** since upstream ships no FG, combining them means the user loads the lsfg-vk layer into the game process, not into gamescope.

### Gaps
- No changelog content was available for gamescope 3.17.0. I could not confirm whether it adds new filters, a headless backend change, or low-latency features. Check commit history directly.
- I did not verify gamescope's current `--help` text for 3.17, so the exact `-S` scaler and `-F` filter list (including `pixel` and `linear`) is unverified.
- There is no source on gamescope's GPU cost for FSR1/NIS at 1080p→4K. It is assumed to be sub-millisecond on an RTX 4090 (unverified).

## 5. Latency and GPU-time cost; interaction with a 60/120 fps capped stream, VRR and Reflex

### Takeaway
Upscaling is cheap. DLSS SR at 4K Performance on an RTX 4090 costs about 0.5 ms (CNN), about 1.1 ms (transformer K) and about 1.4 ms (4.5 model M) per frame, and that buys a large render-time saving. Frame generation is the opposite. Interpolation holds back one real frame, and measured end-to-end PC latency rises by about 6 ms for 2x FG, plus another 2.5–4 ms for 4x MFG, at high base frame rates with Reflex on. At low base rates the increase is 10–15 ms or more. Against Cha Portal's ~5 ms send→shown LAN budget, FG at least doubles the node-side latency contribution.

For a stream capped at 120 fps, generating 240 fps (MFG 4x from a 60 fps base) wastes half the generated frames and adds latency. The useful configuration is 2x FG from a base of exactly stream_fps/2, with Reflex on.

### Cited Findings
- DLSS SR frame cost on an RTX 4090 at 4K Performance (Horizon Zero Dawn, single tester): DLSS 3.7 CNN 0.51 ms, DLSS 4 transformer preset K 1.06 ms, DLSS 4.5 model M 1.39 ms — [Medium: "DLSS Strikes Again: Transformer v2"](https://medium.com/@opinali/dlss-strikes-again-transformer-v2-04e44e63d5fc)
- NVIDIA says presets L/M are "peak performant on RTX 40 series GPUs and above". The second-generation model is "FP8 accelerated", so it is slower on RTX 20/30, which must dequantize — [Medium](https://medium.com/@opinali/dlss-strikes-again-transformer-v2-04e44e63d5fc); [HotHardware DLSS 4.5 benchmarks](https://hothardware.com/news/nvidia-dlss-45-benchmarked)
- TechSpot DLSS 4 MFG review ([TechSpot](https://www.techspot.com/article/2945-nvidia-dlss-4/)):
  - Enabling 2x FG cost about 6 ms of latency in Star Wars Outlaws and Cyberpunk.
  - Going on to 4x cost another 2.5 ms (Outlaws) or 4 ms (Cyberpunk).
  - In Alan Wake 2, native 96 fps measured 32 ms PC latency. 4x MFG tripled output fps, and latency went from 19 ms to 27 ms against the DLSS-SR + Reflex baseline.
- Cyberpunk 2077 at 4K on an RTX 5080 (DLSS 4.5 hands-on): no FG gave 79 fps / 31 ms; 4x FG gave 215 fps / 43 ms — [Yahoo Tech hands-on](https://tech.yahoo.com/gaming/articles/hands-nvidia-dlss-4-5-130000865.html)
- GameGPU measured about 29–33 ms average system latency with 4x–6x Dynamic MFG at 1440p/360 Hz, depending on engine — [GameGPU](https://en.gamegpu.com/test-gpu/test-video-cards/polnyj-razbor-tekhnologii-dinamicheskoj-generatsii-kadrov-nvidia-dlss-4-5-dlya-rtx-50)
- FG requires Reflex to be on. Blog estimates of +8–12 ms FG cost with Reflex are secondary and inconsistent — [respawno](https://respawno.com/hardware-blogs/frame-generation-input-lag); [TechSpot](https://www.techspot.com/article/2945-nvidia-dlss-4/)
- Redstone ML FG hacked onto RDNA 3 showed about 2x the latency of FSR 3.1.6 FG — [TechPowerUp](https://www.techpowerup.com/344284/modder-runs-amd-fsr-4-redstone-frame-generation-on-rdna-3-but-only-on-linux)
- Dynamic MFG adjusts the multiplier toward a target frame rate. On Linux, dxvk-nvapi exposes `NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE` and `NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX` as DRS keys (RTX 50 only) — [Tom's Hardware](https://www.tomshardware.com/pc-components/cpus/nvidia-introduces-dlss-4-5-and-multi-frame-generation-6x-at-ces-2026-updated-models-can-generate-higher-quality-upscaled-frames-and-more-of-them-dynamically); [dxvk-nvapi releases](https://github.com/jp7677/dxvk-nvapi/releases)
- vkd3d-proton 3.0.1's `VK_EXT_present_timing` support enables "smooth frame pacing for SyncInterval > 1", and present-ID decoupling "helps frame-gen in particular" — [vkd3d-proton releases](https://github.com/HansKristian-Work/vkd3d-proton/releases)

### Inferences
- **Lower bound on interpolation delay:** interpolating FG must wait for real frame N+1 before showing generated frames between N and N+1. The minimum extra delay is about one base-frame interval, partly hidden by Reflex shortening the render queue. That is about 16.7 ms at a 60 fps base and about 8.3 ms at 120 fps.
  - The measured +6 ms (2x, high base fps, Reflex) fits a base of roughly 100+ fps.
  - For a 120 fps stream from a 60 fps base, expect +10–15 ms glass-to-glass. That is 2–3x Cha Portal's whole LAN transport budget.
- **Cap the base frame rate at stream_fps / multiplier.** The nested gamescope runs at `-r` (60/90/120) and Cha Portal's compositor encodes at the stream rate. FG output above `-r` is either dropped by gamescope or back-pressures the game. MFG 4x at 240 fps into a 120 fps stream adds latency and GPU load and yields no extra smoothness.
  - Examples: 2x FG from 60 → 120, or from 45 → 90.
  - DLSS FG's own pacing needs a stable cadence matching the consumer. Dynamic MFG with target = stream fps (RTX 50 only) is the cleanest fit.
- **VRR is unavailable here.** VRR does not apply on the node: the "display" is a fixed-rate encoder clock, and the client may have VRR. Uneven FG pacing therefore turns into judder or duplicate frames at the encoder.
- **Upscaling is the better win for latency-sensitive streaming:**
  - DLSS SR Performance at 4K saves many milliseconds of render time for about 1–1.4 ms.
  - It reduces latency, because frames finish sooner.
  - It needs no change to frame cadence.
- **Use FG only as an opt-in "smoothness" mode** for slow-paced games that can't reach stream fps natively, and pair it with Reflex. On the RTX 4090 (no MFG), that means DLSS FG 2x or Smooth Motion.

### Gaps
- No Linux-specific latency measurements for DLSS FG, Smooth Motion or lsfg-vk found. All numbers above are Windows measurements.
- No official NVIDIA per-GPU, per-resolution ms table for DLSS 4/4.5 SR, RR or FG retrieved. The DLSS programming guide has such a table but was not fetched. RTX 50 vs RTX 40 numbers at 1440p are also missing.
- No GPU-time figures for FSR 4 (FP8 on RDNA4 vs INT8 on RDNA3) or XeSS 2/3 at 1440p/4K found in 2026 sources. vkd3d-proton only says the RDNA3 emulation path runs "at significant performance cost".
- No figures on Smooth Motion GPU cost or added latency.

## 6. Interaction of render-side frame generation with a downstream video encoder

### Takeaway
I found no primary source measuring how generated frames behave under real-time video compression. Generated frames are ordinary frames to NVENC, VA-API or PyroWave. Doubling fps through FG roughly doubles encoder sessions and per-frame work. At a fixed bitrate it halves bits per frame. Interpolation artifacts, especially on HUD/UI elements and with game-agnostic FG such as Smooth Motion or lsfg-vk, are likely to be amplified by low-delay, 1-frame-VBV encoding.

### Cited Findings
- Smooth Motion "overrides the application's presentation to inject additional frames". Generated frames are therefore presented like real frames to whatever consumes the swapchain, which in Cha Portal is gamescope and then the compositor and encoder — [NVIDIA README nvpresent](https://download.nvidia.com/XFree86/Linux-x86_64/590.48.01/README/nvpresent.html)
- Smooth Motion presents from an async compute queue, which "can cause issues with third party overlays". This is relevant to mangoapp/MangoHud in Cha Portal's gamescope `--mangoapp` setup — [NVIDIA README nvpresent](https://download.nvidia.com/XFree86/Linux-x86_64/590.48.01/README/nvpresent.html)
- Native DLSS FG combined with Smooth Motion produces "visual artifacts" and lower performance — [NVIDIA doc](https://docs.nvidia.com/datacenter/tesla/driver-installation-guide/gaming.html)
- HDR plus Smooth Motion caused severe over-saturation on 590.48/Wayland, relevant if the pipeline carries HDR — [NVIDIA forums](https://forums.developer.nvidia.com/t/smooth-motion-conflicting-with-hdr/357210)

### Inferences
- **Encoder load:** NVENC's per-frame cost depends on resolution and preset, not on whether a frame was rendered or generated. A 60 → 120 fps FG stream costs the same encoder time as a native 120 fps stream. Bitrate needs to rise less than 2x for the same quality, because adjacent frames are more similar, but it stays well above the 60 fps bitrate (unverified, no measurement found).
- **GPU contention on one GPU:** FG plus encode on the same GPU also competes with the game. Smooth Motion's async-compute work and NVENC run on separate engines, but they share memory bandwidth (unverified).
- **Artifacts under compression:** in-game DLSS FG and FSR FG get UI and HUD-less inputs from the game, so HUD artifacts are smaller. Smooth Motion and lsfg-vk operate on the final composited image, gamescope's mangoapp overlay included if it is composited before the layer, and have no UI mask. Text and HUD ghosting appears on every generated frame. At a 1-frame VBV it costs bits that real detail then lacks, so expect more visible artifacts than on a local display.
- **Order of layers:** generation should happen before any spatial upscaling in gamescope, and overlays should ideally be composited after FG. With gamescope + mangoapp the overlay is drawn by gamescope after the game's (FG-layered) present, which keeps the overlay itself clean (unverified).
- **Frame cadence:** pacing jitter from FG (generated frames arriving unevenly relative to the 60/90/120 Hz encode clock) leads to duplicated or dropped frames at the encoder. vkd3d-proton's present-timing work and Dynamic MFG's target-fps mode are the relevant mitigations.

### Gaps
- No measurements found on encoding bitrate or quality for FG streams versus native-fps streams. The client-side and codec research tracks may cover this.
- No source on NVENC behaviour when FG output fps exceeds the encode rate, or on how gamescope nested drops excess frames.
- No study found of HUD ghosting visibility after H.264/HEVC/AV1 compression for Smooth Motion, lsfg-vk or DLSS FG.
