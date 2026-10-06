// Gates every theme: all roles defined, and every contrast pair passes WCAG 2.x.
// Theme files are plain CSS (`<id>.css`); this parses their custom properties itself.
import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { DEFAULT_PREFS, DEFAULT_THEME, ROLES, THEMES } from "./index";
import { parsePrefs, readSystemEnv, resolvePrefs, type SystemEnv } from "./runtime";

const DIR = import.meta.dir;

type Rgb = [number, number, number]; // linear-light is computed later; these are sRGB 0..1

// ---- colour parsing ----------------------------------------------------------------------

function parseHex(v: string): Rgb | null {
  const m = /^#([0-9a-f]{3,8})$/i.exec(v.trim());
  if (!m) return null;
  let h = m[1]!;
  if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join("");
  if (h.length !== 6 && h.length !== 8) return null;
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255) as Rgb;
}

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));
const encode = (c: number) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

/** OKLCH to sRGB (0..1), gamut-clipped by clamping. */
export function oklchToRgb(L: number, C: number, hDeg: number): Rgb {
  const h = (hDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l_ = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const lin: Rgb = [
    4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
    -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
    -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
  ];
  return lin.map((c) => encode(clamp01(c))) as Rgb;
}

function parseOklch(v: string): Rgb | null {
  const m = /^oklch\(\s*([^\s/)]+)\s+([^\s/)]+)\s+([^\s/)]+)\s*(?:\/\s*[^)]+)?\)$/i.exec(v.trim());
  if (!m) return null;
  const num = (s: string, pctScale: number) => (s.endsWith("%") ? (parseFloat(s) / 100) * pctScale : parseFloat(s));
  const L = clamp01(num(m[1]!, 1));
  const C = num(m[2]!, 0.4);
  const H = parseFloat(m[3]!.replace(/deg$/, ""));
  if ([L, C, H].some(Number.isNaN)) return null;
  return oklchToRgb(L, C, H);
}

export function parseColor(v: string): Rgb | null {
  return parseHex(v) ?? parseOklch(v);
}

/** Follows var(--cha-x) references through the merged variables of a variant. */
export function resolveVar(value: string, vars: Record<string, string>, depth = 0): string {
  const m = /^var\(\s*--cha-([\w-]+)\s*\)$/.exec(value.trim());
  if (!m) return value;
  const next = vars[m[1]!];
  if (next === undefined || depth > 8) throw new Error(`cannot resolve ${value}`);
  return resolveVar(next, vars, depth + 1);
}

// ---- WCAG 2.x ----------------------------------------------------------------------------

const decode = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
export function luminance([r, g, b]: Rgb): number {
  return 0.2126 * decode(r) + 0.7152 * decode(g) + 0.0722 * decode(b);
}
export function contrast(a: Rgb, b: Rgb): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

// ---- theme file parsing ------------------------------------------------------------------

interface Block {
  theme: string | null; // from [data-theme="x"] or [data-theme-preview="x"]; null for a bare :root
  /** The selector is the scoped form for the Appearance page's miniatures. */
  preview: boolean;
  appearance: "dark" | "light" | null;
  contrast: "more" | "standard" | null;
  /** The selector reads :root:not([data-theme]): the no-attribute (default theme) copy. */
  noTheme: boolean;
  vars: Record<string, string>;
}

/** Rule blocks of a theme file. One entry per selector in a comma list. */
export function parseBlocks(css: string): Block[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: Block[] = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const vars: Record<string, string> = {};
    for (const d of m[2]!.split(";")) {
      const i = d.indexOf(":");
      if (i < 0) continue;
      const name = d.slice(0, i).trim();
      if (name.startsWith("--cha-")) vars[name.slice("--cha-".length)] = d.slice(i + 1).trim();
    }
    for (const sel of m[1]!.split(",")) {
      const attr = (n: string) => new RegExp(`\\[data-${n}=["']?([\\w-]+)["']?\\]`).exec(sel)?.[1] ?? null;
      out.push({
        theme: attr("theme") ?? attr("theme-preview"),
        preview: sel.includes("[data-theme-preview"),
        appearance: attr("appearance") as Block["appearance"],
        contrast: attr("contrast") as Block["contrast"],
        noTheme: sel.includes(":not([data-theme])"),
        vars,
      });
    }
  }
  return out;
}

