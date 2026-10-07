/**
 * Turns what a person typed (`48219375`, `4821-9375`, `4821 9375`) into the
 * canonical `4821-9375`, or null when it isn't 8 digits.
 */
export function normalizePairingCode(input: string): string | null {
  const digits = input.replace(/[\s-]/g, "");
  if (!/^\d{8}$/.test(digits)) return null;
  return `${digits.slice(0, 4)}-${digits.slice(4)}`;
}
