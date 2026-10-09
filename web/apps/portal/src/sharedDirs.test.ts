import { describe, expect, test } from "bun:test";

import { differNote, locationsDiffer, stateBadge, whereLine } from "./sharedDirs";

describe("stateBadge", () => {
  test("one label per state", () => {
    expect(stateBadge("ok", "external")).toEqual({ label: "Mounted", tone: "ok" });
    expect(stateBadge("missing", "external").label).toBe("Not mounted");
    expect(stateBadge("incomplete", "external").label).toBe("Missing folders");
    expect(stateBadge("unreachable", "external").label).toBe("Not responding");
    expect(stateBadge("read_only", "external").label).toBe("Read-only");
  });
  test("no report", () => {
    expect(stateBadge(undefined, "external").label).toBe("Not reported");
    expect(stateBadge(undefined, "local").label).toBe("On the node");
  });
});

describe("whereLine", () => {
  test("external with and without details", () => {
    expect(whereLine({ location: "external", fsType: "nfs4", source: "nas:/games" })).toBe("NAS/share (nfs4, nas:/games)");
    expect(whereLine({ location: "external" })).toBe("NAS/share");
    expect(whereLine({ location: "local", fsType: "ext4" })).toBe("On the node");
  });
});

describe("differNote", () => {
  const nodes = [
    { nodeName: "iolinux", location: "external" as const },
    { nodeName: "amd", location: "local" as const },
    { nodeName: "box", location: "local" as const },
  ];
  test("names both sides", () => {
    expect(locationsDiffer(nodes)).toBe(true);
    expect(differNote(nodes)).toBe("Nodes keep this differently: launches on amd and box won't see the library on iolinux.");
  });
  test("silent when they agree", () => {
    expect(differNote(nodes.slice(1))).toBe("");
  });
});
