// Who else is watching, as the session with the controls hears it (ADR 0015).

/** One other session on the environment. */
export interface Watcher {
  /** The session id `giveControl` names. */
  id: number;
  role: "owner" | "admin" | "controller" | "viewer" | "player" | "guest" | "member";
  /** A player's pad index (1 is "player 2"). */
  slot?: number;
  /** What it asks the holder for. */
  request?: "control" | "pad";
  /** A player whose pad slot the holder granted (and can take back), not a share link's. */
  granted?: boolean;
  /** The signed-in user's display name, when the streamer knows it. */
  name?: string;
}

const ROLES = new Set(["owner", "admin", "controller", "viewer", "player", "guest", "member"]);

/** The streamer's `{"t":"viewers","list":[...]}` list; entries that don't fit are dropped. */
export function parseWatchers(list: unknown): Watcher[] {
  if (!Array.isArray(list)) return [];
  const out: Watcher[] = [];
  for (const e of list) {
    const w = e as { id?: unknown; role?: unknown; slot?: unknown; request?: unknown; granted?: unknown; name?: unknown } | null;
    if (typeof w?.id !== "number" || !Number.isInteger(w.id) || w.id < 0) continue;
    if (typeof w.role !== "string" || !ROLES.has(w.role)) continue;
    const slot = typeof w.slot === "number" ? w.slot : undefined;
    const request = w.request === "control" || w.request === "pad" ? w.request : undefined;
    const name = typeof w.name === "string" && w.name ? w.name.slice(0, 64) : undefined;
    out.push({
      id: w.id,
      role: w.role as Watcher["role"],
      ...(slot === undefined ? {} : { slot }),
      ...(request === undefined ? {} : { request }),
      ...(w.granted === true ? { granted: true } : {}),
      ...(name === undefined ? {} : { name }),
    });
  }
  return out;
}

/** "Controller", "Player 2", "Viewer", "Another device of yours" for the owner's or an admin's other sessions. */
export function watcherLabel(w: Watcher): string {
  const who = w.name ?? "A guest";
  const asks = w.request === "control" ? " · asks for the controls" : w.request === "pad" ? " · asks to add a gamepad" : "";
  switch (w.role) {
    case "player":
      return `${w.name ? `${w.name} · ` : ""}Player ${(w.slot ?? 0) + 1}`;
    case "controller":
      return "Guest controller";
    case "viewer":
      return "Guest viewer";
    case "guest":
    case "member":
      return `${who}${asks}`;
    default:
      return "Another device of yours";
  }
}

/** The guests among them the controls can be handed to: share-link controllers and signed-in users. */
export function handable(list: readonly Watcher[]): Watcher[] {
  return list.filter((w) => w.role === "controller" || w.role === "guest" || w.role === "member");
}

/** Sessions asking for a gamepad, and ones the holder can still put on a free one. */
export function padCandidates(list: readonly Watcher[]): Watcher[] {
  return list.filter((w) => w.role === "guest" || w.role === "member" || w.role === "controller");
}

/** The first of pad indices 1 to 3 (players 2 to 4) no session holds, or null when all are taken. */
export function freeSlot(list: readonly Watcher[]): number | null {
  for (const slot of [1, 2, 3]) if (!list.some((w) => w.role === "player" && w.slot === slot)) return slot;
  return null;
}