interface Variant {
  appearance: "dark" | "light";
  contrast: "standard" | "more";
  vars: Record<string, string>;
}

/** Base values with each applicable variant block layered on, more specific blocks last. */
export function resolveVariants(blocks: Block[]): Variant[] {
  const appearances: Variant["appearance"][] = ["dark"];
  if (blocks.some((b) => b.appearance === "light")) appearances.push("light");
  const contrasts: Variant["contrast"][] = ["standard"];
  if (blocks.some((b) => b.contrast === "more")) contrasts.push("more");
  const variants: Variant[] = [];
  for (const appearance of appearances) {
    for (const contrast of contrasts) {
      const applies = blocks
        .map((b, order) => ({ b, order, weight: (b.appearance ? 1 : 0) + (b.contrast ? 1 : 0) }))
        .filter(({ b }) => (b.appearance ?? appearance) === appearance && (b.contrast ?? contrast) === contrast)
        .sort((x, y) => x.weight - y.weight || x.order - y.order);
      const vars: Record<string, string> = {};
      for (const { b } of applies) Object.assign(vars, b.vars);
      variants.push({ appearance, contrast, vars });
    }
  }
  return variants;
}

// ---- load themes -------------------------------------------------------------------------

const files = readdirSync(DIR).filter((f) => f.endsWith(".css"));
const fileIds = files.map((f) => f.replace(/\.css$/, ""));
const themes = files.map((f) => {
  const id = f.replace(/\.css$/, "");
  const blocks = parseBlocks(readFileSync(join(DIR, f), "utf8"));
  const ownBlocks = blocks.filter((b) => b.theme === null || b.theme === id);
  return { id, blocks: ownBlocks, variants: resolveVariants(ownBlocks) };
});

// ---- tests -------------------------------------------------------------------------------

const SURFACES = ["canvas", "panel", "panel-2"] as const;
const CHARTS = ["chart-1", "chart-2", "chart-3", "chart-4"] as const;
const TEXT = ["ink", "ink-2", "ink-3", "accent", "ok", "warn", "danger", "info"] as const;

describe("theme registry", () => {
  test("every theme in THEMES has a CSS file, and every CSS file is registered", () => {
    expect([...THEMES.map((t) => t.id)].sort()).toEqual([...fileIds].sort());
  });
});

