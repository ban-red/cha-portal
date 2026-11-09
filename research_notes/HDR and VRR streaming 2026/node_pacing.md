# Node pacing under VRR: composing and encoding at the game's cadence (cha-streamer, Nov 2026)

Scope: the node only (Smithay compositor, nested gamescope, the game's WSI, the encoders). Most findings come from reading source at the commits and dates given, which is primary evidence for what the code does on that date. Tags: **(vendor)** a vendor's own claim; **(single-dev)** one developer's or project's own measurement; **(unverified)** not confirmed against a primary source. No independent third-party measurements of node-side VRR pacing were found.

## 1. Wayland timing protocols, Smithay support, and how desktop compositors schedule VRR frames

### Takeaway
Everything Cha needs is in Smithay already: presentation-time v2 (`Refresh::Variable`/`Unknown`), `wp_fifo_v1` and `wp_commit_timing_v1`. Cha uses none of it today. It sends `Refresh::Fixed(1/240 s)` + `Kind::Vsync` on every tick and exposes no fifo, commit-timing or tearing-control. Presentation-time v3, which adds explicit `fixed_rate`/`variable_rate` flags, shipped in wayland-protocols 1.50 (2026-10-29), but Smithay master doesn't implement it yet. KWin and Mutter show the scheduling pattern Cha would copy: under VRR, present as soon as the driving surface's content is ready, but no sooner than one minimum refresh interval after the previous frame, and don't let other surfaces drive the refresh.

### Cited Findings
**Protocol history (wayland-protocols git, fetched 2026-11-09)**
- `wp_tearing_control_v1` (staging) was added 2022-11-14 and first released in **1.30** (2022-11-21). — [wayland-protocols commit 6394f0b](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/commit/6394f0b4f3be151076f10a845a2fb131eeb56706); [tag 1.30](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/tags/1.30)
- `wp_fifo_v1` and `wp_commit_timing_v1` (staging) were committed 2024-10-11 and released in **1.38** (2024-10-12). — [fifo-v1 commit](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/commit/1ccc5748acc2ae16ce58914aa5d08d1bd7cfac06); [Phoronix on 1.38](https://www.phoronix.com/news/Wayland-Protocols-1.38)
- **presentation-time v2** ("Specify refresh bounds for VRR", Daniel Stone/Derek Foreman, Collabora) was also in 1.38. For v2+, if the output has no constant refresh, "the refresh argument must be either an appropriate rate picked by the compositor (e.g. fastest rate), or 0 if no such rate exists". For v1 it "must be zero". — [commit 8cdb391](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/commit/8cdb39103247fdde5764fc35b1b5cf60698db3e5); [commit-timing-v1 commit](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/commit/37a1560cf6981a11d44dd200d9409d09b4f0074e)
- **presentation-time v3** adds two `presented` flags. `fixed_rate` (0x10) means the presentation "was aligned to a multiple of this event's 'refresh'". `variable_rate` (0x20) means the update "was displayed as soon as possible after it became ready, but no sooner than 'refresh' nanoseconds after the previous presentation on this surface … determined by when this surface's content became ready, not by a periodic display refresh or by updates to other surfaces". With either flag set, refresh must be non-zero. The author is Will Campbell (NVIDIA), and the commit message says it exists to feed `VK_EXT_present_timing`'s fixed/variable/unknown reporting. It was merged 2026-10-14 and first released in **1.50** (2026-10-29). — [commit b9edf66](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/commit/b9edf66597586c914b5b1ff3d964d7ed201adab9); [tag 1.50](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/tags/1.50)
- fifo-v1 semantics: `set_barrier` sets a condition that "is cleared immediately after the following latching deadline for non-tearing presentation". `wait_barrier` marks an update "not ready while a fifo_barrier condition is present". The compositor "may clear the condition early … to ensure client forward progress" and may ignore it for off-screen surfaces, so "Clients must use an additional mechanism such as frame callbacks or timestamps". — [fifo-v1.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/fifo/fifo-v1.xml)
- commit-timing-v1: an update "should be presented as closely as possible to, but not before, a specified time", in the compositor's presentation clock domain. — [commit-timing-v1.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/commit-timing/commit-timing-v1.xml)

**Smithay**
- At Cha's pin (118e34ff, committed 2026-09-30), Smithay has `src/wayland/fifo` and `src/wayland/commit_timing`. Both place a commit `Blocker` and leave signalling to the compositor: "It is your responsibility to query for a set barrier on a surface and signal it". Each also has an "unmanaged" mode. There is no tearing-control module. — [Smithay @118e34ff fifo](https://github.com/Smithay/smithay/blob/118e34ffc9b99854a2230c4805e1f37dc9029edb/src/wayland/fifo/mod.rs); [commit_timing](https://github.com/Smithay/smithay/blob/118e34ffc9b99854a2230c4805e1f37dc9029edb/src/wayland/commit_timing/mod.rs); fifo is also in the [0.7.0 release on docs.rs](https://docs.rs/crate/smithay/0.7.0/source/src/wayland/fifo/mod.rs)
- Smithay's presentation global is created at **version 2**. `Refresh` is `Unknown | Variable(min) | Fixed(d)`. `Variable(min)` sends the fastest rate to v2+ clients, and "Clients binding an earlier version shall receive a zero refresh". Smithay master on 2026-11-04 is still v2: no v3 flags, no tearing-control. — [presentation/mod.rs @118e34ff](https://github.com/Smithay/smithay/blob/118e34ffc9b99854a2230c4805e1f37dc9029edb/src/wayland/presentation/mod.rs); [Smithay master](https://github.com/Smithay/smithay/tree/master/src/wayland)
- Cha today: every tick, `tick()` calls `take_presentation_feedback(..., Kind::Vsync)` and `presented(now, Refresh::Fixed(tick_period), ticks, Kind::Vsync)`, then `send_frame` to every window and the cursor surface. The comment notes "Chrome follows presentation feedback, not frame callbacks". Cha creates `PresentationState` but no fifo, commit-timing or tearing state. — [compositor/mod.rs](/home/user/cha-portal/crates/cha-streamer/src/compositor/mod.rs) (lines ~635–720, 474)
- The pinned rev's subject line is "drm: allow disabling VRR on legacy surfaces". That is DRM-backend VRR, which a headless output doesn't use. — [Smithay @118e34ff](https://github.com/Smithay/smithay/commit/118e34ffc9b99854a2230c4805e1f37dc9029edb)

**Other compositors**
- **KWin** (master 0406762, 2026-11-08):
  - fifo-v1 is always registered. commit-timing-v1 is registered unless `KWIN_WAYLAND_SUPPORT_COMMIT_TIMING_V1` is false; it defaults to on. — [wayland_server.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland_server.cpp)
  - A cosmic-comp developer said KWin disables commit-timing by default (Oct 2026). That contradicts master's default; it may describe a release branch (unverified). — [cosmic-comp PR #2797](https://github.com/pop-os/cosmic-comp/pull/2797)
  - fifo-v1 has been supported since Plasma 6.4. — [KDE bug 491219](https://bugs.kde.org/show_bug.cgi?id=491219)
- **KWin VRR scheduling** (renderloop.cpp, master):
  - In AdaptiveSync, `targetTimestamp = max(presentNotBefore, lastPresentationTimestamp + vblankInterval)` ("pageflips happen after one vblank interval"; "FIXME take minimum refresh rate into account as well"). With tearing, pageflips happen ASAP.
  - Under VRR only one frame may be pending.
  - A repaint from anything other than the "repaint driver" (the active window's surface item on that output) is deferred by a `1'000ms / 30` timer. Cursor and other surfaces therefore don't drive the refresh, but still get at least ~30 Hz.
  — [renderloop.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/core/renderloop.cpp)
- **KWin presentation-time** sends refresh 0 only to v1 clients under adaptive sync ("version 1 requires sending zero"), and a TODO notes it could send an estimate of the actual rate. — [presentationtime.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/presentationtime.cpp). A 2024 MR to always send refresh says refresh=0 "breaks applications that rely on a non-zero refresh to accurately handle high refresh rates (e.g. chromium)" and that Sway already sends it. — [KWin MR !5697](https://invent.kde.org/plasma/kwin/-/merge_requests/5697)
- **Chromium under VRR on KWin**: it "fails to VSync unless the hardware cursor is moved". zamundaaa: "This is almost certainly a chromium bug. It abuses presentation time to schedule its rendering, which does not work with VRR". A duplicate bug was "Chromium is limited to 60 fps". Resolved upstream; no Chromium fix version found. — [KDE bug 478938](https://bugs.kde.org/show_bug.cgi?id=478938) (Dec 2023–Jan 2024)
- **Mutter**:
  - fifo-v1 and commit-timing-v1 since 48.alpha, with presentation-timing fixes in 48.6/49.1. — [Mutter 49.1 notes](https://newreleases.io/project/gnome-gitlab/GNOME/mutter/release/49.1); [48.6](https://newreleases.io/project/gnome-gitlab/GNOME/mutter/release/48.6)
  - GNOME 50: Michel Dänzer's 11 VRR patches (merged 2026-02-06) move VRR to a deadline timer that aims "for the earliest possible start of the next refresh cycle" ("If it's already too late for that, start ASAP"), ensure "the presentation doesn't happen before the target time" (commit-timing works with VRR), and pick "up the latest mouse cursor position at the deadline". No numbers given. — [LavX summary of Phoronix](https://news.lavx.hu/article/gnome-50-boosts-vrr-performance-with-lower-latency-and-improved-wayland-timing); [Phoronix](https://phoronix.com/news/mutter-50-vrr-deadline)
- **wlroots** has no fifo-v1 or commit-timing-v1 in 0.19.x, 0.20.0 (2026-03-26), 0.20.1/0.20.2, or master (2026-11-02). It has `wlr_presentation_time.c` with `PRESENTATION_VERSION 2` (v1 clients get refresh 0) and `wlr_tearing_control_v1.c`. — [wlroots master types/](https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/types). A commit-timing MR is still a draft. — [wlroots !4463](https://gitlab.freedesktop.org/wlroots/wlroots/-/merge_requests/4463)
- **niri** has a `FrameClock { refresh_interval, vrr }` and on-demand VRR via window rules (since v0.1.9). — [frame_clock.rs](https://github.com/niri-wm/niri/blob/main/src/frame_clock.rs); [niri v0.1.9](https://newreleases.io/project/github/YaLTeR/niri/release/v0.1.9). No fifo/commit-timing PR found; on the open syncobj PR a commenter says "Can't wait for fifo as well!". — [niri PR #3956](https://github.com/niri-wm/niri/pull/3956)
- **cosmic-comp** has an RFC PR implementing both protocols, open since ~2026-08-31, last activity 2026-10-28:
  - barriers are tied to an output and signalled per present ("actually being limited by the vblank");
  - commit-timing uses a 1 ms early-commit margin;
  - on a 240 Hz monitor, a 10.417 ms commit-timing interval gave 12.379 ms average frame time with VRR off, and a 14.063 ms interval gave 14.064 ms with VRR on **(single-dev)**;
  - the author notes abnormal behaviour reports with NVIDIA's Vulkan driver.
  — [cosmic-comp PR #2797](https://github.com/pop-os/cosmic-comp/pull/2797)
- SDL3 prefers Wayland over X11 only when fifo-v1 and commit-timing-v1 are present. — [SDL PR #9345](https://github.com/libsdl-org/SDL/pull/9345); [GamingOnLinux](https://www.gamingonlinux.com/2024/03/sdl-3-will-prefer-wayland-over-x11-if-certain-protocols-are-available/)

### Inferences
- For a headless output, VRR in Cha means only "which `Refresh`/`Kind` and timestamps feedback carries, and when frames are composited". No DRM VRR property is involved.
- `Refresh::Variable(1/max_fps)` splits clients neatly. v1 binders get 0, and gamescope is one (§3), which puts it in VRR mode. v2 binders get a non-zero fastest rate, which is what Chromium-style pacers need. Which version Chrome and Mesa bind is unverified (see Gaps).
- Smithay only "signals barriers"; Cha decides when. Under on-commit compositing, the natural rule is to signal fifo barriers and commit-timers whose target is ≤ the composite time when a composite consumes the surface's content (cosmic's "only the current barrier" lesson). A keep-alive tick should also clear barriers on surfaces that weren't composited, for forward progress.
- KWin's repaint-driver pattern carries over directly. In a gamescope session, gamescope's toplevel is the driver. Cha's cursor travels outside the picture (`publish_cursor`), so cursor motion never needs a composite. Popups and other windows coalesce with a floor of ~30 Hz.

### Gaps
- No Smithay PR numbers or merge dates for fifo/commit-timing were found (the modules exist at the pin). No Smithay work on presentation v3 or tearing-control was found.
- Hyprland's fifo/commit-timing PR ([#12052](https://github.com/hyprwm/Hyprland/pull/12052)) has unknown status. Weston support is reported only by a secondary article (unverified).

## 2. Vulkan WSI on Wayland: how Mesa and NVIDIA pace FIFO, MAILBOX and IMMEDIATE, and what Cha's 240 Hz callbacks do to a game

### Takeaway
Mesa's Wayland WSI (main, 2026-11-09):
- **FIFO**: paced by fifo-v1 barriers when the compositor has fifo, with commit-timing timestamps phase-locked to the presentation-feedback refresh when commit-timing is also present, and by frame callbacks otherwise.
- **MAILBOX**: not frame-throttled; buffer availability limits it.
- **IMMEDIATE**: offered only with tearing-control.

Cha has no fifo, so a Mesa FIFO client connected to Cha directly renders at the 240 Hz callback rate. With fifo + commit-timing and `Fixed(1/240)`, it would target the next 240 Hz phase. With refresh 0, Mesa stops generating timestamps and uses the barrier alone. `VK_EXT_present_timing` shipped in Mesa 26.1 (Wayland) and NVIDIA 595. Mesa's Wayland WSI doesn't expose FIFO_LATEST_READY.

### Cited Findings
- Mesa Wayland present modes: "The following two modes are always supported": MAILBOX and FIFO. IMMEDIATE is added only `if (display.tearing_control_manager)`, and IMMEDIATE sets the tearing hint ASYNC. — [wsi_common_wayland.c @d3c007c5](https://gitlab.freedesktop.org/mesa/mesa/-/blob/d3c007c5dcef02dcaf0745754f21a879601047ff/src/vulkan/wsi/wsi_common_wayland.c)
- Mesa FIFO pacing (same file):
  - `if (mode_fifo && !chain->fifo)` it falls back to the legacy frame-callback throttle ("we must fall back to legacy mechanism for throttling").
  - With fifo it calls `wp_fifo_v1_set_barrier` + `wait_barrier` on every present.
  - With commit-timing as well, `set_timestamp()` targets `next_phase_locked_time(displayed_time, refresh, …)` minus a 500 µs margin, and corrects missed frames.
  - "If refresh is 0, presentation feedback has informed us we have no fixed refresh cycle. In that case we can't generate sensible timestamps at all, so bail out", unless the app supplied an absolute time via present_timing.
- In MAILBOX the image "can be replaced" (`legacy_fifo_ready = true`), so there is no callback wait. "For true mailbox mode, we need at least 4 images". — same file
- Mesa's presentation-feedback handler: "In v1 … we can know if we're likely running VRR, given refresh is 0 … In v2 … we cannot know if we're driving the display VRR or FRR … There is a v3 proposal … just report 'Unknown' for now." — same file
- `MESA_VK_WSI_DEBUG=nowlts` disables the Wayland timestamps (`wsi->wayland.disable_timestamps`). — [wsi_common.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/d3c007c5dcef02dcaf0745754f21a879601047ff/src/vulkan/wsi/wsi_common.c). One user reports FIFO windows running at 45–50 fps on 60 Hz because the compositor held the timed commits, fixed by `nowlts` **(single-dev, Oct 2026)**. — [OmacVM PR #450](https://github.com/gillesgoetsch/OmacVM/pull/450)
- Mesa 24.3 merged Vulkan FIFO on Wayland using fifo-v1 + commit-timing. — [Phoronix](https://www.phoronix.com/news/Mesa-24.3-Vulkan-FIFO)
- `VK_EXT_present_timing` in Mesa:
  - Mesa 26.1.0 (2026-05-06) lists "VK_EXT_present_timing on RADV, NVK, Turnip, ANV, Honeykrisp, panvk". — [Mesa 26.1.0 notes](https://docs.mesa3d.org/relnotes/26.1.0.html)
  - Mesa 26.2.0 (2026-08-05) adds it "now also on wsi/x11" plus `VK_GOOGLE_display_timing` (opt-in for X11 and Wayland). — [Mesa 26.2.0 notes](https://docs.mesa3d.org/relnotes/26.2.0.html)
  - On Wayland `presentAtAbsoluteTimeSupported = has_commit_timing && has_fifo`. — [wsi_common_wayland.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/d3c007c5dcef02dcaf0745754f21a879601047ff/src/vulkan/wsi/wsi_common_wayland.c)
- Khronos announced `VK_EXT_present_timing` on 2025-12-04 (Lionel Duc, NVIDIA). NVIDIA developer drivers had it at launch; Mesa and Android 17 support was "coming soon". **(vendor)** — [Khronos blog](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan)
- NVIDIA proprietary driver:
  - 580.65.06 beta (2025-08-04): "Added support for the fifo-v1 Wayland protocol on Vulkan". — [GamingOnLinux](https://www.gamingonlinux.com/2025/08/nvidia-beta-driver-580-65-06-released-for-linux-with-bug-fixes-and-expanded-wayland-support/)
  - 595 series: added `VK_EXT_present_timing` (secondary report). — [linuxiac 595.45.04](https://linuxiac.com/nvidia-595-45-04-beta-linux-driver-released/)
  - No NVIDIA note mentions commit-timing-v1.
- `VK_EXT_present_mode_fifo_latest_ready` came with Vulkan 1.3.297 (Oct 2024) and NVIDIA 550.40.78 supported it. — [Phoronix](https://phoronix.com/news/Vulkan-1.3.297). Mesa's Wayland present-mode list (above) doesn't include it on main; Mesa's generic WSI code only references the mode in present-timing estimates. — [wsi_common.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/d3c007c5dcef02dcaf0745754f21a879601047ff/src/vulkan/wsi/wsi_common.c)

### Inferences
- Today, a Vulkan FIFO game run directly on Cha (not under gamescope) with Mesa is throttled by Cha's 240 Hz frame callbacks, renders up to 240 fps, and 3 of 4 frames are dropped at a 60 fps encode. MAILBOX runs unthrottled. Neither is VRR-like, and both burn GPU.
- If Cha exposes fifo-v1 and keeps `Fixed(1/240)`, Mesa FIFO stays at 240. With `Refresh::Variable(min)` to a v2 binder, Mesa phase-locks to `min`. With `Unknown`, or 0 to v1, it sends no timestamps, so barrier signalling alone decides the cadence: one frame per Cha composite. Cha's composite policy then *is* the app's FIFO rate, so the rate cap must live in Cha (a minimum composite interval).
- In Cha's gaming sessions, games are X11/Xwayland clients of gamescope, not of Cha. Their WSI pacing is decided by gamescope's own Wayland server (§3). Cha's protocol choices reach games only through gamescope's behaviour, so changes to Cha's WSI-facing protocols mainly matter for desktop sessions (Chrome, native Wayland apps, SDL3 games that need fifo + commit-timing to choose Wayland).

### Gaps
- No public description of NVIDIA's Wayland WSI pacing for MAILBOX/IMMEDIATE, or of its use of frame callbacks versus fifo, was found. Neither was its reaction to refresh 0, nor whether it uses commit-timing.
- No source found on which wp_presentation version Mesa's loader or Chrome binds. The Mesa source wraps it via `loader_wayland_wrap_presentation`, but the version wasn't checked.

## 3. Nested gamescope: how `-r` and the limiter work in `--backend wayland`, and what `--adaptive-sync` does nested

### Takeaway
Nested gamescope ignores Cha's frame callbacks: its Wayland backend requests no frame callbacks at all. It runs its own vblank timer at `-r` (`g_nNestedRefresh`), anchored to the host's presentation-feedback timestamps, and it adopts the host's feedback refresh as its "output refresh". Under Cha it therefore reports 240 Hz to Steam while compositing at `-r`.

VRR turns on only if the host sends **refresh = 0** *and* adaptive sync is enabled (`--adaptive-sync`, or Steam's `GAMESCOPE_VRR_ENABLED` atom). gamescope then paints when the game has a new frame, at most once per `-r` interval. Overlay (mangoapp) and cursor repaints don't drive it, and Steam's FPS limit becomes a per-window time schedule instead of a vblank divisor. So with VRR on, the game's uneven frames survive to the host; with it off, they are quantized to `-r`. Steam's refresh slider has no rates to offer, because the Wayland backend's dynamic-refresh list is empty.

### Cited Findings (gamescope master 89b9e6ee, 2026-11-07, unless noted)
- The Wayland backend binds `wp_presentation` at **version 1** and requests feedback only on commits that carry a buffer. There is no `wl_surface_frame` use. — [WaylandBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/src/Backends/WaylandBackend.cpp) (lines ~1492, 2580)
- In `Wayland_PresentationFeedback_Presented`:
  - if `uRefreshCycle != 0`, it sets `g_nOutputRefresh` to the host rate ("Changed refresh to: …hz") and `SetHostCompositorIsCurrentlyVRR(false)`;
  - otherwise it marks the host VRR and takes the largest `wl_output` mode refresh;
  - then `GetVBlankTimer().MarkVBlank(ulTime, true)` re-arms its timer from the host's presentation time.
  — same file, ~1816–1843
- `IsVRRActive() { return cv_adaptive_sync && m_bHostCompositorIsCurrentlyVRR; }`. `SupportsVRR()` is "host currently VRR". `GetValidDynamicRefreshRates()` returns an empty span. `SupportsTearing()` is false. — same file, ~1219–1240, 2347
- Timer rate: `CVBlankTimer::GetRefresh()` returns `g_nNestedRefresh ? g_nNestedRefresh : g_nOutputRefresh`, so `-r` wins over the host's rate. — [vblankmanager.cpp](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/src/vblankmanager.cpp)
- Timer anchoring by version:
  - **3.16.31** (2026-09-27): the next wake is `GetLastVBlank() + interval(-r) - offset`, so it is phase-locked to the last host feedback timestamp. — [vblankmanager.cpp @3.16.31](https://github.com/ValveSoftware/gamescope/blob/3.16.31/src/vblankmanager.cpp)
  - **3.17.0** (2026-10-23) and master: `VBlankSchedule.h` adds `bRateOverride = SupportsIndependentRefresh() && nNestedRefresh != 0` ("An explicit rate owns its cadence, independent of host feedback"). Its VRR branch has "no fixed phase and can follow a newly ready frame". — [VBlankSchedule.h @3.17.0](https://github.com/ValveSoftware/gamescope/blob/3.17.0/src/Utils/VBlankSchedule.h)
- VRR paint decision (steamcompmgr.cpp):
  - `bVRRCanFlip = IsVRRFlipReady()`, i.e. `now + offset >= lastVBlank + interval(-r)`. With `adaptive_sync_uncapped` (default true; "allow mailbox/immediate clients to run uncapped with adaptive sync by deferring paints until the display can take a new flip") the paint is `FlipType::VRR: bShouldPaint = hasRepaint && bVRRCanFlip`.
  - Overlay-only repaints wait `adaptive_sync_overlay_cycles` (default 1) timer vblanks.
  - Cursor-only repaints are allowed if content is below `adaptive_sync_cursor_min_fps` (default 30).
  - `adaptive_sync_idle_hold` (default true) holds "the Steam UI at the refresh rate" with repeated frames.
  — [steamcompmgr.cpp](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/src/steamcompmgr.cpp) (~303–308, 10555–10575, 11160–11215)
- Frame limiter (steamcompmgr.cpp):
  - Steam writes `GAMESCOPE_FPS_LIMIT` on the X root, which feeds `g_nSteamCompMgrTargetFPS`.
  - Fixed refresh: `nVblankDivisor = nRefreshHz / nTargetFPS` (integer), so a frame callback reaches the game only when the vblank index crosses a divisor boundary.
  - VRR: `g_SteamCompMgrLimitedAppRefreshCycle = 1/targetFPS`, and the callback is released when `now + 0.2 ms ≥ last_commit_first_latch_time + cycle`.
  - The refresh used is `g_nNestedRefresh ? g_nNestedRefresh : g_nOutputRefresh`, and the target is clamped to it.
  — same file, ~1219–1280, 6975–7090, 10848–10868
- Refresh slider: `_update_app_target_refresh_cycle` looks for a dynamic rate in `GetValidDynamicRefreshRates()`, which is empty on Wayland, so `g_nDynamicRefreshRate` stays 0. — same file, ~1229–1265, 3576
- Atoms gamescope publishes: `GAMESCOPE_VRR_CAPABLE` (= host currently VRR), `GAMESCOPE_VRR_FEEDBACK` (in use), and `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK` (= `g_nOutputRefresh`, i.e. the host's feedback rate). It reads `GAMESCOPE_VRR_ENABLED`, which sets `cv_adaptive_sync`. — same file, ~7556–7561, 9646–9700
- `gamescope_limiter` (a 2026 protocol in master) tells presentation layers "1 means the frame limiter is engaged and clients should present in FIFO so the compositor can pace them"; it replaces `GAMESCOPE_LIMITER_FILE`. — [gamescope-limiter.xml](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/protocol/gamescope-limiter.xml). gamescope 3.17.0 adds a `VK_EXT_present_timing` queue to its WSI layer (`layer/present_timing.hpp`); 3.16.31 doesn't have it. — [present_timing.hpp @3.17.0](https://github.com/ValveSoftware/gamescope/blob/3.17.0/layer/present_timing.hpp)
- CLI: `-r, --nested-refresh  game refresh rate (frames per second)`; `--adaptive-sync  Enable adaptive sync if available (variable rate refresh)`; `--framerate-limit` is "a divisor of the refresh rate". — [main.cpp](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/src/main.cpp). `--adaptive-sync` works nested, despite help text that once said embedded-only. — [gamescope #1694](https://github.com/ValveSoftware/gamescope/issues/1694)
- Cha builds **Ubuntu 26.04's gamescope source** with one patch that calls `mangoapp_output_update(ulTime)` in the Wayland presentation-feedback handler ("nothing else calls this in a nested gamescope"). The patch hunk sits at line ~1768, versus 1803 in 3.16.31 and 1839 in master, so Ubuntu's version predates 3.16.31 (exact version unverified) and lacks the 3.17 `bRateOverride` logic. — [images/steam/Dockerfile](/home/user/cha-portal/images/steam/Dockerfile); [gamescope-wayland-mangoapp.patch](/home/user/cha-portal/images/steam/gamescope-wayland-mangoapp.patch); [steam-gaming](/home/user/cha-portal/images/steam/steam-gaming)
- Community (not authoritative): nested mode "introduces some anomalous stuttering", and the advice is to enable VRR "in your monitor, in your DE AND in gamescope". — [GabMus dev log](https://gabmus.org/posts/fix-games-losing-focus-on-multimonitor/). gamescope's limiter is said to add 1–2 frames versus in-engine caps (issues #474, #1672) (unverified). — [Botmonster](https://botmonster.com/self-hosting/gamescope-desktop-linux-hdr-vrr-fps-limiting-steam/)

### Inferences
- **What happens under Cha today**:
  - gamescope sees refresh = 1/240 s on every feedback. It logs "Changed refresh to: 240hz", publishes 240 in `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK` (Steam may build its limit choices from that; unverified), and keeps VRR off.
  - It composites at `-r` = `CHA_REFRESH`, phase-locked to the 240 Hz tick that "presented" its last commit (3.16-era code).
  - A game frame waits for gamescope's next `-r` wake (0–1/r), and the composited frame then waits for Cha's next encode tick (0–1/fps), even though the two clocks share a frequency. That is roughly up to two frames of pure clock wait, and a game below `-r` judders: frames are repeated at `-r`, then repeated or dropped again at the encode tick.
  - The 240 Hz callbacks that helped Chrome in S2 have no effect on gamescope.
- **To get frames at the game's cadence**:
  1. Run gamescope with `--adaptive-sync`, or set the root atom `GAMESCOPE_VRR_ENABLED=1`, which Cha's X helpers could write.
  2. Send gamescope's surface a refresh of 0. Since gamescope binds v1, `Refresh::Variable(min)` or `Unknown` both do this in Smithay.
  3. Send gamescope `presented` at the time Cha actually composited (or encoded) its buffer, not at the next 240 Hz tick, because gamescope anchors `IsVRRFlipReady` to it.
  4. Set `-r` to the stream's ceiling (e.g. 120) so `-r` acts as the max-rate cap.
- In VRR mode, Steam's per-game FPS limit becomes time-based, so 45 or 50 fps work, not just divisors. The `-r` cap no longer fixes the stream rate, which suggests Cha's live 60/90/120 change could work for gamescope sessions with `-r` at the maximum and Cha capping composites (needs a spike).
- The Steam UI idle hold paints at `-r` with `bRepeatFrame`. Cha should detect those repeated buffers (same content or no damage) and skip them, or the Steam UI streams at `-r` with nothing changing.

### Gaps
- Not verified on a live node; everything above comes from reading source. Whether Ubuntu 26.04's gamescope has the same VRR paint path (cursor-min-fps and idle-hold may be newer) needs checking against its exact version.
- How Steam's client uses `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK` and `GAMESCOPE_VRR_CAPABLE` to draw its FPS-limit and VRR toggles isn't documented (closed source).

## 4. Encoders at irregular cadence: NVENC CBR with a one-frame VBV, VA-API HRD, the case for VFR, keyframes and RFI

### Takeaway
None of the encoders Cha uses documents VFR behaviour for low-latency CBR. NVENC's guide treats the frame rate as a rate-control input and `inputTimeStamp` as a frame ID for RFI, and recommends a single-frame VBV (bitrate/framerate). Sunshine, the closest prior art, feeds NVENC exactly as Cha does: a one-frame VBV, fixed `frameRateNum`, frame index as `inputTimeStamp`. It already encodes at irregular cadence when capture is event-driven, and re-encodes the last image after `1/(fps/2)`. So treating the stream as VFR with the encoder configured at the *cap* rate is established practice. The cost is that bits per second fall with the game's frame rate, while bits per frame stay at the cap's budget (inference).

### Cited Findings
- NVENC Programming Guide (SDK 13.0):
  - The low-latency recommendation is "Rate control mode = CBR", "Very low VBV buffer size (e.g. single frame = bitrate/framerate)", "Ultra-low latency or low latency Tuning Info", "Infinite GOP length".
  - `inputTimeStamp` is how "The encoder stores the frames in its DPB … as the unique identifier" for RFI, and it "can be any monotonically increasing unique number".
  - `nvEncReconfigureEncoder` lets clients "change parameters like bit-rate, frame-rate, resolution dynamically".
  - For strict bitrate, set `enableFillerDataInsertion`/`enableBitstreamPadding`.
  - Nothing on VFR input. — [NVENC guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- Forum view, ~7 years old and non-authoritative: frameRateNum/Den "is used only for estimating the average input frame rate", which drives RC sizing. — [Blackmagic forum](https://forum.blackmagicdesign.com/viewtopic.php?t=95068&p=528206). Another report: reconfiguring with a new framerate forced an IDR (old, unverified). — [NVIDIA forum](https://forums.developer.nvidia.com/t/nvenc-generates-idr-frames-when-reconfiguring-the-codec-with-a-new-framerate/125469). This contradicts Cha's own test: 60→120 in place, 480 frames, no keyframe **(single-dev, RTX 4090)**. — [cha-streamer README "Frame rate"](/home/user/cha-portal/crates/cha-streamer/README.md)
- Cha's NVENC config:
  - `frameRateNum = fps`, `frameRateDen = 1`, CBR, `averageBitRate = maxBitRate = bitrate`;
  - "One frame of VBV: a frame never waits behind the previous one's bits", `vbvBufferSize = bitrate / fps`, `vbvInitialDelay = vbvBufferSize`;
  - infinite GOP and IDR period, `inputTimeStamp = frame_index`, RFI over an 8-frame DPB.
  — [cha-nvenc/src/nvenc.rs](/home/user/cha-portal/crates/cha-nvenc/src/nvenc.rs)
- Cha's VUI writes `timing (1, fps*2)` and `fixed_frame_rate_flag = 1`, with no HRD parameters. — [encoder/bitstream.rs](/home/user/cha-portal/crates/cha-streamer/src/encoder/bitstream.rs). Cha's frames carry `rendered: Instant` (GPU finish). — [media.rs](/home/user/cha-portal/crates/cha-streamer/src/media.rs)
- Sunshine (master 0ddc73d, 2026-11-08) NVENC: CBR, `vbvBufferSize = bitrate*1000/framerate` (+ optional `vbv_percentage_increase`), `frameRateNum/Den` from the client fps, `inputTimeStamp = frame_index`, `enableMinQP = 1` in some codec paths. — [nvenc_base.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/nvenc/nvenc_base.cpp)
- Sunshine encode loop:
  - `minimum_fps_target = config > 0 ? config : framerate/2`, `max_frametime = 1000/minimum_fps_target`;
  - `images->pop(max_frametime)`: "Encode at a minimum FPS to avoid image quality issues with static content". On timeout it re-encodes the last converted image.
  — [video.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/video.cpp); [docs: minimum_fps_target](https://github.com/LizardByte/Sunshine/blob/master/docs/configuration.md) ("many clients expect a constant stream of video frames, a certain amount of duplicate frames are sent"; 0 = half the stream fps)
- VA-API:
  - The HRD misc buffer carries CPB `buffer_size` and `initial_buffer_fullness` in bits.
  - The frame-rate misc buffer carries one framerate (a zero denominator means 1).
  — [libva VAEncMiscParameterHRD](https://intel.github.io/libva/structVAEncMiscParameterHRD.html); [VAEncMiscParameterFrameRate](https://evelikov.github.io/libva/structVAEncMiscParameterFrameRate.html)
  - FFmpeg: CBR/VBR behaviour "varies significantly by driver and device". — [FFmpeg VAAPI wiki](https://fftrac-bg.ffmpeg.org/wiki/Hardware/VAAPI)
  - AMD VCU guidance (different hardware): a CPB of ~1–2 frame periods for low latency. — [AMD PG252](https://docs.amd.com/r/2023.1-English/pg252-vcu/Rate-Control-Parameters)
- PyroWave in Cha: the budget is per frame ("It is per frame, so the total grows linearly with the rate"), and pacing derives bytes/frame from a target bit rate divided by `fps`. — [pyro.rs](/home/user/cha-portal/crates/cha-streamer/src/pyro.rs); [README "Frame rate"](/home/user/cha-portal/crates/cha-streamer/README.md)
- Cha already separates "fixed in time" rules (rate control, hold 25 ms, FEC per second) from "fixed in frames" rules (8-frame RFI DPB, ~117 ms at 60 / ~58 ms at 120; re-encode throttle once per frame interval). — [README audit table](/home/user/cha-portal/crates/cha-streamer/README.md)

### Inferences
- **Keep `frameRateNum` at the stream's cap (max fps) and the VBV at one frame of the cap.**
  - When frames arrive less often, a CBR model that refills the VBV per frame at bitrate/fps (NVENC has no wall clock) still gives each frame about one cap-frame of bits. Average bit rate drops in proportion to the game's fps, and latency never suffers. This is unverified against NVENC internals; measure it with Cha's `switches_to_120_fps_in_place_and_keeps_up`-style test by feeding frames at irregular intervals.
  - Reconfiguring `frameRateNum` to the measured fps every few hundred ms is possible (Cha's in-place reconfigure doesn't force IDR), but it adds churn for little gain.
- **Per-frame quality** is what VFR preserves. A 45 fps game at a 60 fps cap gets the same bytes per frame as at 60, and spends 25% fewer bits/s. That leaves headroom which rate control (`rate.rs`, fixed in time) can use. Optionally the bits-per-frame ceiling could rise when fps is low, up to the link rate: VBV = link_rate × min_interval.
- **RFI reach** is fixed in frames, so at low game fps the 8-frame DPB covers *more* time, which helps. Under VFR, "frames lost since X" must be computed from frame indices, not time. Cha already uses indices (`inputTimeStamp = frame_index`).
- **VUI**: with VFR, `fixed_frame_rate_flag = 1` and `time_scale = 2·fps` become false statements. WebRTC/RTP timing comes from RTP timestamps (presumably derived from `rendered`, unverified), so decoders likely ignore it. Still, clearing the flag, or writing timing for the cap with `fixed_frame_rate_flag = 0`, is the honest encoding.
- **Keyframe on request with nothing new** (Cha re-encodes the last frame) and Sunshine's half-fps keep-alive are the same idea. Under VFR, a keep-alive re-encode (cheap P-frame skip blocks) protects against static-content quality issues and against clients that time out on silence. Sunshine defaults to fps/2.

### Gaps
- No primary source on NVENC rate control (bits per frame, VBV refill model) with irregular input, on `enableMinQP` interplay, or on VA-API driver HRD behaviour (Mesa radeonsi, Intel iHD) with VFR.
- x264 and SVT-AV1 VFR handling (x264 `b_vfr_input`/timebase, SVT-AV1 fps parameters) not researched.
- A search snippet described an older Sunshine PR in which a low capture rate froze the last frame for long periods on AMD's encoder, fixed by allowing ~20 fps minimum. The PR wasn't opened and its number isn't confirmed (search listed [Sunshine #1167](https://github.com/LizardByte/Sunshine/issues/1167)), so treat it as unverified evidence that some hardware encoders mishandle long gaps.

## 5. Prior art: how other hosts handle frame timing, and whether any streams frames as rendered

### Takeaway
By Nov 2026 the open-source hosts have converged on **"capture/publish when the compositor has a new frame, held to the stream rate by a budget, not a clock"**:
- Sunshine's wlr capture: event-driven when the output runs ≥1.5× the stream rate;
- Selkies/pixelflux: publish "at once … instead of at the next frame tick", app redraw → encoder 11 → 2.6 ms median **(single-dev)**;
- Vibepollo: a 1000 Hz virtual display in "vrr" mode so each game frame is captured as soon as it's drawn;
- GeForce NOW: varies the stream rate to the game's frame rate **(vendor)**.

None reports an independent end-to-end latency or smoothness measurement.

### Cited Findings
- **Sunshine** (master, 2026-11-08), wlr capture, event-driven versus paced:
  - `use_damage_capture` is true if screencopy v2+ and `refresh ≥ 1.5 × stream_fps`.
  - "Event-driven capture has no clock. The request is answered by the next commit that changed the picture, so a frame is captured once, when it exists … that is a budget rather than a clock: every captured frame moves the earliest time of the next request on by one frame interval. A frame that arrives late resets the budget to a quarter interval before its own time."
  - `next = max(next, frame_time − delay/4) + delay`.
  - Measured on a wlroots headless output **(single-dev)**: at 60 Hz with a 60 fps stream, removing paced copies dropped delivery from 60 to ~55 (the paced copies were keeping clients on time). At twice the rate, repeats went from "up to one repeated picture in ten to none".
  — [wlgrab.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/wlgrab.cpp); [docs](https://github.com/LizardByte/Sunshine/blob/master/docs/configuration.md)
- **Vibepollo** (Nonary, docs at 5e6ed85, 2026-11-02), `frame_limiter_auto_virtual_framegen`:
  - "Windows only hands Vibeshine a new frame when it redraws the display, so on a display that refreshes at the stream rate a frame that finishes just after a redraw waits up to a whole refresh before capture, and frames reach the client unevenly."
  - `vrr` "requests a fixed 1000 Hz virtual display whatever the stream rate, reducing the delay between game presentation and capture for clients with VRR pacing" (capped by OS scan limits, ~463 Hz at 4K on older Windows).
  - `enabled` = 4× refresh with ≤2× capture on the desktop; `legacy` = 2×.
  - All modes but `disabled` "cap games to the stream rate".
  - The frame limiter uses RTSS/NVCP/MangoHud: `frame_limiter_fps_limit`, and `mangohud_limiter_method` early ("Smoother frame pacing with more latency") versus late ("Lower latency").
  — [Vibepollo configuration.md](https://github.com/Nonary/Vibepollo/blob/HEAD/docs/configuration.md)
  - Single-user report: a cap equal to the refresh added >7 ms of queue delay versus <3 ms at 117 fps (Windows/NVIDIA). — [Vibepollo #71](https://github.com/Nonary/Vibepollo/issues/71)
- **Moonlight-qt VRR builds** (client side, mentioned only for the host contract):
  - "VRR timing" presets: Low Latency (≤4.17 ms buffer at 120 fps), Balanced Target (≤8.33 ms), Smooth (≤24 ms), described as "maximum buffer allowances, not delay added all the time".
  - "Host capture stalls, network interruptions, and game-driven frame-rate changes can still be visible", and users should "Select a stream frame rate the host can sustain within the display's VRR range". **(single-dev)**
  — [Nonary/moonlight-qt v6.1.0-vrr18](https://newreleases.io/project/github/Nonary/moonlight-qt/release/v6.1.0-vrr18)
- **Selkies 2.2** (~2026-11-02), pixelflux 2.2.2: "publishes a change at once on XShm, DRI3, and the Wayland compositor, instead of at the next frame tick … an application's own redraw now reaches the encoder in 2.6 ms at the median, down from 11" **(single-dev)**. — [Selkies 2.2.0](https://newreleases.io/project/github/selkies-project/selkies/release/2.2.0)
- pixelflux's `pace.rs` (2026-11-09) keeps a frame timer, and "something that actually happened — input …, a frame a host delivered, a region … damaged — may render one ahead of that timer":
  - a pull is allowed after ≥0.5 period (`INPUT_TICK_MIN_FRACTION`), and the timer may run 10% early;
  - input pulls spend a budget refilling at 1/32 of wall time, so input "raises the output rate by no more than the refill";
  - damage or host frames "move the cadence onto the frames … without raising the rate";
  - "a change arriving on its own schedule is published as it lands rather than up to a period later".
  — [pixelflux pace.rs](https://github.com/linuxserver/pixelflux/blob/HEAD/src/pace.rs); pixelflux is "a full headless Wayland compositor built on Smithay". — [LinuxServer docs](https://docs.linuxserver.io/selkies/components/pixelflux/)
- **Wolf** (gst-wayland-display, 2026-11-07):
  - it composites when the GStreamer source asks for a buffer (`Command::Buffer`), i.e. at the pipeline's rate;
  - it then sends frame callbacks and feedback with `Refresh::fixed(mode refresh)`, or `Refresh::Unknown` when the caps framerate is variable ("A variable or unset framerate (`0/1`) yields 0, which `wl_output` reads as unknown").
  — [comp/mod.rs](https://github.com/games-on-whales/gst-wayland-display/blob/HEAD/wayland-display-core/src/comp/mod.rs)
- **GeForce NOW Cloud G-SYNC** (live 2024-03-07, Ultimate tier): NVIDIA "varies the stream rate to the client, driving down total latency on Reflex-enabled games", pairs with 60/120 fps Reflex modes, and works with G-SYNC/FreeSync/VRR monitors on Windows and Mac **(vendor)**. No server-side mechanics are published. — [NVIDIA blog](https://blogs.nvidia.com/blog/CES-2024-geforce-now-activision-blizzard-day-passes-g-sync); [AnandTech](https://www.anandtech.com/show/21292/variable-refresh-rate-support-comes-to-nvidias-geforce-now-cloud-streaming-service)
- **Steam Remote Play**: no Valve documentation found. Users report the host capped to the client's refresh and capture tied to vsync (unverified). — [Steam homestream group](https://steamcommunity.com/groups/homestream/discussions/0/1368380934278899042)
- **Apollo**'s docs (last commit 2026-05-21) carry the same `minimum_fps_target` text as Sunshine, and no pacing modes were found in them. — [Apollo configuration.md](https://github.com/ClassicOldSong/Apollo/blob/HEAD/docs/configuration.md)

### Inferences
- Sunshine's measured trap applies to Cha. Removing the paced clock on an output whose refresh equals the stream rate starved clients that pace themselves to the compositor. For Cha, presentation feedback and frame callbacks must keep flowing independently of compositing, at the cap rate or faster, or Chrome-like and FIFO clients slow down.
- Vibepollo's 1000 Hz virtual display is Windows's way of getting "capture on present". Cha's equivalent is composite-on-commit inside its own compositor, which is cheaper and exact.
- The pixelflux budget (pull-forward with a refill limit) and the Sunshine budget (`max(next, t − T/4) + T`) are both directly reusable designs for Cha's composite scheduler.

### Gaps
- Nestri: no frame-timing documentation found.
- GeForce NOW and Steam Remote Play server internals: none public.
- No independent measurement of VRR-streaming latency or smoothness gains on the host side.

## 6. Practical design issues for Cha: composite-on-commit vs next tick, multiple surfaces, uncapped games, static desktops, duplicates, keep-alive, callback throttling

### Takeaway
The evidence supports this split:
- **Gaming sessions (gamescope)**: composite and encode when gamescope's toplevel commits new content. Hold to a minimum interval of 1/cap with a Sunshine/pixelflux-style budget. Send gamescope refresh 0 and `presented` at the real composite time, and run gamescope with `--adaptive-sync` and `-r` = cap.
- **Desktop/Chrome sessions**: keep the 240 Hz tick, which delivers S2's latency win through presentation feedback, and add on-commit compositing only as a latency improvement, not as VRR feedback. Chromium paces off feedback and breaks with refresh 0.

The latency gain on the game path is the removal of up to two clock waits: gamescope's `-r` wake and Cha's encode tick.

### Cited Findings
- Cha's current pipeline:
  - the encode tick composites only `if self.dirty || self.force_frame` (with listeners);
  - "Fell behind (e.g. a stall): realign instead of bursting";
  - a 4-buffer GBM pool; a newest-frame-wins mailbox per encoder;
  - the cursor is published separately (`publish_cursor`/`publish_pointer`), not composited into the picture by necessity.
  — [compositor/mod.rs](/home/user/cha-portal/crates/cha-streamer/src/compositor/mod.rs); [media.rs](/home/user/cha-portal/crates/cha-streamer/src/media.rs); [README "How it works"](/home/user/cha-portal/crates/cha-streamer/README.md)
- `tick_rate` exists to round 240 up to a multiple of the encode rate ("90 fps on a 240 Hz clock would alternate 2 and 3 ticks between frames"), giving 270 for 90 fps. — [compositor/mod.rs](/home/user/cha-portal/crates/cha-streamer/src/compositor/mod.rs)
- S2 **(single-dev, Cha's own)**:
  - Chrome reacts ~4.5 compositor frames after a click (~76 ms at 60 fps);
  - a 240 fps compositor encoding every 4th frame cut click → forwarded frame to ~24 ms, and click → browser compositor from 77–83 to 34–35 ms;
  - in P1.3, 24.6 ms "because presentation feedback follows the 240 Hz tick", and Chrome "drew at 60 fps even with 240 Hz callbacks" when feedback was at 60 Hz.
  — [docs/PLAN.md S2 / P1.3](/home/user/cha-portal/docs/PLAN.md); [README](/home/user/cha-portal/crates/cha-streamer/README.md)
- KWin's VRR model for multiple surfaces: only the repaint-driver surface (the active window's) drives VRR frames, and other items wait up to 1/30 s. — [KWin renderloop.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/core/renderloop.cpp)
- gamescope's model: overlay-only repaints are deferred to timer vblanks; cursor-only repaints are allowed below 30 fps content; the Steam UI is held at refresh with repeated frames. — [steamcompmgr.cpp](https://github.com/ValveSoftware/gamescope/blob/89b9e6ee61513767f4771e0022b669a48efb5780/src/steamcompmgr.cpp)
- In VRR mode, gamescope lets MAILBOX/IMMEDIATE games run uncapped while painting at most once per `-r` interval (`adaptive_sync_uncapped`). Steam's FPS limit paces frame callbacks to the game on a time schedule. — same file
- Sunshine's static-content rule: minimum fps = stream fps / 2 by default, with duplicates re-encoded. — [Sunshine docs](https://github.com/LizardByte/Sunshine/blob/master/docs/configuration.md)
- pixelflux: pulls forward no earlier than 0.5 period, and input refill is capped at 1/32. — [pace.rs](https://github.com/linuxserver/pixelflux/blob/HEAD/src/pace.rs)
- Mesa FIFO without fifo-v1 throttles on frame callbacks; with fifo-v1 and no timestamps, on barrier clears. — [Mesa wsi_common_wayland.c](https://gitlab.freedesktop.org/mesa/mesa/-/blob/d3c007c5dcef02dcaf0745754f21a879601047ff/src/vulkan/wsi/wsi_common_wayland.c)

### Inferences (design notes; to validate in a spike)
- **Latency budget, game path**:
  - Today: game present → gamescope `-r` wake (0–T_r, mean T_r/2) → gamescope composite and commit → Cha encode tick (0–T_e, mean T_e/2) → composite (~1.7 ms compositor → encoded measured).
  - At 60/60, the waits average ~16.7 ms and peak ~33 ms.
  - With VRR gamescope plus on-commit Cha, both waits go to ~0 while the game is below the cap; only gamescope's composite and Cha's composite+encode remain. Selkies' 11 → 2.6 ms is the same kind of gain **(single-dev)**.
- **Composite trigger**: on the driving surface's commit, if `now ≥ last_composite + min_interval`, composite now. Otherwise schedule a composite at `last_composite + min_interval`, with newest-wins if several commits arrive.
  - `min_interval` = 1/cap with Sunshine's late-frame rule (`max(next, t − T/4) + T`). That gives the stream rate on average, absorbs jitter, and never bursts. It replaces the encode tick and makes `tick_rate`'s multiple-of-fps rounding unnecessary for gaming sessions.
- **Several surfaces**:
  - Cursor: already out of band, so it never triggers a composite.
  - Inside gamescope: mangoapp and the Steam overlay are already composited into gamescope's buffer, so Cha sees one surface.
  - Desktop popups and secondary windows: mark dirty and coalesce into the next driver-triggered composite, with a ~30 Hz floor timer (KWin's 1/30 s) so they appear even when the driver is idle.
  - Picking the "driver": the focused fullscreen toplevel. In a gaming session that's always gamescope.
- **Uncapped games**: rely on Steam's per-game limit (time-based in VRR mode) or set `GAMESCOPE_FPS_LIMIT` = stream fps from Cha's X helper. Otherwise a 100 fps game feeding a 60 fps cap gets uneven newest-wins drops (judder) and wastes GPU. gamescope's `-r` = cap bounds what reaches Cha.
- **Static desktop / duplicate suppression**:
  - Composite only on damage, as today.
  - gamescope's idle hold (Steam UI) and `bRepeatFrame` produce commits without new content. Detect them through buffer damage from the commit (if gamescope sends full damage on repeats, this fails; unverified) or a cheap content hash of a downscaled frame, and skip the encode.
  - Keep a keep-alive re-encode of the last frame at a low rate (Sunshine uses fps/2; 5–10 Hz may be enough for WebRTC; to measure against the client's jitter buffer and VRR LFC, which are researched separately) so static scenes refine and clients don't stall.
- **Frame callbacks and presentation feedback (avoid spinning)**:
  - *gamescope*: it ignores callbacks. Send it `presented` right after its buffer is composited, with refresh 0 via `Refresh::Variable(1/cap)` (v1 → 0) and `Kind::Vsync` (or empty flags). Send `discarded` for buffers replaced before compositing.
  - *Desktop / Chrome*: keep feedback and callbacks on the 240 Hz tick with `Fixed(1/240)`. S2's latency win depends on it, and refresh 0 risks Chrome's 60 fps fallback (KDE 478938). If a uniform policy is wanted, `Variable(1/240)` gives v2 binders a non-zero 240 Hz, but whether Chrome binds v2 is unverified.
  - *Direct Vulkan or native clients*: if Cha exposes fifo-v1, signal barriers when a composite consumes the surface, and on a fallback timer at the cap rate so occluded or uncomposited surfaces make progress. Don't expose commit-timing until it is wired to the composite scheduler (cosmic's 1 ms-margin lessons; NVIDIA quirk reports).
  - *Frame callback throttling*: throttle each surface to ≤ 1 per composite opportunity (cap rate) in gaming sessions; keep 240 Hz in desktop sessions. Never send callbacks faster than the tick, so MAILBOX-like or callback-paced apps can't spin.
- **Live fps change for gamescope**: with `-r` = 120 and VRR on, the page could change Cha's cap (60/90/120) without restarting gamescope, because `-r` is only an upper bound in VRR mode. This removes the README's "a gamescope app can't change its refresh while running" limitation, but needs a spike.
- **Encoder side**: keep `frameRateNum` and the one-frame VBV at the cap (§4), compute RFI by index, fix the VUI timing flag, and let PyroWave's per-frame budget stand. Rate control's "fixed in time" rules already fit VFR. Where the README calls a rule "per frame", recheck it as "per frame *or* per T_cap".

### Gaps
- Not measured on Cha:
  - whether gamescope (Ubuntu 26.04 build) actually enters VRR mode under Smithay's v1-zero refresh;
  - whether its repeated frames carry damage;
  - the end-to-end latency and smoothness change;
  - the effect on the 4-buffer pool when composites follow commits instead of ticks.
- Not found:
  - which wp_presentation version Chrome binds, and its behaviour with `Variable` refresh;
  - how WebRTC receivers (Chrome's jitter buffer, Safari) handle long gaps between frames on the node's side of the contract (client side researched separately).
