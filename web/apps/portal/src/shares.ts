// Share links (ADRs 0014 and 0015; contract: docs/plans/share-links.md).

/** The gamepad slots a link can name: 1 to 3 is player 2 to 4. */
export const SHARE_SLOTS = [1, 2, 3] as const;
export type ShareSlot = (typeof SHARE_SLOTS)[number];

/** What a link gives: a gamepad slot, watching, or the keyboard, mouse and pads when handed. */
export type ShareRole = "player" | "viewer" | "controller";

/** What the owner's page sends to make a link; only a player's names a slot. */
export type ShareSpec = { role: "player"; slot: number } | { role: "viewer" } | { role: "controller" };

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

/** The slots with no live player link, lowest first. */
export function freeSlots(live: { role?: string; slot: number | null }[]): ShareSlot[] {
  return SHARE_SLOTS.filter((s) => !live.some((l) => l.slot === s && (l.role ?? "player") === "player"));
}

/** The plain warning under a link's button, for the app it controls. */
export function shareWarning(role: ShareRole, app: string): string {
  switch (role) {
    case "viewer":
      return `Anyone with this link can watch and listen to ${app} until it stops (at most 24 hours). They can't use the keyboard, mouse or gamepads. You can make as many as you like.`;
    case "controller":
      return `Anyone with this link can use your keyboard and mouse in ${app} when you hand them the controls, or whenever you aren't holding them. You can take the controls back at any time. A new link replaces the old one; the link works until ${app} stops (at most 24 hours).`;
    default:
      return "Anyone with a link can play as that player until the environment stops (at most 24 hours). A new link for a slot replaces the old one.";
  }
}

/** The invitation on the guest page. */
export function invitation(role: ShareRole, owner: string, slot: number | null): string {
  switch (role) {
    case "viewer":
      return `${owner} invited you to watch.`;
    case "controller":
      return `${owner} invited you to take part. You can use the keyboard and mouse when they hand you the controls.`;
    default:
      return `${owner} invited you to play as ${playerLabel(slot ?? 0)}.`;
  }
}

/** The guest page's join button. */
export function joinLabel(role: ShareRole, slot: number | null): string {
  switch (role) {
    case "viewer":
      return "Watch";
    case "controller":
      return "Join";
    default:
      return `Join as ${playerLabel(slot ?? 0)}`;
  }
}

/** The note under the join button. */
export function joinNote(role: ShareRole): string {
  switch (role) {
    case "viewer":
      return "You will see and hear the game. Nothing you do reaches it.";
    case "controller":
      return "You see and hear the game. Your keyboard, mouse and gamepads reach it only while you have the controls.";
    default:
      return "Connect a gamepad, then press a button on it. Only the gamepad is shared.";
  }
}

/** The line over the picture for a guest who has joined. */
export function seatLine(role: ShareRole, slotPlayer: number): string {
  switch (role) {
    case "viewer":
      return "You are watching.";
    case "controller":
      return "";
    default:
      return `You are player ${slotPlayer}. Press a button on your gamepad.`;
  }
}

/** What a guest controller's page shows about the controls. */
export type ControlPrompt =
  | { kind: "holding"; text: string }
  | { kind: "take"; text: string }
  | { kind: "wait"; text: string };

/**
 * A guest controller's page: it has the controls, can take them (nobody or another guest holds
 * them), or waits for the owner to hand them over. The streamer's `floor` says which
 * (`canTake`); an older streamer reads the role as a viewer and never grants control, which
 * the page says once the floor has spoken and control isn't coming.
 */
export function controlPrompt(hasControl: boolean, canTake: boolean, owner: string): ControlPrompt {
  if (hasControl) return { kind: "holding", text: "You have the controls." };
  if (canTake) return { kind: "take", text: "Take control" };
  return { kind: "wait", text: `Waiting for ${owner} to hand you the controls` };
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
