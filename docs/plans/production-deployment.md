# Plan: from the dev rig to a real deployment

Phase 3's "WAN hardening" and auth, made concrete for this homelab. Today the
portal is `bun run dev` on a MacBook (`CHA_LISTEN=0.0.0.0:7677`, dev login on,
plain HTTP), and the node `gpu-node` reaches it with `CHA_ALLOW_INSECURE_PORTAL`.
That's fine for development and wrong for anything else: anyone on the LAN can
reach the API, the node's channel (which can start containers) crosses the
network unencrypted, and the portal only exists while the laptop is awake.

**Goal:** a portal that is always on, reached only over HTTPS, with real
sign-in; nodes on encrypted channels; streams that work at home, from the
tailnet anywhere, and through a UDP-blocking network; backed up and
upgradable. Then the Phase 3 exit tests for remote access pass.

Status: proposed (2026-10-05). Nothing here is built yet unless marked.

---

## 0. Decisions to make first

| # | Question | Options | Recommendation |
|---|---|---|---|
| D1 | Where the portal runs | (a) on `gpu-node` beside the node; (b) a small always-on VM or LXC on the Proxmox host; (c) another box | **(b)**: the portal must outlive node reboots, GPU driver work and the node's VRAM troubles; it needs ~1 vCPU, 512 MB, a few GB. (a) is acceptable to start: same compose, move later with the DB. |
| D2 | How people reach it | (a) tailnet only (`tailscale serve`, `https://portal.<tailnet>.ts.net`); (b) public DNS + Caddy + Let's Encrypt; (c) both | **(a) first**: no open ports, identity from Tailscale, LE certificates for `*.ts.net` handled by Tailscale. (b) when someone off the tailnet needs it (share links, Phase 3's sharing). |
| D3 | Sign-in | (a) local accounts (built); (b) OIDC (Authentik, Authelia, Pocket ID, Google…); (c) Tailscale identity headers (`tailscale serve` passes `Tailscale-User-Login`) | **(a) now, (c) next**: on a tailnet-only portal, trusting `serve`'s headers gives single sign-on for free; **(b)** when the public profile arrives. |
| D4 | TURN | none (LAN + tailnet need none) / coturn on UDP 3478 / coturn on TLS 443 | **None until a real UDP-blocked case**, then TLS on 443 behind SNI routing (§5). |
| D5 | Release images | keep `:dev` tags / versioned tags from CI | **Versioned** (`cha/streamer:0.2.0`, …) built by a script; the node and portal pin them; `:dev` stays for the dev loop. |

The rest assumes D1 (b), D2 (a), D3 (a)→(c), D4 none at first.

---

## 1. The production portal (step 1, ~1 day)

