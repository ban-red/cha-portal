import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { UPSCALER_KEY, loadUpscaler, saveUpscaler } from "./streamPrefs";

const saved = (globalThis as Record<string, unknown>).localStorage;
const store = new Map<string, string>();

beforeEach(() => {
  store.clear();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
  });
});

afterEach(() => Object.defineProperty(globalThis, "localStorage", { value: saved, configurable: true }));

describe("the upscaler setting", () => {
  test("is off until turned on, and off again", () => {
    expect(loadUpscaler()).toBeUndefined();
    saveUpscaler(true);
    expect(loadUpscaler()).toBe("sgsr");
    saveUpscaler(false);
    expect(loadUpscaler()).toBeUndefined();
    expect(store.has(UPSCALER_KEY)).toBe(false);
  });
  test("an unknown saved value is off", () => {
    store.set(UPSCALER_KEY, "fsr");
    expect(loadUpscaler()).toBeUndefined();
  });
  test("blocked storage reads as off and saves nothing", () => {
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() {
        throw new Error("blocked");
      },
    });
    expect(loadUpscaler()).toBeUndefined();
    expect(() => saveUpscaler(true)).not.toThrow();
  });
});
