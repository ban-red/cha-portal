import { afterEach, describe, expect, test } from "bun:test";

import { STEAM_PASSTHROUGH_KEY, loadSteamPassthrough, saveSteamPassthrough } from "./controllerPrefs";

const g = globalThis as Record<string, unknown>;
const saved = g.localStorage;

function withStorage(store: Record<string, string>) {
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (k: string) => store[k] ?? null,
      setItem: (k: string, v: string) => void (store[k] = v),
    },
  });
}

afterEach(() => Object.defineProperty(globalThis, "localStorage", { value: saved, configurable: true }));

describe("the Steam Controller pass-through setting", () => {
  test("is off until it is turned on, and remembered", () => {
    const store: Record<string, string> = {};
    withStorage(store);
    expect(loadSteamPassthrough()).toBe(false);
    saveSteamPassthrough(true);
    expect(store[STEAM_PASSTHROUGH_KEY]).toBe("1");
    expect(loadSteamPassthrough()).toBe(true);
    saveSteamPassthrough(false);
    expect(loadSteamPassthrough()).toBe(false);
  });

  test("anything but 1 is off", () => {
    withStorage({ [STEAM_PASSTHROUGH_KEY]: "true" });
    expect(loadSteamPassthrough()).toBe(false);
  });

  test("blocked storage is off and doesn't throw", () => {
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: {
        getItem: () => {
          throw new Error("denied");
        },
        setItem: () => {
          throw new Error("denied");
        },
      },
    });
    expect(loadSteamPassthrough()).toBe(false);
    expect(() => saveSteamPassthrough(true)).not.toThrow();
  });
});
