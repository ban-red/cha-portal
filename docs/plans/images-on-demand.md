# Images pulled on demand: the contract

For [ADR 0017](../adr/0017-images-pulled-on-demand.md). The node, the portal and the SPA are built against this; change it here first.

## Catalog entry (`images/catalog.json`, and later external catalogs)

- The document gains `"version": 1` at the top level.
- An entry keeps `image` and gains an optional `localImage`:
  - `image`: the reference to run. For the built-in apps it becomes `ghcr.io/ban-red/cha-env-<name>:{version}` (the publish workflow's names: check `.github/workflows/publish.yml`). `{version}` is a placeholder the **node** fills with its agent release (`CARGO_PKG_VERSION` of cha-node, e.g. `0.1.0`).
  - `localImage`: optional, a dev build name (`cha/env-chrome:dev`) preferred when the node has it.
  - Entries without `localImage` and with a bare `image` behave as today.

## Launch spec (`cha-wire`, `EnvironmentSpec`)

- New field `image_candidates: Vec<String>` (serde `imageCandidates`, default empty, skipped when empty), the ordered choices: `localImage` if any, then `image` (placeholder unexpanded; the node expands `{version}`). The existing `image` field stays, set to the first candidate, for older nodes.
- `EnvironmentProgress` gains optional `done: Option<u64>`, `total: Option<u64>`, `unit: Option<String>` (`"bytes"`), all skipped when absent, beside `detail`.
- The node's inventory (whatever message carries its devices) gains `images: Vec<String>`: the `repo:tag` names the engine has, at most 500, refreshed when the inventory is sent and after each pull. Add `placement: "auto" | "manual"` (default auto) from `CHA_PLACEMENT`.

## Node (`cha-node`)

- Resolve: for each candidate in order, expand `{version}`, apply the `CHA_IMAGE_REGISTRY`/`CHA_IMAGE_TAG` mapping to bare names as today; the first one `image_exists` → use it. Else the first that `names_registry` → pull it. Else fail with every candidate tried in the message.
- The streamer image: candidates are `CHA_STREAMER_IMAGE` (default `cha/streamer:dev`), then `ghcr.io/ban-red/cha-streamer:{version}` unless `CHA_STREAMER_IMAGE` already names a registry.
- `Docker::pull` streams the engine's JSON lines as they arrive (not after) and calls a progress callback. Per layer `id`, keep the latest `progressDetail.current` and `total` from "Downloading" lines (and count "Download complete" or "Already exists" layers as done). Report the sum of `done` and the sum of known `total`, plus a short `detail` such as "Downloading Chrome (412 of 890 MB)". Throttle to 2 per second; always send a final one. Errors in the stream fail the pull, as now.
- `info` logs: which candidate was used or pulled, and the pull time and size.
- Tests: the candidate resolution (exists, pull, bare never pulled, mapping, `{version}`); progress summing over a recorded engine stream fixture; throttling.

## Portal (`cha-control`)

- The catalog loader reads `version` (1, or absent meaning 1), `image` and `localImage`. The launch spec carries `imageCandidates`.
- `EnvironmentProgress` with `done`/`total` is kept on the environment (in memory is fine) and shown in `EnvironmentView` as `progress: { done, total, unit } | null` while `starting`.
- Placement: a node whose inventory `images` contains any candidate (after `{version}` expansion with that node's agent version, if the inventory reports it, else a prefix match on the name before `:`) gets a bonus that beats an otherwise equal node. `placement: manual` nodes are never the `auto` choice, but stay in the options, allowed.
- Tests: catalog parsing with and without `localImage` and `version`; the spec's candidates; progress in the view; placement bonus; manual nodes never auto.

## SPA (`web/apps/portal`)

- The dashboard's launching state and the session view's "Starting…" show a progress bar and text from `progress` when present (`412 of 890 MB`), else the `detail` text as today. Theme roles only (the colour guard).

## Built-in catalog

- `images/catalog.json` gets `"version": 1`, and each entry's `localImage` (today's `image`) and `image` (the published reference with `{version}`).
