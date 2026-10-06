# Architecture decision records

One file per decision, numbered, never rewritten: a later ADR supersedes an earlier one.

| # | Decision | Status |
|---|---|---|
| [0001](0001-node-channel-json-over-websocket.md) | Node ⇄ portal channel: JSON messages over one WebSocket for the MVP | Accepted |
| [0002](0002-local-accounts-first-passkeys-next.md) | Auth: local accounts with Argon2id passwords first, passkeys next | Accepted |
| [0003](0003-gateway-as-a-service-beside-wolf.md) | `cha-gateway` runs as its own service beside Wolf | Superseded by 0004 |
| [0004](0004-own-engine-no-wolf.md) | Our own streaming engine, without Wolf; the build-vs-borrow rule | Accepted |
| [0005](0005-theme-token-contract.md) | Theme token contract: roles, runtime variables and a contrast test | Accepted |
| [0006](0006-claim-on-first-visit.md) | First admin by claiming a fresh portal; 3-character minimums | Accepted; supersedes parts of 0002 |
