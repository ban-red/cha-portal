import { describe, expect, test } from "bun:test";
import { resolveWsUrl } from "./ws-url";

describe("resolveWsUrl", () => {
  test("a path goes to ws on an http page's origin", () => {
    expect(resolveWsUrl("/api/media/abc", "http://portal.lan:8090/s/tok")).toBe("ws://portal.lan:8090/api/media/abc");
  });

  test("a path goes to wss on an https page's origin", () => {
    expect(resolveWsUrl("/api/media/abc", "https://x.trycloudflare.com/s/tok")).toBe("wss://x.trycloudflare.com/api/media/abc");
  });

  test("absolute http(s) URLs become ws(s)", () => {
    expect(resolveWsUrl("https://node.example/ws", "http://a/")).toBe("wss://node.example/ws");
    expect(resolveWsUrl("http://10.0.0.5:7000/ws", "https://a/")).toBe("ws://10.0.0.5:7000/ws");
  });

  test("ws(s) URLs stay", () => {
    expect(resolveWsUrl("wss://h/p?q=1", "http://a/")).toBe("wss://h/p?q=1");
  });
});
