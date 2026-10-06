# 0005: The theme token contract

- **Status:** accepted (2026-10-06).
- **Context:**
  - The portal had one dark theme. Its colours were compile-time Tailwind `@theme` values, so nothing could change at runtime, and there was no light mode.
  - Measuring the old palette found real failures: `ink-3` text at 4.33:1 (WCAG AA needs 4.5:1), field borders at about 1.3:1 (controls need 3:1), a primary button that got *brighter* under white text on hover, and one `accent` colour that meant both "brand" and "running", so a magenta theme would have turned "running" pink.
  - We want themes that are easy to add, a light and a dark look for each, a High Contrast setting, and accessibility that is enforced by a check instead of by eye. The plan is [`docs/plans/theming.md`](../plans/theming.md).
- **Decision:**
  - **Three tiers.** Components only see the middle one.
    1. *Primitives*: per-theme colour ramps written in OKLCH (`--cha-magenta-500: oklch(…)`), so lightness steps are even across hues. They never reach a component.
    2. *Semantic roles*: the contract every theme fills, as `--cha-<role>` CSS variables.
    3. *Component tokens*, only where a component needs its own knob: `--radius-card`, `--radius-control`, `--cha-glow` (the faint accent lift on cards; `none` under contrast "more"), `--sidebar-material`.
  - **The roles** (`ROLES` in `src/themes/index.ts`; each is a Tailwind colour such as `bg-panel` or `text-ink-2`):

    | Role | Use |
    |---|---|
    | `canvas`, `panel`, `panel-2` | Base, raised and more-raised surfaces. In dark themes a raised surface is lighter, not shadowed. |
    | `ink`, `ink-2`, `ink-3` | Primary, secondary and tertiary text. All three reach 4.5:1 on every surface, because `ink-3` carries real information. |
    | `line` | Decorative separators and card edges. Exempt from contrast rules. |
    | `line-strong` | The edge of anything that identifies a control: fields, selects, switches, ghost buttons. At least 3:1. |
    | `accent` | The brand colour as text or an icon on a surface (selection, links, active nav). At least 4.5:1. |
    | `accent-fill`, `accent-fill-hover`, `on-accent` | A solid brand fill (primary button, selected switch) and the text on it. Hover is darker, so contrast only rises. |
    | `accent-soft` | A tinted background for a selected item. Never the only sign of selection: add weight, an icon, a bar or `aria-pressed`/`aria-current`/`aria-checked`. |
    | `focus` | The focus ring. At least 3:1 on every surface. |
    | `ok`, `warn`, `danger`, `info` | Status as text or icon colour. "Running", "online" and "healthy" are `ok`; brand and selection are `accent`. At least 4.5:1. |
    | `danger-fill`, `on-danger` | A solid destructive button (`btn-danger`) and its text. At least 4.5:1. |
    | `scrim` | Behind dialogs and the mobile drawer. |
    | `chart-1…4` | Graph and overlay heading colours. At least 4.5:1. |

    A status always comes with a word or an icon, never colour alone.
  - **Wiring.** `style.css` maps each role with `@theme inline { --color-canvas: var(--cha-canvas); … }`, so `bg-canvas` compiles to `var(--cha-canvas)` and a theme change is an attribute change, with no rebuild and no markup change. Alpha modifiers (`bg-panel/90`) keep working through `color-mix()`.
  - **Attributes on `<html>`:**

    | Attribute | Values |
    |---|---|
    | `data-theme` | a theme id (`cha-magenta`, `cha-jade`) |
    | `data-appearance` | `dark`, `light` |
    | `data-contrast` | `standard`, `more` |
    | `data-motion` | `full`, `reduced` |
    | `data-transparency` | `full`, `reduced` |

    Each setting has a "System" choice that follows the OS (`prefers-color-scheme`, `prefers-contrast`, `prefers-reduced-motion`, `prefers-reduced-transparency`); `src/themes/runtime.ts` resolves it. A short inline script in `index.html` reads the cached choice from `localStorage` (`cha.theme`) and sets all five attributes before first paint, so the first frame is never the wrong colour. The script and `runtime.ts` must agree, and a test checks that the script knows every theme and attribute. `<meta name="color-scheme" content="dark light">` is set, and `theme-color` and the tab icon follow the theme.
  - **The stream view is forced dark.** `setForcedDark(true)` makes the resolved appearance dark whatever the user picked, so the picture's surroundings stay neutral and the theme never tints video. Overlays over the stream use `scrim` and stay legible over any frame.
  - **Contrast is a gate.** `src/themes/themes.test.ts` parses every theme file (hex and OKLCH), resolves each of the four variants (dark and light, each at standard and "more" contrast) and fails by naming the theme, variant, pair, ratio and target. It checks that every role is defined and these WCAG 2.2 pairs:
    - `ink`, `ink-2`, `ink-3`, `accent`, `ok`, `warn`, `danger`, `info` and `chart-1…4` on `canvas`, `panel` and `panel-2`: 4.5:1;
    - `ink` and `accent` on `accent-soft`: 4.5:1;
    - `on-accent` on `accent-fill` and `accent-fill-hover`, and `on-danger` on `danger-fill`: 4.5:1;
    - `line-strong` and `focus` on each surface: 3:1;
    - under contrast "more": `ink` and `ink-2` at 7:1 (AAA) and `line-strong` at 4.5:1.

    The test also checks that the registry and the CSS files agree, that the default theme also applies with no `data-theme` attribute, and that each themed block has an identical scoped twin for the Appearance page's miniatures (`[data-theme-preview="id"]`).
  - **A raw-colour guard.** `scripts/check-portal-colors.sh` (part of `scripts/check.sh`) fails when a `.vue` or `.ts` file under `web/apps/portal/src` uses a Tailwind palette class (`bg-pink-500`), `white`/`black` or a hex value. Only `themes/*.css` and `style.css` may contain colours. The one exception is the stream's own `bg-black`.
  - **Preferences live on the server as well as in the browser.** `GET`/`PUT /api/me/prefs` store one small JSON document per user (`user_prefs`, migration 0010), so the choice follows the user to another device. `localStorage` caches it for the pre-paint script, and on sign-in the server's copy wins. The same document keeps the Environments page's view, sort and pins.
