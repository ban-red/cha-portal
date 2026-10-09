import type { Transport } from "./player";

/** What this page can try: the browser's support, and whether the caller gave the way to ask for it. */
export type TransportAvailability = Record<Transport, boolean>;

/** Tried in this order when nothing else is asked for (today's `"auto"`). */
export const DEFAULT_TRANSPORTS: Transport[] = ["webtransport", "webrtc"];

/**
 * The transports to try, in order. `transports` wins; else `transport` (`"auto"` or unset: the
 * default order; one value: only that one). PyroWave travels over WebTransport alone. Each
 * transport the page can't use is skipped; when none is left, WebRTC is tried so the attempt
 * fails with a reason instead of doing nothing.
 */
export function transportOrder(
  opts: { transports?: Transport[]; transport?: "auto" | Transport },
  pyro: boolean,
  available: TransportAvailability,
): Transport[] {
  const wanted: Transport[] = pyro
    ? ["webtransport"]
    : (opts.transports ?? (opts.transport && opts.transport !== "auto" ? [opts.transport] : DEFAULT_TRANSPORTS));
  const order = wanted.filter((t, i) => wanted.indexOf(t) === i && available[t]);
  return order.length ? order : ["webrtc"];
}
