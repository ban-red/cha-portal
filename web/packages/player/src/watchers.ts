// Who else is watching, as the session with the controls hears it (ADR 0015).

/** One other session on the environment. */
export interface Watcher {
  /** The session id `giveControl` names. */
  id: number;
  role: "owner" | "admin" | "controller" | "viewer" | "player";
  /** A player's pad index (1 is "player 2"). */
  slot?: number;
}

const ROLES = new Set(["owner", "admin", "controller", "viewer", "player"]);

/** The streamer's `{"t":"viewers","list":[...]}` list; entries that don't fit are dropped. */
export function parseWatchers(list: unknown): Watcher[] {
  if (!Array.isArray(list)) return [];
  const out: Watcher[] = [];
  for (const e of list) {
    const w = e as { id?: unknown; role?: unknown; slot?: unknown } | null;
    if (typeof w?.id !== "number" || !Number.isInteger(w.id) || w.id < 0) continue;
    if (typeof w.role !== "string" || !ROLES.has(w.role)) continue;
    const slot = typeof w.slot === "number" ? w.slot : undefined;
    out.push({ id: w.id, role: w.role as Watcher["role"], ...(slot === undefined ? {} : { slot }) });
  }
  return out;
}

/** "Controller", "Player 2", "Viewer", "Another device of yours" for the owner's or an admin's other sessions. */
export function watcherLabel(w: Watcher): string {
  switch (w.role) {
    case "player":
      return `Player ${(w.slot ?? 0) + 1}`;
    case "controller":
      return "Guest controller";
    case "viewer":
      return "Guest viewer";
    default:
      return "Another device of yours";
  }
}

/** The guest controllers among them: the ones the controls can be handed to. */
export function handable(list: readonly Watcher[]): Watcher[] {
  return list.filter((w) => w.role === "controller");
}
