import { describe, expect, test } from "bun:test";

import { diskLabel, diskView, platformLine } from "./nodeInfo";

const GB = 2 ** 30;

describe("platformLine", () => {
  const os = "Debian GNU/Linux 13 (trixie)";
  test("one phrase per kind", () => {
    expect(platformLine(os, { kind: "bare-metal" })).toBe(`${os}, bare metal`);
    expect(platformLine(os, { kind: "lxc" })).toBe(`${os} in an LXC container`);
    expect(platformLine(os, { kind: "wsl" })).toBe(`${os} on Windows (WSL 2)`);
    expect(platformLine(os, { kind: "docker-desktop" })).toBe(`${os} in Docker Desktop`);
    expect(platformLine(os, { kind: "unknown" })).toBe(os);
  });
  test("a VM with and without detail", () => {
    expect(platformLine("Ubuntu 24.04", { kind: "vm" })).toBe("Ubuntu 24.04 in a VM");
    expect(platformLine("Ubuntu 24.04", { kind: "vm", detail: "KVM" })).toBe("Ubuntu 24.04 in a VM (KVM)");
  });
  test("does not say VM twice", () => {
    expect(platformLine("Ubuntu 24.04", { kind: "vm", detail: "Proxmox VE VM (QEMU)" })).toBe("Ubuntu 24.04 in a Proxmox VE VM (QEMU)");
  });
});

describe("diskLabel", () => {
  test("by use", () => {
    expect(diskLabel(["images", "appData"])).toBe("Images and app data");
    expect(diskLabel(["images"])).toBe("Images");
    expect(diskLabel(["appData"])).toBe("App data");
  });
});

describe("diskView", () => {
  test("sizes, bar and path", () => {
    const v = diskView({ uses: ["images"], path: "/var/lib/docker", totalBytes: 500 * GB, freeBytes: 125 * GB });
    expect(v.text).toContain("of 500 GB free");
    expect(v.usedPct).toBeCloseTo(75);
    expect(v.low).toBe(false);
    expect(v.path).toBe("/var/lib/docker");
  });
  test("one decimal under 100 GB", () => {
    expect(diskView({ uses: ["appData"], path: "/", totalBytes: 50 * GB, freeBytes: 20.5 * GB }).text).toBe("20.5 GB of 50.0 GB free");
  });
  test("low under 10 GB", () => {
    expect(diskView({ uses: ["appData"], path: "/", totalBytes: 50 * GB, freeBytes: 9 * GB }).low).toBe(true);
    expect(diskView({ uses: ["appData"], path: "/", totalBytes: 50 * GB, freeBytes: 10 * GB }).low).toBe(false);
  });
  test("an empty total does not divide by zero", () => {
    expect(diskView({ uses: [], path: "/", totalBytes: 0, freeBytes: 0 }).usedPct).toBe(0);
  });
});
