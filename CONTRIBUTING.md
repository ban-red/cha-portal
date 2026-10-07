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
- **Ported code keeps its notice.** Code translated or adapted from another project needs a licence compatible with the AGPL, a header naming the source, its licence and what changed, the upstream licence text beside it when the licence asks for it, and a row in [`docs/PROVENANCE.md`](docs/PROVENANCE.md).

To change a decision, propose a new ADR that supersedes the old one. ADRs are never rewritten.

## Conventions

- **Rust:** edition 2024, `cargo fmt`, clippy clean with `-D warnings`. Match the surrounding module's style and comment density.
- **Web:** Vue 3 `<script setup lang="ts">`, strict TypeScript, Tailwind utility classes. Components use only the theme's colour roles, never palette classes or hex values ([`web/apps/portal/README.md`](web/apps/portal/README.md)). API calls go through `web/apps/portal/src/api.ts`.
- **Wire changes** (`cha-wire`, `cha-proto`, or the player ⇄ streamer control messages) change both ends: update the Rust side, the TypeScript side and their tests in the same pull request. Keep older nodes and portals working where the existing code does: the order in which they are updated is documented in [`deploy/README.md`](deploy/README.md).
- **Database changes:** add a new numbered migration in `crates/cha-control/migrations/`. Never edit one that has shipped.
- **Docs:** when behaviour changes, update the relevant README, the status lines in `docs/PLAN.md`, and `deploy/README.md` for anything an operator runs. Write plain, concrete prose: say what happens, not what's "supported".
- **Benchmarks:** label results with the real browser, its version and the decoded size. Commit curated results under `docs/benchmarks/`; raw spike output stays in the gitignored `spikes/**/results/`.

## Commits and pull requests

- [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): what changed`, lowercase and imperative, at most 72 characters, e.g. `fix(controllers): show WebHID as available where it is`. Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `build`, `chore`. Scopes follow the area: `portal`, `node`, `streamer`, `player`, `proto`, `wire`, `controllers`, `steam`, `images`, `deploy`, `plans`, `spikes`. An optional body, wrapped at 72, explains why.
- Sign the [CLA](CLA.md) once, and sign off every commit (below).
- Keep a pull request to one change, with tests where the code has them.
- Say how you tested it, and on what hardware if the streamer, a node or a browser is involved.

## Contributor License Agreement

Before your first pull request is merged, you sign the [Contributor License Agreement](CLA.md), once, by posting a comment the bot asks for. You keep the copyright in your work. The agreement lets the maintainer also distribute it in builds for stores whose terms the AGPL doesn't allow, such as Apple's App Store, and commits the maintainer to keeping every version that includes it available under the AGPL with its source. Read it before you sign; it's short.

## Sign your commits off

Every commit must carry a `Signed-off-by` line with your name and email, which certifies the [Developer Certificate of Origin](https://developercertificate.org/) (DCO). By adding it, you state that:

1. you wrote the change and have the right to submit it under the project's licence; or
2. it is based on earlier work under a compatible open-source licence, and you have the right to submit it with your changes; or
3. it was given to you by someone who certified (1) or (2), and you haven't changed it;

and that you understand the contribution and your sign-off are public and kept for good. Read the full text at the link before you sign off for the first time.

`git commit -s` adds the line for you:

```text
Signed-off-by: Jane Doe <jane@example.com>
```

Use your real name, or the name you are known by, and an email address that reaches you. Don't sign off code you copied from somewhere whose licence you don't know or that isn't compatible with the AGPL (Magic Mirror / mm-server, for example, is under the BUSL). If you forgot, `git commit --amend -s` fixes the last commit and `git rebase --signoff main` fixes a branch. A check on each pull request looks for the line on every commit.

## Releasing

Maintainers only. A release is a `v*` tag on `main`, and every image is built from it by [`.github/workflows/publish.yml`](.github/workflows/publish.yml): the portal, the node agent, the streamer, the environments' base and the six environments, at `ghcr.io/ban-red/<name>`. All of them share the release's version, since the portal, agent, streamer and environments change together.

Versions are [semver](https://semver.org) without the `v` in image tags: tag `v0.2.0` publishes `:0.2.0`, and every image also gets `:sha-<commit>`. There is no `latest`, so nodes stay on the version they were given.

1. **Check `main` is green.** The `check` workflow passed on the commit you'll tag, and it ran on the node you test with.
2. **Try the images first** (optional, worth it for a large change): **Actions → publish → Run workflow** on `main` builds and pushes `:sha-<commit>` without a release. A node runs those with `CHA_VERSION=sha-<commit>`.
3. **Update the docs that name a version or the release state:** the examples in `SETUP.md` (`v0.1.0`, `--version 0.1.0`).
4. **Tag and push:**

   ```bash
   git tag -a v0.2.0 -m "v0.2.0"
   ```

   ```bash
   git push origin v0.2.0
   ```

5. **Watch the build** (about 15 minutes; Steam is the longest):

   ```bash
   gh run watch
   ```

6. **Publish the release notes** from the commits since the last tag:

   ```bash
   gh release create v0.2.0 --verify-tag --generate-notes
   ```

7. **Check an image** is pullable without signing in and carries its provenance:

   ```bash
   gh attestation verify oci://ghcr.io/ban-red/cha-streamer:0.2.0 --owner ban-red
   ```

A new image name (a new environment) starts out private on GitHub's registry: set it to Public in its package settings once, or nodes can't pull it. A bad release isn't deleted or retagged, since nodes may already run it: fix forward with the next patch version.

## Dependencies and licence

Cha Portal is under the [AGPL-3.0-or-later](LICENSE). New dependencies must have a compatible licence (MIT, Apache-2.0, BSD, ISC, MPL-2.0, LGPL, GPL-3.0 or AGPL-3.0 are fine), and should be lean: say why a new one is worth it.

By contributing, you agree that your contribution is licensed under the AGPL-3.0-or-later, the same as the project. You keep the copyright in your contribution; the project's notice is "Alex Red and the Cha Portal contributors", and the git history records who wrote what.
