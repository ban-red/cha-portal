/** Where a streamer URL points: relative paths (`/api/media/<ticket>`) go to `ws(s)://` on `base`'s origin, http(s) becomes ws(s). */
export function resolveWsUrl(url: string, base: string): string {
  const u = new URL(url, base);
  if (u.protocol === "http:") u.protocol = "ws:";
  else if (u.protocol === "https:") u.protocol = "wss:";
  return u.toString();
}
