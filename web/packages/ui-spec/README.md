# @cha/ui-spec

The in-stream UI's spec, shared by the browser player (`web/apps/portal`) and Cha Player (`crates/cha-player`). [ADR 0016](../../../docs/adr/0016-one-ui-spec-for-both-players.md) has the decision and [`docs/plans/ui-spec.md`](../../../docs/plans/ui-spec.md) the rollout.

| File | Holds |
|---|---|
| `icons.json` | Every icon as 16 x 16 SVG path data. Each part is stroked (`stroke`: width in units) or filled (`fill: true`). Circles and rects are written as paths. `linecap` and `linejoin` apply to the whole icon. |
| `health.json` | The health grade's data: constants, bands, weights, summary words, and each issue's `id`, `platforms`, `title`, `hint` and `detail` templates. |
| `health-cases.json` | Shared test vectors for the grade: histories of neutral snapshots and the grade, score, summary, issues and exact texts both players must produce. |
| `health-cases.ts` | Types for the cases and the neutral snapshot, and the history expander the TypeScript runner uses (`@cha/ui-spec/health-cases`, kept out of the portal's bundle). |
| `fill-cases.json` | Template cases (`fill` and the number formatters), run on both sides. |
| `themes/*.json` | The built-in colour themes, generated from the portal's CSS by `scripts/export-player-themes.ts`; never edit them by hand. |
| `stats-panel.json` | The stats panel: value keys, sections, rows (labels, tooltips, value templates, colouring), folded summaries, the compact line and the copy report. |
| `stats-panel-cases.json` | Shared cases for the panel model: values and health in, the rows, summaries, compact line and report out, per platform. |
| `format-cases.json` | The panel's number formatters (`fill` with "–" for a missing value) and the codec tag, run on both sides. |
| `panel.ts`, `panel-cases.ts` | `buildPanel`, `panelReport`, `fillPanel` and the spec's types; and the cases' types and helper (`@cha/ui-spec/panel-cases`, kept out of the portal's bundle). |
| `prefs.json` | The saved settings: the stats panel's and the toolbar's fields, each with its type, default (or `optional`), limits, rounding, platforms and a doc line. |
| `prefs-cases.json` | Shared cases for the saved settings: the defaults, then raw saved values and what each side must hold after parsing them. |
| `prefs.ts`, `prefs-cases.ts` | The schema's types, the validator (`parsePrefsGroup`, `parsePrefsText`); and the cases' types and helper (`@cha/ui-spec/prefs-cases`). |
| `index.ts` | Types for the JSON, `ICONS`, `HEALTH`, `fill` and the formatters. |

TypeScript imports this package (`import { ICONS, HEALTH, fill, buildPanel } from "@cha/ui-spec"`); `crates/cha-ui-spec` compiles the same files in with `include_str!` and gives them types (`cha_ui_spec::health::spec()`, parsed once), and its `build.rs` turns the icon ids into a Rust enum, so a misspelt id is a compile error.

## Changing the in-stream UI

1. **Edit the spec first.** An icon, a label or a colour change goes into the JSON here, not into a renderer. For themes, change the portal's CSS and run `bun scripts/export-player-themes.ts`.
2. **Use ids, not copies.** The browser draws icons with `<Icon name="copy" />` (`web/apps/portal/src/components/Icon.vue`); the native player with `icons::paint(painter, rect, Icon::Copy, color)` (`crates/cha-player/src/theme/icons.rs`). Don't paste SVG or hand-draw strokes.
3. **Run the cases on both sides** (`cargo test -p cha-ui-spec -p cha-player`, `bun run --cwd web/packages/player test`, `bun run --cwd web/apps/portal test`) and `bun scripts/check-ui-spec.ts`, which validates the spec and checks every `<Icon name>` exists. `scripts/check.sh` runs all of it.
4. **Update both renderers if the structure changed**: a new control, row or icon use goes into the Vue component and the egui code in the same change. Layout, animation and input stay native to each side; the spec holds data, words and structure.
5. **Look at it.** `CHA_SNAPSHOT_DIR=<dir> cargo test -p cha-player --release snapshot -- --ignored --nocapture` writes an icon sheet and the toolbar and stats panel; compare with the browser.

### Adding an icon

Draw it in a 16 x 16 box with absolute or relative path data (`M L H V C S Q T A Z`, any case). Prefer a stroke of 1.4 to 1.75 units with round caps and joins. Give it a kebab-case id that says what it shows (`sound-muted`, `fullscreen-exit`). A state is a second icon, not a flag.

## The health grade

`health.json` holds everything the grade says or weighs; `web/packages/player/src/health.ts` and `crates/cha-player/src/health.rs` hold only the checks (the logic, keyed by issue id), which read it. Neither file has a threshold or a user-visible string.

- **Constants.** `window`, `min_snapshots`, `fallback_fps`, `levels` (how a value becomes a 0..1 level, and the major/critical cut-offs), `severity_cap`, `grades` (score floors), `text` (the hidden, measuring and smooth lines), `params` (the other numbers the checks use) and `bands`. A band is `{ from, to }`: the first value that counts and the value that is as bad as it gets. `notes` says why the numbers are what they are; code doesn't read it.
- **Issues.** An issue has `id`, `platforms` (`["web", "native"]`, `["web"]` or `["native"]`), `title`, `summary` (the one-line words while it is the worst), `weight`, `hint`, `details` (templates by variant; `main` unless a check words one finding several ways) and `placeholders`. `title`, `hint` and each detail are a plain string when both platforms agree, or `{ "web": "...", "native": "..." }` where the words differ ("browser" and "Mac", sent -> shown and received -> shown). A detail variant only one platform measures names only that platform.
- **Templates.** A detail is a string with named placeholders, `{name}` for a string and `{name:format}` for a number, filled with `fill(template, values)` (TypeScript, from `@cha/ui-spec`) and `cha_ui_spec::health::fill(template, &[("name", Val)])` (Rust). The formatters are `f0`, `f1`, `f2` (that many decimals) and `ms0`, `ms1` (the same, then " ms"). A number with no formatter, a string with one, an unknown name or a missing value is an error. Both sides round a half up, as `toFixed` does (Rust's `{:.1}` rounds it to even, so `fixed` in `cha-ui-spec` does the work). `placeholders` maps every name an issue's templates use to `"number"` or `"string"`, so `bun scripts/check-ui-spec.ts` can tell a misspelt name from a real one.
- **The neutral snapshot.** A case's history is made of neutral snapshots: every field optional (missing or null means the player couldn't measure it) and named for what it measures, in `health-cases.ts`. Counters (`lost`, `recovered`, `partial`, `dropped`) are cumulative since the session began; `latency_ms` is send -> shown on the web and received -> shown natively; `delivery_*`, `jitter_buffer_ms`, `audio_*` are the browser's; `awdl_suspected` is native. Each player maps it to its own snapshot in its tests (`fromSpecSnapshot` in `health.test.ts`, `from_snapshot` in `health.rs`'s tests); that is the only place a platform's measurements are translated. Live code doesn't convert anything.
- **Which cases run where.** A case with no `platforms` runs on both players, so its `expect` must be what both produce: where the words differ, write `{ "web": "...", "native": "..." }` for that issue's detail or hint. A case with `"platforms": ["web"]` or `["native"]` runs on that player only; use it for a signal the other can't measure, a hidden page or another interval (`context`).

### Adding an issue

1. Add it to `issues` in `health.json` (after the issues it should lose ties to; order breaks ties between equal ones): id, platforms, title, summary, weight, hint, details, placeholders. A band or param it needs goes in `bands` or `params`, and the typed shapes in `index.ts` (`HealthSpec`) and `crates/cha-ui-spec/src/health.rs` (`Bands`, `Params`).
2. Write the check on each platform in the spec: an entry in `CHECKS` in `health.ts` and in `CHECKS` in `health.rs`, keyed by the id. It returns the window's level and the filled detail; titles, hints and weights come from the spec. The coverage tests fail until the spec and the checks list the same ids.
3. Add cases (below) that show it appear, show it stay quiet, and pin its detail and hint text.
4. Run `bun scripts/check-ui-spec.ts`, `bun run --cwd web/packages/player test` and `cargo test -p cha-ui-spec -p cha-player`.

### Adding a case

Add an object to `cases` in `health-cases.json`:

```json
{
  "name": "latency of 60 ms",
  "history": [{ "repeat": 8, "latency_ms": 60 }],
  "expect": {
    "grade": "C", "score": 79, "summary": "High latency",
    "issues": [{ "id": "latency", "severity": "major" }],
    "details": { "latency": { "web": "60 ms from sent to shown", "native": "60 ms from received to shown" } }
  }
}
```

- `history` is a list of entries, oldest first. Each entry is the file's `base` (a healthy 60 fps LAN stream) with its fields set, `repeat` times (default 1). A field may be an array of `repeat` values for one per snapshot (`"lost": [0, 3, 6, 9, 12, 15, 18, 21]`). `node` merges into the base's node field by field (`null` removes one); `"node": null` is no report. `"NaN"` stands for a NaN.
- `expect` lists the issues exactly, most severe first. `details` and `hints` give exact strings for the issues named; pin a few in every case that has issues so wording drift is caught.
- Work out the expectation by reasoning, then run both runners; if one disagrees, that is the point of the case. Fix the check (or the spec), not the expectation, unless the expectation was wrong.

## The stats panel

`stats-panel.json` says what the panel shows; `buildPanel(spec, values, health, platform)` (`panel.ts`) and `build_panel` (`crates/cha-ui-spec/src/panel.rs`) turn what a player measured into the sections, rows, summaries, compact line and copy report. The renderers draw only what that returns: `StatsOverlay.vue` loops over `panel.sections` and `section.rows`, and `crates/cha-player/src/ui/stats/sections.rs` does the same in egui. Both players' mappings from their own snapshot to value keys are the only translation (`web/apps/portal/src/statsValues.ts`, `StatsSnapshot::values` in `ui/stats/snapshot.rs`).

- **Value keys** are snake_case, declared in `values` with a `kind` (`number` or `string`) and the platforms that fill them. A key a player doesn't have, can't measure yet or got NaN for is left out: its template reads "–" (with its unit: "– ms") and a `when` that needs it hides the row. Names match the neutral snapshot in `health-cases.ts` where the meaning is the same (`shown_fps`, `latency_ms`, `lost`, ...); the node's are `node_cpu`, `node_mem_used`, and so on.
- **Templates** are `fill`'s `{name:formatter}`, with `f0 f1 f2 ms0 ms1` and the panel's `int` (no digit grouping), `gb` (bytes as GiB, one decimal), `pct` (whole number and "%"), `mbit` (one decimal and " Mbit/s") and `s` (the plural "s", for `reconnect{n:s}`). A string takes none. Text that differs by platform is `{ "web": "...", "native": "..." }`, as in the health spec.
- **A row** has `id`, `label`, `tooltip`, `value` (a template, or a list of pieces: `{ text, when?, unless?, hot?, tone? }`), `bad` (issue ids that colour it, the first listed issue wins; a list or per platform), `platforms` and `when` (value keys that must be present, a list or per platform). A piece is `hot` at or over `limit`, in the value or in percent of the key `of`; `tone: "dim"` is the secondary ink. Neighbouring pieces of one tone become one segment.
- **A section** has `id` (it must be in `SECTIONS` in `statsOverlay.ts` and `Section` in `overlay_prefs.rs`), `heading`, `color` (a theme role: `accent`, `chart-1`..`chart-3`), optional `when`, and a `summary` (`parts` joined with " · ", plus `bad` or `hot` for its colour) shown when it is folded.
- **The report** is `lines` (a `label` and `parts` joined with ", ", dropped when empty), the `issue` line template and the `agent` line (`Browser: {agent}` or `Player: {agent}`; the caller passes the user agent or "Cha Player 0.1.0, macOS 15.5").

### Adding a row

1. If it needs a new number, declare the key in `values` (kind, platforms) and fill it in both mappings (`statsValues.ts`, `snapshot.rs`); each side's coverage test fails until the filled keys and the spec's agree.
2. Add the row to its section in `stats-panel.json`: id (kebab-case, unique), label, tooltip (per platform where the words differ), value, `bad`, `platforms`, `when`. A row only one player measures says so in `platforms`; the tooltip says what it measures.
3. Add or extend a case in `stats-panel-cases.json` that shows it, hides it and colours it. Work the expected text out by hand, or run a scratch script over `buildPanel`, then read it before pasting. Run `bun scripts/check-ui-spec.ts`, `bun run --cwd web/apps/portal test` and `cargo test -p cha-ui-spec -p cha-player`; the two runners must agree.
4. Nothing else: the Vue loop and the egui drawing pick the row up.

### Adding a section

As a row, plus the section: add its id to `SECTIONS` (`statsOverlay.ts`) and `Section` (`overlay_prefs.rs`, `section_of` in `ui/stats/sections.rs`) so it can be folded and saved, and a class for its colour in `COLOR_TEXT` in `StatsOverlay.vue` if it uses a new role.

## Saved settings

`prefs.json` says what each player remembers; one small validator per side (`parsePrefsText` in `prefs.ts`, `cha_ui_spec::prefs::parse` in `crates/cha-ui-spec/src/prefs.rs`) turns whatever was saved into the fields, so neither side has a default, limit or id list of its own. The browser keeps `stats_panel` under `cha.statsOverlay` and `toolbar` per user and template in localStorage; Cha Player keeps them in `config.json` (`overlay`, and `toolbar` per transport, host and app).

- **A field** has `type` (`bool`, `int`, `number`, `string`, `enum` with `values`, `list` of the enum's `values`, or `object` with `fields`), `doc`, and either a `default` or `"optional": true` (absent until picked; an object may default to `null`). `min` and `max` bound an int or number; `round` lets an int take a fraction (a half rounds up); `clamp` moves a value outside the limits to the nearest one instead of dropping it; `nonempty` is for a string; `platforms` names a side that keeps the field alone (`codec` and `transport` are the browser's).
- **Parsing.** A field that is missing or invalid falls back on its own, to its default or to absent, so a hand edit never loses the rest. Text that does not parse, or is not an object, gives the defaults. Unknown fields are ignored. A list keeps its entries that are among `values`, once each, in the order of `values`. An object is valid only when all its fields are.
- **The renderers read it.** `statsOverlay.ts` takes `CORNERS`, `SECTIONS` and `OPACITY_MIN` from the `stats_panel` fields; `overlay_prefs.rs` and `stream_prefs.rs` build their structs from the validator's output (`from_json`), and `build.rs` turns int limits into constants (`cha_ui_spec::prefs::limits`) where a Rust range needs one. The section ids are the stats panel's: `check-ui-spec.ts` fails when `stats-panel.json` and `folded.values` differ.

### Adding a field

1. Add it to its group in `prefs.json`: type, `default` or `optional`, limits, `platforms`, `doc`.
2. Add it to the typed struct on each side that keeps it: `OverlayPrefs`/`ToolbarPrefs` in `statsOverlay.ts`/`toolbarPrefs.ts`, and `OverlayPrefs`/`StreamPrefs` in Rust (a field the struct lacks makes `from_json` panic in its tests). A new section or corner also goes in the Rust enums (a test checks them against the spec).
3. Add the default to `defaults` in `prefs-cases.json` (the cases pin it, so changing a default in `prefs.json` fails them), and cases that show the field accepted and rejected, at its limits and, for a list or object, malformed. `expect` holds only the fields that differ from the defaults. Mark a case `platforms` when only one side can run it.
4. Run `bun scripts/check-ui-spec.ts`, `bun run --cwd web/apps/portal test` and `cargo test -p cha-ui-spec -p cha-player`. The script checks every default against its limits, every case's `expect` against the schema, and that each field has a case that keeps it and one that falls back.

## The logo

`brand/logo.png` is the master logo: 1024×1024, transparent, cut out of the original artwork's dark background. Every other logo file is made from it by `sh scripts/export-brand.sh` (ImageMagick, and `iconutil` on macOS):

| File | Use |
|---|---|
| `web/apps/portal/public/favicon.png`, `favicon.ico` | The portal's tab icon (every theme's `favicon`) |
| `web/apps/portal/public/apple-touch-icon.png` | Home-screen icon, on the logo's own dark tile |
| `web/apps/portal/public/logo.png` | `BrandMark.vue`, beside the product name |
| `crates/cha-player/macos/AppIcon.icns` | Cha Player's app icon, on Apple's 824-in-1024 rounded tile |

To change the logo, replace `brand/logo.png`, run the script and commit what it writes.
