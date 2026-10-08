import { describe, expect, test } from "bun:test";

import { expectedVersion, updateLine, updateRunning } from "./nodeUpdate";

const base = { agentVersion: "0.2.0", online: true, updateTo: null, updateBlocked: null, updateProgress: null };

describe("updateLine", () => {
  test("says nothing for a current node", () => {
    expect(updateLine(base, null)).toBeNull();
  });
  test("offers the update with both versions", () => {
    const line = updateLine({ ...base, updateTo: "0.2.1" }, null);
    expect(line).toMatchObject({ text: "Update available: v0.2.0 → v0.2.1", action: "update", tone: "info" });
  });
  test("gives the reason when it can't, without a button", () => {
    const line = updateLine({ ...base, updateBlocked: "a local build: update it by hand" }, null);
    expect(line).toMatchObject({ text: "a local build: update it by hand", action: null, tone: "muted" });
  });
  test("counts the download when the node gives bytes", () => {
    const line = updateLine({ ...base, updateProgress: { state: "pulling", done: 120e6, total: 262e6 } }, null);
    expect(line?.text).toBe("Downloading the new agent");
    expect(line?.progress).toEqual({ done: 120e6, total: 262e6, unit: "bytes" });
    expect(line?.action).toBeNull();
  });
  test("pulling without bytes has no bar", () => {
    const line = updateLine({ ...base, updateProgress: { state: "pulling" } }, null);
    expect(line).toMatchObject({ text: "Downloading the new agent…", progress: null });
  });
  test("restarting", () => {
    expect(updateLine({ ...base, updateProgress: { state: "swapping" } }, null)?.text).toBe("Restarting the agent…");
  });
  test("failures are warnings with a retry while the node is online", () => {
    const failed = { ...base, updateProgress: { state: "failed" as const, detail: "no space left" } };
    expect(updateLine(failed, null)).toMatchObject({ tone: "warn", text: "The update failed: no space left", action: "retry" });
    const back = { ...base, updateProgress: { state: "rolled-back" as const } };
    expect(updateLine(back, null)?.text).toContain("previous one is running again");
    expect(updateLine({ ...failed, online: false }, null)?.action).toBeNull();
  });
  test("remembers the arrival", () => {
    expect(updateLine(base, "0.2.1")).toMatchObject({ text: "Updated to v0.2.1", tone: "ok", action: null });
  });
});

describe("running updates", () => {
  test("only pulling and swapping count", () => {
    expect(updateRunning({ updateProgress: { state: "pulling" } })).toBe(true);
    expect(updateRunning({ updateProgress: { state: "swapping" } })).toBe(true);
    expect(updateRunning({ updateProgress: { state: "failed" } })).toBe(false);
    expect(updateRunning({ updateProgress: null })).toBe(false);
  });
  test("the expected version is known while it runs", () => {
    expect(expectedVersion({ updateTo: "0.2.1", updateProgress: { state: "pulling" } })).toBe("0.2.1");
    expect(expectedVersion({ updateTo: "0.2.1", updateProgress: null })).toBeNull();
  });
});
