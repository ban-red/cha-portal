# 0023: Users, switching and per-user access

- **Status:** accepted (2026-10-09). Extends the accounts of P1.1.
- **Context:** a portal shared by a household or a few friends needs the owner to make accounts, see the portal as a user sees it, and keep some users off some nodes (a laptop-class node, the one running the owner's own games).
- **Decision:**
  - **Local users have an email** (required for new accounts, lower-cased, unique). The username defaults to it; sign-in takes either. There is still no mail: the admin chooses the password.
  - **Switching is a session, not a password.** An admin starts a session for any enabled user (`POST /api/auth/switch`). It records the admin as `impersonator_id`, lasts 12 hours and is replaced, never stacked, by the next switch. Every route sees the target as the user, so admin pages refuse while viewing as a non-admin. `/api/me` reports who is behind it, and `switch-back` starts a normal session for that admin. The switch and the return are audited under the admin. Actions taken while switched are audited under the user viewed, as the session is theirs; device tokens never switch.
  - **Node restriction.** A user flagged `node_restricted` may launch only on nodes in `user_nodes`. The filter sits in placement, so the launch menu, automatic placement and an explicit node all follow it. Moonlight launches check it too, and a restricted user sees only the hosts' apps on nodes they may use.
  - **Grants** (`user_grants`: user, node, template) let a restricted user run one template on one node outside their list. The dashboard shows them as their own "Shared with you" section.
  - **Instance limit.** `users.max_instances` (1 to 64; empty is the portal default of 4) caps live environments for any user, admins included.
  - **Deleting a user** is refused for yourself, for the last enabled admin and while they have live environments. Their sessions, devices, shares, grants and settings go with them (foreign keys cascade); app data on nodes stays.
  - **Audit events:** `user.created`, `user.deleted`, `user.switched`, `user.switched_back`, `user.access_set`, `user.grant_added`, `user.grant_removed`.
- **Consequences:** a restriction is a placement rule, not a security boundary between users on one node. An admin can always act as any user.
