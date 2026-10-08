# Share links for players: the contract

For [ADR 0014](../adr/0014-share-links-for-players.md). The portal, streamer and browser are built against this; change it here first.

## Media token claims (`cha-wire`)

`MediaClaims` gains `slot: Option<u8>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`). A share's token has `role: "player"`, `slot: Some(n)` with `n` in `1..=3` (player 2-4), `sub` = `share:<share id>`. Owner and admin tokens are unchanged (`slot` absent).

## Portal (`cha-control`)

- Migration `0014_shares.sql`: `shares (id TEXT PK, environment_id TEXT NOT NULL REFERENCES environments ON DELETE CASCADE, created_by TEXT NOT NULL REFERENCES users ON DELETE CASCADE, role TEXT NOT NULL CHECK (role = 'player'), slot INTEGER NOT NULL CHECK (slot BETWEEN 1 AND 3), token_hash TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, revoked_at INTEGER)`, at most one live share per (environment, slot). Check what `environments` is called in the existing migrations and match it.
- Owner or admin, cookie session (`CurrentUser`):
  - `POST /api/environments/{id}/shares` `{ "role": "player", "slot": 1 }` → `{ "id", "url": "/s/<token>", "role", "slot", "expires_at" }`. The environment must be running (409 `not_running`). A live share on that slot is replaced (the old one revoked). `expires_at` = now + 24 h. The token is returned only here.
  - `GET /api/environments/{id}/shares` → the live ones: `[{ "id", "role", "slot", "created_at", "expires_at" }]`, never the token.
  - `DELETE /api/environments/{id}/shares/{share id}` → 204.
  - When an environment leaves `running` (stopping, destroyed, failed), its shares are revoked.
- Anyone with the token (no auth):
  - `GET /api/shares/{token}` → `{ "app": "<template name>", "owner": "<display name>", "role": "player", "slot": 1, "state": "<environment state>" }`, or 404 `unknown_share` (unknown, revoked, expired or its environment stopped: one answer for all).
  - `POST /api/shares/{token}/connect` with the same body as `POST /api/environments/{id}/connect` (codec, transport, WebRTC offer) → the same response, with a media token whose claims are as above. 404 `unknown_share` as above; 409 `not_running`.
  - Rate-limited per address (e.g. 30 requests a minute), so tokens can't be guessed cheaply even though they're 256-bit.
- Audit: `share.created`, `share.revoked`, `share.joined` (on each connect, with the address). Tokens never appear in logs or the audit log.

## Streamer (`cha-streamer`)

- `Role::Player(slot)` from the claims (`role == "player"` and a slot in 1..=3; otherwise Viewer).
- A player session never holds or takes the floor and doesn't count as a controller candidate.
- From a player session only `{"t":"input","k":"pad"}` lines are applied: its pad index 0 is put on its slot, and its other indices are dropped. `gone` releases that slot. Every other input, `resize`, `clipboard`, `fps`, `overlay`, `cursor`, `codec` changes from it are ignored (the `codec` switch for its own stream is fine if per-session; otherwise ignore).
- The floor's pads on a slot a live player session holds are dropped.
- Pad feedback (`rumble`, `haptic`, `led`, `players`, `trigger`) for a slot a player holds goes to that player's session, as index 0; the rest to the controller, as today.
- When a player session leaves, its slot is released (the pad goes back to rest).
- `floor` messages to a player say `control: false`; add `"player": n` so the page can say "You are player n+1".

## Browser (`web/apps/portal`, `web/packages/player`)

- Owner: on the environment's session page (and the dashboard card's menu), a **Share** dialog: "Invite player 2/3/4", **Create link** (shows the full URL once, Copy), the live links with their slot and expiry, **Revoke**. Text: "Anyone with this link can play as player N until the environment stops (at most 24 hours)."
- Guest page `/s/:token`, outside the signed-in shell: app name, "<owner> invited you to play as player N", **Join** → plays with `@cha/player` using `POST /api/shares/{token}/connect`, sending only gamepads (a player option such as `input: "pads"`: no keyboard, mouse, pointer lock, clipboard or resize). Errors: unknown/expired → "This link has expired or was revoked"; not running → "The game isn't running right now".
