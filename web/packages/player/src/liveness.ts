// Is the node still there? A pure judgement the player makes each second from what it has heard.
// A still screen sends no video, so the signal is the node's answers to our 1 s pings, plus any
// other message or media that arrives, never frames alone.

/** Silence this long, with the page visible, means the link is dead. */
export const SILENCE_MS = 4000;
/**
 * A page that comes back to the front with a last answer older than this asks the node again
 * (a hidden tab's timers are throttled, so an old answer alone proves nothing)…
 */
export const RESUME_MS = 3000;
/** …and reconnects if that ping isn't answered within this. */
export const RESUME_ANSWER_MS = 1500;
/** Watchdog ticks further apart than this mean the page was frozen (sleep, a suspended tab), not the link. */
export const FROZEN_TICK_MS = 2500;

export type Verdict = "ok" | "dead" | "frozen";

/**
 * `silentMs`: time since anything came from the node. `tickGapMs`: time since the previous check
 * (a check runs every second, so a long gap is the page being asleep). `visible`: a hidden page's
 * timers are throttled and its stream isn't being watched, so it is never judged. "frozen" means
 * give the link a fresh grace period instead of judging it on stale numbers.
 */
export function judgeLiveness(input: { silentMs: number; tickGapMs: number; visible: boolean }): Verdict {
  if (!input.visible) return "ok";
  if (input.tickGapMs > FROZEN_TICK_MS) return "frozen";
  return input.silentMs >= SILENCE_MS ? "dead" : "ok";
}

/** The page is in front again: check the link (a ping, `RESUME_ANSWER_MS` to answer) if the node's last answer is older than `RESUME_MS`. */
export function shouldReconnectOnResume(sincePongMs: number): boolean {
  return sincePongMs > RESUME_MS;
}
