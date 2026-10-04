# 0001: Node ⇄ portal channel: JSON messages over one WebSocket for the MVP

- **Status:** accepted (2026-10-03)
- **Context:** PLAN §5.1 specifies one outbound WSS from each node, multiplexed with yamux and carrying protobuf RPC both ways (the Coder/dRPC pattern). Phase 1 needs:
  - enrollment;
  - heartbeats;
  - inventory;
  - desired-state pushes;
  - SDP relay for session brokering.

  All of them are small messages; none needs a byte stream.
- **Decision:** for the MVP, one WebSocket carrying JSON messages:
  - each message is `{id?, type, …}`;
  - requests carry an `id`, and the reply echoes it;
  - pushes have no `id`.

  The message types live in a shared crate used by `cha-control` and `cha-node`. The node authenticates its first message with its Ed25519 key, the same as the plan.

  As built in P1.2 (`crates/cha-wire`): the portal opens with a random challenge, and the node's hello signs `cha-node-hello/v1:{nonce}:{node_id}`, so a captured hello can't be replayed. When the portal hangs up on purpose, the WebSocket close code says why: 4001 not enrolled or removed (the agent stops), 4002 replaced by a newer connection, 4003 bad signature.
- **Consequences:**
  - Easy to debug (readable frames) and no code generation.
  - yamux + protobuf comes back when we need concurrent streams over the channel: log tails, file transfer, port forwarding. The message types are already isolated, so switching is local to the transport.
