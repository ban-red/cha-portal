# Security

## Reporting a vulnerability

Please report vulnerabilities privately, through GitHub's [**Report a vulnerability**](https://github.com/ban-red/cha-portal/security/advisories/new) form on this repository, not in a public issue.

Say what is affected, how to reproduce it, and what an attacker gains. You'll get an answer within a week. Once a fix is ready, we'll publish an advisory and credit you, unless you'd rather not be named.

## Supported versions

Cha Portal is pre-release and has no tagged releases. Fixes go to `main` only: run a recent `main`.

## What Cha Portal trusts

Cha Portal is built for a homelab: one owner, their own machines, and users they invite. Knowing what it trusts helps you judge a report, and run it safely.

- **The node agent is root-equivalent on its machine.** It holds the Docker socket to start environments. Anyone who controls the agent, or the portal it obeys, controls the node. Run nodes only for a portal you control.
- **Portal admins control every node.** An admin can enroll nodes and launch environments on them. Give admin only to people you would give a shell on your GPU servers.
- **Nodes dial out to the portal.** A node needs no inbound port for control: it holds one outbound WebSocket, authenticated with its own Ed25519 key after enrolling with a one-time join token. Join tokens are stored hashed and expire after 60 minutes. The agent refuses plain `http://` to another machine unless `CHA_ALLOW_INSECURE_PORTAL` is set, which is for development only.
- **Media goes straight from the node to the browser.** The portal issues a short-lived (60 s) signed media token per connection, and the streamer checks it before streaming. Streamer ports should be reachable only from your LAN, your tailnet, or through your own TURN server.
- **Accounts** are local, with Argon2id-hashed passwords and session cookies marked Secure. Serve the portal over HTTPS only.
- **Environments are plain containers**, never `--privileged`, each run under a security profile: a seccomp profile so browsers keep their own sandbox, and an AppArmor profile (`cha-sandbox`) for Steam's sandbox. This is container isolation, not a VM: treat what runs in an environment as able to attack the node's kernel.
- **App data is isolated per mount, not per user id.** Every environment runs as uid 1000, so a process on the node itself running as uid 1000 can read every user's data.
- **`--dev-login` signs anyone in without a password** if their request comes from the portal's own machine. Never use it on a real portal, and above all never behind a proxy or `tailscale serve` on the same machine, where every request comes from `127.0.0.1`.

Known gaps are listed in [`deploy/README.md`](deploy/README.md#known-gaps). Reports about them are still welcome if you've found a way to make them worse.
