import { afterEach, describe, expect, test } from "bun:test";

import type { HidCollectionInfo, HidDevice } from "./hid-types";
import { SteamTritonDriver, isPuckMonitor, steamTritonFactory } from "./steam-triton";

const settle = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** A slot of a puck: refuses writes until a controller is behind it, and then answers late. */
function slot(options: { collections?: HidCollectionInfo[]; controller?: boolean; lateReads?: number; echo?: boolean } = {}) {
  const state = { controller: options.controller ?? false, lateReads: options.lateReads ?? 2, reads: 0 };
  const features: { id: number; data: number[] }[] = [];
  const collections = options.collections ?? [
    { usagePage: 0x01, usage: 0x02 },
    { usagePage: 0x01, usage: 0x06 },
    { usagePage: 0xff00, usage: 0x01 },
  ];
  let last = 0;
  const device = {
    vendorId: 0x28de,
    productId: 0x1304,
    productName: "Steam Controller Puck",
    collections,
    sendFeatureReport: async (id: number, data: Uint8Array) => {
      if (!state.controller) throw new Error("NotAllowedError: failed to write the report");
      features.push({ id, data: [...data] });
      last = data[0]!;
      state.reads = 0;
    },
    receiveFeatureReport: async (_id: number) => {
      if (!state.controller || state.reads++ < state.lateReads) throw new Error("NotAllowedError: failed to receive the report");
      const reply = new Uint8Array(63);
      reply[0] = options.echo === false ? 0 : last;
      return new DataView(reply.buffer);
    },
    sendReport: async () => {},
  } as unknown as HidDevice;
  return { device, state, features };
}

const drivers: { close(): Promise<void> }[] = [];
afterEach(async () => {
  for (const d of drivers.splice(0)) await d.close();
});

const wireless = (state: number) => new DataView(new Uint8Array([state]).buffer);

describe("the puck's own interface", () => {
  const monitor = [{ usagePage: 0xff00, usage: 2 }];

  test("is the one with a vendor collection and no mouse or keyboard", () => {
    expect(isPuckMonitor(slot({ collections: monitor }).device)).toBe(true);
    expect(isPuckMonitor(slot().device)).toBe(false);
    // Nothing is known of a device that lists no collections: it is a slot.
    expect(isPuckMonitor(slot({ collections: [] }).device)).toBe(false);
    // Only the puck has one.
    const cable = { ...slot({ collections: monitor }).device, productId: 0x1302 } as unknown as HidDevice;
    expect(isPuckMonitor(cable)).toBe(false);
  });

  test("gets no driver, so it is never opened or counted as a controller", () => {
    expect(steamTritonFactory.matches(slot({ collections: monitor }).device)).toBe(false);
    expect(steamTritonFactory.matches(slot().device)).toBe(true);
  });
});

describe("a puck slot opened for a relay (deferClaim)", () => {
  test("has the relay hooks, is waiting, and writes nothing to an empty slot", async () => {
    const { device, features } = slot();
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    await driver.open();
    await settle(30);
    expect(driver.relay?.waiting()).toBe(true);
    expect(driver.connected).toBe(false);
    // Nothing reached the (refusing) slot, and no settings were attempted.
    expect(features).toEqual([]);
  });

  test("finds a controller that is already behind it by asking for its attributes, and still does not claim it", async () => {
    const { device, features } = slot({ controller: true, lateReads: 2 });
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    let changes = 0;
    driver.onConnection = () => changes++;
    await driver.open();
    await settle(80);
    expect(driver.connected).toBe(true);
    expect(changes).toBe(1);
    // Only the read-only command went out: no lizard mode, no IMU settings.
    expect(features).toEqual([{ id: 1, data: [0x83, ...new Array(62).fill(0)] }]);
    expect(driver.relay?.waiting()).toBe(true);
  });

  test("a slot that takes the command but never echoes it is not a controller", async () => {
    const { device } = slot({ controller: true, echo: false });
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    await driver.open();
    await settle(200);
    expect(driver.connected).toBe(false);
  });

  test("a controller that wakes later is found by its wireless status, and is not claimed behind the relay's back", async () => {
    const { device, features } = slot();
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    let changes = 0;
    driver.onConnection = () => changes++;
    await driver.open();
    driver.onReport(0x79, wireless(2));
    expect(driver.connected).toBe(true);
    expect(changes).toBe(1);
    await settle(20);
    expect(features).toEqual([]);
    // It leaves.
    driver.onReport(0x79, wireless(1));
    expect(driver.connected).toBe(false);
    expect(changes).toBe(2);
  });

  test("claim() takes the slot over after all, when the relay doesn't", async () => {
    const { device, features, state } = slot();
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    await driver.open();
    state.controller = true;
    driver.onReport(0x79, wireless(2));
    await driver.relay!.claim();
    expect(driver.relay!.waiting()).toBe(false);
    // Lizard mode off, then the IMU on.
    expect(features.map((f) => f.data.slice(0, 5))).toEqual([
      [0x87, 3, 9, 0, 0],
      [0x87, 3, 48, 0x18, 0],
    ]);
  });

  test("claim() on a slot with nothing behind it quietly waits for one", async () => {
    const { device } = slot();
    const driver = new SteamTritonDriver(device, { deferClaim: true });
    drivers.push(driver);
    await driver.open();
    await driver.relay!.claim();
    expect(driver.connected).toBe(false);
  });
});

describe("a puck slot opened for the parsed path", () => {
  test("claims a controller as soon as its wireless status says it is there, as before", async () => {
    const { device, features, state } = slot();
    const driver = new SteamTritonDriver(device);
    drivers.push(driver);
    await driver.open();
    state.controller = true;
    driver.onReport(0x79, wireless(2));
    await settle(20);
    expect(driver.connected).toBe(true);
    expect(features.length).toBeGreaterThanOrEqual(2);
    expect(features[0]!.data.slice(0, 5)).toEqual([0x87, 3, 9, 0, 0]);
  });
});
