import { describe, expect, test } from "bun:test";

import { toNodeStats } from "./stats";

describe("toNodeStats", () => {
  test("reads a full report, with the GPU fields", () => {
    const stats = toNodeStats({
      t: "system",
      cpu: 23.5,
      cores: 16,
      load1: 2.1,
      mem_used: 10,
      mem_total: 20,
      gpu: 87,
      vram_used: 5,
      vram_total: 8,
      enc: 31,
      dec: 0,
      temp: 64,
      power: 310.5,
      power_limit: 450,
      clock: 2400,
      streamer_cpu: 4,
    });
    expect(stats).toEqual({
      cpu: 23.5,
      cores: 16,
      load1: 2.1,
      memUsed: 10,
      memTotal: 20,
      gpu: 87,
      vramUsed: 5,
      vramTotal: 8,
      enc: 31,
      dec: 0,
      temp: 64,
      power: 310.5,
      powerLimit: 450,
      clock: 2400,
      streamerCpu: 4,
    });
  });

  test("leaves out the GPU fields a node without NVML doesn't send", () => {
    const stats = toNodeStats({ t: "system", cpu: 1, cores: 4, load1: 0.2, mem_used: 1, mem_total: 2, streamer_cpu: 0 });
    expect(stats).not.toBeNull();
    expect("gpu" in stats!).toBe(false);
    expect("vramUsed" in stats!).toBe(false);
  });

  test("refuses a message without the basics", () => {
    expect(toNodeStats({ t: "system", cores: 4 })).toBeNull();
    expect(toNodeStats({ t: "system", cpu: "high", mem_total: 2 })).toBeNull();
  });
});
