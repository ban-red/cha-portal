# Node-side HDR on a headless Linux streaming node: from "an app draws HDR" to "a 10-bit/FP16 buffer reaches the encoder" (November 2026)

> Evidence for [research note 12](README.md).

Scope: what has to happen on a Cha Portal GPU node (RTX 4090 with driver 580+ and R615 tracked, plus Intel/AMD VA-API nodes) for HDR from containerised clients to reach the encoder. The clients are Steam Big Picture in nested gamescope with Proton games, Chrome, Firefox, KDE Plasma 6 and XFCE. In between sit `cha-streamer`'s own headless Smithay compositor, its output pool, and the CUDA/NVENC and VA-API paths. Research date: 2026-11-09. Encoder-side bitstream signalling, browser decode and the macOS player are covered in sibling notes (`codec.md`, `browser.md`, `native-client.md`) and are only touched on here.

Method: most claims below were read directly from source at these revisions:
- gamescope `master` at `ecc518b` (2026-10-28), plus the `3.16.20` tag that Ubuntu 26.04 ships.
- wayland-protocols `main` (1.50) and Proton's `proton_10.0`/`proton_11.0`/`experimental_*`/`bleeding-edge` launchers.
- GE-Proton `master`, DXVK `master`, KWin `master` and Mutter `main`.
- Mesa `main`, Chromium `main` and Firefox `main` (162.0a1).
- Smithay `master` (`65aa86fa`, 2026-11-03) and the `dividebysandwich/smithay` HDR forks.
- niri `main` (`90d5c1b`, 2026-11-08), cosmic-comp `main` (`46b26e6`, 2026-11-06) and Sunshine `master`.

Tag dates are the dates of the tagged commits, so they are approximate. Anything secondary or unconfirmed is marked as such.

Cha's current state (from the repo):
- The output pool picks only `[Xrgb8888, Argb8888]` from the renderer's formats ([output.rs](../../../crates/cha-streamer/src/compositor/output.rs), `formats` closure near line 330).
- Each GBM buffer is registered with CUDA as an EGL image.
- NVENC and VA-API signal BT.709 limited, 8-bit ([README](../../../crates/cha-streamer/README.md)).
- `steam-gaming` runs gamescope without `--hdr-enabled` ([steam-gaming](../../../images/steam/steam-gaming)).
- The compositor advertises no colour-management protocol.
- The gamestream handoff already returns 422 for HDR requests ([README](../../../crates/cha-streamer/README.md)).

## 1. Games under Proton: how DXVK/vkd3d-proton, Wine and gamescope's WSI layer produce HDR, and what Steam Big Picture needs before it shows its HDR toggle

### Takeaway
In Cha's setup, games present through Wine's X11 driver to gamescope's Xwayland. In that path, HDR is decided entirely by gamescope:
- gamescope's Vulkan WSI layer offers the HDR10/PQ swapchain formats (`A2B10G10R10`/`A2R10G10B10` + `HDR10_ST2084`) and scRGB (`R16G16B16A16_SFLOAT` + `EXTENDED_SRGB_LINEAR`) only when the `GAMESCOPE_HDR_OUTPUT_FEEDBACK` root property is 1.
- That property is 1 only with `--hdr-enabled` and an HDR-capable connector, or with the `--hdr-debug-force-*` flags.
- In nested mode the connector is "HDR-capable" only when the host compositor reports headroom.
- DXVK (DX11) and vkd3d-proton (DX12) both use DXVK's DXGI. On Valve Proton, DXGI reports HDR to the game only with `DXVK_HDR=1` / `dxgi.enableHDR`.

Valve's Proton 10/11/Experimental launchers have no Wine-Wayland or HDR switch. `PROTON_ENABLE_WAYLAND`, `PROTON_ENABLE_HDR` and `PROTON_FORCE_HDR` are GE-Proton/CachyOS features, and GE-Proton itself says to keep the XWayland driver inside gamescope. No physical monitor or EDID is needed in nested mode: gamescope fabricates the EDID it gives Xwayland.

