import { describe, expect, test } from "bun:test";

import { GenericHidDriver, hatDirections, parseInputFields, readBits } from "./hid-generic";
import type { HidCollectionInfo, HidDevice, HidReportItem } from "./hid-types";
import { BTN } from "./types";

const item = (o: Partial<HidReportItem> & { reportSize: number; reportCount: number }): HidReportItem => ({
  isConstant: false,
  isArray: false,
  isRange: false,
  hasNull: false,
  logicalMinimum: 0,
  logicalMaximum: 1,
  ...o,
});

const GD = 0x10000;
const SIM = 0x20000;
const BUTTON = 0x90000;

/** Report 1: X Y (signed 8 bit), Z Rz (8 bit), a hat (1..8, 0 is null), 4 bits padding, 8 buttons. */
const dinput: HidCollectionInfo[] = [
  {
    usagePage: 0x01,
    usage: 0x05,
    inputReports: [
      {
        reportId: 1,
        items: [
          item({ reportSize: 8, reportCount: 2, usages: [GD | 0x30, GD | 0x31], logicalMinimum: -128, logicalMaximum: 127 }),
          item({ reportSize: 8, reportCount: 2, usages: [GD | 0x32, GD | 0x35], logicalMinimum: 0, logicalMaximum: 255 }),
          item({ reportSize: 4, reportCount: 1, usages: [GD | 0x39], logicalMinimum: 1, logicalMaximum: 8, hasNull: true }),
          item({ reportSize: 4, reportCount: 1, isConstant: true }),
          item({ reportSize: 1, reportCount: 8, isRange: true, usageMinimum: BUTTON | 1, usageMaximum: BUTTON | 8 }),
        ],
      },
    ],
  },
];

const device = (collections: HidCollectionInfo[], vendorId = 0x1234, productId = 0x5678): HidDevice =>
  ({ vendorId, productId, productName: "Test pad", collections }) as unknown as HidDevice;

const view = (...bytes: number[]) => new DataView(new Uint8Array(bytes).buffer);

describe("readBits", () => {
  test("little-endian bit fields across bytes", () => {
    const d = view(0b1010_0101, 0b0000_0011);
    expect(readBits(d, 0, 4, false)).toBe(0b0101);
    expect(readBits(d, 4, 8, false)).toBe(0b0011_1010);
    expect(readBits(d, 0, 10, false)).toBe(0b11_1010_0101);
  });
  test("sign extension", () => {
    expect(readBits(view(0xff), 0, 8, true)).toBe(-1);
    expect(readBits(view(0x80), 0, 8, true)).toBe(-128);
    expect(readBits(view(0x7f), 0, 8, true)).toBe(127);
  });
});

describe("parseInputFields", () => {
  test("positions follow the items, with constants as padding", () => {
    const fields = parseInputFields(dinput);
    expect(fields.map((f) => [f.page, f.usage, f.bit, f.size])).toEqual([
      [1, 0x30, 0, 8],
      [1, 0x31, 8, 8],
      [1, 0x32, 16, 8],
      [1, 0x35, 24, 8],
      [1, 0x39, 32, 4],
      ...[1, 2, 3, 4, 5, 6, 7, 8].map((n, i) => [9, n, 40 + i, 1]),
    ]);
    expect(fields[0]!.signed).toBe(true);
    expect(fields[2]!.signed).toBe(false);
  });

  test("a maximum below the minimum is an unsigned field", () => {
    const fields = parseInputFields([
      {
        usagePage: 0x01,
        usage: 0x04,
        inputReports: [{ reportId: 0, items: [item({ reportSize: 8, reportCount: 1, usages: [GD | 0x30], logicalMinimum: 0, logicalMaximum: -1 })] }],
      },
    ]);
    expect(fields[0]).toMatchObject({ signed: false, min: 0, max: 255 });
  });

  test("report ids keep separate offsets", () => {
    const fields = parseInputFields([
      {
        usagePage: 0x01,
        usage: 0x05,
        inputReports: [
          { reportId: 1, items: [item({ reportSize: 8, reportCount: 1, usages: [GD | 0x30] })] },
          { reportId: 2, items: [item({ reportSize: 8, reportCount: 1, usages: [GD | 0x31] })] },
        ],
      },
    ]);
    expect(fields.map((f) => [f.reportId, f.bit])).toEqual([
      [1, 0],
      [2, 0],
    ]);
  });
});

