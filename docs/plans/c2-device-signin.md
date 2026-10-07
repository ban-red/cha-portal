# C2.1: Cha Player sign-in (device tokens)

The contract between the portal (`cha-control`, the SPA) and Cha Player for [ADR 0013](../adr/0013-native-player-on-cha-stream.md). Both sides build against this; change it here first.

## Credentials

- **Device token:** `chadev_` + 43 characters (256 random bits, URL-safe base64, no padding). Sent as `Authorization: Bearer <token>`. The database keeps its SHA-256 (hex), as for sessions. No expiry; revoked from the Devices page or by a new sign-in from the same install. `last_used_at` and `last_ip` are updated at most once a minute.
- **Install id:** a random UUID the player makes once and keeps. One device row per (user, install id): signing in again from the same install replaces the token on that row.
- **Ticket** (the `cha://` link): 256 random bits, URL-safe base64, one use, 60 seconds, stored hashed.
- **Device code** (polling secret): 256 random bits, one use, 10 minutes, stored hashed. **User code** (typed in the portal): 8 characters from `BCDFGHJKLMNPQRSTVWXZ` shown as `ABCD-EFGH`; case and the dash are ignored on input.

## What a device token opens

A `PlayerUser` extractor accepts a session cookie **or** a device token, on these routes only; everything else stays cookie-only (`CurrentUser`, `AdminUser`):

- `GET /api/me`
- `GET /api/catalog`, `GET /api/catalog/{id}/icon`
- `GET /api/environments`, `POST /api/environments`, `GET /api/environments/{id}`, `DELETE /api/environments/{id}`, `POST /api/environments/{id}/connect`
- `GET /api/ice`
- `GET /api/controllers/apps`, `GET /api/storage`, `GET /api/apps/settings` (read-only preferences)

A revoked or unknown token is `401 {"error":"unauthorized"}`, as for a missing cookie.

## Endpoints

All JSON. Errors are the portal's usual `{"error": "<code>", "message": "..."}`.

### Signed in (cookie)

- `POST /api/devices/tickets` `{ "launch": "<template id>"? }` → `{ "ticket": "...", "expires_in": 60 }`. The SPA builds the link itself:
  `cha://connect?portal=<encodeURIComponent(location.origin)>&ticket=<ticket>[&launch=<template id>]`.
- `GET /api/devices/codes/{user_code}` → `{ "name": "<device name>", "created_at": <unix s>, "expires_at": <unix s> }`, or 404 `unknown_code` (also for expired or used ones).
- `POST /api/devices/codes/{user_code}/approve` → `{ "ok": true }`; `POST /api/devices/codes/{user_code}/deny` → `{ "ok": true }`.
- `GET /api/devices` → `[{ "id", "name", "created_at", "last_used_at", "last_ip" }]`: the caller's own devices.
- `DELETE /api/devices/{id}` → 204: revokes one of the caller's own devices (admins: anyone's, `GET /api/devices?all=1` lists everyone's with `user`).

### Unauthenticated (the player)

- `POST /api/device/ticket` `{ "ticket", "install_id", "name" }` → `{ "token", "device_id", "user": { "id", "username", "role" } }`; `400 invalid_ticket` for unknown, used or expired tickets.
- `POST /api/device/code` `{ "install_id", "name" }` → `{ "device_code", "user_code": "ABCD-EFGH", "verification_path": "/link", "expires_in": 600, "interval": 5 }`.
- `POST /api/device/token` `{ "device_code" }` → the same body as the ticket swap once approved; otherwise `400` with `error` one of `authorization_pending`, `slow_down` (polled faster than `interval`), `access_denied`, `expired_token`.

`name` is what the device calls itself ("Alex's MacBook Pro"), at most 100 characters. Every issue, approval, denial and revocation goes in the audit log.

## The SPA

- **Dashboard:** an "Open in Cha Player" action on each app (and one in the user menu without an app) that asks for a ticket and navigates to the `cha://` link. Shown on macOS only (from the user agent).
- **`/link`:** a page to type a user code, see the device's name, and approve or deny it. Reached from the player's instructions (`<portal>/link`) or the user menu.
- **Settings → Devices:** the caller's devices, with last use and a Revoke button.

## The player

- Keeps per portal origin: the token, device id, username. Never sends a token to any other origin. Plain `http://` portals only with `CHA_ALLOW_INSECURE_PORTAL=true` (or `localhost`).
- `cha://connect?portal=…&ticket=…[&launch=…]`: for a portal it has no token for, asks "Sign in to `<origin>`?" first; swaps the ticket; then shows that portal's apps (and launches `launch` once C2.2 can).
- Device code: "Add portal" with an address; the player shows the user code and `<portal>/link`, polls at `interval`, and stops on `access_denied`/`expired_token`.