- **Alternatives rejected:**
  - *Compile-time `@theme` only.* It is what we had. It can't change at runtime, so no second theme, no light mode and no System choice.
  - *Runtime-generated CSS from TypeScript objects.* Themes would be typed and could be computed, but the contrast test would have to evaluate TypeScript, themes would not be reviewable as plain CSS, and a style element injected at runtime risks a flash and fights a strict CSP. CSS files are what the browser already understands, and the test can parse them.
  - *`localStorage` only.* No sync between devices, and the preference is lost with site data. We keep it as the cache the pre-paint script needs.
  - *SF Symbols.* Their licence limits them to Apple platforms. We use Lucide (`lucide-vue-next`, ISC, compatible with AGPL), one stroke weight, tree-shaken.
- **How to add a theme:**
  1. Copy `web/apps/portal/src/themes/cha-magenta.css` to `<id>.css`. Keep the structure: a base block (dark), a light block, and a contrast "more" block for each. Fill every role in the dark and light blocks; the "more" blocks only need what changes.
  2. Add a scoped twin to every selector list: `[data-theme-preview="<id>"]` beside `:root[data-theme="<id>"]`, with identical values. The default theme also gets `:root:not([data-theme])`; no other theme claims the bare `:root`.
  3. Import the file in `style.css`.
  4. Add an entry to `THEMES` in `src/themes/index.ts` (id, name, appearances, four swatches, favicon) and put a favicon in `public/`. Add the id to the pre-paint script's list in `index.html`.
  5. Run `bun run --cwd web/apps/portal test`. Raise or lower lightness until every pair passes: the failure message names the pair and the ratio. Then `sh scripts/check-portal-colors.sh` and a look at the Appearance page.
- **Consequences:**
  - A new theme is one CSS file, one registry entry and a passing test. A theme that fails contrast can't merge.
  - Components can't carry colours of their own. A new need (a new status, a new fill) becomes a role in every theme and a row in the test, as `danger-fill` and `on-danger` did.
  - "More" contrast and reduced motion/transparency are first-class settings, not afterthoughts, and each costs a variant block per theme.
  - WCAG contrast is the gate; APCA is not used. Contrast is measured on solid colours, so a role that is semi-transparent (`scrim`) is outside the matrix, and text over a stream is judged by design (a scrim behind it), not by test.
  - Real-browser, keyboard-only and VoiceOver passes were not done for this work; the owner dropped them. The contract guards colour and contrast, not behaviour.
  - A custom accent hue (derive the fill, text and focus colours in OKLCH and adjust lightness until the matrix passes) would fit this contract, using the same function the test uses. It is not built.
