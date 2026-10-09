import { describe, expect, test } from "bun:test";

import type { Grant } from "./api";
import { accessBody, accessChanged, formFromAccess, grantLabel, hasGrant, limitHint, parseMaxInstances, toggleNode } from "./userAccess";

describe("parseMaxInstances", () => {
  test("empty is the default", () => {
    expect(parseMaxInstances("  ")).toEqual({ ok: true, value: null });
  });
  test("accepts 1 to 64", () => {
    expect(parseMaxInstances("1")).toEqual({ ok: true, value: 1 });
    expect(parseMaxInstances(" 64 ")).toEqual({ ok: true, value: 64 });
  });
  test("refuses 0, 65, decimals and words", () => {
    for (const t of ["0", "65", "2.5", "-1", "many"]) expect(parseMaxInstances(t).ok).toBe(false);
  });
});

describe("access form", () => {
  const saved = formFromAccess({ nodeRestricted: true, nodeIds: ["b", "a"], maxInstances: null });
  test("round trips", () => {
    expect(saved).toEqual({ nodeRestricted: true, nodeIds: ["a", "b"], maxInstances: "" });
    expect(accessBody(saved)).toEqual({ ok: true, body: { nodeRestricted: true, nodeIds: ["a", "b"], maxInstances: null } });
  });
  test("an invalid limit blocks the body", () => {
    expect(accessBody({ ...saved, maxInstances: "0" }).ok).toBe(false);
  });
  test("detects changes regardless of order", () => {
    expect(accessChanged({ ...saved, nodeIds: ["b", "a"] }, saved)).toBe(false);
    expect(accessChanged({ ...saved, nodeIds: ["a"] }, saved)).toBe(true);
    expect(accessChanged({ ...saved, nodeRestricted: false }, saved)).toBe(true);
    expect(accessChanged({ ...saved, maxInstances: "3" }, saved)).toBe(true);
    expect(accessChanged({ ...saved, maxInstances: " " }, saved)).toBe(false);
  });
  test("toggleNode adds once and removes", () => {
    expect(toggleNode(["a"], "b", true)).toEqual(["a", "b"]);
    expect(toggleNode(["a", "b"], "b", true)).toEqual(["a", "b"]);
    expect(toggleNode(["a", "b"], "a", false)).toEqual(["b"]);
  });
});

describe("grants", () => {
  const nodes = [{ id: "n1", name: "Garage" }];
  const templates = [{ id: "steam", name: "Steam" }];
  const grants: Grant[] = [{ id: "g", nodeId: "n1", templateId: "steam", createdAt: 0 }];
  test("labels with names, falling back to ids", () => {
    expect(grantLabel(grants[0]!, nodes, templates).text).toBe("Steam on Garage");
    expect(grantLabel({ nodeId: "x", templateId: "custom:q" }, nodes, templates).text).toBe("custom:q on a removed node");
  });
  test("hasGrant", () => {
    expect(hasGrant(grants, "n1", "steam")).toBe(true);
    expect(hasGrant(grants, "n1", "firefox")).toBe(false);
  });
});

describe("limitHint", () => {
  test("pluralises", () => {
    expect(limitHint({ live: 2, effectiveMax: 4 })).toBe("2 of 4 environments in use now.");
    expect(limitHint({ live: 1, effectiveMax: 1 })).toBe("1 of 1 environment in use now.");
  });
});
