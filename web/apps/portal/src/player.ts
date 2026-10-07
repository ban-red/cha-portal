// Cha Player sign-in (contract: docs/plans/c2-device-signin.md).

/** The letters a user code uses: no vowels and no look-alikes. */
const USER_CODE = /^[BCDFGHJKLMNPQRSTVWXZ]{8}$/;

/**
 * Turns what a person typed (any case, with or without the dash or spaces) into
 * the canonical `ABCD-EFGH`, or null when it isn't 8 letters of the alphabet.
 */
export function normalizeUserCode(input: string): string | null {
  const code = input.replace(/[\s-]/g, "").toUpperCase();
  if (!USER_CODE.test(code)) return null;
  return `${code.slice(0, 4)}-${code.slice(4)}`;
}

/** Keeps only what can be part of a user code while typing, shown as `ABCD-EFGH`. */
export function typedUserCode(input: string): string {
  const letters = input.replace(/[^A-Za-z]/g, "").toUpperCase().slice(0, 8);
  return letters.length > 4 ? `${letters.slice(0, 4)}-${letters.slice(4)}` : letters;
}

/** The `cha://connect` link that signs Cha Player in with a ticket (and launches an app). */
export function playerLink(origin: string, ticket: string, launch?: string): string {
  let link = `cha://connect?portal=${encodeURIComponent(origin)}&ticket=${encodeURIComponent(ticket)}`;
  if (launch) link += `&launch=${encodeURIComponent(launch)}`;
  return link;
}

/** Cha Player is a Mac app for now; true for a macOS (not iOS) user agent. */
export function isMac(userAgent: string = typeof navigator === "undefined" ? "" : navigator.userAgent): boolean {
  return /Macintosh|Mac OS X/.test(userAgent) && !/iPhone|iPad|iPod|Mobile/.test(userAgent);
}
