# Share links over the internet: the contract

For [ADR 0022](../adr/0022-share-links-over-a-cloudflare-tunnel.md). The portal, node, streamer and browser are built against this; change it here first.

## Wire (`cha-wire`, done)

- `NodeRequest::OpenRelay { relay_id, environment_id, codec, media_token }` → `NodeResponse::RelayOpened`.
- `Inventory::relay: bool` (default false): the agent handles `OpenRelay`. The portal never sends `OpenRelay` to a node without it (an old agent drops the connection on an unknown op); it answers 409 `node_outdated` instead.
- `RELAY_PATH = "/api/node/relay"`, `RELAY_NODE_HEADER = "x-cha-node"`, `RELAY_SIGNATURE_HEADER = "x-cha-signature"`, `relay_message(relay_id, node_id)` (what the node signs, Ed25519, base64 as `sign_b64`).

## The WebSocket media protocol (streamer ⇄ browser, passed through untouched)

- One WebSocket. **Text messages** are control lines: exactly the JSON lines of the WebTransport control stream, one per message, no trailing newline, both directions. The streamer's `hello` says `"transport": "websocket"` (and `maxDatagram: 65536`).
- **Binary messages**, streamer → browser only, are `cha-stream/1` datagrams: the 16-byte `DatagramHeader` then the payload, as WebTransport sends them, with **no FEC** (`fec` field 0, no PARITY datagrams) and fragments of at most 65 536 bytes including the header (`Fragmenter::new(65536)`). Audio: one message per Opus packet, as WebTransport. Messages arrive in order and none are lost, but whole frames may be skipped (dropped by the streamer under backpressure), so `frame_id` gaps still mean loss and resync rules apply.
- The browser sends the WebTransport worker's `{"t":"report",…}` every 100 ms, `{"t":"keyframe"}`, `{"t":"rfi","id"}` and everything else the control stream carries. `{"t":"codec"}` in-session switches work as on WebTransport.
- Codecs: `h264`, `hevc`, `av1`. PyroWave is refused (`bad_codec`).
- Every hop (portal, node) forwards each message as-is (text as text, binary as binary), and closes the other side when one side closes or errors. Pings are answered per hop.

## Streamer (`cha-streamer`)

- `GET /ws/media?codec=<c>&token=<media token>` on the existing loopback HTTP router (`signal.rs`), upgraded with axum's `ws`. Same authorisation and session admission as WebTransport's `accept` (`authorize`, `viewers.join`, `MAX_SESSIONS`, the same HTTP statuses before the upgrade).
- A new `ws.rs` session loop modelled on `wt::run`: the same `Control`, `floor_msgs`, pad replay, clipboard/cursor/rumble/status/stats/system messages, codec switch, `parse_report` / `rfi_request` / `codec_request` interception.
- Rate control: a writer task owns the socket and drains a bounded queue. Backlog = bytes queued and not yet written (track it with an atomic counter). Feed `rate::Sample` from the page's reports plus that backlog (`backlog_ms` from queued bytes over the current rate), `rtt` from the report if present else 0, no QUIC stats. Hold the encoder (`pace.set_hold`) while the backlog is over `HOLD_MS`; if a whole frame doesn't fit the queue's budget (say 2 MB or 250 ms at the current rate, whichever is larger), drop the frame and resync exactly as `Video::send` does (ResyncGate, `ask_resync`). Never block the session loop on the socket.
- `/info` gains `"ws": true`.

## Node (`cha-node`)

- Inventory sets `relay: true`.
- On `OpenRelay`: look up the environment's streamer HTTP port (as `connect_environment` does), open `ws://127.0.0.1:<http_port>/ws/media?codec=…&token=<urlencoded>` (tokio-tungstenite), then dial `<portal ws url>/api/node/relay/<relay_id>` with headers `x-cha-node: <node id>` and `x-cha-signature: <key.sign_b64(relay_message(relay_id, node_id))>`, with the same TLS and `CHA_ALLOW_INSECURE_PORTAL` rules as the node channel. If the streamer refuses (HTTP status before upgrade), answer `Err` with its status and body. Once both are open, answer `RelayOpened` and pump messages both ways in a spawned task, closing both when either ends. Each connection attempt times out at 10 s. Several relays at once are fine (one per guest).
- Unknown environment → `Err("unknown environment")`.

