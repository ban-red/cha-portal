// Turns the portal's theme CSS (web/apps/portal/src/themes/<id>.css) into the JSON the native
// player embeds (crates/cha-player/themes/<id>.json): every role, fully resolved, as sRGB hex
// for each of dark, light, dark-more and light-more.
//
//   bun scripts/export-player-themes.ts           write the files
//   bun scripts/export-player-themes.ts --check   exit 1 if any file would change; writes nothing
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseBlocks, parseColorAlpha, resolveVar, resolveVariants } from "../web/apps/portal/src/themes/css";
import { ROLES, THEMES } from "../web/apps/portal/src/themes/index";

const ROOT = join(import.meta.dir, "..");
const SRC = "web/apps/portal/src/themes";
const OUT = join(ROOT, "crates/cha-player/themes");
const check = process.argv.includes("--check");

const hex2 = (x: number) =>
  Math.round(Math.min(1, Math.max(0, x)) * 255)
    .toString(16)
    .padStart(2, "0");

function toHex(label: string, raw: string, vars: Record<string, string>): string {
  const parsed = parseColorAlpha(resolveVar(raw, vars));
  if (!parsed) throw new Error(`${label}: "${raw}" is not a hex, rgb() or oklch() colour`);
  const [[r, g, b], a] = parsed;
  return `#${hex2(r)}${hex2(g)}${hex2(b)}${a < 1 ? hex2(a) : ""}`;
}

const stale: string[] = [];
for (const theme of THEMES) {
  const cssPath = `${SRC}/${theme.id}.css`;
  const blocks = parseBlocks(readFileSync(join(ROOT, cssPath), "utf8")).filter(
    (b) => b.theme === null || b.theme === theme.id,
  );
  const variants: Record<string, Record<string, string>> = {};
  for (const v of resolveVariants(blocks)) {
    const key = v.contrast === "more" ? `${v.appearance}-more` : v.appearance;
    const colours: Record<string, string> = {};
    for (const role of ROLES) {
      const raw = v.vars[role];
      if (raw === undefined) throw new Error(`${theme.id} [${key}]: role "${role}" is not defined`);
      colours[role] = toHex(`${theme.id} [${key}] ${role}`, raw, v.vars);
    }
    variants[key] = colours;
  }
  const ordered: Record<string, Record<string, string>> = {};
  for (const key of ["dark", "light", "dark-more", "light-more"]) {
    if (!variants[key]) throw new Error(`${theme.id}: missing variant ${key}`);
    ordered[key] = variants[key]!;
  }
  const json =
    JSON.stringify(
      {
        id: theme.id,
        name: theme.name,
        generated: `by scripts/export-player-themes.ts from ${cssPath}; do not edit`,
        variants: ordered,
      },
      null,
      2,
    ) + "\n";
  const file = join(OUT, `${theme.id}.json`);
  const current = existsSync(file) ? readFileSync(file, "utf8") : null;
  if (current === json) continue;
  if (check) stale.push(`crates/cha-player/themes/${theme.id}.json`);
  else {
    mkdirSync(OUT, { recursive: true });
    writeFileSync(file, json);
    console.log(`wrote crates/cha-player/themes/${theme.id}.json`);
  }
}

if (check && stale.length) {
  console.error(`player themes are out of date: ${stale.join(", ")}`);
  console.error("run: bun scripts/export-player-themes.ts");
  process.exit(1);
}
