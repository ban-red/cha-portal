# One UI spec for the browser and native players

The browser player (`web/apps/portal`, `@cha/player`) and the native player (`crates/cha-player`) show the same in-stream UI: the stats panel, its health grade and the toolbar. Today only the theme colours are shared (generated from the portal's CSS, checked by `check.sh`). Everything else was ported by hand and drifts the first time one side changes:

| Piece | Browser | Native | Shared |
|---|---|---|---|
| Theme colours | `src/themes/*.css` | `web/packages/ui-spec/themes/*.json` | generated, checked |
| Health grade: thresholds, weights, issue texts | `health.ts` (545 lines) | `health.rs` (1,188 lines) | no |
| Stats panel: sections, rows, labels, tooltips, formatting | `StatsOverlay.vue` (515) | `ui/overlay.rs` (1,721) | no |
| Panel and toolbar prefs: fields, defaults, limits | `statsOverlay.ts`, `toolbarPrefs.ts` | `overlay_prefs.rs`, toolbar prefs | no |
| Toolbar: controls, tooltips, when each shows | `SessionView.vue` | `ui/toolbar` (in progress) | no |
| Icons | inline SVG | strokes drawn by hand | no |

Vue and egui can't share layout code, and they shouldn't try. They can share everything that isn't drawing: the data, the words, the structure, the icons and, through shared test cases, the behaviour.

## The rule

**The spec is the source of truth; each renderer is thin.** A change to what the UI says or judges goes into the spec first, and both players pick it up. Logic stays as code on each side, but shared test cases fail CI the moment the two disagree. Pixel layout, animation and input handling stay native to each side.

Not chosen:
- Compiling the Rust health logic to WebAssembly for the browser. It would leave one implementation, but it adds a wasm build to the bun/Vite pipeline and a JS↔Rust bridge on every stats update. Shared cases give the same guarantee for less.
- A cross-platform UI toolkit or server-driven layout. Too much machinery for two small surfaces, and it would fight both Vue and egui.

## Layout

- **`web/packages/ui-spec`** (`@cha/ui-spec`, a bun workspace package): the JSON files below, plus a thin TypeScript module giving them types, and the small helpers both sides mirror (`fill(template, values)`, formatters).
- **`crates/cha-ui-spec`**: `include_str!`s the same JSON, deserialises it into typed structs once (`LazyLock`), and holds the Rust side of the helpers. `cha-player` depends on it. It's a plain library with no egui, so it builds and tests on Linux CI.
- The generated themes move from `crates/cha-player/themes/` to `web/packages/ui-spec/themes/`. The exporter writes there, and `cha-player` reads them through `cha-ui-spec`. Their source stays the portal's CSS.

Files:

| File | Holds |
|---|---|
| `icons.json` | Every icon as 16×16 SVG path data (`d`, stroke or fill, width). Circles and rects are written as paths. |
| `health.json` | Constants (window, spike share, levels, severity caps, grade floors, weights), each check's bands, and each issue's `id`, `title`, `detail` and `hint` templates (`"{shown} of {sent} fps"`), with per-platform overrides where the words differ (`"browser"` vs `"Mac"`), and `platforms` for checks only one side can measure. |
| `health-cases.json` | Test vectors: a history of neutral snapshots, the platform, and the expected grade, score, summary and issue ids with their severities. |
| `stats-panel.json` | The sections (id, heading, colour role) and their rows: `id`, `label`, `tooltip` (per platform), the value key, the formatter, which issue ids colour it, a `hot` limit for node values, `platforms`, and what a folded section summarises. Also the compact line and the copy report's lines. |
| `format-cases.json` | Inputs and expected strings for `num`, `gb`, `pct`, `codecTag`, ms and Mbit/s formatting. |
| `prefs.json` | The stats panel's and the toolbar's prefs: each field's type, default, limits or allowed values. |
| `prefs-cases.json` | Raw saved values and what each side's parser must make of them (bad fields fall back to defaults; good ones survive). |
| `toolbar.json` | The controls in order and their groups: `id`, `icon`, `label`, `tooltip` (with states, e.g. muted/unmuted), the menu it opens, and `needs`, the capabilities a session must report for it to show (`fps-change`, `steam-overlay`, `share`, `control-handoff`, `sound-restart`, `controllers`, …). |

**A neutral snapshot.** `health-cases.json` and the panel's value keys use one snapshot shape that both players can fill: the browser's fields, with every field optional and named for what it measures (`shown_fps`, `sent_fps`, `latency_ms` with `latency_from: "send" | "receive"`, …). Each side has one small `toSpecSnapshot` mapping. That mapping is the only place a platform's measurements are translated.

## Drift guards (all in `check.sh`)

- **`bun scripts/check-ui-spec.ts`** validates the spec itself:
  - ids are unique;
  - every icon, issue id, value key and colour role that is referenced exists (roles from `themes/index.ts`);
  - every template's placeholders are values that the check provides;
  - every `platforms` value is `web` or `native`.
- **Both sides run the shared cases**: `health-cases`, `format-cases` and `prefs-cases` run under `bun test` against `health.ts`, the formatters and the prefs parsers, and under `cargo test -p cha-ui-spec -p cha-player` against the Rust ones. A case that only one platform runs says so.
- **Coverage tests on each side**:
  - every spec issue for that platform has a check implemented, and no check exists outside the spec;
  - the side fills every value key its panel rows use;
  - every toolbar control for that platform is rendered, or is gated by a capability the side declares it lacks.
- The themes keep their freshness check, with the new path.

## Phases

Each phase lands both sides together, with its cases, and leaves both players working.

0. **Decision.** [ADR 0016](../adr/0016-one-ui-spec-for-both-players.md): the rule, the layout and the drift guards above. Done.
1. **Package, crate and icons.** Done: `@cha/ui-spec` and `cha-ui-spec` exist, the themes moved, 24 icons are in `icons.json` and drawn by `Icon.vue` (StatsOverlay and SessionView) and `theme/icons.rs` (an SVG path parser with a build-time `Icon` enum), with `scripts/check-ui-spec.ts` and an icon-sheet snapshot.
   - Create `@cha/ui-spec` and `cha-ui-spec` (`cargo hakari generate && cargo hakari manage-deps`).
   - Move the themes there, and point the exporter and `cha-player` at the new path.
   - `icons.json` from the SVGs in `StatsOverlay.vue` and `SessionView.vue`.
   - Browser: an `Icon.vue` component.
   - Native: `theme/icons.rs`, a small SVG path parser (M L H V C Q A Z, absolute and relative) that strokes or fills with egui's painter, plus snapshot renders of every icon at 1× and 2×.
   - The stats panel's icons switch to it on both sides.
   - This is the smallest phase and it proves the plumbing.
2. **Health.** Done: `health.json` holds every constant, band, weight and issue text (templates with `{name:format}` placeholders and per-platform wording), `health-cases.json` holds 89 shared cases (70 for both players, 17 browser-only, 2 native-only) and `fill-cases.json` the template cases; `health.ts` and `health.rs` keep only their checks, keyed by issue id, and both run the cases and a coverage test. `check-ui-spec.ts` validates the issues, placeholders and cases, and `check.sh` now runs the player package's tests. One disagreement found and fixed: Rust rounded a half to even in detail texts where the browser rounds it up.
   - Move the constants, bands, weights and issue texts into `health.json`.
   - Turn `health.test.ts`'s cases into `health-cases.json`; web-only checks (`sound-restart`, `sound-out`, `audio`, delivery spread) get browser-only cases, and native-only ones (`awdl`) get native-only cases.
   - `health.ts` and `health.rs` read the spec and keep their check functions.
   - Add the coverage tests.
   - Highest value: this is where drift does real harm (two players grading the same stream differently).
3. **Stats panel.** Done: `stats-panel.json` holds the sections, rows (labels, tooltips per platform, value templates, which issues colour them, hot limits, platforms, `when`), folded summaries, the compact line and the copy report; `format-cases.json` and `stats-panel-cases.json` (32 cases, run on both platforms by both sides) pin the formatters and the model. `buildPanel` (TypeScript) and `build_panel` (`cha-ui-spec`) are the one pure model; `StatsOverlay.vue` loops over it and the native panel is split into `ui/stats/{mod,header,sections,settings,placement,report,...}.rs`, which only draw it. `check-ui-spec.ts` validates ids, value keys, issue ids, platforms, section ids and colour roles. The browser's HTML is unchanged apart from the rows the browser now has too (Reconnects, and Recovered over WebTransport).
   - Write `stats-panel.json` and `format-cases.json`.
   - Browser: `StatsOverlay.vue`'s four nearly identical `<dl>` blocks become one loop over the spec's sections, and the copy report is built from the spec's lines.
   - Native: split `ui/overlay.rs` into `ui/stats/{mod, header, sections, settings, placement, report}.rs`, with the sections drawn from the spec.
   - Both read prefs through phase 4's parsers, or keep theirs until then.
4. **Prefs.** Done: `prefs.json` holds both groups' fields (`stats_panel`, `toolbar`: type, default or optional, limits, rounding and clamping, platforms, a doc line) and `prefs-cases.json` 96 shared cases (94 run on both platforms, 2 browser-only) with the defaults pinned in the file. One generic validator per side (`prefs.ts`, `cha-ui-spec`'s `prefs.rs`) replaces the four hand-written parsers; `statsOverlay.ts`, `toolbarPrefs.ts`, `overlay_prefs.rs` and `stream_prefs.rs` no longer hold a default, limit or id list, and `config.json` loads whatever it holds. `check-ui-spec.ts` validates the schema, the cases and that the section ids agree with the stats panel's. Disagreements settled in the browser's favour: one bad field in the panel's prefs used to lose them all natively, a volume out of range or fractional was dropped natively, an unknown folded section failed the load.
   - `prefs.json` and `prefs-cases.json`.
   - The parsers in `statsOverlay.ts`, `toolbarPrefs.ts`, `overlay_prefs.rs` and the native toolbar prefs read their defaults and limits from the spec and pass the cases.
