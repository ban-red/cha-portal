# 0016: One UI spec for the browser and native players

- **Status:** accepted (2026-10-08).
- **Context:**
  - The browser player and Cha Player show the same in-stream UI: the stats panel, its health grade and the toolbar. The browser's is Vue and TypeScript (`StatsOverlay.vue`, `health.ts`, `SessionView.vue`); the native one is egui (`crates/cha-player`).
  - Only the theme colours are shared: they're generated from the portal's CSS, and `check.sh` fails when they're stale ([0005](0005-theme-token-contract.md)). The health grade, the panel's rows and tooltips, the saved settings, the toolbar's controls and the icons were all ported by hand. A threshold or a hint changed on one side leaves the two players disagreeing about the same stream, and nothing notices.
  - Vue and egui can't share layout code.
- **Decision:**
  - **A spec package is the source of truth; each renderer is thin.** `web/packages/ui-spec` (`@cha/ui-spec`) holds JSON for the icons (SVG path data), the health grade (constants, bands, weights, issue texts as templates with per-platform wording), the stats panel (sections, rows, labels, tooltips, formatters, which issues colour a value), the saved settings (fields, defaults, limits), the toolbar (controls, tooltips, the capabilities each needs) and the generated themes. TypeScript imports it; `crates/cha-ui-spec` compiles the same files in with `include_str!` and gives them types.
  - **Logic stays as code on each side; shared test cases keep it equal.** Sample histories with the expected grade, score and issues, formatter cases and settings-parsing cases run under both `bun test` and `cargo test`. A case that only one platform runs says so.
  - **Drift fails CI.** `scripts/check-ui-spec.ts` checks the spec's ids, references, template placeholders and platforms. Coverage tests on each side check that every spec issue has a check, every panel value is filled, and every toolbar control is drawn or gated by a capability that side lacks. All of it runs in `check.sh`.
  - **Layout, animation and input stay native.** The spec holds data, words and structure, never pixel positions or an expression language.
  - **To change the in-stream UI:** edit the spec, make the shared cases pass on both sides, and update both renderers if the structure changed.
  - The rollout is phased in [`docs/plans/ui-spec.md`](../plans/ui-spec.md).
- **Alternatives rejected:**
  - *The Rust UI in the browser (egui compiled to WebAssembly).* One renderer, and cheap to draw, but it's a canvas: screen readers, aria labels, browser zoom and text selection are lost, against 0005's accessibility rules. It also adds a 1–3 MB download and fonts we'd have to ship.
  - *The Vue UI in Cha Player (a transparent WebKit view).* One renderer, but a WebKit process costs roughly 50–100 MB. A see-through view over the full-screen Metal layer can stop macOS presenting the video straight to the display, which can add a frame of latency. Input would also have to be split between the web view and the stream.
  - *One Rust framework for both (Dioxus with Blitz, Slint).* It means rewriting the portal's in-stream UI out of Vue and Tailwind, the portal's stack, and Blitz is experimental.
  - *The Rust health logic compiled to WebAssembly for the browser.* One implementation, but it adds a wasm build to the bun/Vite pipeline and a JS↔Rust call on every stats update. Shared cases give the same guarantee for less.
  - *Hand ports with care.* They're what we have, and they drift.
