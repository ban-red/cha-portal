import { describe, expect, test } from "bun:test";

import { ApiError } from "./api";
import { addBody, approvalGrant, availability, errorText, needsApproval, sourceLabel } from "./catalogs";

describe("approvalGrant", () => {
  test("says what each elevated profile grants", () => {
    expect(approvalGrant("browser")).toBe("lets this app's container create user namespaces (browser sandboxes)");
    expect(approvalGrant("steam")).toContain("cha-sandbox AppArmor profile");
    expect(approvalGrant("standard")).toBeNull();
    expect(needsApproval({ security: "steam" })).toBe(true);
    expect(needsApproval({ security: "standard" })).toBe(false);
  });
});

describe("addBody", () => {
  test("leaves a blank slug out", () => {
    expect(addBody("url", "  ", " https://x.test/c.json ")).toEqual({ url: "https://x.test/c.json" });
    expect(addBody("paste", "mine", "{}")).toEqual({ slug: "mine", document: "{}" });
  });
});

test("errorText uses the server's message as is", () => {
  expect(errorText(new ApiError(409, "in_use", "an environment is running"), "x")).toBe("an environment is running");
  expect(errorText("odd", "fallback")).toBe("fallback");
});

test("labels", () => {
  expect(sourceLabel({ url: null })).toBe("Pasted");
  expect(sourceLabel({ url: "https://x.test/c.json" })).toBe("https://x.test/c.json");
  expect(availability({ templates: [{ available: true }, { available: false }] as never })).toBe("2 apps, 1 available");
});
