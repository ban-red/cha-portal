# Share links for players: the contract

For [ADR 0014](../adr/0014-share-links-for-players.md) and, for the viewer and controller roles, [ADR 0015](../adr/0015-share-links-for-viewers-and-controllers.md) (last section). The portal, streamer and browser are built against this; change it here first.

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

## Viewer and controller roles (ADR 0015)

### Media token

`role: "viewer"` or `"controller"`, `slot` absent, `sub` = `share:<share id>`. A streamer that doesn't know `controller` reads it as a viewer.

### Portal

- Migration `0015_share_roles.sql` rebuilds `shares`: `role IN ('player', 'viewer', 'controller')`, `slot` nullable and set only for a player (`CHECK`), the rows kept. Live-link limits are unique indexes over live rows: one per (environment, slot) for players, one per environment for a controller; viewers have none.
- `POST /api/environments/{id}/shares` takes `{ "role": "player", "slot": 1 }`, `{ "role": "viewer" }` or `{ "role": "controller" }`. A slot with a viewer or controller, or none with a player, is 400 `bad_slot`; any other role 400 `bad_role`. A new player link replaces the live one on its slot, a new controller link the live controller link; a viewer link replaces nothing. Replies and `GET .../shares` carry `slot: null` for the new roles; the list is players by slot, the controller, then viewers by age.
- `GET /api/shares/{token}` returns `role` and `slot` (null) accordingly; `connect` signs the token with the link's role. `share.created`, `share.joined` (now with `role`) and `share.revoked` as before.

### Streamer: the floor

`Role` ranks `Player(n) < Viewer < Controller < Admin < Owner`.

- **Viewer:** never has the floor, `take_control` is refused, all its `input`, `resize`, `clipboard`, `cursor`, `fps` and `overlay` are ignored, and it gets no pad feedback (`rumble`, `haptic`, `led`, `players`, `trigger`).
- **Controller:** may hold the floor. `take_control` works when nobody holds it or a controller does; not when an owner or admin does. On joining it takes the floor only if nobody holds it. It is never given the floor when the holder leaves (only the newest owner or admin is).
- **Owner and admin:** `take_control` always works. An owner joining takes the floor from anyone; an admin joining takes it when it is empty or held by a controller.
- **Hand-off:** `{"t":"give_control","to":<session id>}` from the session holding the floor, if it is an owner or admin and `to` is a controller session, moves the floor. Anything else is ignored.
- **Messages to the page.** `floor` gains `"can_take": true` (left out when false): this session could take the floor now. A new `{"t":"viewers","list":[{"id":3,"role":"controller"},{"id":5,"role":"player","slot":2}]}` goes to the session holding the floor, right after each `floor`, listing every other session (`role`: `owner`, `admin`, `controller`, `viewer`, `player`; `slot` only for a player). The ids are what `give_control` names. Defined in `crates/cha-streamer/src/control.rs`.

### Browser

- **Share dialog:** three groups: *Play on a gamepad* (Invite player 2/3/4), *Watch* (Invite to watch; any number of links) and *Control* (Invite to control; one). The controller text: "Anyone with this link can use your keyboard and mouse in <app> when you hand them the controls, or whenever you aren't holding them."
- **Guest page:** a viewer is invited "to watch" and plays with `@cha/player` option `input: "none"` (no input of any kind, no gamepads read). A controller uses `input: "all"` with `fixedSize` (it never resizes the owner's screen); with the controls it shows "You have the controls.", when `floor.can_take` it shows **Take control**, otherwise "Waiting for <owner> to hand you the controls".
- **Owner's toolbar:** while it holds the floor it lists the other sessions from `viewers` and shows **Hand controls** by a controller; without the floor the button reads **Take back**.
- `@cha/player`: `onViewers(list)`, `giveControl(id)`, and `onFloor`'s fourth argument `canTake`.
