// Share links for players (ADR 0014; contract: docs/plans/share-links.md).

/** The gamepad slots a link can name: 1 to 3 is player 2 to 4. */
export const SHARE_SLOTS = [1, 2, 3] as const;
export type ShareSlot = (typeof SHARE_SLOTS)[number];

/** "player 2" for slot 1. */
export function playerLabel(slot: number): string {
  return `player ${slot + 1}`;
}

/** The link as people paste it: the portal's origin and the `/s/<token>` path the portal returned. */
export function shareLink(origin: string, path: string): string {
  return `${origin.replace(/\/+$/, "")}${path.startsWith("/") ? path : `/${path}`}`;
}

/** How long a link has left, as "23 h", "45 min" or "under a minute"; null once over. */
export function timeLeft(expiresAt: number, now = Date.now()): string | null {
  const s = Math.floor(expiresAt - now / 1000);
  if (s <= 0) return null;
  if (s < 60) return "under a minute";
  if (s < 3600) return `${Math.round(s / 60)} min`;
  const h = Math.floor(s / 3600);
  const m = Math.round((s % 3600) / 60);
  return h < 10 && m > 0 && m < 60 ? `${h} h ${m} min` : `${Math.round(s / 3600)} h`;
}

/** The slots with no live link, lowest first. */
export function freeSlots(live: { slot: number }[]): ShareSlot[] {
  return SHARE_SLOTS.filter((s) => !live.some((l) => l.slot === s));
}

/** What the guest page says when the link or the game can't be used. */
export function guestProblem(err: unknown): string {
  const e = err as { status?: unknown; code?: unknown } | null;
  const status = typeof e?.status === "number" ? e.status : 0;
  const code = typeof e?.code === "string" ? e.code : "";
  if (status === 404 || code === "unknown_share") return "This link has expired or was revoked.";
  if (status === 409 && code === "not_running") return "The game isn't running right now.";
  if (status === 429) return "Too many tries. Wait a minute and try again.";
  return "Couldn't reach the game. Try again.";
}

/** Whether trying again can help (a dead link or a stopped game won't come back). */
export function guestCanRetry(err: unknown): boolean {
  const status = (err as { status?: unknown } | null)?.status;
  return !(status === 404 || status === 409);
}

/**
 * The guest's codec: the first the browser decodes (its order, HEVC first)
 * that the environment encodes, so the guest shares the owner's encoder when
 * it can; H.264, which every device encodes, otherwise.
 */
export function guestCodec<C extends string>(browser: readonly C[], device: readonly string[] | null): C | "h264" {
  return browser.find((c) => !device || device.includes(c)) ?? "h264";
}
