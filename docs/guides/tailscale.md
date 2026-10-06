# Remote access with Tailscale

Tailscale (or plain WireGuard) is the simplest way to reach a homelab portal from anywhere. Nothing is exposed to the internet, the portal gets a real HTTPS certificate, and the stream goes straight from the node to your browser over WireGuard.

## What you get

- **The portal** at `https://<portal-host>.<tailnet>.ts.net`, served by `tailscale serve` with a certificate Tailscale issues. Browsers need HTTPS for gamepads, keyboard lock and audio worklets, so this matters even at home.
- **Nodes** that enroll and connect to that URL over the tailnet.
- **Media** straight from node to browser. Each streamer offers every address its node has, including the Tailscale one (`100.x.y.z`), so a browser on the tailnet connects directly. No subnet routes are needed.

## Steps

1. **Install Tailscale** on the portal's host, on every node, and on the devices you'll play from. Run `sudo tailscale up` on each.
2. **Turn on HTTPS** for the tailnet: in the admin console, under **DNS**, enable MagicDNS and **HTTPS Certificates**.
3. **Start the portal** on its host. It listens on `127.0.0.1:7676` only:

   ```bash
   docker compose -f deploy/portal/compose.yaml up -d --build
   ```

4. **Serve it over HTTPS** on the tailnet:

   ```bash
   sudo tailscale serve --bg 7676
   ```

   `tailscale serve status` shows the URL. Open it, and create the first admin with the setup token from `docker compose -f deploy/portal/compose.yaml logs portal`.
5. **Add each node**: **Admin → Nodes → Add node**, then on the node:

   ```bash
   CHA_PORTAL_URL=https://portal-host.your-tailnet.ts.net CHA_JOIN_TOKEN=chajoin_… docker compose -f deploy/node/compose.yaml up -d --build
   ```

   Check it with `docker compose -f deploy/node/compose.yaml run --rm agent --doctor`.
6. **Launch and connect** from any device on the tailnet.

## Check the path

- `tailscale ping <node>` should answer **via** an address and port, not **via DERP**. DERP relays work, but they add latency and cap bandwidth. If a node only gets DERP, open UDP 41641 to it on its router, or see Tailscale's notes on NAT traversal.
- In the session's **Stats**, the RTT should be close to `tailscale ping`'s time.
- WireGuard's MTU (1280) is fine for WebRTC, whose packets stay under 1200 bytes.

## Notes

- **Don't use Funnel** for the portal unless you mean to publish it on the internet.
- **Never enable `--dev-login` on a portal behind `tailscale serve`**, or behind any proxy on the same machine. Every request then comes from `127.0.0.1`, so every tailnet user would count as local.
- **Lock it down** with Tailscale ACLs if the tailnet is shared. Players need TCP 443 to the portal's host and UDP to the nodes' streamer ports (7600–7647 by default). Nodes need TCP 443 to the portal's host.
- **Plain WireGuard** works the same way: streamers offer the `wg0` address too. Serve the portal with any HTTPS proxy, e.g. Caddy with an internal CA.

## Without a mesh

- **Port-forward:** forward the streamers' UDP ports (7600–7647 by default: WebRTC and WebTransport) from the router to the node, and set `CHA_PUBLIC_ADDRESS` to the router's public IP on the node. The portal itself still needs HTTPS: use the compose file's `tls` profile (Caddy, public DNS).
- **TURN:** for networks that block UDP to the node, run the portal compose file's `turn` profile (coturn) where browsers can reach it and it can reach the nodes. The portal mints a day's credentials per connection. TURN over TLS on 443 comes with Phase 3.
