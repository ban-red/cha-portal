# 07 — Control plane, node agent, networking, persistence, and thin native client

Research date: 2026-10-03. Researcher slice 07 of 8.
Scope: how existing systems split control plane and nodes; how nodes enroll and connect; how browsers and clients reach media endpoints (certs, NAT, relays, signaling); persistence; image catalogs; authN/Z; control-plane stack; thin native client options.

Project constraints from the coordinator (2026-10-03): Cha Portal is **copyleft OSS (AGPL/GPL)**. It targets **homelab and small groups first** (a handful of nodes, a few users, simple RBAC), designed so team scale is possible later. **LAN and WAN are equally first-class.** We pick the stack. The user leans toward **Vue**.

Anything I could not confirm from a primary source is tagged **(unverified)**.

---

## TL;DR

- **Every comparable system uses the same control-plane shape.** A stateful API with the only DB connection, plus a node agent next to the container runtime. The agent dials *out* to the control plane, reports inventory and heartbeats, and turns desired state into containers. Kasm (manager/agent), Coder (coderd/agent), Portainer (Edge agent), Nestri (API/host agent) and Fenrir (operator/wolf-agent) all work this way. **Do not expose the Docker Engine API over mTLS.** Use a custom agent that holds `docker.sock` locally.
- **Enrollment.** Use a single-use join token with a short TTL, stored hashed. The token embeds the control-plane URL and a pin of its CA/cert (as in the k3s `K10<ca-hash>::` and Portainer edge-key patterns). On first contact the node generates its own keypair, and from then on authenticates with it. Nestri 2026 is the cleanest reference: `nit_` 128-bit token, 60-min TTL, redeemed with one conditional UPDATE; machine secret returned once and stored hashed; 30 s heartbeats; offline after 3 misses.
- **Node↔control-plane channel.** Use **one outbound WSS connection from the node, multiplexed (yamux), with RPC both ways on top**, as Coder does with dRPC over yamux over WebSocket. It passes through any HTTP reverse proxy, including Cloudflare Tunnel. Prefer *desired-state reconciliation* over fire-and-forget commands so disconnects heal themselves.
- **Browser→node media should go direct; the control plane only signals.** As of 2026 **WebTransport is Baseline** (Chrome 97, Firefox 114, Safari 26.4). MDN BCD now lists **`serverCertificateHashes` in all three engines** (Chrome 100, Firefox 125, Safari 26.4; WebKit implemented it in late 2025 after saying in Dec 2024 it would not). So nodes can use **self-signed ECDSA P-256 certs valid ≤14 days, with the hash handed out by the control plane**. No public CA or DNS is needed, and it works for LAN IPs. WebRTC needs no CA either: DTLS fingerprints travel in SDP.
- **LAN gotcha: Chrome Local Network Access (LNA).** It gates public-origin → private-IP fetches (Chrome 142) and **WebSocket/WebTransport (Chrome 147)**. The enterprise opt-out is removed in Chrome 156 (stable 2026-10-20). WebRTC is *not yet* gated. Serving the portal from a LAN address (split-horizon DNS) avoids the prompt. Otherwise users see a one-time per-origin permission prompt.
- **WAN.** WebTransport has no NAT traversal. Options for a node behind CGNAT: (a) WebRTC ICE with TURN, (b) a small QUIC relay (VPS) that the node dials out to, or (c) **iroh** (Apache-2.0/MIT; 1.0 released 2026-06-15; latest tag v1.3.0), which gives QUIC with NAT traversal, multipath and self-hostable relays. iroh is excellent for the **native client and node↔node**, but in browsers it is **relay-only**. **Cloudflare Tunnel cannot carry media**: public hostnames are HTTP/WebSocket only, and the CDN terms restrict video without paid services. It is fine for the control plane.
- **A mesh VPN should be optional, not required.** Tailscale/Headscale/NetBird help homelabbers who already run one: nodes advertise overlay IPs as extra candidates and can use `*.ts.net` certs. Browsers cannot join a tailnet themselves. `tsnet` is Go-only; tailscale-rs is a pre-alpha preview with DERP only.
- **Persistence.** Use a `volume` abstraction with drivers: `dir` by default, `zfs` and `btrfs` for instant clones of golden homes plus send/recv migration. Backups go to S3 via restic or kopia. The ephemeral/persistent choice is per environment. Persistent means the *home volume* persists and the container is recreated from the image (Kasm style). Keep profile paths unique per (user, template). Kasm warns that browser profiles corrupt otherwise. Shared game libraries mount read-only with per-user overlay uppers. Steam concurrency on one library is a known problem (Wolf issues #69, #83). Running GPU sessions cannot be checkpointed. "Suspend" means stopping the container and keeping the volume.
- **Catalog.** Use a git/HTTPS JSON registry in Kasm's style (schema 1.1 fields), with images pinned by digest, OCI labels, and optional cosign verification. Write import adapters for Kasm registries, Wolf `config.toml` apps, and linuxserver Selkies images.
- **Auth.** Built-in accounts with **passkeys** (homelab, no IdP needed), plus optional generic OIDC (Pocket ID, Authelia, Authentik, Keycloak). Simple roles (admin/user/guest) with template grants. Signed, expiring share links. Append-only audit log. **Media tokens are short-lived Ed25519-signed tokens** (aud = node, sid, scopes, exp ≤ 60 s), verified offline by the node.
- **Stack recommendation: Rust for control plane, node agent, streamer and native client; Vue 3 + TS for the web UI.** Reasons: one language for every component that speaks the protocol, a shared `cha-proto` crate, first-class iroh/quinn/wtransport, and the nestri Rust Pyrowave port (`nespyro`, MIT). Use axum, sqlx (SQLite first, Postgres later), bollard (Docker), webauthn-rs and openidconnect. **Go is the credible alternative** if we want tsnet/Headscale embedding, or Coder/Portainer code reuse (Coder is AGPL-3.0, so compatible).
- **Native client: a Rust "player"**, not Tauri. Build it on SDL3 (input, gamepads, haptics) or winit, with ash/Vulkan presentation. Decode with FFmpeg hwaccel (Vulkan Video, D3D11VA, VideoToolbox, VAAPI) and Pyrowave on Vulkan compute. The dashboard stays in the browser and hands off via a `cha://` deep link plus a ticket. Reference code: Magic Mirror `mm-client` (MIT: ash, winit, ffmpeg-next hwaccel, gilrs, cpal) and moonlight-qt for pacing and HDR (GPL-3.0, AGPL-compatible). Mobile and Apple TV get a Rust core with uniffi bindings to Kotlin/Swift (Magic Mirror already does this for SwiftUI). Tauri is a poor fit: WKWebView pointer lock needs private API, and WebKitGTK lacks WebTransport/WebCodecs. Tauri's CEF runtime is still alpha.

---

## 1. Prior art: control plane / node patterns

### 1.1 Kasm Workspaces (1.17/1.18 docs)

Roles ([System Architecture](https://www.kasmweb.com/docs/latest/guide/system_architecture.html), [docs.kasm.com](https://docs.kasm.com/docs/guide/system_architecture/index.html)):

- **API/Web App**: serves the UI and API. It is the **only role that talks to the DB** (Postgres + Redis), so deploy it close to the DB.
- **Manager**: monitors agents and sessions. Agents **check in automatically** and report available resources.
- **Agent**: provisions session containers on request.
- **Proxy (nginx)**: forwards to the right service or container.
- **Connection Proxy (custom Guacamole)**: turns RDP/VNC/SSH into websockets for external (non-container) workloads. The web app queries the DB for connection proxies in the target zone, shuffles the list, and health-checks before use ([Connection Proxy](https://www.kasmweb.com/docs/latest/guide/windows/connection_proxy.html)).
- **Share service**: session-sharing chat over Redis.

Enrollment ([multi-server install](https://docs.kasm.com/docs/tutorials/install/multi-server-install/index.html)):

- `install.sh --role agent --public-hostname <agent> --manager-hostname <mgr> --manager-token <token>`.
- The manager token is generated at DB install. The agent needs to reach `manager:443` and **reports its hostname** at check-in, which is used for routing.
- This is a single long-lived shared token, which is weaker than Nestri's single-use tokens.

Zones ([Deployment Zones](https://kasm.com/docs/latest/guide/zones/deployment_zones.html)):

- **Upstream Auth Address**: the API server that validates session connections, i.e. per-request auth callback from the proxy.
- **Allow Origin Domain**.
- Optional proxying of sessions through intermediary servers instead of direct-to-agent.
- Load balancing strategies: Least Load, Most Load, Least Kasms, Most Kasms.
- **Prioritize Static Agents** over autoscaled ones; **Search Alternate Zones** for fallback.
- Agents inherit their zone from their manager.

Staging, i.e. pre-warmed pools ([Session Staging](https://www.kasmweb.com/docs/develop/guide/staging.html)):

- Settings: *Desired Sessions*, *Expiration (hours)*, and clipboard flags.
- Settings are fixed at container start, so **users whose group permissions don't match the staged config get an on-demand session instead**. This is a security rule worth copying.

Persistence ([Persistent Profiles](https://kasm.com/docs/latest/guide/persistent_data/persistent_profiles.html)):

- Two modes: volume-mounted profiles (paths with `{username}`, `{user_id}`, `{image_id}` tokens) or **S3 profiles**.
- For S3, containers never get S3 credentials. They request a manifest from the API and get **presigned URLs per profile layer**, time-limited.
- `KASM_PROFILE_SIZE_LIMIT` (KB). An over-limit profile is not saved.
- Explicit warning: **each workspace's profile path must be unique**, because browsers store session config at the same path.

Catalog ([Workspace Registry](https://www.kasmweb.com/docs/latest/guide/workspace_registry.html), [template](https://github.com/kasmtech/workspaces_registry_template)):

- Git-hosted. Each workspace is a folder with `workspace.json` and a PNG.
- Schema 1.1 required fields: `friendly_name`, `description`, `image_src`, `architecture` ([amd64|arm64]).
- Optional: `cores`, `memory`, `gpu_count`, `categories` (max 3), `docker_registry`, `run_config`, `exec_config`.

License: KasmVNC is GPL-2.0 (per GitHub API). Kasm Workspaces itself is proprietary/community edition **(unverified detail)**.

### 1.2 Coder (coder/coder, AGPL-3.0)

- **coderd** is a "thin API connecting workspaces, provisioners and users". It is the only component with Postgres access (PG 13+) ([architecture](https://coder.com/docs/admin/infrastructure/architecture)). **Provisioners** run Terraform, in-process or as external daemons.
- **Workspace agent**:
  - Dials **outbound** WebSocket/HTTPS to `CODER_ACCESS_URL` ([networking](https://coder.com/docs/admin/networking)).
  - Uses **dRPC over yamux over that WebSocket** for its API: `ConnectRPC29/210` return `DRPCAgentClient` and `DRPCTailnetClient` ([pkg.go.dev agent](https://pkg.go.dev/github.com/coder/coder/v2/agent), [storj/drpc](https://github.com/storj/drpc)).
  - Auth types: `token`, or cloud instance identity for AWS, Azure and GCP ([Agents API](https://coder.com/docs/reference/api/agents)). Token scopes include `no_user_data`.
- **Data plane**:
  - **Tailnet**, i.e. WireGuard via Tailscale's open-source code. Direct P2P where possible; **DERP** relays otherwise (embedded in coderd; Tailscale's DERP fleet or custom relays optional).
  - STUN for endpoint discovery. All traffic is E2E encrypted.
  - Browser "workspace apps" are proxied **by coderd/proxies over the tailnet**, because the browser is not on the tailnet.
- **Workspace proxies (Premium)**:
  - Regional relays for apps, SSH and port forwarding. Each has **its own token** (`CODER_PROXY_SESSION_TOKEN`; never shared) and no DB access.
  - Each requires a wildcard domain (`*.east.coderd...`). The user picks a proxy and falls back to primary ([Workspace Proxies](https://coder.com/docs/admin/workspace-proxies)).
- Takeaway for Cha: Coder's split matches what we need: control plane, outbound agent, optional regional relays, browser traffic proxied. Because Coder is **AGPL-3.0**, Cha (AGPL) could legally reuse code (e.g., tailnet/coordination). Check that the `enterprise/` dir is excluded **(unverified license split)**.

### 1.3 Portainer agent and Edge agent (Zlib)

- **Standard agent**: the server connects *in* to the agent (port 9001).
- **Edge agent** is outbound-only ([Edge Agent docs](https://docs.portainer.io/advanced/edge-agent), [oneuptime NAT guide 2026](https://oneuptime.com/blog/post/2026-03-20-edge-agent-nat-firewall-setup/view)):
  - It **polls** the Portainer API over HTTPS (:9443).
  - On demand it opens a **reverse TLS tunnel (chisel, SSH-over-WebSocket) to :8000** for interactive management.
  - **Async mode** (Business Edition) has no tunnel. It works from snapshots plus command queues, with ping/snapshot/command intervals pushed down by the server.
- **Edge key** = base64 (no padding) of `portainer_url|tunnel_server_addr|tunnel_server_fingerprint|endpoint_id`. That is, the join credential **carries the server URL and a pinned tunnel fingerprint** ([portainer/agent README](https://github.com/portainer/agent/blob/develop/README.md)).
- Other features: a "waiting room" for unapproved agents, and edge agent update schedules with automatic rollback ([oneuptime](https://oneuptime.com/blog/post/2026-03-20-update-edge-agents-automatic-rollback/view), secondary source).
- Takeaway: copy the **pinned fingerprint in the join key**, the **waiting room / approve-on-first-contact** flow, and **staged agent updates with rollback**. Avoid polling plus a separate tunnel. One persistent multiplexed WSS is simpler.

### 1.4 Wolf API, Wolf Den, Fenrir (games-on-whales, MIT)

- The Wolf REST API is reachable **only via a UNIX socket** (`WOLF_SOCKET_PATH`). Docs warn that exposing it over TCP is "highly dangerous": it can pair clients and execute arbitrary commands (`docs/modules/dev/pages/api.adoc`, read from repo at commit `facb8e0`, 2026-09-29).
- OpenAPI paths (from `docs/modules/dev/partials/spec.json`):
  - `GET /api/v1/{apps,clients,sessions,lobbies,profiles,events,pair/pending,openapi-schema}`; `events` is an event subscription stream.
  - `POST /api/v1/{apps/add,apps/delete,sessions/add,sessions/start,sessions/stop,sessions/pause,sessions/input,runners/start,lobbies/create|join|leave|stop,profiles/add|remove,pair/client,unpair/client,clients/settings,docker/images/pull}`.
  - `GET /api/v1/docker/images/inspect`.
- The app model (`config.toml` v2) uses runner `docker` with `image`, `env`, `mounts`, `devices`, and **`base_create_json`** (raw Docker create JSON overrides), or a `process` runner. **Profiles** each have their own apps, an optional PIN, and per-app persistent data at `/etc/wolf/profile_data/${profile_id}/${app_title}` ([configuration](https://games-on-whales.github.io/wolf/stable/user/configuration.html)).
- Robustness: a 2026 third-party audit reports that malformed HTTP on `wolf.sock` can `std::terminate` Wolf, plus a crash on empty app lists ([Heeler #3](https://github.com/ydoc-afk/Heeler/issues/3), [#10](https://github.com/ydoc-afk/Heeler/issues/10)). If Cha drives Wolf, wrap it in a strict client and supervise and restart it.
- **[Wolf Den](https://github.com/games-on-whales/wolf-den)** (MIT) is a web UI over the socket.
- **[Fenrir](https://github.com/games-on-whales/fenrir)** (Go, MIT, "NOT IN A USEABLE STATE") is a K8s control plane for many Wolfs:
  - `moonlight-proxy`: client-cert auth; creates `Pairing` CRDs and `Session` objects.
  - `wolf-agent` sidecar: reconciles intended sessions against Wolf via the socket and calls fake-udev for controllers.
  - An operator with leader election that creates port-forwards, PVCs and deployments.
  - The **reconciler-sidecar pattern** is directly applicable.
- Release tags stop at `v2024.07`; Wolf ships from branches and images (git ls-remote, 2026-10-03).

### 1.5 Nestri (2026 rewrite, Apache-2.0) — the closest living reference

Repo read at `ef32d5f` (2026-10-03). The README says "mid-rewrite… nothing here is stable yet".

- **Control plane (TypeScript)**:
  - `apps/api`: Hono REST, OpenAPI at `/doc`. `apps/auth`: self-hosted **OpenAuth** issuer (Steam and SSH-key login). `packages/core`: domain on Drizzle + **Postgres**.
  - Runs as Cloudflare Workers *or* plain containers via `docker-compose.yml`. Compose has no credential defaults, and **migrations are not run on boot**, to avoid races between replicas.
- **Host enrollment** (`packages/core/src/machine/install-token.ts`, `machine.sql.ts`):
  - Dashboard issues a `nit_<base64url 16 bytes>` token, **TTL 60 min**, stored as SHA-256, **spent with one conditional UPDATE**. Racing hosts cannot both win; every failure reason returns the same `null`.
  - The installer (`curl … | sh -s -- <token>`) verifies binary SHA256SUMS, refuses root, asks for a dedicated xfs/ext4 image store (never `/`), and runs the agent as a **systemd user service**.
  - Registration returns **machine id + secret (returned once, stored hashed)**. API auth headers: `x-nestri-machine-id` / `x-nestri-machine-secret`.
  - Ownership is team XOR organisation, enforced by a CHECK constraint.
  - Hosts get a routing **slug** (e.g., `amber-otter-4821.nestri.link`), deliberately not the monotonic id.
  - **`endpointId` (iroh public key) is *reported* by the host on each heartbeat**, never assigned, and is unique.
- **Heartbeat**: `HEARTBEAT_SECONDS = 30` is returned to the host in each response, so cadence is server-controlled. **Offline after 3 missed**. Online is *derived* from `last_seen`, not stored.
- **Placement** (`box/placement.ts`): a single seam, `Placer(request) → machineId`. The current `onlyHost` placer **refuses when there are 0 or >1 hosts**, so no accidental policy gets baked in.
- **Guest components (Rust, inside a microVM)**:
  - `nescope`: headless Wayland compositor (gamescope-like).
  - `nescapture`: Vulkan implicit layer that encodes on the drawing GPU.
  - `neswire`: audio.
  - **`neshub`**: muxes video, audio, cursor and input into **one iroh QUIC endpoint**. Clients dial with a **ticket** `nestri:<b64 JSON {endpoint_addr, stream_name}>`. The ticket is handed out through a socket to `nesinit`, which carries it to the host.
- **`crates/nespyro`**: **Rust port of PyroWave on ash (MIT)**, bitstream frozen at upstream `89f7e47`.
  - Intra-only; "around 170 Mbit/s at 1080p60"; desktop GPUs only (mobile paths not carried).
  - Encoder `encode_after` returns packets that each parse alone. Decoder `push_packet` accepts any order and reports `readiness`.
  - PyroWave frames go over **QUIC datagrams, one packet per datagram**, with an independent sequence space. A skipped frame does not burn a sequence number.
- **`docs/media-transport.md`** has hard-won lessons for any QUIC media design. It is pinned to iroh 1.1/`noq` (a quinn fork); `neshub/Cargo.toml` now says `iroh = "1.2"`.
  - Congestion control is **per path**: a relay path and a direct path can coexist.
  - **DATAGRAM frames are packed before STREAM frames**, so keyframes on streams can be starved by deltas. Fix: stop sending frames that depend on a keyframe the receiver lacks.
  - `send_datagram` **evicts the oldest datagrams silently** and gives no backpressure.
  - **Measure send-queue backlog** (`DGRAM_BUFFER_BYTES − datagram_send_buffer_space()` / drain rate). Loss is a lagging indicator: they saw 8 s of lag with zero loss on a 1000-mile link.
  - Trust receiver reports over sender stats.
- **`nesbox`** (Apache-2.0): a Firecracker-derived microVM using virtio-PCI and **virtio-gpu native context** (virtio-nvgpu for NVIDIA; DRM native context for Intel/AMD). It claims ≤2% of bare metal and **12 guests on one RTX 3060** ([nesbox](https://github.com/nestrilabs/nesbox)). This is the multi-tenant isolation answer that containers lack.
- **DNS/TLS** (`docs/dns.md`, rewritten 2026-09-16):
  - The control plane sits behind a **Cloudflare Tunnel**: proxied CNAME to `<uuid>.cfargotunnel.com`, with no A records.
  - Per-box hostnames live on a **separate registrable domain** (`*.nestri.link`), so box content is outside the sign-in cookie scope.
  - Universal SSL covers only one wildcard level, so `deep.a.nestri.link` fails TLS before HTTP.
  - Media (iroh QUIC) does not go through Cloudflare.

### 1.6 RustDesk (AGPL-3.0) — rendezvous plus relay

- `hbbs` is the ID/rendezvous/signaling server: TCP 21115/21116/21118 (ws), UDP 21116. Clients ping it so it learns their current public address, then it attempts **hole punching**.
- `hbbr` is the relay (TCP 21117/21119 ws), used when hole punching fails ([self-host docs](https://rustdesk.com/docs/en/self-host/)).
- The model is "control plane brokers, relay is separate and stateless". RustDesk is AGPL, so its Rust code is license-compatible with Cha.

### 1.7 Orchestrators: Nomad, k3s, Incus

- **Nomad**: BSL since 2023 (now IBM). It has an NVIDIA device plugin and Docker driver ([CE license](https://developer.hashicorp.com/nomad/docs/ce-license-support), [nvidia plugin](https://developer.hashicorp.com/nomad/plugins/devices/nvidia)). It is **not OSI-open, so do not bundle it** in an AGPL project. Learn from its device fingerprinting.
- **k3s** (Apache-2.0):
  - Token format `K10<sha256 of cluster CA>::<credentials>`. The joining node downloads the CA bundle, hashes it, and **verifies before sending credentials**. "Short" tokens without the hash are MITM-able.
  - `k3s token create` makes expiring bootstrap tokens (default TTL 24 h) ([k3s token](https://docs.k3s.io/cli/token)).
  - Too heavy as the default homelab substrate. It is a plausible *team-scale* backend later (Fenrir-style).
- **Incus** (Apache-2.0):
  - Runs **OCI app containers since 6.3**. Physical GPU passthrough for containers and VMs, plus a **native-context GPU type for VMs**.
  - mTLS REST API. Authorization drivers reworked in 7.3, including OIDC ([Incus GPU](https://linuxcontainers.org/incus/docs/main/reference/devices_gpu/), [news](https://linuxcontainers.org/incus/news/index.html)).
  - ZFS/btrfs instant clones are built in.
  - A plausible **alternative node runtime** if we want VM isolation without writing a VMM.

### 1.8 Docker Engine API over mTLS vs a custom agent

| | Docker API over TCP+mTLS (`dockerd --tlsverify -H tcp://0.0.0.0:2376`) | Custom node agent (local `docker.sock`) |
|---|---|---|
| Network | Needs **inbound** port per node; no NAT traversal | **Outbound-only**; works behind CGNAT and Cloudflare Tunnel |
| Privilege | Remote root on the host; one cert = full control | Agent is root-equivalent locally, but the control plane can only invoke **typed operations** |
| GPU/encoder inventory | None | NVML, VA-API, Vulkan caps, NVENC session counts |
| Media/cert/volume work | None | Generates WebTransport certs, ZFS clones, overlay mounts, uinput, etc. |
| Disconnect behaviour | Control plane must be up for anything | Reconciles locally; sessions survive control-plane outages |

Verdict: custom agent. Optionally put the agent behind a socket proxy or use rootless Podman. GPU and uinput needs usually force privileged pieces anyway.

---

## 2. Recommended node model: enrollment, identity, heartbeats, capabilities, placement, upgrades

### 2.1 Enrollment flow

1. An admin clicks **Add node**. The control plane creates a **join token**:
   - `cha_jt_<base64url(16 random bytes)>`, stored as SHA-256 (Nestri).
   - Single-use, TTL 1 h (configurable). Optionally pre-labelled with zone or pool.
   - The UI renders a `compose.yaml` and `.env` with:
     ```
     CHA_PORTAL_URL=https://portal.example.com
     CHA_JOIN=K1:<sha256-of-portal-TLS-SPKI or internal CA>::cha_jt_...
     ```
   - The `CA hash` part follows k3s and Portainer: the node pins the control plane before sending the token. This matters for self-signed or private-CA homelab portals. With a public-CA portal the hash pins the SPKI, so key rotation needs care. Allow `CHA_JOIN` without a hash plus `CHA_PORTAL_CA=system` for public PKI.
2. The agent starts, generates an **Ed25519 node identity key** (persisted in a volume, never leaves the host), and calls `POST /api/node/v1/enroll {token, pubkey, hostname, inventory}`.
3. The control plane redeems the token atomically (Nestri conditional UPDATE). It creates `node {id, pubkey, status: pending|approved}` and returns `{node_id, portal_signing_pubkey, heartbeat_s}`. The `portal_signing_pubkey` is used later to verify media tokens.
   - Optional **waiting room**: nodes enrolled without a pre-approved token appear as *pending* (Portainer).
4. Afterwards the node authenticates every connection with **a signature over a server challenge or timestamp** using its key. That is application-level auth, so it survives TLS-terminating reverse proxies and Cloudflare Tunnel.
   - Optional mTLS client certs from an internal CA, for deployments without TLS-terminating proxies.
5. Rotation: the node can submit a new pubkey signed by the old key. An admin can revoke a node, which deletes the pubkey and drops the channel.

### 2.2 The channel

- One **outbound WSS** connection: `wss://portal/api/node/v1/connect`.
- **yamux** multiplexing on top. Rust crates: `yamux`, `tokio-tungstenite`. Go: `hashicorp/yamux`.
- **RPC in both directions** over yamux streams. The control plane calls node services (StartSession, StopSession, PullImage, CreateVolume, Snapshot, ExportLogs, GetWebRTCAnswer…). The node calls control-plane services (ReportStatus, StreamEvents, RequestToken…).
  - Rust: `tonic` with a custom transport (`serve_with_incoming` / custom connector) over yamux streams.
  - Go: Coder's dRPC-over-yamux, or connect-go over an in-memory listener.
- **Desired-state reconcile**: the control plane sends `NodeSpec` (sessions that should exist, volumes, images to pre-pull). The node converges and reports `NodeStatus`. Imperative RPCs are used only for latency-critical actions such as signaling. This is Fenrir's wolf-agent model, and it heals after disconnects.
- Keepalive: WS ping every 15 s. The app-level status heartbeat is server-tunable (Nestri returns the interval in each reply). Online = channel up, or `last_seen` within 3 intervals.
- **Rule: agent restarts must not kill sessions.** The agent supervises containers but is not in the media data path. Media runs in the streamer and environment containers, and the agent re-adopts them by label on start.

### 2.3 Capability / inventory report (sent at connect and on change)

```yaml
node:
  agent_version: 0.3.1
  proto: 1                  # control plane supports N and N-1
  os: { kernel: 6.17.x, distro: ..., arch: amd64 }
  runtime: { docker: 29.x, containerd_store: true, nvidia_ctk: 1.18, cdi: true, podman: false }
  cpu: { model: ..., cores: 16 }
  mem_mb: 64000
  storage: [{ driver: zfs, pool: tank, free_gb: 1200 }, { driver: dir, path: /var/lib/cha, free_gb: 300 }]
  devices: { uinput: true, dri: [renderD128, renderD129] }
  gpus:
    - { vendor: nvidia, model: RTX 4080, pci: "0000:01:00.0", driver: "580.xx", vram_mb: 16376, vram_free_mb: 12000,
        render_node: /dev/dri/renderD129,
        encode: { h264: {max: 4096x4096, yuv444: true}, hevc: {10bit: true}, av1: {10bit: true} },
        nvenc_sessions: { active: 2, limit: 12 },   # NVML nvmlDeviceGetEncoderSessions / capacity
        vulkan: { api: "1.4", video_encode: [h264, h265, av1], pyrowave_ok: true } }
    - { vendor: intel, model: "UHD 770", vaapi: { encode: [h264, hevc, av1] }, ... }
  net:
    lan: [192.168.1.50]
    wan: [203.0.113.7]            # from STUN / QUIC address discovery
    nat: { type: "endpoint-independent", upnp: false }
    overlay: [{ kind: tailscale, ip: 100.101.1.2, dns: node1.tail1234.ts.net, cert: true }]
    iroh: { endpoint_id: "...", relay: "https://relay.example.com" }
  media:
    webtransport: { port: 47999, cert_sha256: ["...current", "...next"], not_after: "2026-10-12T00:00Z" }
    webrtc: { udp_ports: "48000-48100", ice_lite: false }
  images: [{ ref: "ghcr.io/cha/kde@sha256:...", size_mb: 4100 }]
```

Notes:

- **NVENC consumer session limit**: 8 from Jan 2024 per [VideoCardz](https://videocardz.com/newz/nvdia-geforce-gpus-now-support-up-to-8-concurrent-nvenc-encoding-sessions). Reported **12 from ~Nov 2025** (driver 591.44 per secondary sources; [Wikipedia NVENC](https://en.wikipedia.org/wiki/NVENC)) **(unverified against NVIDIA's matrix)**. **Read live usage from NVML** instead of hardcoding: `nvmlDeviceGetEncoderSessions` and `nvmlDeviceGetEncoderCapacity` ([NVML](https://docs.nvidia.com/deploy/nvml-api/api/group__nvmlDeviceQueries.html); Go: [NVIDIA/go-nvml](https://github.com/NVIDIA/go-nvml); Rust: `nvml-wrapper` crate).
- AMD and Intel have no session limits of this kind. Report VA-API/Vulkan Video profiles (`vainfo`-equivalent via libva; `vkGetPhysicalDeviceVideoCapabilitiesKHR`).
- PyroWave needs Vulkan compute. nespyro's `DeviceRequirements::query` names missing features, so call it and report `pyrowave_ok`.

### 2.4 Placement / scheduling (homelab-first)

Follow Nestri's **single placement seam** so a real scheduler can replace it later:

1. **Filter**:
   - Node online and approved; user/template allowed on the node pool.
   - Required GPU vendor and encoder for the negotiated codec.
   - `vram_free ≥ template.vram`; NVENC sessions free.
   - Image arch.
   - **Data gravity**: a persistent environment's volume lives on node X, so place on X unless migration is requested.
2. **Score**:
   - Image already cached (big win).
   - Least load (Kasm strategies: least/most load, least/most sessions). Prefer "static" nodes (Kasm).
   - **Client-measured RTT** to each candidate: the browser does a quick WebTransport or HTTP probe to the candidate list before launch.
3. Let the user override ("launch on node X"). Refuse loudly when there is no candidate (Nestri).

### 2.5 Agent upgrades

- The control plane advertises `desired_agent = {version, image@digest, signature}`. By default the agent version is pinned to the control-plane version (Coder serves its agent binary from coderd for the same reason). The protocol is kept compatible for N-1 so mixed fleets work during rollout.
- Self-update: the agent pulls the new image (optionally cosign-verified) and starts a one-shot **updater** container that recreates the agent with the new image. The updater health-checks and rolls back on failure (Portainer's update schedules with rollback). Sessions keep running because the agent is not in the data path.
- Homelab default: auto-update on. Team scale: staged rings, i.e. update a canary node first.

---

## 3. Connectivity

### 3.1 The paths and what each needs

| Path | Volume / latency need | Recommended | Fallbacks |
|---|---|---|---|
| Browser ↔ control plane | low bandwidth | HTTPS (REST + SSE) | — (works via reverse proxy and Cloudflare Tunnel) |
| Node agent ↔ control plane | low bandwidth, must traverse NAT | outbound WSS + yamux | long-poll HTTPS (Portainer async style) |
| **Browser ↔ node media** | 20–200+ Mbit/s, lowest latency | **direct** UDP: WebTransport (hash-pinned cert) or WebRTC | WebRTC + TURN; QUIC/WebTransport relay; WebSocket via relay (TCP, last resort) |
| Native client ↔ node media | same | **iroh** (QUIC, NAT traversal, multipath, relay) or plain QUIC/WebTransport | iroh relay (self-hosted) |
| Node ↔ node (migration, backups, library sync) | bulk | iroh or overlay VPN | via S3 |

### 3.2 Browser platform facts as of Oct 2026

- **WebTransport** is supported in Chrome 97, Firefox 114 and **Safari 26.4** (Mar 2026), which makes it Baseline 2026 ([MDN BCD api/WebTransport.json](https://github.com/mdn/browser-compat-data/blob/main/api/WebTransport.json), [WebKit 26.4 post](https://webkit.org/blog/17862/webkit-features-for-safari-26-4/)). Datagrams are supported in all three. `congestionControl`, `requireUnreliable` and `allowPooling` options are listed for Firefox and Safari but not Chrome.
- **`serverCertificateHashes`**: Chrome 100, Firefox 125, **Safari 26.4** per BCD.
  - Firefox's initial implementation was buggy (it augmented chain verification instead of replacing it). It was fixed for 125 ([bug 1873263](https://bugzilla.mozilla.org/1873263)).
  - WebKit said in Dec 2024 it did "not intend to implement" ([w3c/webtransport#623](https://github.com/w3c/webtransport/issues/623); the removal proposal was closed as not planned). It later implemented it anyway: [webkit-changes 3fbb50 "Implement WebTransportOptions.serverCertificateHashes"](https://www.mail-archive.com/webkit-changes@lists.webkit.org/msg236930.html), bug 300057. The member is in WebKit's `WebTransportOptions.idl` on main today.
  - **Still test Safari empirically.**
  - Requirements: X.509v3, **ECDSA P-256** (RSA rejected), validity **≤ 14 days** (Chrome refuses longer), `sha-256` only, `allowPooling: false`, secure-context page ([MDN WebTransport()](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport), [viroh notes](https://viroh.net/serverhash)).
  - **WebTransport cannot hole-punch.** Raise the QUIC idle timeout or use keep-alives. Browser-initiated streams behave better in Chrome (viroh).
- **Chrome Local Network Access** ([Chrome blog](https://developer.chrome.com/blog/local-network-access), [Sentry radar #29](https://github.com/getsentry/browser-updates-radar/issues/29)):
  - Public-origin → private/loopback/`.local` requests need a **per-origin permission prompt**. Fetch since Chrome 142 (Oct 2025); **WebSocket and WebTransport since Chrome 147**.
  - **Chrome 156 (stable 2026-10-20) removes the enterprise opt-out.**
  - **WebRTC is not yet gated** (stated as of Chrome 138, with future integration planned).
  - Permission-gated local requests to private IP literals or `.local` names are **exempt from mixed-content blocking**.
  - Implication: if the portal page is served from a *public* address (e.g., `portal.example.com` → public IP or Cloudflare), the first direct LAN connection triggers a prompt. **Split-horizon DNS** (portal name resolves to a LAN IP at home) makes it local→local with no prompt. Document both cases.
- **Input and render APIs** (BCD):
  - Pointer lock `unadjustedMovement` (raw mouse): Chrome 88, **Firefox 152**, **Safari 18.4**.
  - `navigator.keyboard.lock`: **Chrome 68 only**. Safari 26.4 ships a *Fullscreen Keyboard Lock* API, apparently a different shape **(unverified)**. WebKitGTK 2.52 added the Keyboard Lock API.
  - `VideoDecoder`: Chrome 94, Firefox 130, Safari 16.4.
  - Gamepad `trigger-rumble`: Chrome 126 only.
  - WebGPU canvas `toneMapping` (HDR output): Chrome only.
  - These gaps are part of why a native client matters.

### 3.3 Getting nodes "valid TLS" — options ranked for Cha

1. **Hash-pinned self-signed certs for WebTransport (default).**
   - Each node streamer generates an ECDSA P-256 cert with ~10-day validity and reports `sha256` to the control plane through the agent channel. It **pre-generates the next cert** and reports both hashes, then switches before expiry. The browser passes `[current, next]` hashes.
   - Works for **raw LAN IPs, overlay IPs, and public IPs**. No DNS, no CA, no ACME rate limits.
   - The trust anchor is the authenticated control-plane session that delivered the hash.
   - Downside: only WebTransport benefits. A WSS or HTTPS fallback directly to the node still needs a trusted cert.
2. **WebRTC**: no CA needed. DTLS fingerprints are carried in SDP over the authenticated signaling channel.
3. **ACME DNS-01 wildcard per deployment**, e.g. `*.nodes.home.example.com`.
   - The control plane holds the DNS-provider API token and issues per-node certs. Use lego or certmagic in Go, `instant-acme` in Rust **(crate unverified)**.
   - Pushes cert+key to nodes over the channel, or proxies DNS-01 for the node.
   - Combine with **plex.direct-style names** that encode the IP: `192-168-1-50.<node-hash>.nodes.example.com` resolves to the embedded IP via a tiny DNS responder, so one wildcard cert per node works for any IP. Plex does this with `*.<hash>.plex.direct` certs from DigiCert ([Filippo's write-up](https://words.filippo.io/how-plex-is-doing-https-for-all-its-users/)).
   - Gives real WSS/HTTPS to nodes, which helps the fallback paths and non-browser tools.
   - Caveat: some home routers block DNS answers that point at private IPs (**DNS-rebinding protection**). A known issue for plex.direct; **(unverified for current routers)**.
4. **Let's Encrypt IP-address certificates**: GA **2026-01-15**. They must use the short-lived profile (~160 h / 6 days) and only work for **public** IPs ([LE announcement](https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability)). Useful for VPS nodes with no domain. Not usable for LAN.
5. **Tailscale certs** (`*.ts.net`, via `tailscale cert` or tsnet): automatic when the node is on a tailnet. Only reachable by tailnet members.
6. **Cloudflare Tunnel**: works for the *control plane* (HTTP/WebSocket). **Public hostnames don't carry UDP.** UDP works only for private networks via WARP ([Tunnels FAQ](https://developers.cloudflare.com/cloudflare-one/faq/cloudflare-tunnels-faq/), [community](https://community.cloudflare.com/t/public-hostname-udp/452994)). WebTransport/WebRTC media can't use it. Cloudflare's CDN terms (effective 2026-09-28) reserve the right to limit accounts serving **video** through the CDN without paid services ([service-specific terms](https://www.cloudflare.com/service-specific-terms-application-services/)), so a WebSocket media fallback through Cloudflare is risky.
7. **Hosted DNS/relay service under `cha.sh`** (e.g., `*.<hash>.nodes.cha.sh`, a public relay): convenient, but it creates an operated service and dependency. Make it optional and self-hostable. See open questions.

### 3.4 NAT traversal and relays

- **WebRTC ICE + TURN**: the only *browser* path with real NAT traversal.
  - Self-host coturn or eturnal with the **TURN REST API** (ephemeral HMAC credentials from the control plane).
  - **Cloudflare Realtime TURN**: $0.05/GB after a shared 1,000 GB/month free tier ([pricing](https://developers.cloudflare.com/realtime/sfu/platform/pricing/)). At 30 Mbit/s that is ~13.5 GB/h, so the free tier is ~70 h/month. **PyroWave at ~170–200 Mbit/s is ~80–90 GB/h, so never relay PyroWave**; it is LAN/direct only.
  - Selkies 2.0 RC now **defaults to WebSocket transport + WebCodecs** (one TCP port, no STUN/TURN). WebRTC is opt-in with an embedded coturn ([Selkies design](https://selkies-project.github.io/selkies/design/)). That shows how painful TURN is for self-hosters.
- **QUIC/WebTransport relay ("cha-relay")**: a small service on a VPS with a real cert.
  - The browser opens WebTransport to `relay.example.com/s/<sid>`. The node keeps an **outbound** QUIC (or iroh) connection to the relay. The relay forwards streams and datagrams.
  - Simpler than TURN for a WebTransport-first design, and it keeps QUIC end to end. Optional inner E2E encryption of media payloads could make the relay untrusted.
- **iroh** (n0, Apache-2.0/MIT):
  - Dial by Ed25519 key. QUIC NAT traversal plus **QUIC multipath** (relay and direct paths in one connection). Self-hostable `iroh-relay`. Wire-stable since **1.0 (2026-06-15)**; tags at v1.3.0 ([iroh 1.0](https://www.iroh.computer/blog/v1)). n0 claims ~95% of bytes flow directly.
  - Browser (WASM) mode is **relay-only**, over WebSocket to the relay, still E2E encrypted ([iroh WASM docs](https://docs.iroh.computer/languages/wasm-browser)). The iroh docs name WebTransport+`serverCertificateHashes` or WebRTC as future direct-browser paths.
  - Nestri uses iroh for all media.
  - **Use iroh for the native client and node↔node.** For browsers it is a relay transport only.
  - A community clean-room **Go port** exists ([tmc/go-iroh](https://github.com/tmc/go-iroh), MIT, wire-compat with pinned releases, pre-v1 API).
- **Mesh VPNs** (optional integration, not a dependency):
  - **Tailscale** (BSD-3): `tsnet` embeds a node in a Go program with its own identity, MagicDNS and certs. It can point at Headscale via `ControlURL` ([tsnet docs](https://tailscale.com/docs/features/tsnet)). **Peer Relays** went GA 2026-02-18 (client ≥1.86, all plans). With Headscale, endpoints need ACL visibility of the relay ([Peer Relays GA](https://tailscale.com/blog/peer-relays-ga)). **tailscale-rs** preview (2026-04-15) is DERP-only with no direct connections, "do not use in production" ([blog](https://tailscale.com/blog/tailscale-rs-rust-tsnet-library-preview)).
  - **Headscale** (BSD-3): v0.28.0 (2026-02-04: tags-as-identity, lighter map updates; min client 1.74). Latest tag v0.29.4. Embedded DERP. One v0.27 bug forced all traffic via DERP ([#2846](https://github.com/juanfont/headscale/issues/2846)). Pin versions.
  - **NetBird**: v0.78 (Sep 2026). Kernel WireGuard when available. Its own relay service. Embeddable Go client [`client/embed`](https://pkg.go.dev/github.com/netbirdio/netbird/client/embed), with knobs for many clients in one process (PR #7365). Repo license is mixed (GitHub reports NOASSERTION); server components are reportedly AGPL-3.0 **(unverified)**.
  - **Plain WireGuard**: zero moving parts, but no NAT traversal or coordination. Fine for "node has a public IP".
  - Integration: if a node reports overlay IPs and certs, add them as candidates. Users already on the tailnet then get direct connectivity across WAN with no relay. Browsers cannot join a tailnet; the OS client must be installed.

### 3.5 Signaling design (session start)

```
Browser                 Control plane                      Node agent / streamer
  | POST /sessions {tmpl} |                                        |
  |---------------------->| place() → node X                       |
  |                       |== NodeSpec{+session} (WSS/yamux) =====>| pull/prepare volume/start env+streamer
  |   SSE: pulling 40%... |<== events ==============================|
  |                       |<== Ready{candidates, cert hashes} =====|
  |<-- {candidates, media_token, iroh_ticket?} ---|                 |
  | race candidates (happy-eyeballs, ~150ms stagger):              |
  |  1. WebTransport https://<lan-ip>:47999/s/<sid> + hashes ------>| verify token offline (portal pubkey), jti once
  |  2. WebTransport https://<wan-ip|overlay>:47999/... ---------->|
  |  3. WebRTC: offer → control plane → node → answer (WHEP-like) ->| ICE (STUN/TURN creds minted by CP)
  |  4. WebTransport via cha-relay (node dialed out) -------------->|
  |  5. WebSocket via relay/control plane (TCP, H.264 only) ------->|
  |== media + input ===============================================|
  | on drop: GET /sessions/{id}/token → reconnect (or in-band resume token)
```

Candidate ordering is decided client-side with server hints: same-LAN first (the control plane compares the browser's public IP with the node's WAN IP), then overlay, WAN, relay. PyroWave is offered only when the chosen path is direct and the measured bandwidth allows it.

### 3.6 Media tokens

- Format: PASETO v4.public or a JWT with EdDSA, signed by the control plane's Ed25519 key. Nodes get the public key at enrollment and rotations over the channel.
- Claims: `aud=node_id`, `sid`, `sub=user`, `scope=[view,input,clipboard,audio,mic,files,admin]`, `exp=now+60s` (connect window only), `jti`.
- The node verifies **offline**, needing no callback. Contrast Kasm's "Upstream Auth Address" callback. The node keeps a `jti` replay cache until `exp`.
- Once connected, the session is bound to the transport connection. Reconnects need a fresh token, or a node-issued resume token tied to the connection's TLS exporter **(design proposal)**.
- Share links mint tokens with reduced scopes (e.g., `view`).

---

## 4. Persistence

### 4.1 Model

- **Environment template** = image@digest + runtime config + persistence policy + mounts.
- **Environment instance** (persistent) = user + template + **home volume** (+ optional extra volumes) + placement (node holding the volume).
- **Session** = a running container set bound to an environment instance (or ephemeral).
- Policies:
  - `ephemeral`: no persisted volume. Scratch volume or tmpfs, destroyed on stop. Optional download-out of files.
  - `persistent-home`: **recreate the container from the image on every start, keep only the home volume**. Image upgrades apply automatically. This is the Kasm volume-mapped-profile model.
  - `persistent-container`: keep the container stopped/started (like a pet VM). Discouraged; offer for "full desktop I tinker with".
- **Profile path uniqueness**: key volumes by `(user_id, template_id)` by default. Kasm explicitly warns that browser profiles break when shared across workspaces.
- **No GPU checkpointing.** CRIU cannot snapshot Vulkan/GL GPU state for graphics workloads **(general knowledge; vendor CUDA/ROCm checkpoint tools don't cover display/graphics, unverified for 2026)**. "Suspend" = stop container + keep volume. Fast resume comes from pre-pulled images and warm caches.

### 4.2 Volume drivers on the node

| Driver | Create from golden | Snapshot | Migrate/backup | Notes |
|---|---|---|---|---|
| `dir` (default) | copy / `cp --reflink=auto` (instant on XFS/btrfs) | none (use backup) | rsync / restic / kopia | Works everywhere |
| `zfs` | **`zfs clone pool/golden@vN`**: O(1), CoW | `zfs snapshot` | **`zfs send \| recv`** node→node (over iroh/overlay), incremental | Best for homelab ZFS users. Set per-dataset quotas |
| `btrfs` | `btrfs subvolume snapshot`: O(1) | yes | `btrfs send/receive` | Quotas (qgroups) are slow at scale **(unverified)** |
| `s3-sync` (Kasm-style) | download manifest via presigned URLs | n/a | it *is* the backup | Good for multi-node roaming; slow start for big homes |

- Docker 29 uses the **containerd image store by default on fresh installs** (overlayfs snapshotter supersedes the legacy `overlay2` driver; images under `/var/lib/containerd`) ([Docker docs](https://docs.docker.com/engine/storage/drivers/overlayfs-driver/), [docs issue #26163](https://github.com/docker/docs/issues/26163)). Cha volumes should be **bind mounts of agent-managed paths/datasets**, independent of the Docker storage driver. Note `--storage-opt size` silently fails on the containerd store ([dev.to](https://dev.to/alexgeorgiev17/docker-engine-29s-default-image-store-lets-storage-opt-size-fail-silently-p65)). Enforce quotas at the ZFS/XFS-project level instead.
- Backups: restic (BSD-2) or kopia (Apache-2.0) to S3/B2/local, scheduled by the agent and triggered from the control plane. Migration = snapshot → send → re-point the environment instance's `node_id`.
- Golden homes: per-template skeleton dataset. Example: a Chrome profile with policies applied, or a Steam home with the client pre-bootstrapped (minus credentials). First launch clones the skeleton.

### 4.3 Shared game libraries

- Wolf's default is one game copy per user, with discussions of overlay mounts and deduped libraries still open ([#83](https://github.com/games-on-whales/wolf/issues/83), [#69](https://github.com/games-on-whales/wolf/issues/69)). "Using the same Steam library folder for multiple clients at the same time" is expected to cause problems.
- Proposal:
  - A **library volume** (read-only lower) owned by an admin "library maintainer" session that does installs and updates.
  - Each user's session mounts `overlay(lower=library, upper=<user>/library-upper)`, so per-user writes (shader caches, updates, saves inside the install dir) land in the upper.
  - Periodically "rebase": when the lower is updated, drop uppers for updated apps **(design proposal; Steam behaviour with overlay uppers needs testing: appmanifest state, file locks, anti-cheat)**.
  - Alternative: per-user libraries on ZFS with **clones of a library snapshot**. CoW gives dedupe without overlay semantics.
- Steam Deck-style shader pre-caching and Proton prefixes belong in the per-user upper or home.

### 4.4 Pre-warmed pools

- Kasm staging semantics are a good spec: `desired`, `expiry`, and **only assign when the user's permissions match the staged config** ([Kasm staging](https://www.kasmweb.com/docs/develop/guide/staging.html)).
- For GPU nodes, pools burn VRAM and NVENC sessions. Default to **image pre-pull + warm caches**, and allow small pools only for cheap CPU environments (ephemeral Chrome).
- Pre-pull by digest on every eligible node when the catalog changes. This is the biggest cold-start win.

---

## 5. Images and catalog

- Prior formats:
  - **Kasm registry**: git repo, `workspace.json` schema 1.1, PNG icon, arch list, resource hints, `run_config`/`exec_config` passed to Docker create.
  - **Wolf apps**: TOML runner with `base_create_json`, so arbitrary Docker create JSON.
  - **Unraid CA**: XML templates per container **(not re-verified this session)**.
  - **linuxserver** desktop images: now **Selkies-based** (`baseimage-selkies` replaced the KasmVNC base: Selkies + pixelflux + pcmflux + NGINX + labwc; Wayland mode via `PIXELFLUX_WAYLAND=true`) ([docs](https://docs.linuxserver.io/images/docker-baseimage-selkies/)). Selkies is MPL-2.0.
- **Recommended Cha catalog**:
  - A git- or HTTPS-hosted `index.json` plus per-app `cha.app.json`:
    ```json
    {
      "schema": "cha.app/v1",
      "id": "kde-plasma",
      "name": "KDE Plasma",
      "kind": "desktop",
      "image": "ghcr.io/cha-portal/kde@sha256:…",
      "arch": ["amd64"],
      "requires": {"gpu": {"vendor": ["nvidia","amd","intel"], "vram_mb": 2048}, "encoders": ["h264|hevc|av1|pyrowave"]},
      "streaming": {"capture": "wayland", "default_codec": "av1", "max_fps": 144},
      "persistence": {"home": "/home/user", "default": "persistent-home", "skeleton": "kde-default"},
      "mounts": [{"type": "library", "name": "steam", "path": "/games/steam", "mode": "overlay"}],
      "devices": ["uinput", "dri"], "caps": [], "env": {},
      "signature": {"cosign_identity": "https://github.com/cha-portal/images/.github/workflows/build.yml@refs/heads/main", "issuer": "https://token.actions.githubusercontent.com"}
    }
    ```
  - Self-describing OCI labels (`sh.cha.app.*`) so any image can be added by reference.
  - **Import adapters**: Kasm registries (map `run_config`), Wolf `config.toml` apps, GoW images, linuxserver Selkies images (run their own web stream, or the Cha streamer when capturing their Wayland socket — other slices decide).
  - **Signing**: cosign (Apache-2.0) keyless, with bundles stored as OCI referrers. Node policy `off|warn|enforce`, using a sigstore library (sigstore-go / sigstore-rs). Homelab default `warn`.
  - Optionally distribute the catalog itself as an OCI artifact (ORAS) so air-gapped registries mirror it.

---

## 6. AuthN / AuthZ

- **Built-in accounts + passkeys** as the homelab default, so no IdP is required. Libraries: `webauthn-rs` (Rust; MPL-2.0 **(unverified)**) or `go-webauthn/webauthn` (Go, BSD-3 **(unverified)**). Optional password + TOTP for bootstrap.
- **OIDC** (generic):
  - **Pocket ID**: BSD-2, passkey-only. OpenID Certified (Basic/Config/Form Post) as of v2.10; v2.12.0 on 2026-07-29 ([repo](https://github.com/pocket-id/pocket-id), [cert article](https://bex.co/blog/2026/08/06/pocket-id-tinyauth-oidc-certification)).
  - **Authelia**: Apache-2.0. OpenID Certified (May 2025). Passkeys since 4.39 (2025-03-16); also device-code grant ([4.39 notes](https://www.authelia.com/blog/4.39-release-notes/)).
  - **Authentik**: core MIT with an enterprise directory **(license split unverified)**.
  - **Keycloak**: Apache-2.0.
  - Map an OIDC `groups` claim to Cha roles. Support PKCE, `prompt`, back-channel logout **(nice-to-have)**.
- **RBAC (homelab-simple, team-ready)**:
  - Roles: `owner`, `admin` (nodes, templates, users), `user` (launch granted templates), `guest` (share-link only).
  - Grants: `role → template-group`, `role → node-pool`.
  - Data model has `org_id` from day one (single org in homelab), like Nestri's "personal team" trick.
- **Share links**: signed, expiring, revocable. Modes `view`, `input` (co-op), `control`. Max viewers. Owner sees who's connected. Kasm has session sharing and casting ([casting](https://kasm.com/docs/latest/guide/casting.html)).
- **Audit log**: append-only table. Records `(ts, actor, actor_ip, action, target, result, details)` for logins, launches, share-link creation, admin changes and node enrollments. Export to syslog/OTLP later.
- **Per-session media tokens**: see §3.6.
- **Cookie scope**: if nodes or sessions ever get their own hostnames serving web content (e.g., an in-session file browser), put them on a **different registrable domain** from the portal (Nestri's `nestri.link`).

---

## 7. Control-plane tech stack

### 7.1 Language

| | Rust | Go | TypeScript/Bun |
|---|---|---|---|
| Shares code with media server and native client | **Yes**: same `cha-proto`, codecs, transport | No (protobuf only) | No |
| QUIC/WebTransport | quinn/noq, **wtransport** (Apache-2.0), **iroh** | quic-go + **webtransport-go** (MIT) | weak (node bindings) |
| WebRTC | webrtc-rs, str0m **(unverified maturity)** | **pion** (MIT, very mature) | node-datachannel |
| Mesh embedding | iroh first-class; tailscale-rs pre-alpha | **tsnet**, Headscale, NetBird `embed`, go-iroh (community) | — |
| Docker API | bollard | official moby client, compose-go | dockerode |
| GPU inventory | nvml-wrapper, ash (Vulkan caps) | go-nvml (official) | — |
| ACME | instant-acme / rustls-acme **(unverified)** | **certmagic / lego** (very mature) | — |
| CRUD velocity | slower compile, good with axum + sqlx | fast | fastest (Nestri chose TS) |
| Reusable AGPL/GPL code | RustDesk (AGPL), moonlight-common-rust (GPL-3) | **Coder (AGPL)** | — |

**Recommendation: Rust**. The data plane (streamer, PyroWave via nespyro or upstream, Vulkan capture, QUIC) and the native client will be Rust regardless. One language across control plane, agent, streamer and player removes a whole class of protocol drift, as Nestri's `nesprotocol` notes. iroh is Rust-native, which matters because LAN and WAN must both be first-class.

- **Go is the strong #2.** Choose Go if the team is more productive there, or if tsnet/Headscale embedding and Coder code reuse become central. The node agent can then still embed or spawn the Rust streamer.
- TypeScript only for the web UI.

### 7.2 Libraries (Rust option)

- HTTP: **axum** + tower. OpenAPI via **utoipa**, generating a TS client (openapi-typescript + openapi-fetch) for Vue.
- Node channel: tokio-tungstenite + **yamux** + **tonic** over yamux streams, or a simple length-prefixed prost envelope protocol.
- DB: **sqlx** with **SQLite (WAL) default**, Postgres later. Keep queries portable or use SeaORM/Diesel for dual-backend. Litestream (Apache-2.0) or `VACUUM INTO` for SQLite backup.
- Realtime to browser: **SSE** for status and event streams (HTTP/1.1-friendly, reverse-proxy and Cloudflare friendly, auto-reconnect). WebSocket only where bidirectional.
- Auth: `openidconnect`, `webauthn-rs`, `pasetors` or `jsonwebtoken` (EdDSA).
- Docker: `bollard`. GPUs: `nvml-wrapper`, `ash`.
- Transport: `iroh`, `iroh-relay` (self-host), `wtransport`. Optional `webrtc-rs`/`str0m` for WebRTC on the node.

(Go equivalents: chi/huma or connect-go, sqlc, River jobs, coreos/go-oidc or zitadel/oidc, go-webauthn, moby client, go-nvml, pion, quic-go/webtransport-go, certmagic, tsnet.)

API style: **REST + OpenAPI for the browser API** (simplest for a Vue app and third-party scripts). **Protobuf for the node channel and the media protocol.** Connect has first-party Go and TS ([connect-go](https://github.com/connectrpc), [connect-es](https://github.com/connectrpc/connect-es); [connect-query-es](https://www.npmjs.com/package/@connectrpc/connect-query) is TanStack-Query-based; there is a community [connect-query-vue](https://github.com/2nofa11/connect-query-vue)). Pick Connect end-to-end only if we go Go.

### 7.3 Frontend (Vue)

- Vue 3 + Vite + TypeScript, Pinia, **@tanstack/vue-query**, vue-router.
- UI kit: **Nuxt UI v4**. It is MIT, now includes the former Pro components, works in plain Vue/Vite, and is built on **Reka UI** + Tailwind ([Nuxt UI v4](https://nuxt.com/blog/nuxt-ui-v4)). Alternatively shadcn-vue on Reka UI.
- **The stream player is a framework-agnostic TS package** (`@cha/player`): WebTransport/WebRTC transport, WebCodecs decode, canvas/WebGPU render, AudioWorklet, Gamepad/pointer-lock/keyboard-lock input, stats overlay. It may include WASM pieces (protocol parsing; PyroWave WebGPU decode if another slice validates the [WebGPU fork](https://github.com/imbcmdth/ffrwd-package-pyrowave)). Vue only mounts it, so the player can be reused in Electron or kiosk shells.

### 7.4 Packaging (homelab)

- `cha-portal`: **one container** (API + static SPA + optional embedded `iroh-relay` and TURN-REST). SQLite on a volume. Sits behind the user's reverse proxy (Caddy/Traefik) or Cloudflare Tunnel.
- `cha-node`: a compose stack:
  - `cha-agent`: docker.sock, `/dev/dri`, NVIDIA CDI, `/dev/uinput`, volume roots.
  - `cha-streamer`: per session or shared; other slices decide.
  - Optional `wolf` for Moonlight compatibility.
  - Optional `coturn`/`cha-relay`.
- Optional `cha-relay` on a VPS for CGNAT'd homes: QUIC/WebTransport relay + iroh-relay + TURN, all in one container.

---

## 8. Thin native client

### 8.1 Why native at all

The browser cannot give us:

- Keyboard lock outside Chromium.
- Raw mouse on older Safari/Firefox.
- HDR presentation control (WebGPU `toneMapping` is Chrome-only).
- VRR, exclusive or low-latency present modes (MAILBOX/IMMEDIATE), and precise frame pacing.
- **DualSense adaptive triggers / HD haptics** (browser has only `dual-rumble`; `trigger-rumble` is Chrome 126+ and Xbox-only semantics).
- **PyroWave** (Vulkan compute; browser needs a WebGPU port).
- iroh direct connectivity (browser iroh is relay-only).
- Apple TV (tvOS has no WebKit).

### 8.2 Options

| Option | Video decode / present | Raw input, kbd/mouse grab | HDR / VRR | Gamepad haptics | PyroWave | Verdict |
|---|---|---|---|---|---|---|
| **Pure Rust: SDL3 (or winit) + ash/Vulkan (or wgpu) + FFmpeg hwaccel** | Vulkan Video, D3D11VA, VAAPI, VideoToolbox via FFmpeg; zero-copy into Vulkan | SDL relative mouse + keyboard grab | Vulkan HDR swapchain (`VK_EXT_hdr_metadata`, gamescope on Deck) | **SDL3**: rumble, trigger rumble, DualSense effects, gyro, touchpad, Steam Deck rumble | **yes** (nespyro / upstream) | **Recommended** |
| moonlight-qt fork | mature D3D11/VAAPI/Vulkan(libplacebo)/Metal renderers, frame pacing | yes | yes (Vulkan renderer HDR on Linux/Deck) | SDL2/SDL3 | needs protocol work | Reference, not base (Qt/QML, GameStream protocol) |
| Tauri v2 | WebView2 = Chromium OK; **WKWebView** tied to OS; **WebKitGTK** distro-dependent | WKWebView **denies pointer lock without private API** | weak | Gamepad API only | no | **No** for the stream surface |
| Tauri + CEF runtime | Chromium everywhere | Chromium | Chromium | Gamepad API | no | **alpha** (tauri-cef v3.0.0-alpha.7, 2026-05-23); Linux issues in Oct 2026 |
| Electron | Chromium: WebTransport, WebCodecs, WebGPU, pointer-lock `unadjustedMovement`, keyboard lock | good | partial | Gamepad API | WebGPU port only | OK as "packaged browser client"; heavy; no PyroWave |
| Android native (Kotlin + Rust core) | MediaCodec low-latency | n/a | HDR10 | Android input | yes (Nova shows it) | Phase 3 |
| iOS / tvOS (Swift + Rust core) | VideoToolbox + Metal | n/a | EDR | GameController | via Metal port **(unverified)** | Phase 3; **only option for Apple TV** |

Evidence:

- **WKWebView pointer lock**: "the web view denies requestPointerLock, even from a user gesture". Projects work around it with private `WKUIDelegate` APIs ([craft#333](https://github.com/craft-native/craft/issues/333), [cmux#14518](https://github.com/manaflow-ai/cmux/pull/14518)).
- **WebKitGTK 2.52** (2026-03-18) highlights mention Keyboard Lock, WebRTC network-process moves, and PQ→SDR tone-mapping, but **no WebTransport/WebCodecs/WebGPU** ([highlights](https://webkitgtk.org/2026/03/18/webkitgtk-2.52-highlights.html)). Treat them as unavailable in Tauri on Linux **(absence-of-mention; unverified)**.
- Tauri's own docs: macOS/iOS WKWebView is tied to OS updates; Linux webkit2gtk versions vary widely by distro ([webview versions](https://v2.tauri.app/reference/webview-versions/)).
- **Tauri CEF**: tauri-cef v3.0.0-alpha.7 (2026-05-23). Linux issues: Chromium 151 first-run EULA blocks startup ([#15979](https://github.com/tauri-apps/tauri/issues/15979)); idle 100% CPU, reported 2026-10-01 ([#16189](https://github.com/tauri-apps/tauri/issues/16189)). A standalone [SableClient/tauri-runtime-cef](https://github.com/SableClient/tauri-runtime-cef) exists.

### 8.3 Existing codebases to learn from

- **Magic Mirror** ([colinmarc/magic-mirror](https://github.com/colinmarc/magic-mirror)): Rust game streaming, headless multitenant compositor, Vulkan Video.
  - `mm-client` is **MIT**: ash 0.38 + winit + **ffmpeg-next 7 hwaccel (Vulkan, VideoToolbox)** + gilrs + cpal + imgui. It **prefers MAILBOX present mode**.
  - `mm-client-common` is **MIT**: **quiche** QUIC + **uniffi**, which backs a native **SwiftUI** macOS client.
  - `mm-protocol` is MIT. `mm-server` is **BUSL-1.1**, so do not reuse it.
  - Last commit 2025-10-04 (possibly dormant).
  - **Best single reference for our player architecture.**
- **moonlight-qt** (GPL-3.0, AGPL-compatible):
  - Latest *tag* is still **v6.1.0 (2024-09-17)**, but master is active (pushed 2026-10-03).
  - v6.0 added the Vulkan renderer (libplacebo ≥ v7.349) with HDR on Linux/Steam Deck, the Metal renderer, Vulkan Video decode (H.264/HEVC/AV1), AV1, and E2E encryption. v6.1 added YUV444 and 500 Mbit/s ([releases](https://github.com/moonlight-stream/moonlight-qt/releases)).
  - Reference for **frame pacing**, renderer selection, and Steam Deck behaviour.
  - Forks: Nonary's VRR fork (`v6.1.0-vrr18`). Claims of lower Linux VRR present latency (acquire at present time; 3.9→2.0 ms median on Deck 4K) appear in fork notes **(unverified)**.
- **moonlight-common-rust** ([MrCreativ3001](https://github.com/MrCreativ3001/moonlight-common-rust), GPL-3.0): sans-IO GameStream protocol. Hosts: Sunshine, Wolf, Apollo. H.264/H.265; no AV1, no video encryption, no RFI. Compiles to WASM. Useful if Cha connects to **external Sunshine/Vibepollo/Wolf hosts** (feature 8).
- **moonlight-web-stream** (GPL-3.0, Rust): bridges Sunshine to browsers via **WebRTC with WebSocket fallback**, browser `VideoDecoder` (needs HTTPS), TURN config ([repo](https://github.com/MrCreativ3001/moonlight-web-stream)). A ready-made "external host" bridge pattern.
- **Nestri** guest stack + **nespyro** (MIT), iroh tickets, media-transport lessons (§1.5).
- **Nova** (Android, [papi-ux/nova#355](https://github.com/papi-ux/nova/pull/355), merged 2026-09-25):
  - PyroWave decode on Vulkan compute, borrowing the app's Vulkan device and writing straight to presentable images. BT.709 conversion; **4:4:4, HDR10**; partial-frame decode on loss.
  - moonlight-common-c with 5 protocol patches to negotiate the codec. ~0.35 bits/pixel guidance. Debug builds only.
  - Proves PyroWave on Android Vulkan.
- **ALVR** (MIT, Rust): Android MediaCodec decode, adaptive bitrate on decoder latency, frame-pacing knobs ([pipeline](https://deepwiki.com/alvr-org/ALVR/5.1-video-streaming-pipeline)).
- **Moonshine** (BSD-2, Rust Moonlight server, compositor per stream, Vulkan encode) ([repo](https://github.com/hgaiser/moonshine)).
- **RustDesk** (AGPL-3.0): Rust core + Flutter UI; hole-punch/relay.
- **PyroFling** (Themaister): Vulkan layer capture + client, the PyroWave origin ([blog](https://themaister.net/blog/2023/11/12/my-scuffed-game-streaming-adventure-pyrofling/)).
- Vulkan Video in Rust: `vulkan_video` (ralfbiedert), `vacc`, `gpu-video` (wgpu integration) **(maturity unverified)**. FFmpeg hwaccel remains the pragmatic path.

### 8.4 Recommended native-client strategy

1. **Phase 0 — browser only.** Chromium first; Firefox/Safari 26.4+ supported with degraded input. Same player package in Electron for a kiosk build if wanted.
2. **Phase 1 — `cha-player` desktop (Linux incl. Steam Deck, Windows, macOS)**:
   - Rust, **SDL3** for windowing, input, gamepads, haptics. SDL is Zlib; latest tag 3.4.18. SDL3 exposes trigger rumble and DualSense/Steam Deck features ([SDL_RumbleJoystickTriggers](https://wiki.libsdl-org/SDL3/SDL_RumbleJoystickTriggers), [Phoronix: Deck rumble](https://www.phoronix.com/news/SDL3-Steam-Deck-Rumble)). Adaptive-trigger specifics are via raw effect packets **(partially verified)**.
   - **ash/Vulkan** presenter (MoltenVK on macOS, or a Metal path later). Mailbox/immediate modes, HDR swapchain, VRR-friendly pacing modelled on moonlight-qt.
   - **FFmpeg hwaccel** decode; **PyroWave** via nespyro on Vulkan 1.3 desktop GPUs.
   - Transport: **iroh** (direct + relay) and plain QUIC/WebTransport to the same streamer endpoints.
   - Launch: the dashboard "Open in Cha Player" button uses `cha://connect?ticket=<one-time>`. The player exchanges the ticket for a media token, so it holds no long-lived credentials. Also `cha-player login` for device-code login on Deck Game Mode.
   - Ship as Flatpak (Deck) and MSI/DMG.
   - Minimal in-stream overlay (imgui/egui); no full UI.
3. **Phase 2 — `cha-client-core`** extracted (transport, protocol, jitter buffer, stats, input encoding) with **uniffi** bindings, the Magic Mirror pattern.
4. **Phase 3 — Android (Kotlin, MediaCodec + Vulkan PyroWave) and iOS/tvOS (Swift, VideoToolbox + Metal)** on the core. Apple TV is native-only.
5. **External hosts (Sunshine/Vibepollo/Wolf)**: speak GameStream via moonlight-common-c or moonlight-common-rust (both GPL-3.0, compatible with an AGPL/GPL Cha) inside the player. The control plane stores pairing certs per user and host. For browsers, use a node-side bridge in the moonlight-web-stream style.

---

## 9. Recommended architecture

```
                       ┌───────────────────────────── cha-portal (1 container) ──────────────────────────────┐
 Browser (Vue SPA) ───►│ REST+OpenAPI · SSE · Auth (passkeys/OIDC) · RBAC · audit · catalog · placement seam │
   │  ▲                │ Node hub (WSS+yamux, node-initiated) · token minting (Ed25519) · SQLite→Postgres     │
   │  │ media token    │ [optional] iroh-relay · TURN-REST creds · DNS-01 cert broker                         │
   │  │ + candidates   └───────────────▲────────────────────────────────────────────────────────────────────┘
   │  │                                │ outbound WSS (signed node identity)
   │  │                ┌───────────────┴──────────── cha-node (docker compose, per host) ──────────────────┐
   │  └──────────────► │ cha-agent: reconcile NodeSpec · inventory (NVML/VA/Vulkan) · volumes (dir/zfs/btrfs)│
   │                   │           · image pre-pull · cert rotation (14d WebTransport) · self-update        │
   └══ media (direct) ═► cha-streamer(s) ◄─ env containers (Chrome, KDE, Steam …) · optional Wolf bridge     │
     WebTransport (hash-pinned) / WebRTC / iroh                                                          │
     └─ fallback ─► cha-relay (VPS: QUIC/WebTransport relay + iroh-relay + TURN) ◄─ node dials out          │
                   └──────────────────────────────────────────────────────────────────────────────────────┘
 cha-player (Rust native) ══ iroh / QUIC ══► cha-streamer
```

### Decisions that depend on scale

| Concern | Homelab / small group (default) | Team / multi-tenant (later) |
|---|---|---|
| DB | SQLite (WAL) in the portal container | Postgres; multiple stateless portal replicas; LISTEN/NOTIFY or NATS for fan-out |
| Node auth | join token + node key; waiting room optional | same + short-lived certs, SPIFFE-like identities, node pools per tenant |
| Placement | manual pick or filter/score; data gravity | quotas, fair-share, preemption, reservations, autoscaling (cloud GPUs) |
| Isolation | containers (privileged bits for GPU/uinput) | gVisor/Kata/**microVM with virtio-gpu native context** (nesbox / Incus native-context GPUs) |
| Relays | none on LAN; optional single VPS relay/TURN | regional relays (Coder workspace-proxy / Kasm connection-proxy / RustDesk hbbr model) |
| TLS for nodes | hash-pinned self-signed (+ optional DNS-01 wildcard) | DNS-01 per tenant + hosted relay hostnames |
| Auth | built-in passkeys; optional OIDC | OIDC mandatory, group→role mapping, SCIM, org/teams |
| Catalog | git registry, `warn` signature policy | private registries, `enforce` cosign policy, per-tenant catalogs |
| Persistence | `dir` or ZFS on node; restic backups | S3 profile sync for roaming; replicated storage; migration automation |
| Pools | pre-pull only | staged pools per template with permission matching |

---

## 10. Risks and gotchas

- **Chrome LNA prompts for LAN direct connections** from a publicly served portal (WebTransport since Chrome 147; opt-out removed in 156). WebRTC is not gated today but may be later. Mitigate with split-horizon DNS guidance, a first-run explainer, and a WebRTC fallback.
- **Safari `serverCertificateHashes`**: BCD and a WebKit commit say it ships in 26.4, but WebKit was opposed in 2024. **Test before relying on it**, and keep WebRTC or a real-cert path for Safari.
- **14-day cert ceiling**: rotate and advertise the next hash early. Clock skew on nodes breaks validity checks, so require NTP.
- **Cloudflare Tunnel** cannot carry UDP media to public users, and video through the CDN risks ToS action. Use it only for the control plane.
- **TURN costs and bandwidth**: PyroWave is LAN/direct only. Never relay it.
- **QUIC media pitfalls** (Nestri): datagrams starve keyframe streams; `send_datagram` drops silently; loss is a lagging indicator. Design congestion control around queue backlog and receiver reports.
- **Agent holds docker.sock = root on node.** Keep control-plane → node operations typed (no arbitrary exec by default). Signed agent images. Never expose Wolf's socket over TCP (the Wolf docs warn).
- **Wolf API robustness** (2026 audit findings): malformed requests can kill Wolf. Wrap and supervise.
- **Steam on shared libraries**: concurrent writers and update semantics are untested in overlay mode. Plan per-user libraries as the fallback.
- **Docker 29 containerd store**: `--storage-opt size` silently ignored; legacy graph-driver assumptions break. Do quotas at the volume layer.
- **Tauri/WebKit webviews** lack the APIs a streaming surface needs. Don't build the player there.
- **moonlight-qt** has had no tagged release in two years. Expect to learn from master and forks rather than depend on releases.
- **Licensing**: AGPL/GPL Cha can absorb GPL-3.0 (moonlight-*, Sunshine, Vibepollo), AGPL-3.0 (Coder core, RustDesk), and permissive code. **Cannot** absorb BUSL (Magic Mirror server, Nomad) or GPL-2.0-only code into an AGPL-3.0 work (KasmVNC is GPL-2.0; check "or later" before reuse) **(KasmVNC "or later" status unverified)**.

## 11. Open questions

1. **Control-plane language**: Rust (recommended) vs Go. Decide with whoever owns the media and streamer slice. If the streamer is C++/GStreamer (Wolf-like) rather than Rust, Go becomes more attractive.
2. **Do we operate anything under `cha.sh`?** Options: a public iroh relay, a plex.direct-style `*.nodes.cha.sh` DNS + cert broker, a hosted WebTransport relay. Each is convenient but makes us an operator. Default: all self-hostable, none required.
3. **Primary browser media transport**: WebTransport (direct, hash-pinned) vs WebRTC (ICE/TURN, mature NAT traversal). This determines whether we ship `cha-relay` or TURN by default. Likely both, with WebTransport preferred on LAN.
4. **Does the node agent drive Wolf** (for Moonlight-client compatibility and GoW images) or only our own streamer?
5. **Isolation roadmap**: containers only, or a VM tier (Incus native-context or nesbox) for untrusted multi-tenant use?
6. **Steam library model**: overlay-per-user vs ZFS clones vs per-user installs. Needs a prototype.
7. **Session handoff semantics**: can a browser session be "picked up" by `cha-player` mid-session? (Desirable. The streamer must support re-negotiating codec and transport on reconnect.)

## 12. License notes (reuse candidates)

| Project | License | Reuse in AGPL/GPL Cha |
|---|---|---|
| iroh / iroh-relay | Apache-2.0 / MIT | yes |
| nespyro (Nestri PyroWave port) / PyroWave | MIT | yes |
| Nestri (rest) | Apache-2.0 | yes |
| Wolf, Wolf Den, Fenrir | MIT | yes |
| Coder (core) | AGPL-3.0 | yes (exclude enterprise code; verify) |
| Portainer + agent | Zlib | yes |
| Headscale / Tailscale | BSD-3 | yes |
| NetBird | mixed (BSD-3 client; AGPL server reportedly) | yes (unverified split) |
| RustDesk | AGPL-3.0 | yes |
| moonlight-qt / -common-c / -android / -ios, Sunshine, Vibepollo, Apollo | GPL-3.0 | yes (GPLv3 ↔ AGPLv3 compatible) |
| moonlight-common-rust, moonlight-web-stream | GPL-3.0 | yes |
| Magic Mirror mm-client / mm-protocol / mm-client-common | MIT | yes |
| Magic Mirror mm-server | BUSL-1.1 | **no** |
| ALVR | MIT | yes |
| Moonshine | BSD-2 | yes |
| Selkies | MPL-2.0 | yes (file-level copyleft) |
| KasmVNC | GPL-2.0 | only if "or later" (verify) |
| SDL3 | Zlib | yes |
| pion/webrtc, quic-go/webtransport-go, chisel, coder/wgtunnel | MIT | yes |
| wtransport | Apache-2.0 | yes |
| Tauri | Apache-2.0/MIT | yes |
| cosign | Apache-2.0 | yes |
| Pocket ID | BSD-2 | (external service) |
| Authelia | Apache-2.0 | (external service) |
| Nomad | BSL | **no** |

Licenses come from the GitHub API (`license.spdx_id`) as of 2026-10-03, except where noted.

## Sources (primary first)

- Repos read locally (shallow clones): `nestrilabs/nestri@ef32d5f` (2026-10-03), `games-on-whales/wolf@facb8e0` (2026-09-29), `colinmarc/magic-mirror` (last commit 2025-10-04).
- Kasm: [architecture](https://www.kasmweb.com/docs/latest/guide/system_architecture.html) · [multi-server install](https://docs.kasm.com/docs/tutorials/install/multi-server-install/index.html) · [zones](https://kasm.com/docs/latest/guide/zones/deployment_zones.html) · [staging](https://www.kasmweb.com/docs/develop/guide/staging.html) · [persistent profiles](https://kasm.com/docs/latest/guide/persistent_data/persistent_profiles.html) · [registry](https://www.kasmweb.com/docs/latest/guide/workspace_registry.html)
- Coder: [architecture](https://coder.com/docs/admin/infrastructure/architecture) · [networking](https://coder.com/docs/admin/networking) · [workspace proxies](https://coder.com/docs/admin/workspace-proxies) · [agents API](https://coder.com/docs/reference/api/agents) · [agent pkg](https://pkg.go.dev/github.com/coder/coder/v2/agent)
- Portainer: [edge agent](https://docs.portainer.io/advanced/edge-agent) · [agent README](https://github.com/portainer/agent/blob/develop/README.md)
- Wolf: [configuration](https://games-on-whales.github.io/wolf/stable/user/configuration.html) · [Fenrir](https://github.com/games-on-whales/fenrir) · [Wolf Den](https://github.com/games-on-whales/wolf-den) · [#83](https://github.com/games-on-whales/wolf/issues/83) · [#69](https://github.com/games-on-whales/wolf/issues/69)
- Nestri: [repo](https://github.com/nestrilabs/nestri) · [nesbox](https://github.com/nestrilabs/nesbox)
- k3s tokens: [docs](https://docs.k3s.io/cli/token) · Incus: [GPU devices](https://linuxcontainers.org/incus/docs/main/reference/devices_gpu/) · Nomad: [license](https://developer.hashicorp.com/nomad/docs/ce-license-support)
- WebTransport: [MDN BCD](https://github.com/mdn/browser-compat-data/blob/main/api/WebTransport.json) · [WebKit 26.4](https://webkit.org/blog/17862/webkit-features-for-safari-26-4/) · [w3c/webtransport#623](https://github.com/w3c/webtransport/issues/623) · [WebKit commit](https://www.mail-archive.com/webkit-changes@lists.webkit.org/msg236930.html) · [Firefox bug](https://bugzilla.mozilla.org/1873263)
- LNA: [Chrome blog](https://developer.chrome.com/blog/local-network-access) · [Sentry radar](https://github.com/getsentry/browser-updates-radar/issues/29)
- Certs: [LE 6-day/IP GA](https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability) · [plex.direct](https://words.filippo.io/how-plex-is-doing-https-for-all-its-users/)
- Cloudflare: [Tunnels FAQ](https://developers.cloudflare.com/cloudflare-one/faq/cloudflare-tunnels-faq/) · [service terms](https://www.cloudflare.com/service-specific-terms-application-services/) · [Realtime pricing](https://developers.cloudflare.com/realtime/sfu/platform/pricing/)
- iroh: [1.0](https://www.iroh.computer/blog/v1) · [WASM](https://docs.iroh.computer/languages/wasm-browser) · [go-iroh](https://github.com/tmc/go-iroh)
- Tailscale: [tsnet](https://tailscale.com/docs/features/tsnet) · [peer relays GA](https://tailscale.com/blog/peer-relays-ga) · [tailscale-rs](https://tailscale.com/blog/tailscale-rs-rust-tsnet-library-preview) · Headscale [releases](https://github.com/juanfont/headscale/releases) · NetBird [embed](https://pkg.go.dev/github.com/netbirdio/netbird/client/embed)
- Selkies: [design](https://selkies-project.github.io/selkies/design/) · linuxserver [baseimage-selkies](https://docs.linuxserver.io/images/docker-baseimage-selkies/)
- Docker 29 storage: [overlayfs driver](https://docs.docker.com/engine/storage/drivers/overlayfs-driver/)
- Auth: [Pocket ID](https://github.com/pocket-id/pocket-id) · [Authelia 4.39](https://www.authelia.com/blog/4.39-release-notes/)
- Frontend: [Nuxt UI v4](https://nuxt.com/blog/nuxt-ui-v4) · [connect-es](https://github.com/connectrpc/connect-es)
- Native client: [moonlight-qt releases](https://github.com/moonlight-stream/moonlight-qt/releases) · [moonlight-common-rust](https://github.com/MrCreativ3001/moonlight-common-rust) · [moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream) · [Magic Mirror](https://github.com/colinmarc/magic-mirror) · [Nova PyroWave PR](https://github.com/papi-ux/nova/pull/355) · [PyroWave](https://github.com/Themaister/pyrowave) · [PyroFling](https://github.com/Themaister/pyrofling) · [Tauri webview versions](https://v2.tauri.app/reference/webview-versions/) · [WebKitGTK 2.52](https://webkitgtk.org/2026/03/18/webkitgtk-2.52-highlights.html) · [craft#333 WKWebView pointer lock](https://github.com/craft-native/craft/issues/333) · [Tauri CEF #16189](https://github.com/tauri-apps/tauri/issues/16189) · [SDL3 trigger rumble](https://wiki.libsdl-org/SDL3/SDL_RumbleJoystickTriggers) · [ALVR pipeline](https://deepwiki.com/alvr-org/ALVR/5.1-video-streaming-pipeline) · [Moonshine](https://github.com/hgaiser/moonshine)
- NVENC / NVML: [VideoCardz](https://videocardz.com/newz/nvdia-geforce-gpus-now-support-up-to-8-concurrent-nvenc-encoding-sessions) · [NVML device queries](https://docs.nvidia.com/deploy/nvml-api/api/group__nvmlDeviceQueries.html)
- RustDesk: [self-host](https://rustdesk.com/docs/en/self-host/)
