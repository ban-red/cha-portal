# Architecture decision records

One file per decision, numbered, never rewritten: a later ADR supersedes an earlier one. Fixes to wording that leave the decision as it was are fine.

| # | Decision | Status |
|---|---|---|
| [0001](0001-node-channel-json-over-websocket.md) | Node ⇄ portal channel: JSON messages over one WebSocket for the MVP | Accepted |
| [0002](0002-local-accounts-first-passkeys-next.md) | Auth: local accounts with Argon2id passwords first, passkeys next | Accepted |
| [0003](0003-gateway-as-a-service-beside-wolf.md) | `cha-gateway` runs as its own service beside Wolf | Superseded by 0004 |
| [0004](0004-own-engine-no-wolf.md) | Our own streaming engine, without Wolf; the build-vs-borrow rule | Accepted |
| [0005](0005-theme-token-contract.md) | Theme token contract: roles, runtime variables and a contrast test | Accepted |
| [0006](0006-claim-on-first-visit.md) | First admin by claiming a fresh portal; 3-character minimums | Accepted; supersedes parts of 0002 |
| [0007](0007-claim-nodes-found-on-the-lan.md) | Claim nodes found on the LAN with a pairing code (mDNS + SPAKE2) | Accepted |
| [0008](0008-moonlight-hosts-adopted-by-a-node.md) | Moonlight hosts found and adopted by a node, streamed through cha-gateway | Accepted |
| [0009](0009-gamestream-host-module.md) | A GameStream host module (cha-gamestream), ported from Moonshine, behind traits and a cargo feature | Accepted |
| [0010](0010-native-client-macos-first.md) | A native client, macOS first: Rust core, pluggable transports, GameStream first | Accepted |
| [0011](0011-own-gamestream-client.md) | Our own GameStream client in cha-gamestream, replacing moonlight-common-rust | Accepted; changes the library choice in 0008 and 0010 |
| [0012](0012-cla-for-app-store-builds.md) | A contributor licence agreement, so Cha Player can ship in app stores; no public App Store exception | Accepted; settles the open question in 0010 |
| [0013](0013-native-player-on-cha-stream.md) | Cha Player on `cha-stream/1`: per-install device tokens (a `cha://` link first, a device code too) and WebTransport to the existing endpoint | Accepted; shapes C2 of 0010 |