**Build/ship**
- `deploy/portal/compose.yaml` already runs `cha-control` with the SPA built in (`deploy/portal/Dockerfile`), SQLite in the `data` volume, `CHA_SECURE_COOKIES=true`. Check the image builds the SPA with `vite build` and serves it from `--web-dir`; no dev login (`CHA_DEV_LOGIN` unset — make the image refuse `--dev-login` unless `CHA_ALLOW_DEV_LOGIN=1`, so a copied dev env file can't turn it on in production).
- Pin the image by version (D5). Add `deploy/release.sh`: builds `cha-portal`, `cha-node`, `cha/streamer` and the env images with one version, from a clean `git` tree, tagging the commit.

**Carry the dev database over (important)**
- App data is keyed by user id (`/srv/cha-portal/users/<uuid>/<app>`), the node by its enrolled id, and settings (storage, controllers, frame rates, audit) live in `data/dev.db`. A fresh database would orphan the owner's Steam home and need the node re-enrolled.
- Cutover: stop the dev portal → `sqlite3 data/dev.db ".backup data/cutover.db"` → copy into the production `data` volume as `cha.db` (the image's `CHA_DATABASE=/var/lib/cha/cha.db`), owned by the image's `cha` user (the directory is private to it); the database also holds the media-token key, so tokens keep validating → start the production portal (migrations run) → check the users, nodes and settings are there.
- Remove the `dev` account afterwards (it was created by dev login); keep the owner's `admin`.

**Host the portal (D1 b)**
- A Debian/Ubuntu VM or LXC with Docker and Tailscale; the portal publishes on `127.0.0.1:7676` only (the compose default).
- `sudo tailscale serve --bg 7676` → `https://portal.<tailnet>.ts.net` (the existing guide, `docs/guides/tailscale.md`).
- Health: `GET /api/health` (exists) for an uptime check.

**Exit for step 1:** the owner signs in at `https://portal.<tailnet>.ts.net` with their password, sees the node, their environments' history and settings; the dev portal on the Mac is off.

---

## 2. Nodes on encrypted channels (step 2, ~½ day)

- On `gpu-node`: `deploy/node/.env` → `CHA_PORTAL_URL=https://portal.<tailnet>.ts.net`, remove `CHA_ALLOW_INSECURE_PORTAL`. The agent already refuses plain HTTP to another host without that flag (built).
- The node is on the tailnet already or joins it (`tailscale up` on the host; the agent uses host networking, so it reaches the portal through the host's tailnet).
- The node keeps its identity (`state` volume) and its id (carried in the DB): no re-enrollment.
- Doctor: the "Portal" check already reports the URL and clock; add a line that says whether the channel is TLS and warns when `CHA_ALLOW_INSECURE_PORTAL` is set.
- **Firewall the node**: streamer ports 7600–7647 (TCP and UDP) only from the LAN and the tailnet (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`); nothing else inbound except SSH. A `deploy/node/host/` nftables snippet installed by `install.sh` (optional, `--firewall`), since it's a host change the owner makes.

**Exit for step 2:** the node connects over `wss://`, the doctor is clean, a launch works.

---

## 3. Streams from anywhere on the tailnet (step 3, ~1 day plus measurement)

Already built: streamers offer the mesh address alongside LAN ones; WebTransport uses certificate hashes (no CA needed, works from any origin); WebRTC uses ICE; rate control, FEC and RFI handle WAN conditions.

To do:
- **Chrome Local Network Access.** A page on `https://portal.<tailnet>.ts.net` connecting to a LAN address (`192.168.x.x`) or a tailnet one (`100.x`, CGNAT space Chrome may treat as private) can trigger Chrome's local-network permission prompt. Test it; if it prompts, show a one-time explanation in the session page before connecting ("Chrome will ask to reach devices on your network: allow it for the stream"), and document it.
- **Address choice.** The player gets several candidate addresses (LAN, tailnet). Prefer the LAN address when the browser is on the same LAN (WebRTC ICE does this; for WebTransport, try LAN first with a short timeout, then the tailnet one — check what the player does now and make it race them).
- **DERP fallback.** When Tailscale can't make a direct path, traffic relays through DERP (slow, TCP-like). The health grade's network signals will show it; add a hint ("traffic is relayed by Tailscale (DERP): check UDP 41641 on both ends").
- **Measure the Phase 3 exit test:** a laptop on another network (phone hotspot) on the tailnet → a stream with the WAN codecs (AV1/HEVC), health grade B or better, no stall over 1 s. Record in PLAN.md.

---

## 4. Sign-in hardening (step 4, ~1–2 days)

Built: Argon2 password hashes, session cookies (HttpOnly, Secure, SameSite), claim-on-first-visit for the first admin, roles (admin/user/guest), audit log, dev login only from loopback and only with `--dev-login`.

To do:
- **Login throttling**: per account and per client address, exponential backoff after failures; audit `login.failed`; never reveal whether an account exists.
- **Real client address behind a proxy** (a known gap: the audit log records the proxy). `CHA_TRUSTED_PROXIES` (e.g. `127.0.0.1/32`): only then read `X-Forwarded-For` / `Tailscale-*`.
- **Tailscale identity sign-in (D3 c)**: `CHA_AUTH_TAILSCALE=true` with trusted proxies → a request carrying `Tailscale-User-Login` from the trusted proxy signs in the portal user linked to that login (an admin links logins to accounts on the Users page; optional auto-create as `user`). Passwords keep working as a fallback.
- **OIDC (D3 b, with the public profile)**: authorization code + PKCE, `openid email profile`, linking by verified email or an admin-approved link; groups → roles optional.
- **Sessions page**: a user sees and revokes their sessions; an admin can revoke anyone's.
- **Security headers** from the portal (or Caddy): CSP for the SPA (scripts from self, `connect-src` self plus the nodes' WebTransport/WebRTC — mind `blob:` workers), `X-Frame-Options: DENY`, `Referrer-Policy`, HSTS on the public profile.

---

## 5. TURN over TLS on 443 (step 5, only when needed, ~1 day)

For networks that block UDP and allow only HTTPS (some offices, hotels).
- coturn with TLS on 443 next to Caddy on 443 needs **SNI routing**: a layer-4 router (Caddy with the `layer4` module, or HAProxy/nginx `stream` with `ssl_preread`) sends `turn.example.com` to coturn and everything else to the portal. Certificates for `turn.example.com` from Caddy's storage, shared read-only with coturn, reloaded on renewal.
- Enable `--tls-listening-port`, keep `--no-dtls` off for DTLS if useful, keep the peer allow-list to the nodes (built).
- The portal hands out `turns:turn.example.com:443?transport=tcp` beside UDP TURN (it mints credentials per connection already).
- Only meaningful on the public profile (D2 b); a tailnet-only portal has no need (Tailscale's DERP already crosses UDP-blocking networks over 443).
- **Exit test**: a client with UDP blocked (netem drop or a firewall rule on the client) connects via TURN-TLS 443, WebRTC transport, health grade C or better.

---

## 6. Optional: a public profile (later, with sharing)

- `CHA_DOMAIN=portal.example.com`, the compose `tls` profile (Caddy + Let's Encrypt, built), OIDC (step 4), TURN-TLS (step 5).
- Nodes stay private: they dial out to the portal; only the streamer ports need a path from players (port-forward + `CHA_PUBLIC_ADDRESS`, built; or TURN).
- **WebTransport with a real certificate (ACME)**: certificate hashes need a cert valid ≤ 14 days and aren't supported by every browser's WebTransport; an optional mode where a node gets a real certificate for `node1.example.com` (DNS-01) and uses it, for browsers that need it. Defer until a browser needs it.

---

## 7. Operations (alongside steps 1–4)

- **Backups**:
  - portal: `sqlite3 cha.db ".backup …"` nightly (a sidecar or a host timer) + the `data` volume's TLS state; keep 14 days; test a restore.
  - node: app data under `/srv/cha-portal` (users' homes) — a host-side job (restic/borg to the NAS), documented, not run by the agent. The NAS Steam library is the NAS's own business.
- **Upgrades**: `deploy/release.sh` + `deploy/upgrade.md`: portal first (migrations are forward-only and the wire is compatible both ways: older nodes keep working), then nodes (the agent picks up running environments), then images; roll back by pinning the previous tags (`:prev` today).
- **Monitoring**: the portal's health endpoint, node online/offline (the Nodes page), and a simple alert when a node goes offline or VRAM stays full (an admin notice in the portal; email/ntfy later).
- **Logs**: container logs with rotation (Docker's `local` driver, size caps in the compose files); the node keeps failed environments' tails (built).

---

## 8. Security checklist before calling it done

- [ ] No `--dev-login` in production (and the image refuses it without an explicit override).
- [ ] No `CHA_ALLOW_INSECURE_PORTAL` on any node.
- [ ] The portal listens on localhost only; HTTPS terminates in `tailscale serve` or Caddy.
- [ ] Node streamer ports reachable only from the LAN and the tailnet (or the forwarded range for the public profile).
- [ ] The Docker socket stays mounted only into the node agent (it's root-equivalent: §4.2).
- [ ] Portal claimed right after its first start; the `dev` account removed; every admin has a strong password (or Tailscale/OIDC sign-in).
- [ ] Backups exist and a restore was tested.
- [ ] `install.sh --check` clean on every node; the doctor clean.
- [ ] Secrets (`CHA_TURN_SECRET`, OIDC client secret) in env files readable by root only, not in the repository.

---

## 9. Order and exit

1. Decide D1–D5 (owner).
2. Steps 1 → 2 → 3 (the tailnet deployment): **exit** — the owner uses the portal from home and from a remote network over the tailnet, with the WAN codecs and grade B or better; the Mac's dev portal is only for development.
3. Step 4 (sign-in hardening, Tailscale identity).
4. Steps 5–6 only when someone outside the tailnet needs access (with Phase 3's sharing).
5. Operations (§7) in parallel; the checklist (§8) closes it.

Phase 3's remote-access exit tests (PLAN.md) map to step 3 (tailnet, WAN codecs) and step 5 (TURN-TLS on 443).
