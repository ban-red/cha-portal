# Engineering roadmap: review and plan (2026-10-27)

A review of the repository as it stands at 0.5.2, measured against how the strongest open-source projects of a similar shape run in 2026, and a plan in four phases. [`docs/PLAN.md`](../PLAN.md) stays the product roadmap: this plan is about the engineering that carries it (supply chain, security, correctness at the seams, tests, CI, operations, releases, contributors). Where it touches the product, it points at PLAN's phases rather than restating them.

Phase A is done, in the change that adds this file, and so is an optimisation pass that followed it ([§3a](#3a-optimisation-pass-done)). Phases B to D are proposals; the decisions they need from the owner are collected in [§5](#5-decisions-for-the-owner).

---

## 1. Where the project stands

### 1.1 Size and shape

| Area | Size | Notes |
|---|---|---|
| Rust | ~184,000 lines in 21 crates | `cha-streamer` 48.5k, `cha-node` 25.2k, `cha-control` 25.1k, `cha-gamestream` 23.3k, `cha-player` 16.8k, `cha-nvenc` 12.6k (11.3k of it bindgen output) |
| Web | ~28,000 lines | the portal 14.5k (Vue 3), `@cha/player` 12.5k, `@cha/pyrowave-webgpu` 1k plus WGSL |
| Tests | 1,359 Rust tests in 65 test binaries (23 integration-test files plus inline modules; 16 need a GPU and are ignored in CI), 1,002 TypeScript tests in 49 files, Python tests for the images and the host installer | `cha-control/tests/api.rs` alone is 4.8k lines |
| Decisions | 23 ADRs, 15 plans (this one included), a research survey, benchmark results | |
| Data | 19 SQLite migrations | forward-only, numbered |

### 1.2 What it already does well

These are the practices the strongest 2026 projects are known for, and this repository already has them. They should be kept, not reworked:

- **Decisions written down.** ADRs that are superseded, never rewritten; plans with exit criteria; a provenance file for ported code.
- **A pinned, least-privilege CI.** Every action is pinned by commit, checkouts don't keep credentials, workflows default to `contents: read`.
- **Signed build provenance** on every published image, and images rebuilt only when their inputs' hash changes (`scripts/image-inputs.py`).
- **One gate locally and in CI.** `scripts/check.sh` is exactly what CI runs, and clippy is clean with `-D warnings` across the whole workspace on Rust 1.93 (checked for this review).
- **Unsafe code that explains itself.** Outside bindgen output, about 95% of `unsafe` blocks carry a `SAFETY:` comment.
- **Shared contracts with shared tests.** `@cha/ui-spec` gives both players one set of cases.
- **An operator's view.** `cha-node --doctor`, the stats panel's copyable report, `install.sh --check`.

### 1.3 What the review found

Each finding was checked against the code or a tool's output, not inferred.

| # | Finding | Evidence | Status |
|---|---|---|---|
| F1 | **Shipped dependencies had published vulnerabilities**, and nothing checked for them | `rustls` 0.23.45: RUSTSEC-2026-0346 to -0349 (among them an unauthenticated ServerHello downgrading later connections); `rustls-webpki` 0.103.15: RUSTSEC-2026-0382 (distrusted intermediates accepted); `tokio` 1.53.2: RUSTSEC-2026-0386; Vite 8.3.2: six advisories in the dev server, which `bun run dev` exposes on the LAN | **Fixed** (A1) |
| F2 | The licence rule in CONTRIBUTING was enforced by review only | no `deny.toml`; 21 licence kinds in the graph | **Fixed** (A2) |
| F3 | Dependabot watched actions and the base image, not Cargo or Bun | `.github/dependabot.yml` | **Fixed** (A3) |
| F4 | A release meant editing the version in 19 manifests; shared dependencies were declared 2 to 15 times each, with their versions and comments drifting | `tokio` declared 15 times, `serde_json` 13, `serde` 11; comments like "0.29: the version axum uses" in two crates | **Fixed** (A4) |
| F5 | Argon2 (tens of milliseconds of CPU per call) ran on the async runtime's workers, so a burst of sign-ins stalled the API and every node's channel | `auth::verify_password` called inline in `api::login` | **Fixed** (A5) |
| F6 | The CLA workflow uses an action whose repository is **archived**, with write permissions at workflow level | zizmor 1.31.1: `archived-uses`, `excessive-permissions` | Permissions **fixed** (A6); replacement open (B2) |
| F7 | Sign-in has no throttling, a 3-character password minimum (ADR 0006), and records the proxy's address behind a proxy | `auth.rs`, `production-deployment.md` step 4 "to do" | Open (B1) |
| F8 | ~77 API types are written twice, in Rust and in `web/apps/portal/src/api.ts` (931 lines), with nothing checking they agree | a renamed field compiles on both sides and fails at run time | Open (C1) |
| F9 | The parsers that take bytes from the network have no fuzzing | `cha-gamestream`'s host-side RTSP, SDP, control and input parsers read any LAN client's bytes before pairing; `cha-proto` reassembly; the streamer's PulseAudio protocol server reads the app container's | Open (B4) |
| F10 | Large modules | `cha-node/src/environments.rs` is 7.7k lines (one `impl` of 1.9k); `SessionView.vue` 1.07k; `api.ts` 931 | Open (C2) |
| F11 | No linter for the web code, and two TypeScript majors in one workspace | 6.0 in the portal, player and specs; 7.0 in `@cha/pyrowave-webgpu`; the player's and decoder's own typechecks never ran in CI | Typechecks **added to CI** (A7); lint open (C3) |
| F12 | The metrics PLAN §7 describes don't exist yet | only `/api/health` | Open (D1) |
| F13 | One CI job of up to 45 minutes builds and tests the spikes, GStreamer included, on every push | `check.yml`, `members = ["crates/*", "spikes/*/server"]` | Open (C4) |
| F14 | Release notes are generated from commit titles; a release edits ~10 places in the docs by hand | `CONTRIBUTING.md` *Releasing* | Open (D2) |
| F15 | No software bill of materials on the images | `publish.yml` attests provenance only | Open (B5) |
| F16 | 53 `unsafe` sites outside bindgen output fail `clippy::undocumented_unsafe_blocks`, almost all because the `SAFETY:` comment sits above a wrapping call rather than the block; 24 `#[allow]`s have no reason | clippy run for this review | Open (C5) |
| F17 | `SECURITY.md` said there were no tagged releases | | **Fixed** (A8) |
| F18 | The toolchain is six releases behind | CI, `rust-version` and the Dockerfiles' builders are 1.93 (2026-01-22); Rust is at 1.99.1 (2026-10-15) | **Fixed** (§3a) |

---

## 2. What the strongest projects do in 2026

A survey on 2026-10-27 of 19 projects of a similar shape (21 repositories): Rust, web or both, self-hosted, delivered as containers. They were Zed, uv, ruff, Deno, Tauri, Bevy, Pingora, Vector, nextest, RustDesk, Immich, Jellyfin (server and web), Home Assistant (core and frontend), Coder, Headscale, Sunshine, Apollo, Selkies and Wolf. The survey read their root configs and `.github/`, and took tool versions from crates.io, npm and the tools' releases. It complements [`docs/research/`](../research/README.md) (2026-10-03), which covers the product field.

### 2.1 Engineering practice

| Topic | What the strong projects do | Cha after Phase A |
|---|---|---|
| Dependency policy | cargo-deny 0.20.2 in CI (uv, ruff, Bevy, Vector); uv also allowlists crates with build scripts (`[bans.build]`), a tripwire against a malicious `build.rs` | licences, bans and sources on every push; advisories weekly; no build-script list yet (C7) |
| Unused dependencies | cargo-shear 1.14 (uv, ruff, Zed), which supersedes cargo-machete | none (C7) |
| Lints | `[workspace.lints]` everywhere but Deno and RustDesk; Zed denies `dbg_macro` and `todo` | table in place with those guards |
| Toolchain | a `rust-toolchain.toml` pinned to a current release (uv and ruff 1.99.0, Zed 1.98.1), separate from `rust-version`, the minimum, which uv raises only once a release is ~85 days old | **1.93, six releases behind 1.99.1** (C6) |
| Update bots and hakari | Renovate run as a scheduled workflow with a post-upgrade script that reruns `cargo hakari generate` (nextest, Oxide). Cooldowns are the norm: Dependabot waits 3 days by default since 2026-07-14, Astral's preset 30 | Dependabot, lockfile-only for Cargo, 7-day cooldown |
| Actions security | zizmor in uv, ruff, Zed, Bevy, Coder, Home Assistant, Immich, nextest; OpenSSF Scorecard weekly (Coder, Vector); harden-runner in audit mode (Coder); workflow lockfiles, in public preview since 2026-10-14 | zizmor offline on every push (B3 for the rest) |
| Image supply chain | provenance plus an SBOM: `sbom: true` on `docker/build-push-action` (stored in the image index), or syft and cosign; `actions/attest` v4 is now the primary action and `attest-sbom` is deprecated | provenance only (B5) |
| Fuzzing | ruff fuzzes daily; **Selkies**, the nearest project, runs ClusterFuzzLite on its STUN, RTP/RTCP, SCTP and SDP parsers, 5 minutes per pull request and an hour nightly | none (B4) |
| Tests | cargo-nextest (uv, ruff, Zed, Vector, Deno, Pingora); insta snapshots; proptest; Playwright end to end (Immich, Home Assistant, Coder, Selkies) | `cargo test`; Bun's runner; no end-to-end (C4, C8) |
| Web lint | ESLint 10 with `eslint-plugin-vue` is the only linter that checks Vue templates; oxlint 1.90 reads `<script>` only; Biome 2.6's Vue support is experimental | none (C3) |
| TypeScript | TypeScript 7 (2026-07-08) has no API yet, and Microsoft tells Vue projects to stay on 6.0; 7.1 (beta 2026-10-20, RC due ~November) adds one, and `@vue/language-core` 4 alpha targets it | 6.0 for Vue, correct; `@cha/pyrowave-webgpu` already on 7, also fine (no Vue) |
| Releases | release-plz 0.4 `git_only` mode (tags, no crates.io) with git-cliff changelogs; Keep a Changelog; Zed's PR template asks for release notes | generated notes (D2) |
| Metrics | Prometheus off by default on a private listener (Headscale `127.0.0.1:9090`, Coder `localhost:2112`); OpenTelemetry for Rust 0.33: logs and metrics stable, traces and the Prometheus exporter still beta | none (D1) |
| Community | issue forms (17 of 21), PR templates (14), CODEOWNERS (11), codes of conduct (9), `AGENTS.md` (11), a GOVERNANCE file (0) | forms and template (A8) |
| Docs sites | Starlight (Tauri, Biome), mdBook (Zed), mkdocs-material (uv, ruff, Headscale), Docusaurus (Immich) | Markdown on GitHub (D3) |

### 2.2 What users of comparable projects ask for most

From the issue trackers of Sunshine, Apollo, Moonlight, Wolf, Selkies, Kasm and Steam-Headless, most-requested first, with where Cha stands:

| # | Ask | Cha today |
|---|---|---|
| 1 | A virtual display that follows the client's resolution and refresh rate | **Built**: the compositor resizes to the player at run time (P1.3) |
| 2 | HDR end to end | Backlog ("End-to-end HDR10") |
| 3 | Clipboard and file transfer | Text clipboard built; files in the backlog |
| 4 | Multi-monitor | **Not planned.** Punktfunk 0.45 (2026-10-25) streams up to four screens per session, and Vibepollo 2.0 up to eight virtual displays |
| 5 | Controllers that work fully without privileged containers (Steam Input, gyro, touchpad) | **Built**: uhid and uinput through the streamer, never `--privileged` |
| 6 | Wayland desktops and a cursor that feels local | **Built**: our compositor; the cursor drawn client-side in desktop mode |
| 7 | Placement across several GPUs | Built across nodes and device kinds (NVIDIA, VA-API, CPU); within one node, at most one NVIDIA device today (`docs/devices.md`) |
| 8 | Shared game libraries | Built, experimental (a shared Steam library, local or NAS) |
| 9 | Kubernetes, Podman and rootless, Prometheus, Unraid | Prometheus is D1; the rest not planned |
| 10 | Touch modes, audio latency, adaptive bitrate | Delay-based rate control built; touch partly (Steam's touch mode) |

Two needs come up in every tracker, whatever the feature: **regular releases**, and visible signs that a project is alive. Moonlight users ask for "a release in 2025/2026", Apollo users ask whether the project is dead (no commit since May), and Wolf users ask for semver tags. Cha already tags releases. A changelog (D2) and a published cadence would make that visible.

### 2.3 What changed in the field since the 2026-10-03 survey

- **PyroWave froze bitstream v1 and API 1.0 on 2026-10-03**, and on 2026-10-07 added a `PWV1` header with a version byte. That answers the "no version field" risk in PLAN §10: negotiate v1 rather than pinning a commit. Whether the WGSL port matches v1 is unchecked (D4).
- **Selkies 2.1.0** (2026-10-21) turned congestion control on by default (WebSocket median latency on a 5 Mbit/s link from 142 to 35 ms), falls back to WebSocket when WebRTC fails, added 10-bit, and had a security audit with fuzzing. It still has no WebTransport.
- **Punktfunk 0.44 and 0.45** (2026-10-15 and -25) moved to one UDP port and four screens per session; its browser client still disables PyroWave. Cha remains the only browser PyroWave decoder.
- **Vibepollo 2.0.1** (2026-10-13) added paced UDP bandwidth calibration for PyroWave. Wolf's PyroWave PR (#517) is still open, and Wolf now has monthly releases.
- **Safari's WebTransport stall** (WebKit 319818) still reproduces on Safari 27 (2026-10-18), so PLAN's "Safari = WebRTC" stands.

---

## 3. Phase A: done in this change

Each item was verified locally on the CI toolchain (Rust 1.93, Bun 1.4) with every step of `scripts/check.sh`. 1,342 Rust tests passed and none failed. One more, `cha-gamestream`'s `video_and_audio_follow_only_pings_from_the_sessions_client`, binds IPv6 sockets, and the review's container has no IPv6 (`EAFNOSUPPORT` from the kernel, before any changed code runs); CI's runners have it.

- **A1. Vulnerable dependencies updated**, each to the smallest release that fixes it: `rustls` 0.23.45 → 0.23.46, `rustls-webpki` 0.103.15 → 0.103.16, `tokio` 1.53.2 → 1.53.3 (`cargo update -p … --precise`, three packages in `Cargo.lock` and nothing else), and Vite 8.3.2 → 8.3.3 (one line of `bun.lock`; Vite 8.3.3 has the same dependencies). The one advisory left, RUSTSEC-2023-0071 in `rsa` (no fixed release exists), reaches only the S3 spike and `cha-gamestream`'s tests; it is ignored in `deny.toml` with that reason.
- **A2. `cargo-deny`** ([`deny.toml`](../../deny.toml)): licences (CONTRIBUTING's list, plus the permissive ones the graph already holds; egui's bundled fonts as a named exception), sources (crates.io, plus the two git dependencies pinned by commit), bans (OpenSSL, and GStreamer anywhere but the S2 spike, per ADR 0004; checked to fire). `check.sh` runs licences, bans and sources; advisories run in a new [`audit.yml`](../../.github/workflows/audit.yml), weekly and on every lockfile change, together with `bun audit`, so an advisory published today reds that workflow instead of an unrelated pull request.
- **A3. Dependabot for Cargo and Bun**, monthly and grouped, with a 7-day cooldown. Cargo is `lockfile-only`, so the manifests and `cha-workspace-hack` don't move under it; majors stay deliberate.
- **A4. Workspace inheritance.** `version.workspace = true` in every crate but `cha-proto` and `cha-workspace-hack`, so a release sets the version in one place. Every third-party crate that two or more members use is declared once in `[workspace.dependencies]`; our own crates too; hakari writes `cha-workspace-hack.workspace = true`. A `[workspace.lints]` table, opted into by every crate, starts with `dbg_macro`, `todo` and `unimplemented` (no violations today). **Proven build-neutral:** `Cargo.lock` byte-identical, `cargo tree -e features` identical for both platforms in `hakari.toml`, and every member's resolved dependency declarations identical except the seven `default-features = true` → `features = ["default"]` swaps where the workspace entry turns defaults off (`axum`, `rustix`).
- **A5. Argon2 off the runtime.** `auth::hash_password` is async and runs on the blocking pool; `auth::password_matches` checks a login there too, and spends a dummy verification when there's no hash, now also for a user without a password, whose answer used to come back faster.
- **A6. Workflow hygiene.** The CLA workflow's write permissions moved from the workflow to its one job, each with its reason; the two intended zizmor findings are marked inline with theirs. A new `workflows` job in `check.yml` runs zizmor (offline audits) on every push.
- **A7. Typechecks** for `@cha/player` and `@cha/pyrowave-webgpu` in `check.sh`; both passed already, but nothing kept them passing.
- **A8. Contributor scaffolding:** a pull-request template from CONTRIBUTING's own rules, bug and feature issue forms that ask for what a streaming bug can't be diagnosed without (GPU and driver, browser, network, the stats panel's report, `--doctor`), and the security form linked from "New issue". `SECURITY.md`'s supported-versions section now matches the releases.

---

## 3a. Optimisation pass: done

Measured before and after on this review's machine (Intel Xeon with AVX-512 and GFNI, 4 cores), each figure the median of repeated runs.

| What | Before | After | How |
|---|---|---|---|
| FEC encode, a 1440p WAN frame (35 shards + 6 parity) | 124 µs | 6.6 µs (**19×**) | GF(2⁸) multiply with SIMD nibble tables (SSSE3, AVX2, NEON) or GFNI's affine instruction, chosen at first use; tables built at compile time |
| FEC encode, a keyframe block (128 + 32 parity) | 2.35 ms | 0.13 ms (**18×**) | same |
| FEC recovery, 128 shards with 32 lost | 2.49 ms | 0.15 ms (**17×**) | same, and the pivot row no longer cloned per elimination step |
| The same on a CPU without GFNI | | AVX2 13×, SSSE3 9.6× | every x86-64 CPU since 2006 has SSSE3; ARM always has NEON |
| Browser worker, one PyroWave frame (1,275 datagrams) | 1.42 ms | 0.68 ms (**2.1×**) | payloads kept as views of their datagrams instead of copied; no per-packet arrays when joining |
| Browser FEC recovery (128 shards, 32 lost, in Bun) | 5.0 ms | 3.6 ms (1.4×) | one 64 KiB product table: one lookup per byte |
| Native client CPU at 732 Mbit/s | 64.5% of a core | 59.5% (**−8%**) | images built with `--profile dist` (fat LTO, one codegen unit) |
| The portal's web app over the wire, first visit | 775 KiB | 237 KiB (**−69%**) | Brotli 11 and gzip 9 copies written at build time and served by `ServeDir`; no compression per request |
| The portal's web app, later visits | revalidated per file | one `index.html` check | hashed `assets/` sent `immutable` for a year; everything else `no-cache` with `Last-Modified` (304s); `Vary: Accept-Encoding` |
| `@cha/player` typecheck | 1.9 s (TypeScript 6) | 0.28 s (**6.7×**) | TypeScript 7's native compiler (no Vue in that package) |

Also in this pass: Rust 1.93 → 1.99.1 everywhere (fourteen `chunks_exact` sites became `as_chunks`, whose fixed-size chunks drop bounds checks; the I420 converter's output is byte-identical and its speed unchanged, its loops were already vectorised), every semver-compatible crate update (66, among them tokio 1.54.1, hyper 1.12, aws-lc-rs 1.18.2), Bun 1.4.4, Vite 8.3.5, Vue 3.5.44, Vue Router 5.4, Pinia 4.1. The FEC kernels are tested against the reference arithmetic for every coefficient, length and alignment, the NEON one under QEMU for aarch64 and clippy-checked for `aarch64-apple-darwin`. The browser's FEC and intra (PyroWave) reassembly have tests now (`fec.test.ts`, the intra cases in `reassembly.test.ts`), which pass on the code from before and after.

Measured and left alone, with the numbers for whoever picks them up:

- **The native client's receive path is transport-bound.** At 732 Mbit/s, quinn and wtransport take 36% of the client's CPU and tokio's scheduling and wake-ups 28%; `cha-client-stream`'s own code 7%, its copies and allocations a few percent more. Draining queued datagrams in a batch per wake does nothing: the queue held 1.001 datagrams per wake, as the connection driver, on another thread, wakes the session once per datagram. The way to cut it is the connection's driver and the session on one thread (a current-thread runtime for the stream), a change to Cha Player's runtime to make and measure on a Mac.
- **The macOS player isn't built with `dist` yet.** `crates/cha-player/macos/bundle.sh` reads `target/release`; building it with `--profile dist` should save it the same 8% on the receive path. Untested here (no Mac).
- **Majors that would duplicate crates or need their own review:** `tower-http` 0.7 (strong ETags in `ServeDir`) waits for `reqwest` to leave 0.6, or the tree builds both; the RustCrypto 2026 wave (`ed25519-dalek` 3, `sha2` 0.11, `hmac` 0.13, `aes-gcm` 0.11) waits for `spake2`, still on `curve25519-dalek` 4 and `sha2` 0.10; `sqlx` 0.9 (no SQLite speed-ups in its changelog) wraps every runtime-built query in `AssertSqlSafe`, which deserves its own change; `wgpu` 30 and `egui` 0.37 (0.36 needed Rust 1.95, now there) for Cha Player need a Mac to build.
- **The browser still has its own FEC and reassembly**, in TypeScript, kept in step with `cha-proto` by tests. `cha-proto` compiled to WebAssembly with `simd128` (whose `i8x16.swizzle` is the nibble lookup) would give the worker the 17× kernel and one implementation of both: `cha-proto`'s own docs already plan it.
- **An AVX-512 GFNI kernel** (64 bytes per instruction) measured no faster than AVX2 GFNI on 1,180-byte shards (159 µs against 133 µs for 128 + 32), so there is none.

---

## 4. Phases B to D

Sizes are relative (S, M, L), as in PLAN.

### Phase B: security and correctness (next; S–M each)

- **B1. Sign-in hardening**, in the order `production-deployment.md` step 4 already gives, because each needs the one before:
  1. `CHA_TRUSTED_PROXIES`: read `X-Forwarded-For` only from them, so the audit log and any limit see the real client.
  2. Login throttling per account and per client address: exponential backoff after failures (not a lockout, which would let anyone lock an admin out), an audit event when it triggers, no hint whether the account exists. The share links' `RateLimit` (`shares.rs`) is the starting point; it needs a sliding window and a per-key backoff.
  3. Security headers from `cha-control`: a CSP for the SPA (scripts from self; `connect-src` self plus the nodes' WebRTC and WebTransport; `worker-src blob:` for the player's workers), `frame-ancestors 'none'`, `Referrer-Policy`, and HSTS when serving HTTPS itself.
  4. A sessions page (see and revoke your sessions; an admin, anyone's).
  5. **Decision:** a new ADR raising the password minimum. NIST SP 800-63B rev. 4 asks for at least 8 characters where a password is one factor among several and 15 where it is the only one. A homelab portal behind Tailscale can reasonably pick 8, keeping usernames at 3. Existing passwords keep working; the rule applies when one is set.
- **B2. Replace the archived CLA action.** Options: (a) fork it into the `ban-red` organisation and pin the fork, which changes nothing for contributors and buys time; (b) the hosted CLA Assistant app, which needs no workflow at all; (c) keep only the DCO check and collect the CLA another way. (a) now, then (b) or (c) on its own schedule.
- **B3. zizmor's online audits** (archived actions, actions with known vulnerabilities, impostor commits): drop `--offline` and pass `GH_TOKEN` once a first run is green.
- **B4. Fuzz the parsers that take untrusted bytes.** `cargo-fuzz` targets, each a few lines, for what a client on the LAN can send before pairing: `cha-gamestream`'s RTSP request parser, SDP, control-stream messages and input packets. Then `cha-proto`'s header and reassembly, the GameStream *client*'s XML, RTSP and SDP readers (what a host sends), `cha-wire`'s messages, and the streamer's PulseAudio protocol server (what the app container sends). Run them the way Selkies does, with ClusterFuzzLite: 5 minutes on pull requests that touch a parser, an hour weekly. Keep the corpus, and turn every crash into a unit test. Add `proptest` round-trips for `cha-proto` and `cha-wire` alongside.
- **B5. An SBOM for each image**, so an operator can answer "does my node run the vulnerable version?" from the registry. `sbom: true` on `docker/build-push-action` stores it in the image index, so the publish workflow's re-tagging of an unchanged image carries it along. Move `attest-build-provenance` to `actions/attest` v4, which it now only wraps (`attest-sbom` is deprecated). Run [OpenSSF Scorecard](https://github.com/ossf/scorecard-action) weekly alongside, as Coder and Vector do, to track the rest.
- **B6. Watch the rest of the supply chain:** trial GitHub's workflow lockfiles (public preview since 2026-10-14, Free plans included) on `publish.yml` first, where a moved action would do the most harm.

### Phase C: maintainability (M–L)

- **C1. One source for the API's types.** Derive TypeScript from the Rust request and response structs (`ts-rs`, or `utoipa` to an OpenAPI document and `openapi-typescript`), generate into `web/apps/portal/src/generated/`, and fail `check.sh` when the output is stale, as it already does for the player themes. Start with the types `api.ts` declares by hand; `api.ts` keeps the calls. Do the same for the player ⇄ streamer control messages, which CONTRIBUTING already asks to change on both ends at once.
- **C2. Split the largest modules, moving code only.** `cha-node/src/environments.rs` becomes a directory along the seams it already has: `mod.rs` (the types, `DockerRuntime`, its event loop and the `Runtime` impl), `shared.rs` (shared-directory and overlay watching, mountinfo), `storage.rs` (homes, volumes, copies, deletion), `host.rs` (host options, port planning), `containers.rs` (the app, streamer and gateway configurations), `devices.rs` (render nodes, hidraw, KVM), `gamestream.rs` (ports and access), with its 4k lines of tests split the same way. Rust lets each submodule add an `impl DockerRuntime` block with the same field access, so the diff is a move. Do it in its own pull request, when no branch is working in the file. `SessionView.vue` gives up its stream, input and toolbar state to composables the same way.
- **C3. Lint the web code.** ESLint 10 (flat config) with `eslint-plugin-vue` and `typescript-eslint`, because it is the only linter that reads Vue templates. Start with correctness rules only, since the code already has a style, and put oxlint in front later if ESLint is slow. Give the extension-less imports in `vite.config.ts` and `dev-mcp/` their extensions, which Vite 8's build now warns about ahead of its native config loader. **TypeScript:** stay on 6.0 for the Vue packages until vue-tsc supports 7.1 (its language core's alpha appeared 2026-10-23; revisit in December). `@cha/pyrowave-webgpu` can keep 7 for its own check, but the portal's vue-tsc reads its sources too, under 6.0, so its code has to suit both until the switch.
- **C4. A faster, clearer CI.** Split `check.yml` into parallel jobs (format and lints; Rust tests; web; Python) so a failure names its area and a run takes the longest job, not the sum. Use `cargo-nextest` for the Rust tests (a process per test, so one hanging test can't hold the run; retries reported as flaky, not passed). Take the spikes out of the default build: Phase 2 is closed, and S2 is kept as an A/B baseline, which `cargo build -p s2-streamer` still gives without GStreamer in every CI run. Keeping the spikes as workspace members with `default-members` set to the product crates is the smallest step.
- **C5. Two more workspace lints**: `clippy::undocumented_unsafe_blocks` (53 sites, nearly all a comment to move next to its block) and `clippy::allow_attributes_without_reason` (24 sites outside bindgen output).
- **C6. A current toolchain.** *Done* (§3a): `rust-toolchain.toml` pins 1.99.1 for contributors, CI and the Dockerfiles' builders, and `rust-version` is 1.99.
- **C7. Dependency hygiene:** `cargo shear` in `check.sh` for dependencies no code uses, and a `[bans.build]` allowlist in `deny.toml` of the crates allowed a build script, seeded from today's graph, as uv does.
- **C8. End-to-end tests in a browser.** Playwright against `bun run dev`'s portal with a fake node (the test rigs in `cha-control/tests` already speak the node channel): sign in, launch, the share-link guest page, the session toolbar. That covers the portal flows `api.rs`'s tests can't see, such as a route the SPA calls that no longer exists.

### Phase D: operations and releases (M–L)

- **D1. Metrics, as PLAN §7 describes.** A Prometheus `/metrics` on the portal and the agent, the way Headscale and Coder do it: off by default, on its own listener (`CHA_METRICS_ADDR`, unset unless the owner sets it).
  - The portal: launches and their failures by reason, sessions, nodes online, API latency.
  - The agent: environments, image pulls, Docker errors, and the GPU use it already reads.
  - The streamer: the numbers the stats panel already computes (encode time, frame pacing, loss), as summaries per session.

  `metrics` with `metrics-exporter-prometheus` is the light choice. Ship a Grafana dashboard under `deploy/`. OpenTelemetry over OTLP (the standard `OTEL_EXPORTER_OTLP_ENDPOINT`), bridged from the `tracing` spans the code already has, can come later as an option, once its tracing SDK leaves beta.
- **D2. Releases with less hand work.**
  - A `CHANGELOG.md` generated with git-cliff from the Conventional Commits the project already writes, edited before tagging and used as the release notes. release-plz's `git_only` mode can drive it from tags, without crates.io.
  - A `scripts/set-version` that sets the workspace version, the portal's `package.json` and the version examples in `SETUP.md` and `deploy/`, leaving the upgrade notes to a person.
  - A stated cadence (say, a release each month while there are changes), since "is it alive?" is a question every comparable tracker gets.
- **D3. A documentation site** built from the Markdown already in `docs/`, `deploy/` and `SETUP.md`: Starlight, which fits the Bun and Tailwind stack, with link checking, published from `main` to GitHub Pages. The files stay where they are and stay readable on GitHub.
- **D4. Product follow-ups from §2**, for PLAN's owner to place in its phases:
  1. Negotiate PyroWave bitstream v1 through its new `PWV1` header on every path (node encoder, browser WebGPU decoder, Cha Player, the Vibepollo contract), and check that the WGSL port decodes v1 bit-exactly. This replaces "pin the commit everywhere" in PLAN §10.
  2. Multi-monitor: several outputs from one compositor, each a stream, which no Cha plan covers yet and which comparable hosts now ship.
  3. Files and microphone, already in the backlog, are the third most requested feature in comparable trackers: worth moving up.
  4. Several NVIDIA GPUs in one node, each its own placement device.
  5. Podman and rootless Docker on nodes, at least documented as unsupported with the reason (the agent's Docker socket, CDI, uhid).

---

## 5. Decisions for the owner

| Decision | Options | Recommendation |
|---|---|---|
| Password minimum (B1.5) | keep 3 (ADR 0006); 8; 15 | 8, in a new ADR superseding that part of 0006 |
| CLA bot (B2) | fork the archived action; hosted CLA Assistant; DCO only | fork now, decide the rest later |
| API types (C1) | `ts-rs`; `utoipa` and OpenAPI | `ts-rs` first: least code, no runtime cost; OpenAPI if the API is to be public |
| Spikes in CI (C4) | keep building them; `default-members`; separate workspace | `default-members` |
| Web linter (C3) | ESLint 10 + `eslint-plugin-vue`; oxlint; Biome | ESLint: the only one that reads Vue templates |
| Toolchain (C6) | stay on 1.93; pin a current release in `rust-toolchain.toml` | pin 1.99, keep `rust-version` as the minimum |
| Dependency bot (A3) | Dependabot lockfile-only (now); Renovate as a scheduled workflow running `cargo hakari generate` (nextest, Oxide) | Dependabot until manifest bumps by hand become a chore |
| Metrics exposure (D1) | separate listener; token; off | off by default, separate listener when on |

---

## 6. Considered and not recommended now

- **`cargo-vet`.** Audit records per crate version pay off for projects with several reviewers and a security team; of the 21 surveyed, only Tauri keeps them. For one maintainer, `cargo-deny`, Dependabot's cooldown and the weekly audit cover most of the risk for a fraction of the work.
- **Renovate instead of Dependabot, for now.** Renovate can run `cargo hakari generate` after an update only when self-hosted (nextest and Oxide run it as a scheduled workflow). With Cargo on `lockfile-only`, Dependabot doesn't need that; revisit if bumping majors by hand becomes a chore.
- **cosign on top of GitHub's attestations.** The attestations are already signed through Sigstore; add cosign only if an operator asks for it.
- **A GOVERNANCE file.** None of the 21 projects surveyed keeps one; CONTRIBUTING's "Decisions that stand" and the ADRs already say how decisions are made.
- **Postgres and multi-portal high availability.** Already in PLAN's backlog for team scale; SQLite fits the homelab target.
