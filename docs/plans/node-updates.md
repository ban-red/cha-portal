# Node agents updated from the portal: the contract

For [ADR 0018](../adr/0018-node-agents-updated-from-the-portal.md). The node, the portal and the SPA are built against this; change it here first.

## Wire (`cha-wire`)

- `Inventory` gains `update: Option<AgentUpdatability>` (serde `update`, skipped when absent; absent from older agents, which the portal treats as not updatable):
  - `AgentUpdatability { image: String, updatable: bool, reason: Option<String> }` (camelCase). `image` is the image the agent container runs (`ghcr.io/ban-red/cha-node:0.2.0`, or `cha-node:dev`). `updatable` is false, with a `reason`, when: the image has no registry ("a local build: update it by hand"), the agent can't find its own container ("not running in a container the agent can see"), or the Docker socket is missing.
- `ToNode::UpdateAgent { version: String }`: the release to move to (`0.2.1`). The agent refuses anything that isn't `MAJOR.MINOR.PATCH` (digits only).
- `ToPortal::AgentUpdate { state: AgentUpdateState, detail: Option<String>, done: Option<u64>, total: Option<u64> }` (camelCase; `state` lowercase-kebab): `pulling` (with `done`/`total` bytes as image pulls report them, and `detail` such as "Downloading the agent (120 of 262 MB)"), `swapping` ("Restarting the agent"), `failed` (`detail` says why; the old agent keeps running), `rolled-back` (the new agent didn't connect; the previous one is running again). Success needs no message: the node reconnects with the new `agentVersion`.
- Serde round-trip tests; an inventory without `update` still parses.

## Node (`cha-node`)

- **Own container:** find the agent's container id from `/proc/self/mountinfo` (the `/var/lib/docker/containers/<64 hex>/` paths of its `hostname`/`resolv.conf` mounts; `network_mode: host` means `hostname` doesn't give it), then `GET /containers/<id>/json`. Report `update` in the inventory from that.
- **On `UpdateAgent`:** validate; refuse with `failed` if not updatable, already at that version, or an update is running. New image = the current image's repository with the tag `version`. Pull it with progress (reuse `Docker::pull_with` and its tracker) as `pulling`; also pull `ghcr.io/ban-red/cha-streamer:<version>` (the streamer candidate the new agent will use; same registry as the agent image) as part of `pulling`. Then send `swapping` and start the helper container:
  - name `cha-node-update`, image = the new image, `Cmd`/entrypoint `cha-node --replace-agent <container id> --image <new image>` plus `--env-dir <compose working dir>` when the container has the label `com.docker.compose.project.working_dir`; binds `/var/run/docker.sock` and, when known, the working dir at the same path; `AutoRemove: true`; `NetworkMode: host`; no other privileges.
- **`--replace-agent <id> --image <image> [--env-dir <dir>]` (the helper):**
  1. inspect `<id>`; keep its `Name`, `Config`, `HostConfig`, `NetworkingConfig` (endpoints for its networks), labels;
  2. rename it `<name>-previous`, stop it (30 s);
  3. create a container named `<name>` from the same config with `Image = <image>` (and `Config.Image`), start it;
  4. wait up to 90 s for its log to contain the agent's connected line (`connected node_id=`) — read with the logs API, following;
  5. success: remove `<name>-previous`; if `--env-dir`, update `<dir>/.env` in place: a `CHA_VERSION=` line gets the new version; `CHA_NODE_IMAGE=`/`CHA_STREAMER_IMAGE=` lines whose image has the same repository get the new tag; `CHA_IMAGE_TAG=` gets the version; nothing else changes, file mode kept. Log what changed.
  6. failure (create/start error or no connection): stop and remove the new container, rename `<name>-previous` back to `<name>`, start it. Log why.
  - The helper's own logs are the record (the new or restored agent reports to the portal). Pure functions with unit tests: the image retagging, `.env` rewriting, the mountinfo container-id parse, the version check.
- `--doctor`: an "Updates" info line: "can be updated from the portal" or the reason not.

## Portal (`cha-control`)

- `NodeView` gains `updateTo: Option<String>` (the portal's version, `env!("CARGO_PKG_VERSION")`, when the node's `agentVersion` is older by semver and `inventory.update.updatable`), `updateBlocked: Option<String>` (the reason when older but not updatable, or "its agent predates updates from the portal" when `update` is absent), and `updateProgress: Option<{state, detail, done, total}>` (in memory, from `AgentUpdate`; cleared when the node reconnects with the new version; `failed`/`rolled-back` kept until the next attempt).
- `POST /api/nodes/{id}/update` (admin): 409 `not_updatable` / `up_to_date` / `offline` / `in_progress`; else sends `UpdateAgent { version: <portal version> }`, sets progress `pulling` with no bytes, 202. Audit `node.update_requested` with the versions; `node.updated` when it reconnects at the new version; `node.update_failed` on `failed`/`rolled-back` with the detail.
- Tests: the semver comparison, the route's refusals, progress kept and cleared.

## SPA (`web/apps/portal`)

- Nodes page, per node (admins): when `updateTo`, a line "Update available: v<agent> → v<updateTo>" with an **Update** button; when `updateBlocked`, the reason in `text-ink-3` (no button). While `updateProgress`: the state in words ("Downloading the new agent (120 of 262 MB)" with the progress bar used for launches, "Restarting the agent…"), then "Updated to v…" briefly when the node returns with the new version; `failed`/`rolled-back` in `text-warn` with the detail and a **Try again** button. Poll the nodes list as the page already does (faster while an update runs). Theme roles only.
