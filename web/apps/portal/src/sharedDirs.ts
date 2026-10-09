// How a shared directory's state shows: the badge text and tone, and the
// note about nodes that keep an app's library in different places.
import type { AdminStorageNode, SharedDirState } from "./api";

export type Tone = "ok" | "warn" | "danger" | "muted";

export const STATE_LABEL: Record<SharedDirState, string> = {
  ok: "Mounted",
  missing: "Not mounted",
  incomplete: "Missing folders",
  unreachable: "Not responding",
  read_only: "Read-only",
};

const STATE_TONE: Record<SharedDirState, Tone> = {
  ok: "ok",
  missing: "danger",
  incomplete: "warn",
  unreachable: "danger",
  read_only: "warn",
};

export const TONE_CLASS: Record<Tone, string> = {
  ok: "border-ok/40 text-ok",
  warn: "border-warn/50 bg-warn/10 text-warn",
  danger: "border-danger/50 bg-danger/10 text-danger",
  muted: "border-line text-ink-3",
};

/**
 * The badge for a reported state. A local directory has nothing to mount, so a
 * missing report reads as fine there; an external one without a report is "Not reported".
 */
export function stateBadge(
  state: SharedDirState | undefined,
  location: "external" | "local",
): { label: string; tone: Tone } {
  if (state) return { label: STATE_LABEL[state], tone: STATE_TONE[state] };
  return location === "external" ? { label: "Not reported", tone: "muted" } : { label: "On the node", tone: "muted" };
}

/** "NAS/share (nfs4, nas:/games)" or "On the node". */
export function whereLine(n: Pick<AdminStorageNode, "location" | "fsType" | "source">): string {
  if (n.location !== "external") return "On the node";
  const bits = [n.fsType, n.source].filter(Boolean).join(", ");
  return bits ? `NAS/share (${bits})` : "NAS/share";
}

export function locationsDiffer(nodes: Pick<AdminStorageNode, "location">[]): boolean {
  return new Set(nodes.map((n) => n.location)).size > 1;
}

const list = (names: string[]) =>
  names.length <= 1 ? (names[0] ?? "") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;

/** One line for when nodes keep the app's folder differently. Empty when they agree. */
export function differNote(nodes: Pick<AdminStorageNode, "location" | "nodeName">[]): string {
  if (!locationsDiffer(nodes)) return "";
  const local = list(nodes.filter((n) => n.location === "local").map((n) => n.nodeName));
  const share = list(nodes.filter((n) => n.location === "external").map((n) => n.nodeName));
  return `Nodes keep this differently: launches on ${local} won't see the library on ${share}.`;
}