describe("hatDirections", () => {
  test("eight directions, and the null state", () => {
    expect(hatDirections(1, 1, 8)).toEqual([true, false, false, false]);
    expect(hatDirections(2, 1, 8)).toEqual([true, false, false, true]);
    expect(hatDirections(3, 1, 8)).toEqual([false, false, false, true]);
    expect(hatDirections(5, 1, 8)).toEqual([false, true, false, false]);
    expect(hatDirections(8, 1, 8)).toEqual([true, false, true, false]);
    expect(hatDirections(0, 1, 8)).toEqual([false, false, false, false]);
    expect(hatDirections(15, 1, 8)).toEqual([false, false, false, false]);
  });
  test("four directions", () => {
    expect(hatDirections(1, 0, 3)).toEqual([false, false, false, true]);
    expect(hatDirections(3, 0, 3)).toEqual([false, false, true, false]);
  });
});

describe("GenericHidDriver", () => {
  test("decodes signed axes, a hat and buttons of report 1", () => {
    const driver = new GenericHidDriver(device(dinput));
    // X 127, Y -128, Z 255, Rz 0, hat 3 (right), buttons 1 and 3.
    driver.onReport(1, view(0x7f, 0x80, 0xff, 0x00, 0x03, 0b0000_0101));
    // Padding is 4 bits of byte 4, so the buttons are byte 5.
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.axes[1]).toBe(-1);
    expect(s.axes[2]).toBe(1);
    expect(s.axes[3]).toBe(-1);
    expect(s.buttons[BTN.right]).toBe(1);
    expect(s.buttons[BTN.up]).toBe(0);
    expect(s.buttons[BTN.south]).toBe(1);
    expect(s.buttons[BTN.east]).toBe(0);
    expect(s.buttons[BTN.west]).toBe(1);
  });

  test("a centred stick and a null hat read as neutral", () => {
    const driver = new GenericHidDriver(device(dinput));
    driver.onReport(1, view(0x00, 0x00, 0x80, 0x80, 0x00, 0x00));
    const s = driver.state();
    expect(Math.abs(s.axes[0]!)).toBeLessThan(0.01);
    expect(Math.abs(s.axes[1]!)).toBeLessThan(0.01);
    expect(s.buttons.slice(BTN.up, BTN.right + 1)).toEqual([0, 0, 0, 0]);
  });

  test("reports of another id leave it alone", () => {
    const driver = new GenericHidDriver(device(dinput));
    driver.onReport(7, view(0x7f, 0x7f, 0xff, 0xff, 0x01, 0xff));
    expect(driver.state().axes).toEqual([0, 0, 0, 0]);
    expect(driver.state().buttons.every((b) => b === 0)).toBe(true);
  });

  test("Simulation Controls triggers are analog, and shift the buttons to an Xbox layout", () => {
    // Report 1: X Y Z Rz (16 bit), brake and accelerator (10 bit + 6 padding each), 10 buttons.
    const xbox: HidCollectionInfo[] = [
      {
        usagePage: 0x01,
        usage: 0x05,
        inputReports: [
          {
            reportId: 1,
            items: [
              item({ reportSize: 16, reportCount: 4, usages: [GD | 0x30, GD | 0x31, GD | 0x32, GD | 0x35], logicalMinimum: 0, logicalMaximum: 65535 }),
              item({ reportSize: 10, reportCount: 1, usages: [SIM | 0xc5], logicalMinimum: 0, logicalMaximum: 1023 }),
              item({ reportSize: 6, reportCount: 1, isConstant: true }),
              item({ reportSize: 10, reportCount: 1, usages: [SIM | 0xc4], logicalMinimum: 0, logicalMaximum: 1023 }),
              item({ reportSize: 6, reportCount: 1, isConstant: true }),
              item({ reportSize: 1, reportCount: 10, isRange: true, usageMinimum: BUTTON | 1, usageMaximum: BUTTON | 10 }),
            ],
          },
        ],
      },
    ];
    const driver = new GenericHidDriver(device(xbox));
    const d = new DataView(new ArrayBuffer(8 + 4 + 2));
    d.setUint16(0, 0xffff, true); // X: right
    d.setUint16(2, 0, true); // Y: up
    d.setUint16(4, 0x8000, true);
    d.setUint16(6, 0x8000, true);
    d.setUint16(8, 1023, true); // brake: full
    d.setUint16(10, 0, true);
    // Buttons 1 (A), 7 (View) and 9 (L3).
    d.setUint16(12, 0b01_0100_0001, true);
    driver.onReport(1, d);
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.axes[1]).toBe(-1);
    expect(s.buttons[BTN.leftTrigger]).toBe(1);
    expect(s.buttons[BTN.rightTrigger]).toBe(0);
    expect(s.buttons[BTN.south]).toBe(1);
    expect(s.buttons[BTN.back]).toBe(1);
    expect(s.buttons[BTN.leftStick]).toBe(1);
    expect(s.buttons[BTN.start]).toBe(0);
  });

  test("a device with nothing to map has no bindings", () => {
    const driver = new GenericHidDriver(device([{ usagePage: 0xff00, usage: 1, inputReports: [] }]));
    expect(driver.mappedFields).toBe(0);
  });
});
