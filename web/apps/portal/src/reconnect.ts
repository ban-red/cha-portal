// When and whether the session page reconnects: the backoff schedule and what a failed connect means.

/** Waits before each automatic reconnect: 1, 2, 4, 8, then 15 s for as long as it takes. */
export const BACKOFF_MS = [1000, 2000, 4000, 8000, 15000] as const;

/** The wait before attempt `attempt` (1 is the first retry after a drop). */
export function backoffDelay(attempt: number): number {
  const i = Math.max(1, Math.floor(attempt)) - 1;
  return BACKOFF_MS[Math.min(i, BACKOFF_MS.length - 1)]!;
}

/** What a failed connect calls for. */
export type ConnectFailure =
  /** Transient (network, 5xx, timeout): back off and try again. */
  | { kind: "retry" }
  /** Retrying won't help: show the reason and wait for the user. */
  | { kind: "stop"; reason: string }
  /** The environment isn't running: reconnect once it is. */
  | { kind: "wait-running"; reason: string }
  /** The session expired: back to sign-in. */
  | { kind: "login" };

/** An `ApiError` (api.ts), by shape, so this file needs nothing from the app. */
function apiShape(err: unknown): { status: number; code: string; message: string } | null {
  if (typeof err !== "object" || err === null) return null;
  const e = err as { status?: unknown; code?: unknown; message?: unknown };
  if (typeof e.status !== "number") return null;
  return {
    status: e.status,
    code: typeof e.code === "string" ? e.code : "",
    message: typeof e.message === "string" && e.message ? e.message : `the portal answered ${e.status}`,
  };
}

/** Decides what to do about an error from connecting: only a real answer that says no stops the retries. */
export function classifyConnectError(err: unknown): ConnectFailure {
  const api = apiShape(err);
  if (!api) return { kind: "retry" }; // a fetch TypeError, a timeout, a dropped socket
  const { status, code, message } = api;
  if (status === 401) return { kind: "login" };
  if (status === 409 && code === "not_running") return { kind: "wait-running", reason: message };
  if (status === 408 || status === 429 || status >= 500) return { kind: "retry" };
  if (status >= 400) return { kind: "stop", reason: message };
  return { kind: "retry" };
}
