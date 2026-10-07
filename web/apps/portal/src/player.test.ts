import { describe, expect, test } from "bun:test";

import { isMac, normalizeUserCode, playerLink, typedUserCode } from "./player";

describe("normalizeUserCode", () => {
  test("accepts any case, with or without the dash or spaces", () => {
    expect(normalizeUserCode("BCDFGHJK")).toBe("BCDF-GHJK");
    expect(normalizeUserCode("bcdf-ghjk")).toBe("BCDF-GHJK");
    expect(normalizeUserCode(" bcdf ghjk\n")).toBe("BCDF-GHJK");
  });
  test("rejects anything else", () => {
    expect(normalizeUserCode("")).toBeNull();
    expect(normalizeUserCode("BCDF-GHJ")).toBeNull();
    expect(normalizeUserCode("BCDF-GHJKL")).toBeNull();
    expect(normalizeUserCode("BCDF-GHJA")).toBeNull(); // vowels aren't used
    expect(normalizeUserCode("1234-5678")).toBeNull();
  });
});

describe("typedUserCode", () => {
  test("upper-cases, drops the rest and adds the dash after four letters", () => {
    expect(typedUserCode("bcd")).toBe("BCD");
    expect(typedUserCode("bcdfg")).toBe("BCDF-G");
    expect(typedUserCode("bc-df ghjk lmn")).toBe("BCDF-GHJK");
  });
});

describe("playerLink", () => {
  test("encodes the portal and the ticket", () => {
    expect(playerLink("https://portal.example:8443", "abc_-123")).toBe(
      "cha://connect?portal=https%3A%2F%2Fportal.example%3A8443&ticket=abc_-123",
    );
  });
  test("adds the app to launch", () => {
    expect(playerLink("http://localhost:8090", "t", "chrome")).toBe(
      "cha://connect?portal=http%3A%2F%2Flocalhost%3A8090&ticket=t&launch=chrome",
    );
  });
});

describe("isMac", () => {
  test("is true on macOS browsers only", () => {
    expect(isMac("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/150.0 Safari/537.36")).toBe(true);
    expect(isMac("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148")).toBe(false);
    expect(isMac("Mozilla/5.0 (X11; Linux x86_64) Chrome/150.0")).toBe(false);
    expect(isMac("")).toBe(false);
  });
});
