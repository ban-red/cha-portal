import { describe, expect, test } from "bun:test";

import { ApiError, type CustomOverrides, type Template } from "./api";
import {
  checkEnv,
  cleanOverrides,
  customId,
  dataNote,
  envRowProblems,
  envToRows,
  isCustomId,
  isOverridden,
  launchErrorText,
  overrideCount,
  portLines,
  resetOverride,
  resolve,
  rowsToEnv,
  securityNote,
  setOverride,
  slugProblem,
  suggestSlug,
  summaryLine,
  widerSecurity,
} from "./customEnv";

describe("slug", () => {
  test("pattern and reserved names", () => {
    for (const ok of ["steam", "a", "my-steam-2", "a".repeat(40)]) expect(slugProblem(ok)).toBeNull();
    for (const bad of ["", "Steam", "-a", "a-", "a_b", "a".repeat(41), "migrated", "migrating", "a b"]) expect(slugProblem(bad)).not.toBeNull();
  });
  test("suggests from a name", () => {
    expect(suggestSlug("Steam (Big Picture)")).toBe("steam-big-picture");
    expect(suggestSlug("Café  Éclair")).toBe("cafe-eclair");
    expect(suggestSlug("***")).toBe("custom");
    expect(slugProblem(suggestSlug("x".repeat(60)))).toBeNull();
    expect(isCustomId(customId("a"))).toBe(true);
    expect(isCustomId("steam")).toBe(false);
  });
});

describe("variables", () => {
  test("names the node owns or that aren't names are refused", () => {
    expect(checkEnv({ PROTON_LOG: "1", _x1: "" })).toBeNull();
    expect(checkEnv({ CHA_WIDTH: "1" })).toContain("set by the node");
    for (const r of ["HOME", "USER", "XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "PULSE_SERVER", "DISPLAY"]) expect(checkEnv({ [r]: "x" })).not.toBeNull();
    expect(checkEnv({ "1BAD": "x" })).not.toBeNull();
    expect(checkEnv({ "A-B": "x" })).not.toBeNull();
    expect(checkEnv({ A: "a\0b" })).toContain("NUL");
  });
  test("limits", () => {
    const many = Object.fromEntries(Array.from({ length: 65 }, (_, i) => [`V${i}`, "x"]));
    expect(checkEnv(many)).toContain("64");
    expect(checkEnv({ BIG: "x".repeat(16 * 1024) })).toContain("KiB");
  });
  test("rows", () => {
    const rows = [
      { key: " A ", value: "1" },
      { key: "", value: "" },
      { key: "A", value: "2" },
      { key: "CHA_X", value: "" },
      { key: "", value: "orphan" },
    ];
    expect(envRowProblems(rows)).toEqual({
      2: "A is listed twice.",
      3: "CHA_X is set by the node",
      4: "Give the variable a name.",
    });
    expect(rowsToEnv(rows.slice(0, 2))).toEqual({ A: "1" });
    expect(envToRows({ A: "1" })).toEqual([{ key: "A", value: "1" }]);
    expect(envToRows(undefined)).toEqual([]);
  });
});

describe("overrides", () => {
  test("set and reset leave the original alone", () => {
    const o: CustomOverrides = { shmMb: 2048 };
    const a = setOverride(o, "fps", 120);
    expect(a).toEqual({ shmMb: 2048, fps: 120 });
    expect(o).toEqual({ shmMb: 2048 });
    expect(isOverridden(a, "fps")).toBe(true);
    expect(resetOverride(a, "fps")).toEqual({ shmMb: 2048 });
    expect(isOverridden(resetOverride(a, "fps"), "fps")).toBe(false);
    // false and 0 are values, not "inherit".
    expect(isOverridden({ persistent: false }, "persistent")).toBe(true);
  });
  test("clean drops blanks and empty variables, trims text", () => {
    expect(cleanOverrides({ name: "  Mine ", image: "  ", env: {}, fixedSize: false, description: "" })).toEqual({
      name: "Mine",
      fixedSize: false,
      description: "",
    });
    expect(overrideCount({ name: "x", image: "", env: { A: "1" } })).toBe(2);
  });
  test("resolve puts the overrides on the base", () => {
    const base = { id: "steam", name: "Steam", description: "d", image: "i:1", class: "gaming", security: "steam", shmMb: 1024 } as Template;
    const r = resolve(base, { shmMb: 4096, name: "Mine" });
    expect(r.name).toBe("Mine");
    expect(r.shmMb).toBe(4096);
    expect(r.image).toBe("i:1");
    expect(base.shmMb).toBe(1024);
  });
});

describe("security", () => {
  test("wider than the base gets a note", () => {
    expect(widerSecurity("standard", "browser")).toBe(true);
    expect(widerSecurity("steam", "browser")).toBe(false);
    expect(widerSecurity("browser", "browser")).toBe(false);
    expect(securityNote("standard", "steam")).toContain("audit log");
    expect(securityNote("steam", "standard")).toBeNull();
  });
});

describe("dashboard words", () => {
  test("ports", () => {
    expect(
      portLines({
        nodeName: "iolinux",
        streamer: { host: "10.0.0.5", httpPort: 1, webrtcPort: 2 },
        ports: [
          { container: 27015, protocol: "udp", host: 27016 },
          { container: 25565, protocol: "tcp", host: null },
        ],
      }),
    ).toEqual(["10.0.0.5:27016/udp", "10.0.0.5:25565/tcp"]);
    expect(portLines({ nodeName: "n", streamer: null, ports: [{ container: 1, protocol: "tcp", host: 2 }] })).toEqual(["n:2/tcp"]);
    expect(portLines({ nodeName: "n", streamer: null })).toEqual([]);
  });
  test("data note", () => {
    expect(dataNote({ custom: { base: "steam", shareData: true, host: null } }, "Steam")).toBe("uses Steam's data");
    expect(dataNote({ custom: { base: "steam", shareData: false, host: null } }, "Steam")).toBeNull();
    expect(dataNote({}, "Steam")).toBeNull();
  });
  test("data_in_use", () => {
    const text = launchErrorText(new ApiError(409, "data_in_use", "Steam (Mine) is running"), "x");
    expect(text).toContain("Steam (Mine) is running.");
    expect(text).toContain("can't run on the same data");
    expect(launchErrorText(new ApiError(500, "boom", "it broke"), "x")).toBe("it broke");
    expect(launchErrorText("odd", "fallback")).toBe("fallback");
  });
  test("summary line", () => {
    expect(summaryLine({ baseName: "Steam", shareData: true, overrides: { shmMb: 1 }, host: { privileged: true } })).toBe(
      "Based on Steam · 1 change · shares its saved data · host options",
    );
    expect(summaryLine({ baseName: "Steam", shareData: false, overrides: {}, host: null })).toBe(
      "Based on Steam · no changes yet · own saved data",
    );
  });
});
