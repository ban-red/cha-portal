import { describe, expect, test } from "bun:test";

import type { HostMount, HostOptions, HostPolicy, NodeHostOptions } from "./api";
import {
  allowedChoices,
  allows,
  anyFull,
  checkShape,
  isEmptyHost,
  mountChoices,
  nodeVerdicts,
  noNodeAccepts,
  normalizeHost,
  policySummary,
  portHint,
  refusals,
} from "./hostOptions";

const named = (name: string, target: string): HostMount => ({ source: { kind: "named", name }, target, readOnly: false });

describe("checkShape", () => {
  test("accepts a named mount", () => {
    expect(checkShape({ mounts: [named("media", "/mnt/media")] })).toBeNull();
  });

  test("refuses reserved, relative and unnormalised targets", () => {
    for (const bad of ["/home/cha/x", "/home", "/run/cha", "/dev/input/event0", "/", "rel", "/a/../b"]) {
      expect(checkShape({ mounts: [named("media", bad)] })).not.toBeNull();
    }
    expect(checkShape({ mounts: [named("m", "/dev/hidraw0")] })).toContain("controllers");
  });

  test("refuses overlapping targets and the Docker socket", () => {
    expect(checkShape({ mounts: [named("a", "/mnt"), named("b", "/mnt/b")] })).toContain("overlap");
    const socket: HostOptions = { mounts: [{ source: { kind: "path", path: "/var/run" }, target: "/x", readOnly: true }] };
    expect(checkShape(socket)).toContain("Docker socket");
  });

  test("ports, capabilities, devices", () => {
    expect(checkShape({ ports: [{ container: 0, protocol: "tcp" }] })).toContain("port 0");
    expect(
      checkShape({
        ports: [
          { container: 80, protocol: "tcp" },
          { container: 80, protocol: "tcp", host: 8080 },
        ],
      }),
    ).toContain("listed twice");
    expect(checkShape({ ports: [{ container: 80, protocol: "tcp" }], networkHost: true })).toContain("host's network");
    expect(checkShape({ capAdd: ["CAP_SYS_NICE"] })).toContain("capability");
    expect(checkShape({ capAdd: ["sys_nice"] })).not.toBeNull();
    expect(checkShape({ devices: ["/etc/passwd"] })).toContain("under /dev");
    expect(checkShape({ devices: ["/dev/dri/card1"] })).toBeNull();
  });

  test("network shares need a device without commas", () => {
    const share = (device: string): HostOptions => ({
      mounts: [{ source: { kind: "network", fsType: "nfs", device, options: "addr=10.0.0.5" }, target: "/mnt/nas" }],
    });
    expect(checkShape(share(":/export"))).toBeNull();
    expect(checkShape(share(""))).not.toBeNull();
    expect(checkShape(share("a,b"))).not.toBeNull();
  });
});

describe("refusals", () => {
  const allowlist: HostPolicy = {
    mode: "allowlist",
    mounts: [{ name: "media", readOnly: true }],
    ports: [{ start: 27015, end: 27030, protocol: "udp" }],
    caps: ["SYS_NICE"],
    devices: [],
  };
  const req: HostOptions = {
    mounts: [named("media", "/mnt/media")],
    ports: [{ container: 27015, protocol: "udp", host: 27016 }],
    capAdd: ["SYS_NICE"],
  };

  test("an allowlist names what it lacks", () => {
    expect(allows(allowlist, req)).toBe(true);
    const wider: HostOptions = {
      ...req,
      ports: [{ container: 80, protocol: "tcp" }],
      capAdd: ["NET_ADMIN"],
      privileged: true,
    };
    expect(refusals(allowlist, wider)).toEqual([
      "doesn't allow tcp ports",
      "doesn't allow NET_ADMIN",
      "allows privileged only in full mode",
    ]);
  });

  test("a requested host port must be inside a range of its protocol", () => {
    const at = (host: number, protocol: "tcp" | "udp" = "udp"): HostOptions => ({ ports: [{ container: 1, protocol, host }] });
    expect(allows(allowlist, at(27030))).toBe(true);
    expect(refusals(allowlist, at(27031))).toEqual(["doesn't allow 27031/udp"]);
    expect(refusals(allowlist, at(27016, "tcp"))).toEqual(["doesn't allow 27016/tcp"]);
  });

  test("named mounts only in an allowlist; paths and shares need full", () => {
    const path: HostOptions = { mounts: [{ source: { kind: "path", path: "/srv/x" }, target: "/x" }] };
    const share: HostOptions = {
      mounts: [{ source: { kind: "network", fsType: "cifs", device: "//nas/media" }, target: "/m" }],
    };
    expect(refusals(allowlist, path)).toEqual(["allows host paths (/srv/x) only in full mode"]);
    expect(refusals(allowlist, share)).toEqual(["allows network shares (//nas/media) only in full mode"]);
    expect(refusals(allowlist, { mounts: [named("roms", "/r")] })).toEqual(["has no mount named roms"]);
    expect(refusals(allowlist, { networkHost: true, securityOpt: ["seccomp=unconfined"] })).toHaveLength(2);
  });

  test("off allows nothing but an empty request; full allows everything", () => {
    const off: HostPolicy = { mode: "off" };
    expect(allows(off, {})).toBe(true);
    expect(allows(off, null)).toBe(true);
    expect(refusals(off, req)).toEqual(["allows no host options"]);
    const full: HostPolicy = { mode: "full" };
    expect(allows(full, { ...req, privileged: true, networkHost: true, securityOpt: ["x"] })).toBe(true);
  });
});

