import type { User } from "./api";

const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();

/** The users the switcher lists: those matching the search words (name, username or email), by name. */
export function filterSwitchable(users: readonly User[], query: string): User[] {
  const words = fold(query).split(/\s+/).filter(Boolean);
  return users
    .filter((u) => {
      const hay = fold(`${u.displayName} ${u.username} ${u.email ?? ""} ${u.role}`);
      return words.every((w) => hay.includes(w));
    })
    .sort((a, b) => a.displayName.localeCompare(b.displayName));
}

/** The second line under a name: the username, and the email when it adds something. */
export function identityLine(u: Pick<User, "username" | "email">): string {
  return u.email && u.email.toLowerCase() !== u.username.toLowerCase() ? `${u.username} · ${u.email}` : u.username;
}

/** Where the highlighted row goes for an arrow key, wrapping at both ends; -1 when the list is empty. */
export function moveActive(current: number, delta: 1 | -1, count: number): number {
  if (count <= 0) return -1;
  if (current < 0) return delta === 1 ? 0 : count - 1;
  return (current + delta + count) % count;
}
