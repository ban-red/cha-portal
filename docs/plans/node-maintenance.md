# Node maintenance actions

Status: **proposal** (2026-10-09). Nothing here is built. If it is accepted, it becomes an ADR (the next number, 0024) and this file becomes its contract, as `wan-sharing.md` is for ADR 0022.

## Why

On 2026-10-09 the NFS share behind `/mnt/games` on the Proxmox host went stale twice in a few hours. The node agent noticed (`shared_status`, the dashboard warning), but the fix is a host operation: lazily unmount the share, let the automount mount it again, restart the LXC container that binds it. [`deploy/proxmox/repair-mounts.sh`](../../deploy/proxmox/repair-mounts.sh) does that, and today someone has to SSH to the Proxmox host to run it.

The question was whether the portal can run scripts on a node. The answer has two halves:

- **On the machine the agent runs on**, it can already run anything: it holds the Docker socket, which is root-equivalent ([SECURITY.md](../../SECURITY.md)). Nothing in the wire protocol exposes that, on purpose.
- **Above that machine it can't reach.** For a node in an LXC container, the stale mount lives on the Proxmox host one level up. The agent has no way to run `pct` or `umount` there, and it shouldn't try to climb out of its container.

So the plan is not "run arbitrary scripts". It is **named maintenance actions that the node's owner installs, that the portal can only trigger by name**, with a narrow, owner-configured channel for the ones that have to run on a machine above the agent.

## Goals and non-goals

Goals:

1. An admin sees, on the node and on the dashboard warning, a **Repair** button for a stale share, and the node is fixed without anyone opening a terminal.
2. The same mechanism carries other owner-defined chores later (restart Docker, clear an image cache, rescan a share), without a protocol change per chore.
3. A compromised portal, or an admin session someone stole, can do no more than the owner already allowed: fixed actions, validated arguments, audited.

Non-goals:

- No script text, shell string or file ever travels from the portal to a node. There is no "run this" box.
- No new privilege for the agent. Actions that need more than the agent has run through a separate, owner-installed host helper.
- No automatic remediation in the first versions (see "Later").
- No generic remote shell, no interactive sessions, no file transfer.

## Concepts