for (const theme of themes) {
  describe(`theme ${theme.id}`, () => {
    test("has a base variant", () => {
      expect(theme.blocks.some((b) => !b.appearance && !b.contrast)).toBe(true);
    });

    for (const v of theme.variants) {
      const label = `${theme.id} [${v.appearance}, contrast ${v.contrast}]`;
      const color = (role: string): Rgb => {
        const raw = v.vars[role];
        if (raw === undefined) throw new Error(`${label}: role "${role}" is not defined`);
        const resolved = resolveVar(raw, v.vars);
        const rgb = parseColor(resolved);
        if (!rgb) throw new Error(`${label}: role "${role}" is "${raw}", which is not a hex or oklch() colour`);
        return rgb;
      };
      const failures: string[] = [];
      const check = (fg: string, bg: string, target: number) => {
        const ratio = contrast(color(fg), color(bg));
        if (ratio < target) {
          failures.push(`${label}: ${fg} on ${bg} is ${ratio.toFixed(2)}:1, needs ${target}:1`);
        }
      };

      test(`${v.appearance}/${v.contrast}: every role is defined`, () => {
        failures.length = 0;
        const missing = ROLES.filter((r) => v.vars[r] === undefined);
        expect(missing, `${label}: missing roles`).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: text and status colours reach 4.5:1 on every surface`, () => {
        failures.length = 0;
        for (const fg of TEXT) {
          for (const bg of SURFACES) {
            const strict = v.contrast === "more" && (fg === "ink" || fg === "ink-2");
            check(fg, bg, strict ? 7 : 4.5);
          }
        }
        expect(failures).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: ink and accent are readable on accent-soft`, () => {
        failures.length = 0;
        check("ink", "accent-soft", 4.5);
        check("accent", "accent-soft", 4.5);
        expect(failures).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: on-accent is readable on accent-fill and accent-fill-hover`, () => {
        failures.length = 0;
        check("on-accent", "accent-fill", 4.5);
        check("on-accent", "accent-fill-hover", 4.5);
        expect(failures).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: on-danger is readable on danger-fill`, () => {
        failures.length = 0;
        check("on-danger", "danger-fill", 4.5);
        expect(failures).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: line-strong and focus reach 3:1 on every surface`, () => {
        failures.length = 0;
        for (const fg of ["line-strong", "focus"]) for (const bg of SURFACES) check(fg, bg, 3);
        if (v.contrast === "more") for (const bg of SURFACES) check("line-strong", bg, 4.5);
        expect(failures).toEqual([]);
      });

      test(`${v.appearance}/${v.contrast}: chart colours reach 4.5:1 on every surface`, () => {
        failures.length = 0;
        for (const fg of CHARTS) for (const bg of SURFACES) check(fg, bg, 4.5);
        expect(failures).toEqual([]);
      });
    }
  });
}

describe("scoped preview selectors", () => {
  // The Appearance page draws each theme's miniature under [data-theme-preview="id"], with
  // data-appearance and data-contrast copied onto the same element. Every :root[data-theme]
  // block must have a scoped twin with identical values, and the reverse.
  for (const t of themes) {
    test(`${t.id}: every themed block has an identical preview block`, () => {
      const key = (b: Block) => `${b.appearance}/${b.contrast}`;
      const themed = t.blocks.filter((b) => b.theme === t.id && !b.preview);
      const previews = t.blocks.filter((b) => b.preview);
      expect(themed.length).toBeGreaterThan(0);
      expect(previews.length).toBe(themed.length);
      for (const b of themed) {
        const twin = previews.find((p) => key(p) === key(b));
        expect(twin, `${t.id}: no preview block for ${key(b)}`).toBeDefined();
        expect(twin!.vars).toEqual(b.vars);
      }
    });
  }
});

describe("the default theme", () => {
  const theme = themes.find((t) => t.id === DEFAULT_THEME)!;
  const combos = new Set(
    theme.blocks.filter((b) => b.theme === DEFAULT_THEME).map((b) => `${b.appearance}/${b.contrast}`),
  );

  test("registry and CSS agree on the default", () => {
    expect(theme).toBeDefined();
    expect(THEMES.some((t) => t.id === DEFAULT_THEME)).toBe(true);
    expect(DEFAULT_PREFS.theme).toBe(DEFAULT_THEME);
  });

  test("every variant also has a :root:not([data-theme]) selector with the same values", () => {
    expect(combos.size).toBeGreaterThanOrEqual(4);
    for (const key of combos) {
      const mine = theme.blocks.find((b) => b.theme === DEFAULT_THEME && `${b.appearance}/${b.contrast}` === key)!;
      const bare = theme.blocks.filter(
        (b) => b.theme === null && `${b.appearance}/${b.contrast}` === key && (b.noTheme || key === "null/null"),
      );
      expect(
        bare.some((b) => JSON.stringify(b.vars) === JSON.stringify(mine.vars)),
        `no default-case selector for ${key}`,
      ).toBe(true);
    }
  });

  test("no other theme claims the bare :root", () => {
    for (const t of themes.filter((t) => t.id !== DEFAULT_THEME)) {
      expect(t.blocks.every((b) => b.theme === t.id)).toBe(true);
    }
  });

  test("every theme has all four variants", () => {
    for (const t of themes) expect(t.variants.length).toBe(4);
  });
});

describe("the pre-paint script in index.html", () => {
  const html = readFileSync(join(DIR, "../../index.html"), "utf8");
  test("knows every theme id, the storage key and the five attributes", () => {
    for (const t of THEMES) expect(html).toContain(`"${t.id}"`);
    expect(html).toContain('"cha.theme"');
    for (const a of ["theme", "appearance", "contrast", "motion", "transparency"]) {
      expect(html).toContain(`"data-${a}"`);
    }
    expect(html).toContain('content="dark light"');
  });
});

describe("parsePrefs", () => {
  test("garbage gives the defaults", () => {
    for (const bad of [null, undefined, 42, "x", [], true, {}]) expect(parsePrefs(bad)).toEqual(DEFAULT_PREFS);
  });
  test("a partial object keeps what is valid and defaults the rest", () => {
    expect(parsePrefs({ appearance: "light", motion: "reduced" })).toEqual({
      ...DEFAULT_PREFS,
      appearance: "light",
      motion: "reduced",
    });
  });
  test("an unknown theme id or an invalid field falls back field by field", () => {
    expect(parsePrefs({ theme: "cha-nope", appearance: "sepia", contrast: "more", transparency: 3 })).toEqual({
      ...DEFAULT_PREFS,
      contrast: "more",
    });
  });
  test("a known theme id is kept", () => {
    expect(parsePrefs({ theme: "cha-jade" }).theme).toBe("cha-jade");
  });
  test("the Environments page's view, sort and pins are kept when valid", () => {
    expect(parsePrefs({ envView: "list", envSort: "recent", pinned: ["chrome", "steam-2"] })).toEqual({
      ...DEFAULT_PREFS,
      envView: "list",
      envSort: "recent",
      pinned: ["chrome", "steam-2"],
    });
  });
  test("invalid view, sort and pins fall back; pins are cleaned", () => {
    expect(parsePrefs({ envView: "table", envSort: "size", pinned: "chrome" })).toEqual(DEFAULT_PREFS);
    expect(parsePrefs({ pinned: ["a", "a", "B", 3, "ok-1", "x".repeat(41)] }).pinned).toEqual(["a", "ok-1"]);
    const many = Array.from({ length: 80 }, (_, i) => `app-${i}`);
    expect(parsePrefs({ pinned: many }).pinned).toHaveLength(64);
  });
});

describe("system resolution", () => {
  const none: SystemEnv = { light: false, moreContrast: false, reducedMotion: false, reducedTransparency: false };
  test("system follows the OS", () => {
    const r = resolvePrefs(DEFAULT_PREFS, { light: true, moreContrast: true, reducedMotion: true, reducedTransparency: true });
    expect(r).toEqual({ theme: DEFAULT_THEME, appearance: "light", contrast: "more", motion: "reduced", transparency: "reduced" });
    expect(resolvePrefs(DEFAULT_PREFS, none)).toEqual({
      theme: DEFAULT_THEME,
      appearance: "dark",
      contrast: "standard",
      motion: "full",
      transparency: "full",
    });
  });
  test("explicit choices beat the OS", () => {
    const os: SystemEnv = { light: true, moreContrast: true, reducedMotion: false, reducedTransparency: false };
    const r = resolvePrefs({ ...DEFAULT_PREFS, appearance: "dark", contrast: "standard", transparency: "reduced" }, os);
    expect(r.appearance).toBe("dark");
    expect(r.contrast).toBe("standard");
    expect(r.transparency).toBe("reduced");
  });
  test("forced dark wins over light", () => {
    expect(resolvePrefs({ ...DEFAULT_PREFS, appearance: "light" }, none, true).appearance).toBe("dark");
  });
  test("reads the four media queries, and survives a missing matchMedia", () => {
    const seen: string[] = [];
    const env = readSystemEnv((q) => {
      seen.push(q);
      return { matches: q.includes("contrast") };
    });
    expect(env).toEqual({ light: false, moreContrast: true, reducedMotion: false, reducedTransparency: false });
    expect(seen.length).toBe(4);
    expect(readSystemEnv(undefined)).toEqual(none);
  });
});

describe("colour maths", () => {
  test("black on white is 21:1", () => {
    expect(contrast([0, 0, 0], [1, 1, 1])).toBeCloseTo(21, 5);
  });
  test("oklch white and black convert", () => {
    expect(parseColor("oklch(1 0 0)")!.every((c) => Math.abs(c - 1) < 1e-3)).toBe(true);
    expect(parseColor("oklch(0 0 0)")!.every((c) => Math.abs(c) < 1e-3)).toBe(true);
  });
  test("short and long hex agree", () => {
    expect(parseColor("#fff")).toEqual(parseColor("#ffffff"));
  });
});
