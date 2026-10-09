-- Share links over the internet (ADR 0022; the contract is docs/plans/wan-sharing.md).
-- A link with `wan` set is on the portal's Cloudflare Tunnel hostname and is the
-- only kind the guest-only listener knows. Every existing link is a LAN one.
ALTER TABLE shares ADD COLUMN wan INTEGER NOT NULL DEFAULT 0;