## Portal (`cha-control`)

### Config (`main.rs`, `Config`)

| Variable | Default | Meaning |
|---|---|---|
| `CHA_TUNNEL` | `on` | `off` hides internet links entirely |
| `CHA_CLOUDFLARED` | `cloudflared` | the binary |
| `CHA_TUNNEL_TOKEN` | | a named tunnel's token (secret, `hide_env_values`); without it, quick tunnels |
| `CHA_TUNNEL_HOSTNAME` | | the named tunnel's public hostname (required with the token) |
| `CHA_GUEST_LISTEN` | `127.0.0.1:7680` | the guest-only listener the tunnel points at |

### The tunnel (`tunnel.rs`)

- `Tunnel` in `AppState`: mode `off | quick | named`, state `stopped | starting | up | failed`, `url` (`https://host`), `error`.
- `ensure_up()`: if `up`, return the URL. Otherwise spawn `cloudflared tunnel --no-autoupdate --url http://<guest listen>` (quick) or `cloudflared tunnel --no-autoupdate run --token <token>` (named; the token is passed in the env var `TUNNEL_TOKEN`, not argv). Quick: read stderr until a line holds `https://<something>.trycloudflare.com`, timeout 30 s. Named: the URL is `https://<CHA_TUNNEL_HOSTNAME>`; it is up once stderr says `Registered tunnel connection`, timeout 30 s. Concurrent callers wait on the same start. `kill_on_drop`, and stderr keeps being drained (log at debug) so the pipe never fills. If the process exits, state goes `failed` (error = last stderr lines, never the token), and the next `ensure_up` retries.
- Stop: a background task every 30 s stops the process when no live `wan` share exists and none has for 60 s.
- The guest listener is bound at start-up whenever `CHA_TUNNEL` isn't `off` (it only listens on loopback by default).
- `GET /api/tunnel` (signed in) → `{ "mode", "state", "url"|null, "error"|null }`.

### Shares

- Migration `0018_share_wan.sql`: `ALTER TABLE shares ADD COLUMN wan INTEGER NOT NULL DEFAULT 0`.
- `POST /api/environments/{id}/shares` takes optional `"wan": true`. With it: 409 `tunnel_off` if the mode is off; `ensure_up()` (503 `tunnel_failed` with the error on failure); the reply's `url` is absolute, `https://<tunnel host>/s/<token>`. Without it, `url` stays `/s/<token>`. Replies and the list carry `wan`.
- On the **main listener** every live link works, as now. On the **guest listener** only `wan` links exist (any other token is 404 `unknown_share`).
- `GET /api/shares/{token}` adds `"wan": <bool>` and `"turn": <bool>` (the portal has TURN configured).
- `GET /api/shares/{token}/ice` → `{ "iceServers": [...] }`, the same as `/api/ice` (TURN credentials with `sub` = `share:<id>`), rate-limited like the other guest routes.

### The WebSocket transport

- `Transport::WebSocket` (`"websocket"`) in `ConnectRequest`. In `broker()`: codec must not be PyroWave (400 `bad_codec`); the node's inventory must say `relay` (409 `node_outdated`, "The node needs updating to stream over the internet"); sign the media token as for the other transports; store a **ticket** in memory (`RelayTickets`: 32 random bytes, base64url → { environment id, node id, codec, media token, guest sub for audit }, 60 s, one use) and answer `{ "codec", "transport": "websocket", "urls": ["/api/media/<ticket>"] }` (a path; the browser resolves it against its own origin).
- `GET /api/media/{ticket}` (WebSocket upgrade, on both listeners, no other auth): take the ticket (unknown/used/expired → 404 before upgrading). Make a `relay_id` (16 random bytes, hex), register a pending relay `relay_id → (node id, oneshot<WebSocket>)`, send `OpenRelay` with a 15 s timeout, and on `RelayOpened` take the node's socket from the oneshot and pump both ways until either closes. On any failure close the browser's socket with code 1011 and a short reason (`node_error`, `node_outdated`). Count live relays in the log; no per-message logging.
- `GET /api/node/relay/{relay_id}` (WebSocket upgrade, main listener only): headers must name the node the pending relay is for and carry a valid signature of `relay_message(relay_id, node_id)` by its enrolled key; otherwise 403. Hand the socket to the oneshot. A relay id is good once, for 15 s.
- Audit: `share.joined` gets `"transport": "websocket"` as for the others. Nothing per message.

