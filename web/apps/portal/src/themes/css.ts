// Shared CSS machinery for the theme files: colour parsing (hex, oklch), var() resolution,
// block parsing and variant layering. Used by themes.test.ts and scripts/export-player-themes.ts.

export type Rgb = [number, number, number]; // sRGB 0..1

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
  return parseOklchAlpha(v)?.[0] ?? null;
}

function parseOklchAlpha(v: string): [Rgb, number] | null {
  const m = /^oklch\(\s*([^\s/)]+)\s+([^\s/)]+)\s+([^\s/)]+)\s*(?:\/\s*([^)\s]+))?\s*\)$/i.exec(v.trim());
  if (!m) return null;
  const num = (s: string, pctScale: number) => (s.endsWith("%") ? (parseFloat(s) / 100) * pctScale : parseFloat(s));
  const L = clamp01(num(m[1]!, 1));
  const C = num(m[2]!, 0.4);
  const H = parseFloat(m[3]!.replace(/deg$/, ""));
  if ([L, C, H].some(Number.isNaN)) return null;
  return [oklchToRgb(L, C, H), parseAlpha(m[4])];
}

function parseAlpha(s: string | undefined): number {
  if (s === undefined) return 1;
  const a = s.endsWith("%") ? parseFloat(s) / 100 : parseFloat(s);
  return Number.isNaN(a) ? 1 : clamp01(a);
}

/** Like parseColor but keeps alpha: hex (#rgba, #rrggbbaa), oklch(... / a) and rgb(r g b / a). */
export function parseColorAlpha(v: string): [Rgb, number] | null {
  const t = v.trim();
  const hex = /^#([0-9a-f]{3,8})$/i.exec(t);
  if (hex) {
    let h = hex[1]!;
    if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join("");
    const rgb = parseHex(t);
    if (!rgb) return null;
    return [rgb, h.length === 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1];
  }
  const fn = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)\s*(?:[/,]\s*([^)\s]+))?\s*\)$/i.exec(t);
  if (fn) return [[fn[1], fn[2], fn[3]].map((x) => clamp01(parseFloat(x!) / 255)) as Rgb, parseAlpha(fn[4])];
  return parseOklchAlpha(t);
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

export interface Block {
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

export interface Variant {
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

