import { describe, expect, test } from "bun:test";

import type { User } from "./api";
import { filterSwitchable, identityLine, moveActive } from "./switcher";

const u = (over: Partial<User>): User => ({
  id: "1",
  username: "ann",
  displayName: "Ann",
  role: "user",
  disabled: false,
  createdAt: 0,
  email: null,
  maxInstances: null,
  nodeRestricted: false,
  ...over,
});
const people = [
  u({ id: "a", displayName: "Zoë Ray", username: "zoe", email: "zoe@home.lan" }),
  u({ id: "b", displayName: "Admin", username: "root", role: "admin" }),
  u({ id: "c", displayName: "Bob", username: "bob@x.io", email: "bob@x.io" }),
];

describe("filterSwitchable", () => {
  test("sorts by display name with no query", () => {
    expect(filterSwitchable(people, "").map((p) => p.id)).toEqual(["b", "c", "a"]);
  });
  test("matches name, username, email and role, ignoring case and accents", () => {
    expect(filterSwitchable(people, "ZOE").map((p) => p.id)).toEqual(["a"]);
    expect(filterSwitchable(people, "home.lan").map((p) => p.id)).toEqual(["a"]);
    expect(filterSwitchable(people, "admin").map((p) => p.id)).toEqual(["b"]);
  });
  test("every word must match", () => {
    expect(filterSwitchable(people, "bob x.io")).toHaveLength(1);
    expect(filterSwitchable(people, "bob zoe")).toHaveLength(0);
  });
  test("does not change its input", () => {
    const copy = [...people];
    filterSwitchable(people, "");
    expect(people).toEqual(copy);
  });
});

describe("identityLine", () => {
  test("adds the email only when it differs", () => {
    expect(identityLine({ username: "zoe", email: "zoe@home.lan" })).toBe("zoe · zoe@home.lan");
    expect(identityLine({ username: "bob@x.io", email: "bob@x.io" })).toBe("bob@x.io");
    expect(identityLine({ username: "root", email: null })).toBe("root");
  });
});

describe("moveActive", () => {
  test("wraps and handles the empty list", () => {
    expect(moveActive(-1, 1, 3)).toBe(0);
    expect(moveActive(-1, -1, 3)).toBe(2);
    expect(moveActive(2, 1, 3)).toBe(0);
    expect(moveActive(0, -1, 3)).toBe(2);
    expect(moveActive(0, 1, 0)).toBe(-1);
  });
});
