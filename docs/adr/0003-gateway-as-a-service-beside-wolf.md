# 0003: `cha-gateway` runs as its own service beside Wolf

- **Status:** accepted (2026-10-03)
- **Context:** the Phase 1 media path is Wolf → GameStream → `cha-gateway` → WebRTC → browser (proved in S3). PLAN §5.1 requires that the node agent is **never in the media path**, so that restarting or upgrading it doesn't drop sessions.
- **Decision:** `cha-gateway` runs as a separate long-lived service on the node, next to Wolf, as in the S3 compose file. The `cha-node` agent drives it over a local unix socket: start and stop an environment's stream, relay SDP offers and answers, report stats. The gateway owns its Moonlight identities, so it pairs once.
- **Consequences:** agent restarts and upgrades leave streams running. There is one more process to deploy and supervise; compose handles that.
