# Contributing to Cha Portal

Thanks for your interest. Cha Portal is pre-release and moves quickly, so for anything larger than a fix, open an issue first and describe what you want to change. That saves you building something that clashes with work in progress or with a decision already made.

Security problems don't go in issues: see [SECURITY.md](SECURITY.md).

## Getting set up

You need Rust ≥ 1.93 and [Bun](https://bun.sh) ≥ 1.4. The JS tooling is Bun only: use `bun install` and `bun run`, never npm or pnpm.

```bash
bun install
```

```bash
./scripts/check.sh
```

`check.sh` is what CI would run, and a pull request should pass it: `cargo fmt --check`, clippy with `-D warnings`, the workspace tests, the Python tests for the Steam image's scripts and the node's host installer, the portal's colour guard and theme tests, its typecheck and its build.

`bun run dev` starts a portal with hot reload and a passwordless dev login; the [README](README.md#development) explains it. Faster loops while you work:

```bash
cargo test -p cha-control
```

```bash
bun run --cwd web/packages/player test
```

```bash
bun run --cwd web/apps/portal typecheck
```

`cha-streamer` and `cha-testpattern` are Linux only: build and test them on a Linux machine with a GPU, in the streamer's dev container ([`deploy/streamer/compose.dev.yaml`](deploy/streamer/compose.dev.yaml)). To run a whole node against your dev portal, see [SETUP.md](SETUP.md).

## Decisions that stand

Read [`docs/PLAN.md`](docs/PLAN.md) and the [ADRs](docs/adr/README.md) before proposing an architectural change. These are settled, and pull requests that undo them won't be merged:

- **Our own media engine** ([ADR 0004](docs/adr/0004-own-engine-no-wolf.md)). No Wolf, Games-on-Whales, GStreamer, FFmpeg, PulseAudio, PipeWire, inputtino or fake-udev in the node's media path. We borrow only foundations too big to rebuild (Smithay, str0m, quinn, libopus, libpyrowave).
- **Self-hosted only.** No hosted relays, DNS or other services run by the project. Remote access is Tailscale, a port-forward or the owner's own TURN server.
- **No code from Magic Mirror / mm-server**, which is under the BUSL.

To change a decision, propose a new ADR that supersedes the old one. ADRs are never rewritten.

## Conventions

- **Rust:** edition 2024, `cargo fmt`, clippy clean with `-D warnings`. Match the surrounding module's style and comment density.
- **Web:** Vue 3 `<script setup lang="ts">`, strict TypeScript, Tailwind utility classes. Components use only the theme's colour roles, never palette classes or hex values ([`web/apps/portal/README.md`](web/apps/portal/README.md)). API calls go through `web/apps/portal/src/api.ts`.
- **Wire changes** (`cha-wire`, `cha-proto`, or the player ⇄ streamer control messages) change both ends: update the Rust side, the TypeScript side and their tests in the same pull request. Keep older nodes and portals working where the existing code does: the order in which they are updated is documented in [`deploy/README.md`](deploy/README.md).
- **Database changes:** add a new numbered migration in `crates/cha-control/migrations/`. Never edit one that has shipped.
- **Docs:** when behaviour changes, update the relevant README, the status lines in `docs/PLAN.md`, and `deploy/README.md` for anything an operator runs. Write plain, concrete prose: say what happens, not what's "supported".
- **Benchmarks:** label results with the real browser, its version and the decoded size. Commit curated results under `docs/benchmarks/`; raw spike output stays in the gitignored `spikes/**/results/`.

## Commits and pull requests

- Subject line: `Area: what changed, in plain words`, e.g. `Controllers: WebHID showed as unavailable everywhere`. An optional body explains why.
- Keep a pull request to one change, with tests where the code has them.
- Say how you tested it, and on what hardware if the streamer, a node or a browser is involved.

## Dependencies and licence

Cha Portal is under the [AGPL-3.0-or-later](LICENSE). New dependencies must have a compatible licence (MIT, Apache-2.0, BSD, ISC, MPL-2.0, LGPL, GPL-3.0 or AGPL-3.0 are fine), and should be lean: say why a new one is worth it.

By contributing, you agree that your contribution is licensed under the AGPL-3.0-or-later, the same as the project.
