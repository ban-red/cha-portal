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
| `index.ts` | Types for the JSON, `ICONS`, `HEALTH`, and `fill`. |

TypeScript imports this package (`import { ICONS, HEALTH, fill } from "@cha/ui-spec"`); `crates/cha-ui-spec` compiles the same files in with `include_str!` and gives them types (`cha_ui_spec::health::spec()`, parsed once), and its `build.rs` turns the icon ids into a Rust enum, so a misspelt id is a compile error.

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

## The logo

`brand/logo.png` is the master logo: 1024×1024, transparent, cut out of the original artwork's dark background. Every other logo file is made from it by `sh scripts/export-brand.sh` (ImageMagick, and `iconutil` on macOS):

| File | Use |
|---|---|
| `web/apps/portal/public/favicon.png`, `favicon.ico` | The portal's tab icon (every theme's `favicon`) |
| `web/apps/portal/public/apple-touch-icon.png` | Home-screen icon, on the logo's own dark tile |
| `web/apps/portal/public/logo.png` | `BrandMark.vue`, beside the product name |
| `crates/cha-player/macos/AppIcon.icns` | Cha Player's app icon, on Apple's 824-in-1024 rounded tile |

To change the logo, replace `brand/logo.png`, run the script and commit what it writes.