5. **Toolbar.** Done: `toolbar.json` holds the controls in order and their groups (left: back, power off, share, name, GPU badge, viewers, take back, the watchers' Hand controls; right: stream settings, exclusive input, mouse, sound, controllers, full screen, stats), each with its `kind`, icon, `label`, `aria_label`, `tooltip`, platforms, the capabilities it `needs` and per-state `states` (a variant lays icon, words, tone or disabled over the control while its conditions hold), the three menus and their rows (choices, buttons, the volume slider, the controller list, notes), the power-off countdown's words and length, the folded bar's and the hide tab's labels, the visibility rules and the fold timing. It declares the capability vocabulary (12, each with a doc line), what each side can ever report (`reports`) and the neutral state the model reads (`state`). `buildToolbar` and `build_toolbar` return the controls to draw with everything resolved, and `toolbar-cases.json` has 48 cases (12 run on both players, 20 on the browser only, 16 on the native one only: 60 runs). `check-ui-spec.ts` checks the ids, icons, capabilities, state keys, conditions, templates, platforms and that every control, row and capability has a case.
   - Browser: `SessionView.vue` (1,045 lines, from 1,220) keeps the player, the state and the handlers; the toolbar's markup is `components/SessionToolbar.vue` with `StreamMenu.vue`, `SoundMenu.vue` and `ControllerMenu.vue` and draws `buildToolbar`'s controls. The SSR output of the old and new views was compared in 33 states: identical but for the order of the custom-overlay option (fixed in the spec) and edge whitespace inside a few buttons.
   - Native: `ui/toolbar.rs` draws `build_toolbar`; the session's capabilities come from `TransportStats` (`toolbar::capabilities`) without a new method on `SessionControl`, because everything the toolbar needs is already there and the wrappers (`QuitsEnvironment`) would each have to forward another call. `ui/chrome.rs` holds what the panel and the toolbar share: the look, the lit background and icon button, the menu box and widget style, the chips and the pointer-routing rule (`Press`).
   - Disagreements settled in the browser's favour: the stream settings tooltip, the mouse tooltip (the keyboard still works), Back's tooltip, the sound tooltip carries the volume, the controllers button counts them, no footnote under the controller list, no frame rate row until the host reports one (it showed a dash), and the frame rate chips stay up when the host runs a rate outside 60, 90 and 120.
   - A launcher extra: `scripts/export-brand.sh` also writes `brand/logo-64.rgba` (64 x 64 raw RGBA), which the native launcher shows before "Cha Player".
6. **Docs.** *Done: pointers in AGENTS.md, both players' READMEs and `docs/PLAN.md`.*
   - Add a short "Changing the in-stream UI" section in `web/packages/ui-spec/README.md`: edit the spec, run the cases on both sides, update both renderers if the structure changed.
   - Add a pointer to it in `AGENTS.md` and both players' READMEs.
   - Update `docs/PLAN.md` status lines.

## Sequencing with other work

- The native toolbar (in progress) lands first, as a hand port. Phase 5 then moves both toolbars onto the spec.
- Share links are being built in `web/` and `crates/cha-control`, and they touch `SessionView.vue` (the Share dialog). Phases 3 and 5 edit the same files, so they start after share links are committed.
- Phases 1, 2 and 4 touch `@cha/player` and `StatsOverlay.vue`, but not the share-link files, so they can start now.

## Risks

- **Over-specifying.** The spec holds data, words and structure, never pixel layout or logic. If a field needs an expression language to describe, it belongs in code with a shared case instead.
- **Templates.** Detail strings are templates with named placeholders. The spec check catches a placeholder a check doesn't fill; the cases catch formatting differences.
- **The browser's measurements are richer.** Native skips some checks. The `platforms` field and the coverage tests make that explicit instead of silent.
