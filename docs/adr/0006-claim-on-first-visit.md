# 0006: First admin by claiming a fresh portal; 3-character minimums

- **Status:** accepted (2026-10-06). Supersedes two points of [0002](0002-local-accounts-first-passkeys-next.md): the setup token and the 10-character password minimum.
- **Context:** first-run setup asked the owner to dig a one-time token out of the container log before they could create an account, and passwords needed 10 characters with a strict username alphabet. For a homelab portal that sits on localhost or a tailnet, that was friction on the very first screen.
- **Decision:**
  - A portal with no accounts shows **Claim this portal**. The first visitor picks the first admin's username and password and is signed in; there is no token. `POST /api/setup` takes `{ username, password, displayName? }`, holds a lock while it creates the account, and answers `409 already_set_up` once any account exists.
  - Usernames need 3–64 characters after trimming, with no control characters. Passwords need at least 3 characters. The same rules apply to accounts admins create later.
- **Consequences:** setup is one form. Anyone who reaches a fresh portal before the owner can claim it, so the owner should open it right after the first start; the shipped compose files bind the portal to `127.0.0.1`, which keeps that window local. Short passwords are the owner's choice; Argon2id still hashes them, and login rate limiting is still to do.