### Cited Findings
**DXVK / vkd3d-proton**
- DXVK sets `enableHDR = config.getOption<bool>("dxgi.enableHDR", env::getEnvVar("DXVK_HDR") == "1")`, so HDR is off unless the env var or config is set. It force-disables HDR for UE4 DX11 titles (`-Win64-Shipping` executable without `d3d12.dll`) unless NVAPI is enabled — [DXVK dxgi_options.cpp](https://github.com/doitsujin/dxvk/blob/master/src/dxgi/dxgi_options.cpp)
- `dxvk.conf` describes `dxgi.enableHDR` as follows: "This shows to the game that the global Windows 'HDR Mode' is enabled. Many (broken) games will need this ... This defaults to the value of the DXVK_HDR environment variable." — [dxvk.conf](https://github.com/doitsujin/dxvk/blob/master/dxvk.conf)
- GE-Proton documents that HDR "covers DX11 through DXVK and DX12 through VKD3D-Proton: both use the DXVK-provided DXGI for output queries, with their respective Vulkan presenters checking HDR surface support" — [GE-Proton README](https://github.com/GloriousEggroll/proton-ge-custom/blob/master/README.md)

**Valve Proton vs GE-Proton / CachyOS**
- Valve's launcher scripts on `proton_10.0`, `experimental_10.0`, `proton_11.0`, `experimental_11.0` and `bleeding-edge` contain no Wine-Wayland driver or HDR option. The only Wayland mentions are an OPWR comment and `WINE_DISABLE_VULKAN_OPWR` / `WINE_USE_KWIN_HACKS` in the README (read 2026-11-09) — [Proton proton_11.0 launcher](https://github.com/ValveSoftware/Proton/blob/proton_11.0/proton); [Proton experimental_11.0 README](https://github.com/ValveSoftware/Proton/blob/experimental_11.0/README.md)
- A single-developer report, consistent with the source: "Valve's Proton Experimental 11 ignores `PROTON_ENABLE_WAYLAND` and `PROTON_ENABLE_HDR`, leaving the game on X11 with HDR unavailable; GE-Proton honors them." — [franzjeger/cosmic-hdr-reference README](https://github.com/franzjeger/cosmic-hdr-reference)
- An older blog (about early 2026, date approximate) said HDR did not work with Proton 10 or Experimental but did with GE-Proton10-26. Community posts pair `PROTON_ENABLE_WAYLAND=1 PROTON_ENABLE_HDR=1` — [wolf.nereid.pl](https://wolf.nereid.pl/posts/hdr-games/); [CachyOS forum](https://discuss.cachyos.org/t/proton-with-wayland-driver-new-parameters-etc/18930)
- GE-Proton (latest tag `GE-Proton11-12`) now auto-detects HDR, according to its README ([GE-Proton README](https://github.com/GloriousEggroll/proton-ge-custom/blob/master/README.md)):
  - `PROTON_ENABLE_HDR`/`PROTON_USE_HDR` are "no longer required", and setting them to 1 "no longer forces HDR on Wayland". `PROTON_FORCE_HDR=1` forces it, and `DXVK_NO_HDR=1` disables reporting.
  - "Gamescope's XWayland path uses its HDR output feedback."
  - "When using Gamescope, keep its HDR configuration and use Wine's default XWayland driver instead of adding the Wayland option."
- GE-Proton's Wine Wayland path treats an output as HDR "only when the maximum is above the reference" luminance from the colour-management protocol ([GE-Proton README](https://github.com/GloriousEggroll/proton-ge-custom/blob/master/README.md)):
  - GE-Proton11-10 and 11-11 briefly read KWin's private `kde-output-device-v2` instead.
  - That was removed after KWin and colour-management developers objected.
- GE-Proton launcher logic ([GE-Proton `proton`](https://github.com/GloriousEggroll/proton-ge-custom/blob/master/proton)):
  - `PROTON_ENABLE_WAYLAND`/`PROTON_USE_WAYLAND` select the Wayland driver only if `WAYLAND_DISPLAY` is set.
  - `forcehdr` sets `DXVK_HDR=1` and `WAYLANDDRV_FORCE_HDR=1`, and `hdr` sets `DXVK_HDR=1`.
  - Otherwise `DXVK_HDR` is left unset so that "shared DXGI detects HDR ... from Wine Wayland output state or Gamescope XWayland feedback". This is a GE patch, not upstream DXVK behaviour.

**gamescope WSI layer and Steam signalling**
- The gamescope WSI layer (`layer/VkLayer_FROG_gamescope_wsi.cpp`) ([gamescope WSI layer](https://github.com/ValveSoftware/gamescope/blob/master/layer/VkLayer_FROG_gamescope_wsi.cpp)):
  - Adds the extra surface formats `{A2B10G10R10_UNORM_PACK32, HDR10_ST2084}`, `{A2R10G10B10_UNORM_PACK32, HDR10_ST2084}` and `{R16G16B16A16_SFLOAT, EXTENDED_SRGB_LINEAR}` only if `shouldExposeHDR()`. That function is `hdrOutput && !DisableHDR`.
  - Reads `hdrOutput` from the X root property `GAMESCOPE_HDR_OUTPUT_FEEDBACK`.
  - Appends `VK_EXT_hdr_metadata`.
  - Sets `DXVK_HDR=0` for apps it blocklists, for example a game that "picks a HDR colorspace ... but does not render as HDR".
  - Notes that "an HDR colorspace always bypasses" Xwayland.
- `steamcompmgr.cpp` publishes two root properties ([gamescope steamcompmgr.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/steamcompmgr.cpp)):
  - `GAMESCOPE_HDR_OUTPUT_FEEDBACK = (bOutputHDRCapable || g_bForceHDRSupportDebug)`, with `bOutputHDRCapable = (g_bSupportsHDR_CachedValue || g_bForceHDR10OutputDebug) && cv_hdr_enabled`.
  - `GAMESCOPE_DISPLAY_SUPPORTS_HDR` = `connector->SupportsHDR()`. `SupportsHDR()` is `IsHDR10()` = `bExposeHDRSupport && eOutputEncodingEOTF == EOTF_PQ` ([backend.h](https://github.com/ValveSoftware/gamescope/blob/master/src/backend.h)).
  - Debug atoms `GAMESCOPE_DEBUG_FORCE_HDR_SUPPORT` and `GAMESCOPE_DEBUG_FORCE_HDR10_PQ_OUTPUT`.
  - SDR-on-HDR, ITM and tonemap atoms (`GAMESCOPE_SDR_ON_HDR_CONTENT_BRIGHTNESS`, `GAMESCOPE_HDR_ITM_*`, `GAMESCOPE_HDR_TONEMAP_*`).
- gamescope `--help` text ([gamescope main.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp)):
  - `--hdr-enabled`: "enable HDR output (needs Gamescope WSI layer enabled for support from clients)".
  - `--hdr-debug-force-support`: "forces support for HDR, etc even if the display doesn't support it. HDR clients will be outputted as SDR still in that case."
  - `--hdr-debug-force-output`: "forces support and output to HDR10 PQ even if the output does not support it (will look very wrong if it doesn't)".
- Commit `6513879` "steamcompmgr: Add content-driven HDR output mode" (first tag 3.16.29, about 2026-09) ([gamescope 6513879](https://github.com/ValveSoftware/gamescope/commit/6513879)):
  - Adds a `hdr_content_driven` convar: the panel is in PQ only while any live window has an HDR commit.
  - Makes `GAMESCOPE_HDR_OUTPUT_FEEDBACK` report capability rather than live state, because "The WSI layer reads it once at surface creation, so a game launched while the panel idles in SDR would otherwise never be offered an HDR colorspace."
- The nested Wayland connector sets `m_HDRInfo.bAlwaysPatchEdid = true`, so gamescope always synthesises the EDID it gives Xwayland rather than needing a real one ([gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)).
- Secondary sources (ArchWiki content seen only via search snippets, because the page was bot-blocked when fetched) ([ArchWiki HDR](https://wiki.archlinux.org/title/HDR); [ArchWiki Gamescope](https://wiki.archlinux.org/title/Gamescope)):
  - Steam shows its HDR settings under Display only when the Steam session runs in gamescope with HDR on, after which "HDR" and "Experimental HDR support" are enabled.
  - Nested HDR10 needs `--hdr-enabled`.
- A user reports that `--hdr-debug-force-output` makes in-game HDR options appear "but the output isn't real HDR" (anecdotal) — [Answer Overflow (Bazzite)](https://www.answeroverflow.com/m/1368681192123142265)

### Inferences
- **Minimum for a Proton game's HDR10/scRGB swapchain inside Cha's gamescope:**
  1. Add `--hdr-enabled` to `steam-gaming`.
  2. Make gamescope's Wayland connector report HDR10 (section 2), or pass `--hdr-debug-force-output`.
  3. Set `DXVK_HDR=1` in the game environment on Valve Proton. GE-Proton's auto-detection is not available there.
  4. The user turns on the game's own HDR option.
- **Steam's HDR toggle:** most likely keyed on `GAMESCOPE_DISPLAY_SUPPORTS_HDR` (inferred from the atom name and the ArchWiki snippet, not verified against the closed Steam client). That atom follows `IsHDR10()`, so the host compositor's advertised headroom ultimately controls whether the toggle appears.
- **`--hdr-debug-force-support` is not useful for streaming.** It lets games pick HDR swapchains but outputs SDR. `--hdr-debug-force-output` makes gamescope encode PQ whatever the host says. That is only safe if Cha's compositor knows to treat gamescope's surface as BT.2020/PQ, either by an out-of-band convention or by implementing the protocol properly.
- **No hardware dependency:** nothing in this path needs a monitor, EDID or KMS connector. The "display" is Cha's compositor, and its luminance numbers are arbitrary. Ideally they come from the viewer's display, which other notes cover.
- **Do not set `PROTON_ENABLE_WAYLAND` inside gamescope** (GE-Proton's own advice). The Wine-Wayland HDR path matters only if Cha ever runs Proton games directly on its compositor without gamescope. Even then it needs GE-Proton or CachyOS, plus Cha's compositor reporting max luminance > reference.

### Gaps
- I could not confirm who sets `DXVK_HDR=1` in a SteamOS-style `-steamos3 -gamepadui` session: Steam itself, a session script, or nobody. The WSI layer only ever forces it to 0.
- No vkd3d-proton-specific HDR switch was checked. GE-Proton's statement that vkd3d-proton uses DXVK's DXGI was not checked against vkd3d-proton source.
- The exact property or atom Steam's closed client reads before showing "HDR" in Big Picture is unverified.
- I did not check whether upstream Wine (as opposed to GE patches) has wp_color_management HDR in winewayland.drv as of Wine 11.x.

## 2. Nested gamescope (`--backend wayland`): which host protocols it needs for HDR, what buffers and transfer functions it hands the host, its SDR-in-HDR/ITM options, and which version made nested HDR work

### Takeaway
gamescope's Wayland backend speaks two host protocols: `frog_color_management_v1` (any version present) and `wp_color_manager_v1` (bound at version 1). It does not use `wp_color_representation_v1`.

With wp_color_management, gamescope requires all of the following:
- **Features:** parametric, set_primaries, set_mastering_display_primaries, extended_target_volume, set_luminances and windows_scrgb.
- **Transfer function:** PQ.
- **Primaries:** sRGB and BT.2020.

Ubuntu's 3.16.20 (Cha's base, Ubuntu 26.04) also requires the deprecated `srgb` transfer function, and Ubuntu's newer 3.16.24 still does. That requirement was dropped upstream only in 3.16.25 (July 2026).

When it is satisfied, gamescope exposes HDR only if `--hdr-enabled` is set and the host's preferred image description has target max luminance > reference luminance (3.16.18+).

It then forwards each app buffer as its own subsurface with an image description:
- scRGB FP16 as `windows_scrgb`.
- 10-bit PQ as parametric BT.2020/PQ plus mastering metadata.

When it must composite, it sends one BT.2020/PQ plane in `XBGR2101010` (or `XRGB2101010`). Nested HDR via frog has worked since the 2024 Wayland-backend rewrite (3.14.x). wp_color_management_v1 nested HDR first shipped in 3.16.5 (May 2025).

### Cited Findings
**Protocol binding and feature gate**
- gamescope binds `frog_color_management_factory_v1` and `wp_color_manager_v1` at version 1. `SupportsColorManagement()` is `m_pFrogColorMgmtFactory != nullptr || (m_pWPColorManager && bSupportsGamescopeColorManagement)`. No `wp_color_representation` symbol appears in `WaylandBackend.cpp` — [gamescope WaylandBackend.cpp (master)](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)
- Master's `bSupportsGamescopeColorManagement` requires these features: `PARAMETRIC`, `SET_PRIMARIES`, `SET_MASTERING_DISPLAY_PRIMARIES`, `EXTENDED_TARGET_VOLUME`, `SET_LUMINANCES` and `WINDOWS_SCRGB`. It also requires TF `ST2084_PQ` and primaries `SRGB` and `BT2020` — [gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)
- Tag `3.16.20` has the same list plus `WP_COLOR_MANAGER_V1_TRANSFER_FUNCTION_SRGB` — [gamescope 3.16.20 WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/3.16.20/src/Backends/WaylandBackend.cpp)
- Commit `f1156ce` (2026-06-19, first tag 3.16.25) "don't require the 'SRGB' transfer function" says: "It's deprecated because it doesn't actually describe how sRGB content should be displayed, and gamescope didn't use it either way." — [gamescope f1156ce](https://github.com/ValveSoftware/gamescope/commit/f1156ce)
- Ubuntu publishes gamescope 3.16.20+ds-1 in resolute (26.04), 3.16.15-2 in questing (25.10), and 3.16.24+ds-2 in the two newer series — [Launchpad gamescope](https://launchpad.net/ubuntu/+source/gamescope)

**HDR exposure and what is sent to the host**
- With wp_color_management, gamescope:
  - Gets the surface feedback's `get_preferred` description and its `get_information`, using a blocking roundtrip.
  - Sets `bExposeHDRSupport = cv_hdr_enabled && m_uMaxTargetLuminance > m_uReferenceLuminance`, where both default to 203.
  - Sets the output EOTF to PQ if exposed, otherwise Gamma 2.2.
  - Logs "HDR INFO ... uMaxLum, uRefLum".

  This replaced the earlier TF==PQ check in `7d4e835` (2025-10-25), whose message says "HDR doesn't depend on PQ or any specific transfer function" (first tag 3.16.18, about 2025-12) — [gamescope 7d4e835](https://github.com/ValveSoftware/gamescope/commit/7d4e835)
- With frog, `bExposeHDRSupport = cv_hdr_enabled && preferred transfer function == ST2084_PQ`. The host's preferred metadata also supplies display primaries and max/min/full-frame luminance — [gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)
- Per-plane presentation (the connector has 9 planes as subsurfaces) ([gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)):
  - **scRGB** layer: `wp_color_manager_v1_create_windows_scrgb`.
  - **HDR10_PQ** layer: a parametric creator with `BT2020` (or custom primaries scaled by convar `wayland_hdr10_saturation_scale`), `ST2084_PQ`, and the mastering primaries and luminance from the app's HDR metadata infoframe. Render intent is perceptual.
  - **SDR**: unsets the description.
  - **frog** equivalents: REC709 + GAMMA_22, REC2020 + ST2084_PQ, REC709 + SCRGB_LINEAR.
- When a full composite is needed, gamescope runs `vulkan_composite` and presents one plane with colorspace `HDR10_PQ` if `outputEncodingEOTF == EOTF_PQ`, else sRGB ([gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)). Triggers are:
  - FSR/NIS/SGSR upscaling, blur, or nearest/pixel filter scaling.
  - Colour slider, fade, or ReShade.
  - ITM enabled with HDR output.
  - An HDR layer when the host lacks colour management.
- `GetPreferredOutputFormat` picks `XBGR2101010`, then `XRGB2101010`, from the host's linux-dmabuf formats for 10-bit, and `XRGB8888`/`XBGR8888`/`ARGB8888`/`ABGR8888` for 8-bit. The backend binds `zwp_linux_dmabuf_v1` v3, `wl_compositor` v4, `wl_subcompositor`, `wp_viewporter`, `wp_presentation`, `wp_single_pixel_buffer_manager_v1` and `wp_fractional_scale_manager_v1` — [gamescope WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)
- Composite intermediates ([gamescope rendervulkan.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/rendervulkan.cpp)):
  - "scRGB needs FP16 range. PQ needs 10 bits and UNORM clamps filter overshoot below 0 before the PQ decode", so scRGB uses `ABGR16161616F` and PQ uses `ABGR2101010`.
  - NVIDIA lacks linear `A2R10G10B10` storage-image support, and "imageStore lands in XBGR order there", so captures use `XBGR2101010`.

**CLI options and version history**
- HDR CLI options and defaults ([gamescope main.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp)):
  - `--hdr-sdr-content-nits` "Default: 400 nits".
  - `--sdr-gamut-wideness` (0–1).
  - `--hdr-itm-enabled` ("SDR->HDR inverse tone mapping. only works for SDR input"), plus `--hdr-itm-sdr-nits` and `--hdr-itm-target-nits`.
  - `--hdr-debug-heatmap`.
- Recent HDR fixes: `96bfc10` (2026-10-18, NIS in linear HDR mode for scRGB), `43d4bb0` (2026-10-06, keep HDR range in the upscale intermediate) and `ecc518b` (2026-10-28, untagged, "fix overflow when forwarding mastering primaries"). The latest tag is 3.17.0 (about 2026-10-19) — [gamescope commits](https://github.com/ValveSoftware/gamescope/commits/master); [gamescope tags](https://github.com/ValveSoftware/gamescope/tags)
- Version history ([gamescope ce1156a](https://github.com/ValveSoftware/gamescope/commit/ce1156a); [e2a277e](https://github.com/ValveSoftware/gamescope/commit/e2a277e); [1feb11f](https://github.com/ValveSoftware/gamescope/commit/1feb11f); [0a84f57](https://github.com/ValveSoftware/gamescope/commit/0a84f57); [a5302a8](https://github.com/ValveSoftware/gamescope/commit/a5302a8); [cd8141d / PR #1867](https://github.com/ValveSoftware/gamescope/pull/1867); [4065f30](https://github.com/ValveSoftware/gamescope/commit/4065f30)):

  | Commit | Date | First tag | Change |
  |---|---|---|---|
  | `ce1156a` | 2024-03-01 | 3.14.3 (about 2024-04) | New Wayland backend, already containing frog colour management |
  | `e2a277e` | 2024-07-24 | 3.14.26 | xx-color-management-v3 "beginnings" |
  | `1feb11f` | 2025-03-15 | 3.16.5 (about 2025-05-05) | "Support wp-color-management-v1" |
  | `0a84f57` | 2025-04-27 | 3.16.5 | Forward HDR metadata |
  | `a5302a8` | 2025-05-05 | — | Ship the color-management-v1 XML |
  | `cd8141d` (PR #1867) | 2025-08-01 | 3.16.18 | Check features before using colour management; fixes a protocol error and "allows gamescope on gnome/mutter" |
  | `4065f30` | 2026-08-26 | 3.16.29 | Clear frog transfer function on passthrough |

**Host-compositor compatibility**
- Issue #2221 ([gamescope issue #2221](https://github.com/ValveSoftware/gamescope/issues/2221); [KDE Discuss](https://discuss.kde.org/t/gamescope-hdr-on-6-7/47895)):
  - HDR stopped being exposed nested on Plasma 6.7, because `SupportsColorManagement()` returned false on the SRGB-TF requirement.
  - A KDE Discuss thread says Plasma 6.7.1 fixed it. The mechanism is unverified.
- KWin `master` advertises no `srgb` TF at any bound version: only gamma22, st2084_pq, ext_linear, bt1886, hlg, plus compound_power_2_4 at v2+. Its `src/wayland` tree has `colormanagement_v1` and `colorrepresentation_v1` but no frog implementation file — [KWin colormanagement_v1.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/colormanagement_v1.cpp)
- Mutter `main` advertises only `ICC_V2_V4`, `PARAMETRIC`, `SET_PRIMARIES`, `SET_TF_POWER` and `SET_LUMINANCES`. It has no `SET_MASTERING_DISPLAY_PRIMARIES`, `EXTENDED_TARGET_VOLUME` or `WINDOWS_SCRGB` feature, and its mastering-display requests post "not supported" errors. gamescope's wp path is therefore disabled under GNOME — [Mutter meta-wayland-color-management.c](https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/wayland/meta-wayland-color-management.c)
- An Umbriel feature request says nested gamescope hands its HDR image to the host as scRGB and asks for `windows_scrgb` / frog support (2026, single project) — [noctalia-dev/umbriel #535](https://github.com/noctalia-dev/umbriel/issues/535)

### Inferences
- **Getting HDR out of Ubuntu's gamescope 3.16.20**, which Cha builds from source, needs one of these routes:
  - **(a) Implement frog server-side.** It is a small protocol: send `preferred_metadata` with TF = PQ and luminances. gamescope then skips the feature gate.
    - Catch: in both 3.16.20 and master, plane creation checks `if (GetWPColorManager())` first and only falls back to frog when no `wp_color_manager_v1` global was bound ([WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/WaylandBackend.cpp)).
    - So the frog route needs Cha to hide `wp_color_manager_v1` from gamescope's connection, for example with a per-client global filter, while still showing it to Chrome, Firefox and KWin.
  - **(b) Implement `wp_color_manager_v1` with all six features, PQ, sRGB/BT.2020 primaries, and the deprecated `srgb` TF.** Advertising `srgb` should be fine to a version-1 binder, since it is deprecated only "since 2". This is the simpler single-protocol route. Alternatively, bump the Ubuntu source to ≥3.16.25, or carry `f1156ce` as a second patch next to `gamescope-wayland-mangoapp.patch`.
  - **Either way, the preferred image description must report target max luminance above reference** (for example ref 203, target max 1000), or gamescope stays SDR.
- **What Cha's compositor receives once this works:** in the common passthrough case it is not one gamescope buffer. It gets several subsurfaces:
  - The game's own swapchain image (FP16 scRGB or 10-bit PQ dmabuf, with per-surface metadata).
  - SDR planes for Steam overlay and mangoapp (`--mangoapp` overlay).

  So Cha must composite mixed image descriptions per surface, not just tag one output. Upscaling (FSR/NIS), blur, fades and ITM force gamescope to composite to a single PQ 10-bit plane instead.
- **Version choice:** moving to upstream 3.17.0 also brings content-driven HDR output, the capability-not-state feedback fix, and the scRGB NIS and upscale-range fixes. All of these help a streaming node, where HDR should follow the game rather than a panel toggle.
- **Option split between the two compositors:**
  - Where gamescope's own SDR-on-HDR and ITM options (`--hdr-sdr-content-nits`, `--hdr-itm-*`) apply, gamescope tone-maps before Cha sees anything.
  - If Cha composites SDR itself (passthrough), gamescope's SDR planes arrive untagged or sRGB, and Cha's own SDR-in-HDR mapping applies (section 5). Choose one owner for this to avoid double mapping.

### Gaps
- I did not test how gamescope ≤3.16.24 behaves on a host that offers both frog and wp_color_manager_v1 but fails the wp feature gate.
  - From the code, planes use wp surfaces whenever the wp manager is bound.
  - `SupportsColorManagement()` is still true because frog is bound.
  - The pre-built HDR10/scRGB wp image descriptions are not created.
  - The likely result is broken or missing HDR tagging rather than a clean frog fallback (inference).
  - Advertise either a complete wp set or frog alone to gamescope.
- Not checked: whether gamescope's nested mode honours host `wp_presentation` timings for HDR frames differently, and whether `--mangoapp` forces a full composite.
- The exact first gamescope tag carrying frog nested HDR end to end (as opposed to the protocol objects) was not separately verified.

## 3. wp_color_management_v1: when it merged, which compositors implement it by November 2026, and Smithay's status (PRs, forks, reusable MIT code)

### Takeaway
`color-management-v1` entered wayland-protocols *staging* in 1.41 (2025-02-17); `color-representation-v1` followed in 1.44 (2025-04-23). The protocol is at version 3 (`create_windows_bt2100`) since 1.49 (2026-06-07) and is still staging in 1.50 (2026-10-29). Status by compositor:

- **KWin:** v3 with every feature gamescope wants.
- **Mutter:** since 48, but without scRGB or mastering features.
- **wlroots/sway:** HDR10 on the Vulkan renderer in 0.20 / sway 1.12.
- **Hyprland:** implements it.
- **niri and cosmic-comp:** nothing upstream (both Smithay-based).
- **Smithay upstream:** nothing either. Drakulix's PR #1143 is still a 2023 draft that "just compiles".

The reusable code is in MIT-licensed forks, chiefly `dividebysandwich/smithay` branch `ds-hdr-master`: a 1,732-line `wp-color-management-v1` v3 server module plus color-representation, built on Smithay master of 2026-07-06. Second is `franzjeger/cosmic-hdr-reference` (MIT, hardware-validated on NVIDIA 610 and AMD).

### Cited Findings
**Protocol history and defaults**
- In git ([wayland-protocols tags](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/tags); [GamingOnLinux (Feb 2025)](https://gamingonlinux.com/2025/02/wayland-colour-management-and-hdr-protocol-finally-merged)):
  - Tag 1.40 lacks `staging/color-management/color-management-v1.xml` and 1.41 (2025-02-17) has it.
  - `staging/color-representation` was added 2025-04-22 and is first in 1.44 (2025-04-23).
  - The v2 changes are in 1.47 (2025-12-12): deprecating `srgb`/`ext_srgb` TFs, adding `compound_power_2_4`, `preferred_changed2` and `get_image_description`, and `e2bacdd` "forbid advertising deprecated".
  - v3 `create_windows_bt2100` (`c364bf6`, 2026-02-19) is in 1.49 (2026-06-07). The latest tag is 1.50 (2026-10-29).
- The XML is at `wp_color_manager_v1 version="3"` ([color-management-v1.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/color-management/color-management-v1.xml)):
  - `srgb` TF is "Deprecated (ambiguous sRGB transfer function) ... use gamma22 for video, motion picture and computer graphics, or compound_power_2_4 for ICC calibrated print workflows".
  - `st2084_pq` default luminances are min 0.005, max 10000 and reference 203 cd/m². The `set_luminances` defaults (from the sRGB spec) are 0.2/80/80.
  - `windows_scrgb`: "R=G=B=1.0 corresponds to ... 80 cd/m²", max 125.0 = 10k, and the reference white "should be R=G=B=2.5375 corresponding to 203 cd/m² of Report ITU-R BT.2408-7".
  - `windows_bt2100` is BT.2020/PQ with an assumed 203 cd/m² reference.
- The Rust `wayland-protocols` crate is 0.32.13 in Cha's lockfile. Another project's Oct 2026 notes say the colour-management XML is vendored there as staging — [Cha Cargo.lock](../../../Cargo.lock); [scoot PR #564](https://github.com/scoot-sh/scoot/pull/564)

**Compositor implementations**
- **KWin** ([KWin colormanagement_v1.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/colormanagement_v1.cpp); [KWin cc49de8f](https://invent.kde.org/plasma/kwin/-/commit/cc49de8f)):
  - Registers `wp_color_manager_v1` at version 3.
  - Advertises parametric, extended_target_volume, set_mastering_display_primaries, set_primaries, set_luminances, windows_scrgb, and windows_bt2100 (v3).
  - Advertises 10 named primaries and TFs gamma22, PQ, ext_linear, bt1886, hlg and compound_power_2_4 (v2+).
  - Has a `colorrepresentation_v1` server.
  - Switched to the upstream protocol in `cc49de8f` (2025-02-13), first in v6.3.90, so Plasma 6.4.
- **Mutter:** "Update to upstream wp_color_management_v1" is `4fe1e41a` (2025-02-25), first tag 48.0. The earlier xx v4 implementation is `2341346c` (2024-08-29, GNOME 47). Main still lacks the scRGB, mastering and extended-volume features (section 2) — [Mutter 4fe1e41a](https://gitlab.gnome.org/GNOME/mutter/-/commit/4fe1e41a); [Mutter source](https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/wayland/meta-wayland-color-management.c)
- **wlroots/sway:** wlroots merged color-management-v1 in 2025. wlroots 0.20 / sway 1.12 bring "full Vulkan renderer, back-end, and scene-graph coverage" and HDR10 with the Vulkan renderer. Wikipedia dates sway 1.12 to 2026-05-25 — [Phoronix (wlroots CM)](https://www.phoronix.com/news/wlroots-color-management); [Phoronix (wlroots 0.20)](https://www.phoronix.com/news/wlroots-0.20-Sway-1.12-rc1); [Wikipedia Sway](https://en.wikipedia.org/wiki/Sway_(window_manager))
- **Hyprland:** PR #9444 "Support wp color management proto" and PR #11084 (wp-color-management transfer functions incl. PQ/HLG, about Jul 2025, merged per a third-party tracker) — [Hyprland #9444](https://github.com/hyprwm/Hyprland/pull/9444); [Hyprland #11084](https://github.com/hyprwm/Hyprland/pull/11084)
- **Weston:** status not verified in this pass.
- **niri** `main` (`90d5c1b`, 2026-11-08, pinned to Smithay `65aa86fa`): no colour-management protocol code. Its only HDR code resets the connector's `HDR_OUTPUT_METADATA`, and it lists 10-bit scanout formats (`SUPPORTED_COLOR_FORMATS_10BIT = [Abgr2101010, Argb8888, Abgr8888]`) — [niri tty.rs](https://github.com/niri-wm/niri/blob/main/src/backend/tty.rs)
- An older niri discussion pointed to Smithay #1143 as the blocker ("probably not any time in the near future") — [niri discussion #1128](https://github.com/niri-wm/niri/discussions/1128)
- **cosmic-comp** `main` (`46b26e6`, 2026-11-06): no HDR or colour-management code. The HDR issue is still open — [pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp); [cosmic-comp #1384](https://github.com/pop-os/cosmic-comp/issues/1384)

**Smithay upstream**
- Smithay `master` (`65aa86fa`, 2026-11-03) has no `wayland/color` module. `src/backend/renderer/color.rs` is only the `Color32F` RGBA type — [Smithay master](https://github.com/Smithay/smithay)
- PR #1143 "color management/representation protocols" (Drakulix) ([Smithay PR #1143](https://github.com/Smithay/smithay/pull/1143)):
  - Draft since 2023-09-25 and described as "completely untested, just compiles".
  - Rendering pipeline "hasn't been started at all".
  - In Aug 2025 Drakulix said the PR only exposes the protocol.
  - A Jul 2026 comment announced dividebysandwich's fork. A Sep 2026 question about upstreaming it got no reply.
- Drakulix said in Dec 2025 that #1143 was the only Smithay HDR work and that COSMIC had none yet — [Wolf #222](https://github.com/games-on-whales/wolf/issues/222)

**Forks with reusable code**
- `dividebysandwich/smithay` ([dividebysandwich/smithay ds-hdr-master](https://github.com/dividebysandwich/smithay/tree/ds-hdr-master); [hdr-for-niri](https://github.com/dividebysandwich/smithay/tree/hdr-for-niri)):
  - **`hdr-for-niri`** (tip 2026-07-04, base upstream 2026-04-24) carries #1143's commits plus "port to the stabilized wp-color-management-v1", "stable identities, feedback tracking, change notifications" and "gles: renderer-level tex program override and solid color transform".
  - **`ds-hdr-master`** (tip 2026-07-10, base upstream `efeb5976` 2026-07-06) adds +3,435/−9 lines:
    - `src/wayland/color/management/mod.rs` (1,732 lines): v3, Windows scRGB and BT.2100, extended target volume, custom primaries, "always send luminances and target_luminance in info", and "treat extended-linear transfer as HDR".
    - `src/wayland/color/representation/*`.
    - `src/backend/drm/color.rs` with atomic connector colour state.
    - 52 lines in `gles/mod.rs`.
    - Merges `losnoco/smithay` "color-management-features".
- `alexparlett/smithay` PRs #6 (DRM connector colour state) and #7 ("wp-color-management-v1, version 3 (ds-hdr-master)", 2026-11-02) are merged in that fork — [alexparlett/smithay #7](https://github.com/alexparlett/smithay/pull/7); [#6](https://github.com/alexparlett/smithay/pull/6)
- `franzjeger/cosmic-hdr-reference` (MIT, © 2026) ([cosmic-hdr-reference](https://github.com/franzjeger/cosmic-hdr-reference)):
  - Adds 3,808 lines to Smithay (based on `e3d461a`) and 2,692 to cosmic-comp.
  - Covers per-output HDR10 KMS, SDR→BT.2020/PQ with adjustable reference white, colour-management image descriptions, and scRGB client passthrough "using the scRGB reference-white convention used by Wine/Proton".
  - Validated 2026-08-31 on an RTX 5090 with open modules 610.57.04, and 2026-09-02 on a Radeon 680M.
  - Community report: RTX 4090 on 595.104.02 running Cyberpunk 2077 in HDR10/PQ under GE-Proton.
  - Says it is "not an upstream-ready pull request". A community rebase exists at `Liquidmasl/smithay` `hdr`.
- Cha pins Smithay `118e34ff` (2026-09-28, "drm: allow disabling VRR on legacy surfaces"), which is later than `ds-hdr-master`'s base (2026-07-06) — [Cha cha-streamer Cargo.toml](../../../crates/cha-streamer/Cargo.toml); [Smithay 118e34ff](https://github.com/Smithay/smithay/commit/118e34ff)

### Inferences
- **The reusable MIT code for Cha** is the protocol and state plumbing in `ds-hdr-master` (`src/wayland/color/management/mod.rs`, `src/wayland/color/representation/*`). Cha needs neither the DRM/KMS parts (`drm/color.rs`, atomic connector state) nor most of cosmic-hdr-reference's output code. The rebase onto `118e34ff` is about three months of upstream drift. Alternatively, vendor the module into `cha-streamer` the way gst-wayland-display was told it could ("move the code into gst-wayland-comp"), avoiding a Smithay fork pin.
- **To satisfy every client in Cha's catalogue, advertise:**
  - Features: parametric, set_primaries, set_luminances, set_mastering_display_primaries, extended_target_volume and windows_scrgb.
  - TFs: gamma22, st2084_pq, ext_linear, and `srgb` for v1 binders.
  - Primaries: srgb and bt2020.
  - `wp_color_management_output_v1` and surface feedback, with a preferred description whose target max luminance exceeds reference.

  Each client checks a different part of this set:

  | Client | What it needs from the compositor |
  |---|---|
  | gamescope | All six features, PQ, sRGB + BT.2020 primaries (plus `srgb` TF for ≤3.16.24) |
  | Chromium | parametric, and the output description |
  | Firefox | parametric + BT.2020 + PQ |
  | nested KWin | parametric + set_primaries + set_luminances + gamma22 |
  | Wine/GE-Proton | max > reference |
- **Upstream Smithay renderers will not do colour conversion for Cha**, even with the protocol. Smithay's GLES renderer has no colour pipeline beyond whatever custom texture shaders Cha adds (section 5).

### Gaps
- Weston's and Hyprland's exact release versions with wp_color_management_v1 were not verified from source.
- Whether `dividebysandwich/smithay` is meant to be upstreamed, and its test coverage, are unknown. The thread question went unanswered.
- I did not check if the Rust `wayland-protocols` crate has `frog_color_management_v1` (it is not a wayland-protocols protocol). Cha would likely need to generate it from gamescope's XML.

## 4. HDR clients Cha runs: Chromium and Firefox on Wayland, nested kwin_wayland, NVIDIA's Vulkan driver on Wayland, and Mesa

### Takeaway
- **Chromium (`main`):** binds `wp_color_manager_v1` v1 by default (`WaylandWpColorManagerV1`, enabled by default). It reads HDR capability from `wp_color_management_output_v1`'s image description (HDR when target max luminance / reference > 1) and draws HDR into `RGBA_F16` if the compositor's dmabuf formats include it, else `RGBA_1010102`.
- **Firefox (162 nightly):** HDR is still off by default on Linux, behind `gfx.color_management.hdr`. When enabled it needs parametric + BT.2020 + PQ and the WebRender native compositor (dmabuf + viewporter). It covers video overlays only.
- **Nested KWin (Plasma ≥6.4):** forwards HDR to a host compositor. It adopts the host's preferred image description, encoding gamma 2.2 over an extended luminance range, so it needs parametric + set_primaries + set_luminances + gamma22.
- **Mesa's Vulkan WSI:** has spoken the protocol since 25.1.
- **NVIDIA's Vulkan WSI:** gained `VK_EXT_hdr_metadata` (Wayland-only) in the 580.94.11 Vulkan beta. HDR colourspaces reportedly became default in 595.58.03. HDR-metadata crash fixes followed in 595.104.02 and 615.78.08.

Inside gamescope, games do not use the driver's Wayland WSI to reach Cha. gamescope forwards dmabufs itself, so NVIDIA's Wayland HDR support mainly matters for native Vulkan clients presenting directly to Cha's compositor.

### Cited Findings
**Chromium (Ozone Wayland)**
- `BASE_FEATURE(kWaylandWpColorManagerV1, base::FEATURE_ENABLED_BY_DEFAULT)` binds with `kMinVersion = 1` as both min and max. Image descriptions need `PARAMETRIC` ("Server does not support parametric color descriptions") — [Chromium wayland_wp_color_manager.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/ozone/platform/wayland/host/wayland_wp_color_manager.cc)
- The output image description computes `sdr_max_luminance_nits_` from the reference luminance and `hdr_max_luminance_relative_ = target_max_lum / reference`, with `is_hdr = relative > 1` — [Chromium wayland_wp_image_description.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/ozone/platform/wayland/host/wayland_wp_image_description.cc)
- `WaylandWpColorManagementOutput::GetDisplayColorSpaces()` returns null unless `wp_color_manager()->hdr_enabled()` — [Chromium wayland_wp_color_management_output.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/ozone/platform/wayland/host/wayland_wp_color_management_output.cc)
- The HDR buffer format prefers `kRGBA_F16`, then `kRGBA_1010102`, from the dmabuf format table, falling back to the 8-bit format — [Chromium wayland_screen.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/ozone/platform/wayland/host/wayland_screen.cc)
- History and known problems ([Phoronix](https://www.phoronix.com/news/Chrome-Lands-Wayland-CM); [Chromium commit 07c9a59](https://github.com/chromium/chromium/commit/07c9a59c2a5256ce49c22445a6c5108182c7da11); [Hyprland discussion #11910](https://github.com/hyprwm/Hyprland/discussions/11910); [Chromium issue 477318785](https://issues.chromium.org/issues/477318785); [Januschka](https://www.januschka.com/chromium-wayland-crash.html)):
  - Landed in 2025 and tested on Plasma 6.4.
  - Washed-out HDR on Hyprland in Chromium 141.
  - Blurry text on KDE until disabled.
  - A crash fix merged 2025-10-06.

**Firefox**
- `gfx.color_management.hdr` "Enable reporting video-dynamic-range: high ... and allows video overlays to use HDR transfer functions" defaults to `true` on macOS/Windows and `false` elsewhere. `gfx.color_management.hdr.force_enabled` defaults to false. No `gfx.wayland.hdr` pref exists in `StaticPrefList.yaml` on main (version 162.0a1), so the older name appears to have been replaced — [Firefox StaticPrefList.yaml](https://github.com/mozilla-firefox/firefox/blob/main/modules/libpref/init/StaticPrefList.yaml)
- `nsWaylandDisplay::IsHDREnabled()` = parametric && BT2020 primaries && ST2084_PQ ("BT2020/PQ is the minimum to enable HDR"), binding `wp_color_manager_v1` v1. The WebRender native compositor ("HDR requires compositor to work") needs Wayland, dmabuf and `wp_viewporter` — [nsWaylandDisplay.cpp](https://github.com/mozilla-firefox/firefox/blob/main/widget/gtk/nsWaylandDisplay.cpp); [gfxPlatformGtk.cpp](https://github.com/mozilla-firefox/firefox/blob/main/gfx/thebes/gfxPlatformGtk.cpp)
- Mozilla's Martin Stransky (Jan 2026) said HDR video on Wayland is disabled by default, uses color-management-v1 with BT.2020/PQ and 10-bit, and needs VP9 profile 2 hardware decode. NVIDIA users reported yellowed or washed-out output — [Stransky blog (2026-01-23)](https://mastransky.wordpress.com/2026/01/23/firefox-linux-in-2025/); [NVIDIA forum](https://forums.developer.nvidia.com/t/yellowed-washed-out-hdr-in-both-wine-wayland-gamescope-on-nvidia-575-51-02/331232); [Bugzilla 1642854](https://bugzilla.mozilla.org/show_bug.cgi?id=1642854)

**Nested KWin**
- KWin's Wayland (nested) backend ([KWin wayland_output.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/backends/wayland/wayland_output.cpp); [wayland_layer.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/backends/wayland/wayland_layer.cpp); [wayland_egl_backend.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/backends/wayland/wayland_egl_backend.cpp)):
  - Binds the host `wp_color_manager_v1` (v1).
  - If the host has parametric + set_primaries + set_luminances + gamma22, it subscribes to the surface's preferred description.
  - It sets its output colour to the host's container colorimetry and `TransferFunction(gamma22, minLum, maxLum)`, with the host's reference, min, max-average and max-HDR luminance ("we don't actually know this, but we have to assume *something*").
  - It tags its layer surfaces with `wp_color_management_surface_v1_set_image_description`.
  - The render format is chosen from the host's linux-dmabuf table via `filterAndSortFormats(..., colorPowerTradeoff)`.
- Commits ([KWin 975cc8ab](https://invent.kde.org/plasma/kwin/-/commit/975cc8ab); [1c320a86](https://invent.kde.org/plasma/kwin/-/commit/1c320a86)):
  - `975cc8ab` "backends/wayland: add support for HDR" (2025-01-31).
  - `cc49de8f` upstream protocol (2025-02-13).
  - `1c320a86` "automatically adjust to the preferred image description" (2025-05-05).
  - All are first in v6.3.90, so they shipped in Plasma 6.4.
- The virtual backend (`virtual_output.cpp`) has no colour code — [KWin virtual_output.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/backends/virtual/virtual_output.cpp)

**NVIDIA Vulkan on Wayland**
- **580.94.11 Vulkan beta:** added `VK_EXT_hdr_metadata`, "only under Wayland and not X.Org/X11" (Phoronix headline and snippet; the page returned 403 to the fetcher). An NVIDIA forum thread is titled "Vulkan Beta 580.94.11 has multiple issues with HDR support" (2025-11-24) — [Phoronix](https://www.phoronix.com/news/NVIDIA-580.94.11-Linux-Driver); [NVIDIA forum](https://forums.developer.nvidia.com/t/vulkan-extensions-needed-for-hdr-is-missing/334268/12)
- **595.58.03 (2026-03-24):** ArchWiki (via search snippet) says "mesa 25.1 and later as well as nvidia-open 595.58.03 and later include these extensions by default". Before that, `vk-hdr-layer-kwin6-git` + `ENABLE_HDR_WSI=1` were needed. GamingOnLinux's changelog for 595.58.03 lists no HDR item, so this is a reported behaviour change and not confirmed from NVIDIA's notes — [ArchWiki HDR](https://wiki.archlinux.org/title/HDR); [GamingOnLinux (2026-03-24)](https://www.gamingonlinux.com/2026/03/nvidia-driver-595-58-03-released-as-the-big-new-recommended-stable-driver-for-linux/); [Ubuntu Handbook](https://ubuntuhandbook.org/index.php/2026/03/nvidia-595-58-03-released-with-better-wayland-linux-gaming-support/)
- **595.104.02 and 615.78.08:** fixed a crash where unvalidated `VkHdrMetadataEXT` values were forwarded to color-management-v1 (secondary news sites) — [Linux Today (595.104.02)](https://www.linuxtoday.com/blog/nvidia-595-104-02-linux-graphics-driver-is-out-now-with-better-wayland-support/); [linuxcompatible (615.78.08)](https://www.linuxcompatible.org/story/nvidia-6157808-linux-driver-adds-proton-latency-fixes-wayland-hdr-crash-patches/)
- **No vendor statement:** users asked NVIDIA in Aug 2025 for wp_color_management support in 580, and no staff reply is visible — [NVIDIA forum](https://forums.developer.nvidia.com/t/wayland-color-management-protocol-not-yet-supported-by-nvidia-driver/338444)

**Mesa**
- Mesa MR 32038 "vulkan/wsi: implement the Wayland color management protocol" merged 2025-02-25 (Mesa 25.1). The same day brought `4b663d56` "implement support for VK_EXT_hdr_metadata on Wayland" — [Mesa MR 32038](https://gitlab.freedesktop.org/mesa/mesa/-/merge_requests/32038); [wsi_common_wayland.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/vulkan/wsi/wsi_common_wayland.c)
- Later Mesa WSI changes ([wsi_common_wayland.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/vulkan/wsi/wsi_common_wayland.c)):
  - HDR-metadata validation to avoid protocol errors (2025-04-08).
  - Removal of `VK_COLOR_SPACE_EXTENDED_SRGB_NONLINEAR_EXT` (2025-10-23).
  - The colour-management surface moved to the swapchain (2026-06-20).
  - Supported colourspaces include `EXTENDED_SRGB_LINEAR`, `HDR10_ST2084` (PQ, with metadata) and `HDR10_HLG`.

### Inferences
- **Chrome on Cha:** Chrome turns HDR on with no flag once Cha implements `wp_color_management_output_v1` with HDR headroom and lists `ABGR16161616F` (or `ABGR2101010`) in its dmabuf formats. Cha would then receive FP16 extended-range (scRGB-like) or 10-bit buffers tagged per surface. The `hdr_enabled()` gate's source was not traced (see Gaps).
- **Firefox on Cha:** gives HDR only if the image sets `gfx.color_management.hdr=true`, and only for video overlays, so desktop/page content stays SDR.
- **Nested Plasma on Cha:** if Cha advertises parametric + set_primaries + set_luminances + gamma22 and a preferred description with HDR luminances, nested KWin hands Cha gamma-2.2-encoded buffers whose 1.0 means the advertised max luminance, not PQ. Cha must honour `set_luminances` on gamma22 descriptions, not assume gamma22 = SDR. In return Cha gets KWin's own SDR-in-HDR handling, and HDR passthrough for apps inside Plasma, without writing it.
- **NVIDIA driver timing:** the node's 580 branch predates the reported default-on HDR Vulkan WSI (595.58.03+). It matters only for native Vulkan Wayland clients on Cha's socket: GE-Proton Wine-Wayland, mpv, or a nested Plasma's Vulkan apps. R615 also carries the metadata-validation crash fix.

### Gaps
- I could not trace where Chromium's `WaylandWpColorManager::OnHdrEnabledChanged` is called from, so it is unknown whether HDR also needs a GPU-side capability report.
- NVIDIA's own release notes for the first stable driver exposing `VK_COLOR_SPACE_HDR10_ST2084_EXT` on Wayland were not found; the 595.58.03 claim is ArchWiki-only.
- Whether nested KWin picks a 10-bit or FP16 dmabuf format from the host when the preferred description is HDR (the `filterAndSortFormats` policy) was not read.
- Firefox's current release-channel default and the exact rename date from `gfx.wayland.hdr` were not confirmed.

## 5. Compositing HDR headlessly: 10-bit/FP16 GBM/EGL buffers, GLES FP16 targets, linear-light blending, SDR in HDR (BT.2408, KWin/Mutter, sRGB vs gamma 2.2), and 10-bit/FP16 zero-copy into CUDA/NVENC and VA-API

### Takeaway
Smithay's `GlesRenderer` already maps `Abgr2101010` to `RGB10_A2` and `Abgr16161616f` to `RGBA16F`, and its GLES3 memory-import list includes both. The render-side formats are therefore there, but Smithay has no colour pipeline. Cha must write per-surface decode shaders (sRGB/gamma22, PQ, scRGB-linear → linear BT.2020), blend in linear FP16, and encode PQ at the end.

For SDR in HDR, KWin treats sRGB-tagged and untagged content as gamma 2.2 with a user-set reference white. The protocol now formally deprecates the ambiguous `srgb` TF in favour of gamma22. BT.2408's 203 cd/m² is the protocol's reference for PQ and for scRGB (2.5375).

The zero-copy encoder hand-off is the hard part. CUDA 13's `CUeglColorFormat` has no packed 2:10:10:10 RGB or FP16 RGBA entry, so registering a 10-bit EGL image with CUDA is unproven. NVENC (SDK 13 in Cha's bindings) does take `ABGR10`/`ARGB10` and `YUV420_10BIT` (P010) input and per-picture mastering/CLL metadata for HEVC and AV1. Mesa's VA frontend imports `AR30`/`AB30`/`XR30`/`XB30` dmabufs.

### Cited Findings
**Render formats (Smithay, gamescope, Cha)**
- Smithay GLES ([Smithay gles/format.rs](https://github.com/Smithay/smithay/blob/master/src/backend/renderer/gles/format.rs); [gles/mod.rs](https://github.com/Smithay/smithay/blob/master/src/backend/renderer/gles/mod.rs); [allocator/format.rs](https://github.com/Smithay/smithay/blob/master/src/backend/allocator/format.rs)):
  - `format.rs` maps `Fourcc::Abgr2101010 => (RGB10_A2, RGBA, UNSIGNED_INT_2_10_10_10_REV)` and `Fourcc::Abgr16161616f => (RGBA16F, RGBA, HALF_FLOAT)`.
  - `SUPPORTED_MEM_FORMATS_3` (GLES3) adds `Abgr2101010`, `Xbgr2101010`, `Abgr16161616f` and `Xbgr16161616f` to the 8-bit set.
  - The allocator defines `Argb/Xrgb/Abgr/Xbgr2101010` and the `16161616f` variants.
- Cha's output pool currently filters the renderer's dmabuf render formats to `[Xrgb8888, Argb8888]` only — [Cha output.rs](../../../crates/cha-streamer/src/compositor/output.rs)
- gamescope's Vulkan code notes NVIDIA does not support `XRGB2101010` linear storage images, and that "imageStore lands in XBGR order there", so it uses `XBGR2101010` — [gamescope rendervulkan.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/rendervulkan.cpp)
- cosmic-hdr-reference drives "10-bit scanout state" on NVIDIA 610.57.04. Its SDR desktop is rendered "into an 8-bit intermediate, then ... SDR-to-BT.2020/PQ conversion into a 10-bit scanout buffer". It says "A production design should composite all surfaces in a linear, high-precision intermediate (for example F16) ... and encode only at the output boundary". HDR clients that do not cover the output (for example windowed scRGB Blender) "are currently composited as SDR" — [cosmic-hdr-reference](https://github.com/franzjeger/cosmic-hdr-reference)
- Drakulix's requirements for a Smithay streaming compositor ([Wolf #222](https://github.com/games-on-whales/wolf/issues/222)):
  - The framebuffer must be "at least 10-bit (ARGB2101010, or ARGB16161616f, which costs more)".
  - A shader is needed to convert sRGB windows to BT.2020.
  - Color-representation is needed for alpha modes.
  - The suggested minimal set is parametric, gamma22 + st2084_pq, srgb + bt2020 primaries.
  - "if the encoder only accepts 8-bit input, the protocol should not be advertised".

**SDR in HDR: KWin, Mutter, protocol**
- **KWin** ([KWin colorspace.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/core/colorspace.cpp); [KWin colormanagement_v1.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/colormanagement_v1.cpp)):
  - `ColorDescription::sRGB` = BT.709 + `TransferFunction::gamma22`.
  - `defaultReferenceLuminanceFor`: PQ/HLG = 203, linear/sRGB/gamma22 = 80, BT1886 = 100.
  - Image description info sends `target_luminance` max as `maxHdrLuminance().value_or(800)`.
  - Client PQ descriptions without a max default to 1,000 nits.
- KWin blog and MRs ([Xaver Hugl, part 4 (2024-11-01)](https://planet.kde.org/xavers-blog-2024-11-01-hdr-and-color-management-in-kwin-part-4-nonlinear-blending); [KWin MR 4648](https://invent.kde.org/plasma/kwin/-/merge_requests/4648); [KWin MR 5999](https://invent.kde.org/plasma/kwin/-/merge_requests/5999); [KDE bug 499103](https://bugs.kde.org/show_bug.cgi?id=499103)):
  - Since Plasma 6.2 KWin blends HDR in a gamma 2.2 space, where 1.0 is max luminance, instead of linear (2024).
  - MR 4648 switched untagged content to gamma 2.2 in both directions.
  - MR 5999 maps SDR to a user-configured reference luminance.
  - scRGB uses a 203-nit reference.
- Users dispute gamma 2.2 vs piecewise sRGB ([KDE bug 515402](https://bugs.kde.org/show_bug.cgi?id=515402); [ledoge/win11hdr-srgb-to-gamma2.2-icm](https://github.com/ledoge/win11hdr-srgb-to-gamma2.2-icm)):
  - KDE bug 515402 argues sRGB is piecewise.
  - Windows' SDR-in-HDR uses piecewise sRGB, and a community ICC profile converts it to gamma 2.2.
- **Mutter** ([Mutter clutter-color-state-params.c](https://gitlab.gnome.org/GNOME/mutter/-/blob/main/clutter/clutter/clutter-color-state-params.c)):
  - With a PQ output, the blending colour state is sRGB primaries + `GAMMA22`. Otherwise blending can be `LINEAR`.
  - CICP sRGB maps to `SRGB_PIECEWISE`.
  - Required encoding precision: PQ → `UINT10`, linear → `FP16`, sRGB/BT.1886/gamma22 → `UINT8`. A mastering max above the max luminance → `FP16`.
- **Protocol:** `srgb` TF is deprecated as ambiguous, with gamma22 recommended for video/CG. PQ and scRGB references are 203 cd/m² (BT.2408-7) — [color-management-v1.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/color-management/color-management-v1.xml)
- **gamescope:** its own SDR-in-HDR default is 400 nits (`--hdr-sdr-content-nits`) — [gamescope main.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp)

**Encoder input: CUDA/NVENC and VA-API**
- CUDA 13.0's `CUeglColorFormat` ([CUDA 13.0 driver API types](https://docs.nvidia.com/cuda/archive/13.0.3/cuda-driver-api/group__CUDA__TYPES.html)):
  - Its RGB entries are only `RGB`, `BGR`, `ARGB`, `RGBA`, `ABGR` and `BGRA`, all 8-bit.
  - Its 10/12-bit entries are YUV (for example `Y10V10U10_420_SEMIPLANAR`, `_709`, `_2020`, `Y10_ER`, `Y12V12U12_*`) and Bayer.
  - It has no packed 10-bit RGB and no FP16 entry.
  - By contrast, `CUarray_format` does have `CU_AD_FORMAT_UNORM_INT_101010_2` ("4 channel unorm R10G10B10A2 RGB format") and `CU_AD_FORMAT_HALF`.
- Cha's README already found that "CUDA's EGL interop won't import an R8 image". It plans a Vulkan compute pass that writes NV12 into a CUDA-imported buffer that NVENC reads as `CUDADEVICEPTR` — [Cha README](../../../crates/cha-streamer/README.md)
- Cha's NVENC bindings are SDK 13.0 (`NVENCAPI_MAJOR_VERSION = 13`). They include `NV_ENC_BUFFER_FORMAT_ARGB10`, `ABGR10`, `YUV420_10BIT` and `YUV444_10BIT`, `NV_ENC_CAPS_SUPPORT_10BIT_ENCODE`, `inputBitDepth`/`outputBitDepth`, BT.2020 VUI enums, and `pMaxCll: *mut CONTENT_LIGHT_LEVEL` / `pMasteringDisplay: *mut MASTERING_DISPLAY_INFO` in both `NV_ENC_PIC_PARAMS_HEVC` and `NV_ENC_PIC_PARAMS_AV1`. `nvenc.rs` currently sets BT.709 primaries and transfer — [cha-nvenc sys.rs](../../../crates/cha-nvenc/src/sys.rs); [cha-nvenc nvenc.rs](../../../crates/cha-nvenc/src/nvenc.rs)
- NVIDIA's docs list ABGR10 as an input format and 10-bit H.264/HEVC/AV1 encode. A developer-forum user got correct HDR from ABGR10 + HEVC Main10 only after setting VUI to BT.2020 / SMPTE 2084 / BT.2020-NCL (anecdotal) — [Video Codec SDK 13.0 read-me](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/read-me/index.html); [NVENC Application Note 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-application-note/index.html); [NVIDIA forum "Nvenc & HDR10"](https://forums.developer.nvidia.com/t/nvenc-hdr10/351194)
- Mesa VA frontend ([Mesa va_private.h](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/gallium/frontends/va/va_private.h); [Mesa va surface.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/gallium/frontends/va/surface.c)):
  - `VaFourccToPipeFormat` maps `AR30`→`B10G10R10A2_UNORM`, `AB30`→`R10G10B10A2_UNORM`, `XR30`→`B10G10R10X2_UNORM` and `XB30`→`R10G10B10X2_UNORM`.
  - `surface.c` maps `VA_RT_FORMAT_RGB32_10` to `X2R10G10B10` and `YUV420_10` to `P010`.
- Cha's VA-API path offers only HEVC Main 8-bit and AV1 Profile 0 8-bit today, and its video processor converts to NV12 BT.709 — [Cha README](../../../crates/cha-streamer/README.md)
- Sunshine's CUDA path accepts `P010LE` frames as well as NV12 and 4:4:4 variants — [Sunshine cuda.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/cuda.cpp)

### Inferences
- **A design that fits Cha's GLES/Smithay stack:**
  1. Composite into an `Abgr16161616f` (RGBA16F) render target in linear light, with BT.2020 or scRGB-style extended BT.709 as the working space and a fixed scale such as 1.0 = 80 or 203 nits.
  2. Decode each surface from its image description with a custom texture shader: gamma22 for untagged/sRGB per KWin and the protocol's advice, PQ EOTF, scRGB linear, and gamma22-with-luminances for nested KWin.
  3. Map SDR white to a configurable reference, defaulting to 203 per BT.2408 or following the viewer's display.
  4. Encode once at the end: PQ BT.2020 into `Abgr2101010`, or straight to P010 in a compute pass.
- **Precision:** an 8-bit intermediate (cosmic-hdr-reference's shortcut) is not enough for HDR passthrough of partially covering windows. Blending in linear FP16 is what both cosmic-hdr-reference and Mutter (linear blending → FP16) point to. KWin chose gamma-2.2 blending instead for visual parity with SDR alpha blending. This is a policy choice; linear is "correct", while gamma-space blending matches how SDR apps expect translucency to look.
- **NVENC zero-copy:** the safest route is the Vulkan compute pass the README already plans (EGL/dmabuf → Vulkan import → compute PQ + BT.2020 YCbCr → P010 in CUDA external memory → `NV_ENC_BUFFER_FORMAT_YUV420_10BIT` from `CUDADEVICEPTR`). Two alternatives:
  - Register an `ABGR2101010` EGL image with CUDA directly. It might work through a `CUarray` with `UNORM_INT_101010_2` despite the missing `CUeglColorFormat`, but this is unverified and needs a test on the 4090.
  - Have NVENC read `ABGR10` from a pitch-linear CUDA buffer. NVENC's internal RGB→YUV CUDA kernel has the same contention issue the README notes for 8-bit.
- **Metadata:** per-picture `pMasteringDisplay`/`pMaxCll` let Cha forward gamescope's or the game's HDR10 metadata into HEVC SEI or AV1 metadata OBUs without new bindings.
- **VA-API zero-copy:** a 10-bit RGB dmabuf (`AB30`/`XB30`) is likely importable on radeonsi, given the fourcc mapping. Whether the VPP will convert it to P010 with BT.2020/PQ semantics, and whether iHD accepts it, must be probed the way `vaapi::accepts_import` already probes 8-bit formats. HEVC Main10 / AV1 10-bit encoders would also have to be written; today only 8-bit is built.
- **Headless:** nothing here needs a monitor. EGL on the render node, GBM allocation and the encoders work without one. Only KMS-based capture (Sunshine) and DRM-backend gamescope need a real or emulated HDR EDID.

### Gaps
- I did not verify that NVIDIA's GBM allocates renderable `ABGR16161616F` or `ABGR2101010` buffers with block-linear modifiers (driver 580/615), nor what `Bind::<Dmabuf>::supported_formats` returns on the 4090. The cosmic-hdr-reference 10-bit scanout is indirect evidence for 10-bit only.
- Whether `cuGraphicsEGLRegisterImage` accepts a 10-bit or FP16 EGL image, and what `CUeglFrame` it returns, is unknown; this needs a test.
- Intel iHD (media-driver) import and VPP of `A2R10G10B10`/`A2B10G10R10` dmabufs to P010 was not checked.
- No measurement of FP16 vs 8-bit compositing cost in Smithay GLES at 1440p/4K, 120–240 Hz.
- The BT.2408 document itself was not fetched. The 203-nit figure is taken from the protocol text, which cites BT.2408-7.

## 6. Prior art in streaming compositors and capture paths (Wolf / gst-wayland-display, Selkies pixelflux, Nestri, Sunshine on Linux, gamescope PipeWire, Steam Remote Play's Linux host)

### Takeaway
As of November 2026, no open-source Smithay-based streaming compositor composites and encodes HDR end to end:
- **Wolf / gst-wayland-display:** has no shipped HDR. Its "Vulkan encode + HDR support" PR mostly adds Vulkan/VA zero-copy NV12 and lists 10-bit formats, and it says the compositor side waits on Smithay #1143.
- **Selkies/pixelflux:** a Smithay-based headless compositor with 10-bit encode, but nothing on PQ or colour management.
- **Nestri:** nothing found.
- **Sunshine on Linux:** does HDR only with KMS capture, reading the real connector's `HDR_OUTPUT_METADATA`, which needs an HDR-mode display or EDID. Its wlroots, portal and X11 capture is SDR.
- **gamescope's PipeWire stream:** BGRx/NV12 with BT.601/709 only.
- **Steam Remote Play:** the 22 Oct 2026 Steam beta claims HDR capture from Linux desktops through PipeWire (PipeWire ≥1.6). The gamescope path was still not working as of 10 Oct 2026.

### Cited Findings
**Wolf and gst-wayland-display**
- The HDR request in Wolf issue #222 (opened 2025-07-05) ([Wolf #222](https://github.com/games-on-whales/wolf/issues/222)):
  - The maintainer (ABeltramo) said in Nov 2025 that Smithay lacked HDR.
  - In Jul 2026 a contributor said HDR "had been in testing in gst-wayland for a few weeks".
  - ABeltramo split the work into (1) HDR encode/stream, in gst-wayland-display PR #37, and (2) an HDR compositor needing Smithay #1143, with no ETA.
  - The last comment, 2026-08-31, was unanswered.
- gst-wayland-display PR #37 "Vulkan encode + HDR support" (JBailes, opened 2026-06-20, merged into `stable`) ([gst-wayland-display PR #37](https://github.com/games-on-whales/gst-wayland-display/pull/37)):
  - Adds a Vulkan compute converter from the GLES output to NV12 dmabufs (BT.601) and a `vulkanh264enc` zero-copy path.
  - Its visible description has no PQ, colour-management or FP16 design. It only shows P010 in encoder caps and AR30/XR30/AB30 in advertised dmabuf formats.
  - Tested on a 7900 XTX, UHD 770, RTX 5080 and RX 9070.

**Selkies and Nestri**
- Selkies/pixelflux ([linuxserver Selkies docs](https://docs.linuxserver.io/selkies/components/pixelflux/); [Selkies 2.1.0 release](https://newreleases.io/project/github/selkies-project/selkies/release/2.1.0)):
  - Pixelflux's Wayland mode is "a full headless Wayland compositor built on Smithay, running in process", rendering into GPU buffers the encoder reads in place.
  - Selkies 2.1.0 added `--video-10bit`, offered only when the host encodes it and the browser decodes a real 10-bit key frame.
  - No HDR, PQ or colour-management handling is documented.
- Nestri: no documentation of HDR in Nestri's node or server was found (searches only surfaced the repo and mirrors) — [nestrilabs/nestri](https://github.com/nestriness/nestri)

**Sunshine on Linux**
- `kmsgrab.cpp` ([Sunshine kmsgrab.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/kmsgrab.cpp)):
  - Reads the connector property `HDR_OUTPUT_METADATA`.
  - `is_hdr()` accepts only "Traditional Gamma SDR or SMPTE 2084 PQ HDR EOTFs" and warns on HLG and traditional-gamma HDR.
  - `get_hdr_metadata()` returns the blob's mastering and CLL values.
  - Capture is reinitialised "after HDR metadata change".
- `wlgrab.cpp`, `wayland.cpp`, `portalgrab.cpp` and `vaapi.cpp` contain no HDR handling — [Sunshine linux platform](https://github.com/LizardByte/Sunshine/tree/master/src/platform/linux)
- An NVIDIA-forum feature request for Sunshine/Moonlight HDR on Wayland/KMS (2026-04-24) had no replies — [NVIDIA forum](https://forums.developer.nvidia.com/t/wayland-color-management-protocol-not-yet-supported-by-nvidia-driver/338444)

**gamescope PipeWire and Steam Remote Play**
- gamescope `pipewire.cpp` ([gamescope pipewire.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/pipewire.cpp)):
  - Offers only `SPA_VIDEO_FORMAT_BGRx` and `SPA_VIDEO_FORMAT_NV12`.
  - Colorspaces are BT.601/BT.709, limited or full.
  - The last format-related commits date from 2022 (NV12 path, colorspaces).
- steam-for-linux issue #13086 "Remote Play: Add support for HDR capture" (opened 2026-04-12 by flibitijibibo, labelled by Valve, still open) ([steam-for-linux #13086](https://github.com/ValveSoftware/steam-for-linux/issues/13086)):
  - Needs PipeWire ≥1.6 (BT.2020/ST2084 formats), compositor support (KWin MR 8293 in progress, gamescope issue #2126 for HDR PipeWire streams), and Steam's PipeWire backend.
  - 2026-07-10: a gamescope-side patch.
  - 2026-10-10: HDR still not working from a Linux host to a Steam Machine.
- The Steam Client Beta of 22 Oct 2026 "revamps Remote Play HDR support" ([GamingOnLinux (Oct 2026)](https://www.gamingonlinux.com/2026/10/steam-client-beta-october-22nd-revamps-remote-play-hdr-support/); [igor'sLAB (2026-10-25)](https://www.igorslab.de/en/steam-deck-oled-valve-restores-hdr-streaming-expands-remote-play-linux-macos-android/)):
  - Adds an Automatic/On/Off HDR option.
  - Restores HDR on Steam Deck OLED and adds Android 15+.
  - States that "Valve now supports HDR capture on Linux desktops with PipeWire". This is beta only; the gamescope path is not mentioned.
- Secondary sources conflict on whether gamescope-hosted HDR Remote Play works. One personal project's log saw the receiver decline an HDR swapchain offered under gamescope — [jedwards1230/tv-shell #506](https://github.com/jedwards1230/tv-shell/issues/506)

### Inferences
- **Cha would be among the first** headless, containerised, Smithay-based streamers with real HDR composition. There is no drop-in code to copy for the compositor-to-encoder half. The pieces to borrow are:
  - The protocol module from `ds-hdr-master`.
  - Shader and policy ideas from KWin, Mutter and cosmic-hdr-reference.
  - gst-wayland-display's Vulkan-compute zero-copy converter pattern, which would need a P010/PQ variant.
- **Sunshine's method doesn't transfer.** It depends on a KMS connector in HDR mode, which a headless container node does not have. A virtual connector or EDID-override approach would need host privileges outside Cha's container model.
- **Steam Remote Play HDR from Cha's gamescope is not available.** gamescope's PipeWire stream is SDR-only on master, so this is not a fallback or alternative to Cha's own encoder.

### Gaps
- PR #37's later commits (the 84k characters not read) may include HDR conversion work; only the first part of the page was examined.
- The content and merge state of KWin MR 8293 (HDR screencast) and gamescope issue #2126 were not checked directly.
- Nestri's current capture path (gamescope-based or own compositor) and any HDR plans were not found.
- The official Steam beta changelog text was not read directly. The PipeWire claim rests on GamingOnLinux and igor'sLAB summaries.