describe("normalizeHost", () => {
  test("empty options become null; false flags and empty lists are dropped", () => {
    expect(isEmptyHost({ mounts: [], privileged: false })).toBe(true);
    expect(normalizeHost({ mounts: [], capAdd: [] })).toBeNull();
    expect(normalizeHost(null)).toBeNull();
    expect(normalizeHost({ capAdd: ["SYS_NICE"], privileged: false, ports: [{ container: 80, protocol: "tcp", host: null }] })).toEqual({
      capAdd: ["SYS_NICE"],
      ports: [{ container: 80, protocol: "tcp" }],
    });
  });
});

describe("across nodes", () => {
  const node = (nodeName: string, policy: HostPolicy | null, specFeatures = ["env", "host-options"]): NodeHostOptions => ({
    nodeId: nodeName,
    nodeName,
    online: true,
    specFeatures,
    policy,
  });
  const nodes = [
    node("a", { mode: "allowlist", mounts: [{ name: "media", readOnly: true }], caps: ["SYS_NICE"], ports: [{ start: 25565, end: 25565, protocol: "tcp" }] }),
    node("b", { mode: "full", mounts: [{ name: "media" }, { name: "roms" }], devices: ["/dev/dri/card1"] }),
    node("c", { mode: "off" }),
    node("d", null),
  ];

  test("mount choices are the union over nodes that allow any, with read-only marked", () => {
    expect(mountChoices(nodes)).toEqual([
      { name: "media", readOnlyOn: ["a"], nodes: ["a", "b"] },
      { name: "roms", readOnlyOn: [], nodes: ["b"] },
    ]);
    expect(anyFull(nodes)).toBe(true);
    expect(anyFull(nodes.slice(0, 1))).toBe(false);
    expect(allowedChoices(nodes, "caps")).toEqual(["SYS_NICE"]);
    expect(allowedChoices(nodes, "devices")).toEqual(["/dev/dri/card1"]);
    expect(portHint(nodes)).toBe("25565/tcp");
  });

  test("verdicts per node, and the none-accepts warning", () => {
    const req: HostOptions = { capAdd: ["SYS_NICE"] };
    const v = nodeVerdicts(nodes, req);
    expect(v.map((x) => x.reasons.length === 0)).toEqual([true, true, false, false]);
    expect(v[2]!.reasons).toEqual(["allows no host options"]);
    expect(noNodeAccepts(v)).toBe(false);
    expect(noNodeAccepts(nodeVerdicts(nodes, { privileged: true }).slice(0, 1))).toBe(true);
    expect(noNodeAccepts([])).toBe(false);
  });

  test("an empty request is accepted everywhere, but variables need the agent to pass them", () => {
    const old = node("old", { mode: "off" }, []);
    expect(nodeVerdicts([old], null)[0]!.reasons).toEqual([]);
    expect(nodeVerdicts([old], null, true)[0]!.reasons).toHaveLength(1);
    expect(nodeVerdicts([node("x", { mode: "full" }, ["env"])], { privileged: true })[0]!.reasons).toHaveLength(1);
  });

  test("policy summary", () => {
    expect(policySummary(null)).toBe("Not reported");
    expect(policySummary({ mode: "off" })).toBe("Off");
    expect(policySummary({ mode: "allowlist", mounts: [{ name: "m" }], caps: ["A", "B"] })).toBe("Allowlist: 1 mount, 2 capabilities");
    expect(policySummary({ mode: "allowlist" })).toBe("Allowlist: nothing named yet");
  });
});
