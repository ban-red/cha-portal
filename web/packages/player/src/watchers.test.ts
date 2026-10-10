import { describe, expect, test } from "bun:test";

import { freeSlot, handable, parseWatchers, watcherLabel } from "./watchers";

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

  test("only guests can be handed the controls", () => {
    const list = parseWatchers([
      { id: 1, role: "viewer" },
      { id: 2, role: "controller" },
      { id: 3, role: "admin" },
      { id: 4, role: "guest" },
      { id: 5, role: "member" },
    ]);
    expect(handable(list).map((w) => w.id)).toEqual([2, 4, 5]);
  });

  test("signed-in guests carry their name, what they ask for and a granted pad", () => {
    const [ask, player] = parseWatchers([
      { id: 4, role: "guest", name: "Sam", request: "pad" },
      { id: 5, role: "player", slot: 2, granted: true, name: "Kim", request: "nothing" },
    ]);
    expect(ask).toEqual({ id: 4, role: "guest", name: "Sam", request: "pad" });
    expect(player).toEqual({ id: 5, role: "player", slot: 2, granted: true, name: "Kim" });
    expect(watcherLabel(ask!)).toBe("Sam · asks to add a gamepad");
    expect(watcherLabel({ id: 6, role: "guest", request: "control" })).toBe("A guest · asks for the controls");
    expect(watcherLabel(player!)).toBe("Kim · Player 3");
  });

  test("the first free pad slot", () => {
    expect(freeSlot([])).toBe(1);
    expect(freeSlot([{ id: 1, role: "player", slot: 1 }])).toBe(2);
    expect(
      freeSlot([1, 2, 3].map((slot) => ({ id: slot, role: "player" as const, slot }))),
    ).toBeNull();
  });
});
