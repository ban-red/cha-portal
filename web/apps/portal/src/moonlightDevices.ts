// Pairing a Moonlight device: the PIN the user types and how long a request has left.

import type { GamestreamHost } from "./api";

/** Keeps only digits, at most four, as the user types or pastes. */
export function cleanPin(input: string): string {
  return input.replace(/\D/g, "").slice(0, 4);
}

/** The PIN when it is exactly four digits (spaces ignored), else null. */
export function validPin(input: string): string | null {
  const digits = input.replace(/\s/g, "");
  return /^\d{4}$/.test(digits) ? digits : null;
}

/** "in 45 s", "in 3 min", or "expired" (times are unix seconds). */
export function expiresIn(expiresAt: number, now = Date.now()): string {
  const s = Math.ceil(expiresAt - now / 1000);
  if (s <= 0) return "expired";
  if (s < 90) return `in ${s} s`;
  return `in ${Math.round(s / 60)} min`;
}

/** What to type in Moonlight: the address, with the port unless it is the default 47989. */
export function hostAddress(host: Pick<GamestreamHost, "address" | "httpPort">): string {
  return host.httpPort === 47989 ? host.address : `${host.address}:${host.httpPort}`;
}

/** The message under the PIN box for a failed pairing. */
export function pairErrorText(status: number | null, code: string | null, message: string | null): string {
  if (code === "wrong_pin" || status === 403) return "That PIN doesn't match the one on the device. Start pairing again in Moonlight.";
  if (code === "not_found" || status === 404) return "That request expired. Start pairing again in Moonlight.";
  if (code === "timeout" || status === 504) return "The device didn't finish pairing. Try again from Moonlight.";
  if (code === "bad_pin") return "Type the 4-digit PIN Moonlight shows.";
  return message || "Pairing failed.";
}