- **Action:** a named operation the node's owner has installed: an id (`repair-shared-mount`), a title, what it does in a sentence, whether it stops environments, whether it restarts the node's container, and which argument it takes (if any).
- **Executor:** where an action runs.
  - `agent`: in a throwaway helper container the agent starts from its own image, with only the mounts the action's manifest lists. For chores that concern the node's own machine.
  - `host`: through the **host helper**, a channel the owner sets up from the agent to the machine above it (the Proxmox host, or the bare-metal host the agent's container runs on). Needed for repair-mounts.
- **Manifest:** a small file per action, written by the owner, read by the agent. It is the only place an action is defined.

## Where actions are defined

A directory the owner controls, mounted read-only into the agent: `deploy/node/maintenance/` in the repo for the shipped ones, `CHA_MAINTENANCE_DIR` (default `/etc/cha-node/maintenance`) at runtime. One file per action:

```toml
# repair-shared-mount.toml
id = "repair-shared-mount"
title = "Remount the shared library"
description = "Remounts the NAS share on the host and restarts this node's container."
executor = "host"                  # or "agent"
command = "repair-mounts"          # host: the name the helper looks up, never a path
stops_environments = true          # the portal asks the admin to confirm; refused while any run unless confirmed
restarts_node = true               # the agent's connection drops during it (see "Reconnects")
timeout_s = 120
[arg]                              # optional: one argument, validated by the agent
name = "mount"
kind = "enum-from-shared-dirs"     # one of the node's CHA_SHARED_DIRS paths; other kinds: "enum" with listed values, "none"
```

Rules the agent enforces when loading (and `--doctor` reports): ids are lowercase `a-z0-9-`; `executor = "host"` needs a configured host channel; `command` has no `/`, spaces or shell characters; an unknown field is an error, not ignored.

The shipped repair script gets its manifest next to it, so `create-node.sh` can install both.

## The host helper

For `executor = "host"`, the owner decides how the agent reaches the host. Two options, one config switch (`CHA_MAINTENANCE_HOST`):

1. **SSH with a forced command** (works for an LXC node on Proxmox, and for any node whose host runs sshd).
   - The owner makes a key pair for the agent, mounts the private key read-only into the agent container, and puts the public key in the host's `authorized_keys` with `command="/usr/local/sbin/cha-maintenance",restrict`.
   - `cha-maintenance` is a root-owned script on the host. It reads `SSH_ORIGINAL_COMMAND`, which is `<command> [<arg>]`, and looks `<command>` up in `/etc/cha-maintenance/commands/` (a directory only root writes). Anything not there is refused. The arg is validated (`^[A-Za-z0-9_./-]+$`, and for `mount` it must be a path an installed `mp` line or fstab entry names).
   - `create-node.sh` on a Proxmox host can set this up: generate the key, install the wrapper, install `repair-mounts.sh` as `commands/repair-mounts`.
2. **A local unix socket** (`/run/cha-maintenance.sock`) served by a small root systemd unit on a bare-metal or VM host, for nodes that aren't in a container. Same command lookup and validation as option 1. This is a later milestone; SSH covers the case that exists today.

In both, what the host runs is a file the owner put there. The portal and the agent can choose among them and pass the one validated argument; they cannot add one.

## Wire protocol

Follows the existing patterns (`NodeRequest` is tagged by `op`; capabilities appear in `Inventory`, as `relay` does).

```rust
// cha-wire
pub struct MaintenanceAction {
    pub id: String,
    pub title: String,
    pub description: String,
    pub stops_environments: bool,
    pub restarts_node: bool,
    /// None: the action takes no argument.
    pub arg: Option<MaintenanceArg>,   // name + the allowed values the agent resolved
}
// Inventory gains:
//   #[serde(default, skip_serializing_if = "Vec::is_empty")]
//   pub maintenance: Vec<MaintenanceAction>,

// NodeRequest gains:
//   RunMaintenance { run_id: String, action: String, arg: Option<String>, confirm_stops: bool }
// NodeResponse gains:
//   MaintenanceStarted { run_id }                    // accepted; the run goes on in the background
//   (refusals use the existing error response: unknown_action, bad_arg, environments_running, busy, disabled)
// ToPortal gains:
//   MaintenanceFinished { run_id, outcome: ok | failed | timed_out, exit_code: Option<i32>, output: String /* last 16 KiB */ }
```

Compatibility, the lesson of [ADR 0020](../adr/0020-vm-environments.md): an older agent drops the connection on an op it doesn't know. The portal therefore sends `RunMaintenance` only to a node whose inventory lists the action, and an agent that predates this has an empty list.

Both ends update together: the Rust types and tests in `cha-wire`, the portal's use in `cha-control`, the agent's in `cha-node`, and the web types in `web/apps/portal/src/api.ts`.

## Agent behaviour

1. At start and on a manifest change, load the manifests, validate them, fill `Inventory::maintenance` (the `enum-from-shared-dirs` arg gets the node's actual shared paths).
2. On `RunMaintenance`:
   - refuse with `disabled` if `CHA_MAINTENANCE` isn't `actions` (the default is `off`; see Policy);
   - `unknown_action` if the id isn't loaded; `bad_arg` if the arg isn't one of the resolved values;
   - `busy` if another run is in progress on this node (one at a time);
   - `environments_running` if the action `stops_environments`, any environment runs, and `confirm_stops` is false;
   - otherwise answer `MaintenanceStarted` and run in the background with a hard timeout.
3. Run through the executor. `agent`: a helper container, no network unless the manifest asks, `--rm`, killed at the timeout. `host`: the SSH or socket call. Stdout and stderr are captured together, kept to the last 16 KiB, control characters stripped.
4. Send `MaintenanceFinished`. If the action `restarts_node`, the agent can't: the connection is going away. See below.

## Reconnects

`repair-shared-mount` reboots the container the agent lives in, so the agent never sends `MaintenanceFinished`. The portal handles that case, not the agent:

- When it sends a run for an action with `restarts_node`, it records the run as `restarting`, with a deadline (the manifest's timeout plus two minutes).
- When the node reconnects, the portal reads its fresh inventory: if the action was a share repair and `shared_status` for that path is `ok`, the run is `ok`; otherwise `failed: the node came back and the share is still <state>`. A node that doesn't come back by the deadline is `timed_out: node did not reconnect`.
- Before it restarts, the agent persists `run_id` to its data directory so that, once back, it can send a `MaintenanceFinished { outcome: ok, output: "the node restarted" }` itself, which lets the portal close the run without guessing from inventory when the agent supports that.

## Portal: API, audit, policy

Endpoints (admin only, as the Nodes page is):

- `GET /api/nodes/{id}/maintenance` → the node's actions from its last inventory, plus its recent runs.
- `POST /api/nodes/{id}/maintenance/{action}` `{ "arg"?: string, "confirm_stops"?: bool }` → `202 { "run_id" }`; refusals map the agent's errors (`409 environments_running`, `409 busy`, `422 bad_arg`, `404 unknown_action`, `503 node_offline`).
- `GET /api/nodes/{id}/maintenance/runs/{run_id}` → state, outcome, output.

Storage: a migration `NNNN_maintenance_runs.sql` (next free number, append-only) with `id, node_id, action, arg, actor_id, started_at, finished_at, outcome, exit_code, output`. Output is capped at 16 KiB.

Audit: `audit_log` entries `node.maintenance.start` and `node.maintenance.finish` with the node, action and arg in `detail`, and the admin as the actor. Failed attempts and refusals are audited too.

Policy:

- **Owner, per node:** `CHA_MAINTENANCE=off|actions` (default `off`), like `CHA_HOST_OPTIONS` (ADR 0021): the owner of the machine, not the portal's admin, decides whether the portal may trigger anything at all. `off` means the inventory lists no actions.
- **Portal admin:** the only role that sees or triggers actions. Switched-to-user admin sessions (ADR 0023) can trigger them; the audit entry carries the real admin.
- **Rate limit:** one run at a time per node, and the same action at most once a minute per node.

## Web UI

- **Nodes page:** a "Maintenance" section per node, listing its actions with a button each. A confirm dialog states what it does (`description`), that it stops environments if `stops_environments` (with the count of running ones), and that the node will go offline for a minute if `restarts_node`. After it runs, the section shows the last result and its output in an expandable block.
- **Shared-folder warning** (the one added on 2026-10-09 on the app card, and the node row's share state): for an admin, when the node offers an action whose arg kind is `enum-from-shared-dirs` and the share is unusable, a **Repair** button runs it with that share's path. Non-admins see the warning only.
- Words and states go through the existing text patterns; no in-stream UI changes, so nothing in `ui-spec`.

## Security model

- The portal can say only: an action id the owner installed, one validated argument, a confirm flag. No path, flag or script from the portal is ever executed.
- Manifests and host commands are written by the node's owner on the node. A portal compromise does not let anyone add one.
- The host channel is as narrow as the owner makes it: with the SSH option, the key can run only the forced wrapper, which runs only files in a root-owned directory.
- Everything is audited, with who, what and the outcome.
- This does not change the existing trust statement: whoever controls the portal controls the node's environments. It adds only what the owner chose to expose, and the default is nothing.
- Output shown in the UI is rendered as text, never as HTML.

## Later

- **Opt-in auto-repair:** `CHA_MAINTENANCE_AUTO=repair-shared-mount`: when a share has been unusable for N minutes (default 5) and no environment is running, run the action. At most once an hour, audited with no actor ("the node did it"), and off by default. Running Steam sessions are never stopped without an admin.
- A socket-based host helper for bare-metal and VM hosts.
- Scheduled actions (a nightly cleanup), if there is a use for them.
- Per-action roles, if more than admins should run some.

## Milestones

| | What | Check |
|---|---|---|
| M1 | `cha-wire` types (`MaintenanceAction`, request, finished message); agent loads and validates manifests, fills inventory, `--doctor` reports them | wire round-trip tests; manifest validation tests (bad id, unknown field, `host` without a channel) |
| M2 | Agent runs `executor = "agent"` actions: one-at-a-time, timeout, output cap, refusals | node tests with a stub executor; a timeout test |
| M3 | Host channel over SSH: `cha-maintenance` wrapper, `create-node.sh` installs it and the repair command; agent calls it | wrapper tests (unknown command, bad arg, no `SSH_ORIGINAL_COMMAND`); a run on mini-titan against LXC 103 |
| M4 | Portal: migration, endpoints, audit, reconnect handling (`restarts_node`) | `crates/cha-control/tests/api.rs`: admin only, refusals, run recorded, run closed on reconnect with a fixed share, timed out when the node doesn't return |
| M5 | Web: Nodes page section and the Repair button on the shared-folder warnings | `vue-tsc`; the repair flow by hand with a deliberately stale share |
| M6 | Docs: ADR 0024, `deploy/README.md` (`CHA_MAINTENANCE*`, the wrapper), `deploy/proxmox/README.md`, PLAN status | |

M1–M2 are useful without M3 (agent-side chores); M3 is what makes the stale-share repair work. Auto-repair stays out until M1–M5 have been used by hand for a while.

## Open questions

1. **Does `restarts_node` need the agent to persist the run?** The portal can infer the result from inventory for share repairs, but a generic restarting action has no such signal. The persisted `run_id` answers it; the cost is a small file in the agent's data directory.
2. **One helper per node, or one per host?** Several LXC containers on one Proxmox host would each get their own key, so one container can't trigger another's repair. The wrapper could map a key to the container IDs it may touch (`--id` fixed in the `command=` line).
3. **Should the argument for `repair-mounts` be the mount, or the container ID?** The mount is what the agent knows (`CHA_SHARED_DIRS`); the container ID is what `pct` needs. The wrapper currently finds containers from their `mp` lines, so the mount is enough.
4. **Is the Unraid side worth an action of its own?** Remedies there (raising `fuse_remember`, rescanning) are on another machine and aren't the Cha node's to run; they stay manual.
