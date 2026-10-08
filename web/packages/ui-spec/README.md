# @cha/ui-spec

The in-stream UI's spec, shared by the browser player (`web/apps/portal`) and Cha Player (`crates/cha-player`). [ADR 0016](../../../docs/adr/0016-one-ui-spec-for-both-players.md) has the decision and [`docs/plans/ui-spec.md`](../../../docs/plans/ui-spec.md) the rollout.

| File | Holds |
|---|---|
| `icons.json` | Every icon as 16 x 16 SVG path data. Each part is stroked (`stroke`: width in units) or filled (`fill: true`). Circles and rects are written as paths. `linecap` and `linejoin` apply to the whole icon. |
| `themes/*.json` | The built-in colour themes, generated from the portal's CSS by `scripts/export-player-themes.ts`; never edit them by hand. |
| `index.ts` | Types for the JSON, and `ICONS`. |

TypeScript imports this package (`import { ICONS } from "@cha/ui-spec"`); `crates/cha-ui-spec` compiles the same files in with `include_str!` and gives them types, and its `build.rs` turns the icon ids into a Rust enum, so a misspelt id is a compile error.

## Changing the in-stream UI

1. **Edit the spec first.** An icon, a label or a colour change goes into the JSON here, not into a renderer. For themes, change the portal's CSS and run `bun scripts/export-player-themes.ts`.
2. **Use ids, not copies.** The browser draws icons with `<Icon name="copy" />` (`web/apps/portal/src/components/Icon.vue`); the native player with `icons::paint(painter, rect, Icon::Copy, color)` (`crates/cha-player/src/theme/icons.rs`). Don't paste SVG or hand-draw strokes.
3. **Run the cases on both sides** (`cargo test -p cha-ui-spec -p cha-player`, `bun run --cwd web/apps/portal test`) and `bun scripts/check-ui-spec.ts`, which validates the spec and checks every `<Icon name>` exists. `scripts/check.sh` runs all of it.
4. **Update both renderers if the structure changed**: a new control, row or icon use goes into the Vue component and the egui code in the same change. Layout, animation and input stay native to each side; the spec holds data, words and structure.
5. **Look at it.** `CHA_SNAPSHOT_DIR=<dir> cargo test -p cha-player --release snapshot -- --ignored --nocapture` writes an icon sheet and the toolbar and stats panel; compare with the browser.

### Adding an icon

Draw it in a 16 x 16 box with absolute or relative path data (`M L H V C S Q T A Z`, any case). Prefer a stroke of 1.4 to 1.75 units with round caps and joins. Give it a kebab-case id that says what it shows (`sound-muted`, `fullscreen-exit`). A state is a second icon, not a flag.
