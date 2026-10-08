// The text and fill of a starting environment's counted step (an image
// download), from `Environment.progress`.
import type { EnvironmentProgress } from "./api";

const MB = 2 ** 20;
const GB = 2 ** 30;

function trim(n: number): string {
  return n.toFixed(1).replace(/\.0$/, "");
}

/** "412 of 890 MB", or "1.2 of 3.4 GB" from a gigabyte up; other units print as counted. */
export function progressText(p: EnvironmentProgress): string {
  if (p.unit !== "bytes") return `${p.done} of ${p.total} ${p.unit}`;
  if (p.total >= GB) return `${trim(p.done / GB)} of ${trim(p.total / GB)} GB`;
  return `${Math.round(p.done / MB)} of ${Math.max(1, Math.round(p.total / MB))} MB`;
}

/** How full the bar is, 0 to 100. */
export function progressPercent(p: EnvironmentProgress): number {
  if (p.total <= 0) return 0;
  return Math.min(100, Math.max(0, (p.done / p.total) * 100));
}
