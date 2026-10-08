import { describe, expect, test } from "bun:test";

import { handable, parseWatchers, watcherLabel } from "./watchers";

describe("watchers", () => {
  test("the streamer's list parses, and bad entries are dropped", () => {
    expect(
      parseWatchers([
        { id: 3, role: "controller" },
        { id: 5, role: "player", slot: 2 },
        { id: 6, role: "viewer", slot: "x" },
        { id: "7", role: "viewer" },
        { id: 8, role: "root" },
        { id: -1, role: "viewer" },
        null,
        42,
      ]),
    ).toEqual([
      { id: 3, role: "controller" },
      { id: 5, role: "player", slot: 2 },
      { id: 6, role: "viewer" },
    ]);
    expect(parseWatchers(undefined)).toEqual([]);
    expect(parseWatchers({ id: 1 })).toEqual([]);
  });

  test("labels", () => {
    expect(watcherLabel({ id: 1, role: "player", slot: 1 })).toBe("Player 2");
    expect(watcherLabel({ id: 1, role: "controller" })).toBe("Guest controller");
    expect(watcherLabel({ id: 1, role: "viewer" })).toBe("Guest viewer");
    expect(watcherLabel({ id: 1, role: "owner" })).toBe("Another device of yours");
  });

  test("only controllers can be handed the controls", () => {
    const list = parseWatchers([
      { id: 1, role: "viewer" },
      { id: 2, role: "controller" },
      { id: 3, role: "admin" },
    ]);
    expect(handable(list).map((w) => w.id)).toEqual([2]);
  });
});
