import { describe, expect, test } from "bun:test";

import type { AdoptedHost, Environment } from "./api";
import { hostMatches, launchState, liveByTemplate, visibleApps } from "./moonlight";

const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
const app = (name: string, id = 1) => ({ templateId: `moonlight:h:${id}`, appId: id, name, hdr: false });
const host = (over: Partial<AdoptedHost> = {}): AdoptedHost => ({
  id: "h", name: "Gaming PC", nodeId: "n", nodeName: "iolinux", online: true, codecs: [],
  apps: [app("Steam Big Picture", 1), app("Desktop", 2), app("Café Racer", 3)], appsAt: 1, busy: null, ...over,
});
const env = (templateId: string, state: Environment["state"], createdAt: number) =>
  ({ id: `${templateId}-${createdAt}`, templateId, state, createdAt }) as Environment;

describe("visibleApps", () => {
  test("filters by folded name and sorts by name", () => {
    expect(visibleApps(host(), "", fold, "name", new Map()).map((a) => a.name)).toEqual(["Café Racer", "Desktop", "Steam Big Picture"]);
    expect(visibleApps(host(), "cafe", fold, "name", new Map()).map((a) => a.name)).toEqual(["Café Racer"]);
  });
  test("recent puts the last used first", () => {
    const used = new Map([["moonlight:h:2", 50]]);
    expect(visibleApps(host(), "", fold, "recent", used)[0]?.name).toBe("Desktop");
  });
});

describe("hostMatches", () => {
  test("empty query matches; name matches folded", () => {
    expect(hostMatches(host(), "", fold)).toBe(true);
    expect(hostMatches(host(), "GAMING", fold)).toBe(true);
    expect(hostMatches(host(), "zzz", fold)).toBe(false);
  });
});

describe("liveByTemplate", () => {
  test("keeps the newest live environment and skips ended ones", () => {
    const m = liveByTemplate([env("a", "running", 1), env("a", "starting", 5), env("a", "failed", 9), env("b", "destroyed", 2)]);
    expect(m.get("a")?.createdAt).toBe(5);
    expect(m.has("b")).toBe(false);
  });
});

describe("launchState", () => {
  const a = app("Desktop", 2);
  test("free host launches", () => expect(launchState(host(), a, new Map()).disabled).toBe(false));
  test("offline says so", () => {
    const s = launchState(host({ online: false }), a, new Map());
    expect(s).toMatchObject({ disabled: true, reason: "Host is offline." });
  });
  test("busy by someone else names them", () => {
    const s = launchState(host({ busy: { environmentId: "x", owner: "sam" } }), a, new Map());
    expect(s.reason).toBe("In use by sam.");
  });
  test("own running environment opens even when the host is busy", () => {
    const e = env(a.templateId, "running", 1);
    const s = launchState(host({ busy: { environmentId: e.id, owner: "me" } }), a, new Map([[a.templateId, e]]));
    expect(s.disabled).toBe(false);
    expect(s.instance).toBe(e);
  });
});
