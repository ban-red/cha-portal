# 0002: Auth: local accounts with Argon2id passwords first, passkeys next

- **Status:** accepted (2026-10-03)
- **Context:** PLAN §5.3 calls for built-in accounts with passkeys, plus optional OIDC. The portal needs working sign-in before nodes and environments can be built and tested.
- **Decision:** P1.1 ships local accounts:
  - **Passwords:** Argon2id, minimum 10 characters.
  - **Sessions:** a 256-bit random token in an `HttpOnly`, `SameSite=Lax` cookie (`Secure` behind HTTPS). Only the token's SHA-256 is stored.
  - **CSRF:** every mutating endpoint takes a JSON body, so a cross-site form can't send it without a CORS preflight.
  - **First admin:** created with a one-time setup token printed in the server log.

  Passkeys (webauthn-rs) come next, as an extra credential on the same accounts and session model. OIDC comes after that.
- **Consequences:** a homelab can sign in today. Adding passkeys needs no change to sessions. Login rate limiting is still to do; Argon2's cost slows guessing in the meantime.
