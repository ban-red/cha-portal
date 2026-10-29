<!--
Thanks for contributing. CONTRIBUTING.md has the details; for anything larger
than a fix, open an issue first. Security problems go to SECURITY.md, not here.
-->

## What changes

<!-- One change per pull request. Say what happens now that didn't before. -->

## Why

<!-- The issue, ADR or plan this follows (docs/PLAN.md, docs/adr/, docs/plans/). -->

## How it was tested

<!--
What you ran, and on what: for the streamer, a node or a browser, name the
GPU and driver, the browser and its version, the OS and the network (wired
LAN, Wi-Fi, tailnet, WAN). Paste a stats panel report or `cha-node --doctor`
output where it helps.
-->

## Checklist

- [ ] `./scripts/check.sh` passes.
- [ ] Docs follow the change: the relevant README, the status lines in `docs/PLAN.md`, and `deploy/README.md` for anything an operator runs.
- [ ] A wire change (`cha-wire`, `cha-proto`, player ⇄ streamer messages) updates both ends and their tests, and keeps older nodes and portals working.
- [ ] A database change is a new numbered migration; no shipped migration is edited.
- [ ] A new dependency has a licence `deny.toml` allows, and the description says why it's worth it.
- [ ] Every commit is signed off (`git commit -s`), and I've signed the [CLA](../CLA.md).