### Guest listener (`guest_app()`)

Only: the SPA static files with the `index.html` fallback, `GET /api/shares/{token}`, `GET /api/shares/{token}/ice`, `POST /api/shares/{token}/connect`, `GET /api/media/{ticket}`. Everything else under `/api` is 404. `ClientInfo` there takes the address from `CF-Connecting-IP` (falling back to the peer), with `forwarded = true`. The share routes know which listener they serve (a router state flag or an extension), to apply the `wan`-only rule.

## Browser

### `@cha/player`

- `Transport` gains `"websocket"`. `PlayerOptions.webSocket?(codec): Promise<{ urls: string[] }>`. `PlayerOptions.transports?: Transport[]`: the order to try (default `["webtransport", "webrtc"]`, as today's `auto`); each is skipped when unsupported or its option is missing, and the next is tried when one fails to connect. WebRTC gets a connect timeout (`webrtcTimeoutMs`, default 6000) so a fallback after it is quick.
- A `ws-worker.ts` with the same `ToWorker`/`FromWorker` messages as `wt-worker.ts` (sharing the reassembly and report code by moving it to a module both import, not by copying): `new WebSocket(url)`, `binaryType = "arraybuffer"`, relative URLs resolved to `ws(s)://` on `location`'s origin. The player's WebTransport decode path is reused for it (generalise `this.wt` to a worker transport). `supportsWebSocket()` = WebCodecs `VideoDecoder`, `AudioDecoder` and `MediaStreamTrackGenerator` present.
- `onTransport` reports `"websocket"`.

### Portal SPA

- `api.ts`: `transport` accepts `"websocket"`; `shareIce(token)`; `tunnel()`; `createShare` takes `wan`; `ShareInfo` gains `wan`, `turn`.
- **Share dialog:** when `GET /api/tunnel` says the mode isn't `off`, a switch *Over the internet* (off by default). With it on, creating a link may take a while ("Opening the tunnel…"), then shows the `https://…` URL. Its live links are marked *Internet*. Text under the switch: "Through a Cloudflare Tunnel: anyone with the link can join from anywhere. Cloudflare relays the stream and can see it, and it plays with more delay than on your network." For a quick tunnel add: "The address changes when the tunnel restarts, which ends these links."
- **Guest page (`JoinView.vue`):** fetch `/api/shares/{token}/ice` and pass the ICE servers. Transports: for a `wan` link `["webrtc", "websocket"]`, otherwise `["webtransport", "webrtc", "websocket"]` (each filtered by support). `webSocket: c => shareConnect(token, {codec: c, transport: "websocket"})`. Error `node_outdated` → "The host's node needs updating to stream over the internet".
- **SessionView** transport choice gains WebSocket (for testing; the owner's default is unchanged). If the transport list lives in `web/packages/ui-spec`, change it there first, per its README.

## Deploy and docs

- `deploy/portal/Dockerfile`: install a pinned `cloudflared` release from GitHub (the `.deb` or static binary for the target arch, checked against its SHA-256). `deploy/portal/compose.yaml` and `deploy/README.md`: the variables above, how a named tunnel is set up (public hostname → `http://localhost:7680`), that the guest listener is loopback-only, the privacy note.
- `deploy/README.md` "Reaching the portal" paragraph of share links, the remote-access table (a row: *Internet, nothing forwarded: share links over a Cloudflare Tunnel, media over WebSocket*), `docs/PLAN.md` §3.1/§3.4 status lines, `docs/adr/README.md` row for 0022.
