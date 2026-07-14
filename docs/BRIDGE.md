# WebSocket -> TCP bridge

## What it is

sshmux runs a full SSH client inside your browser, but browsers cannot open
raw TCP connections. The bridge is a dumb relay: it accepts a WebSocket
connection and pipes the bytes to `sshd` on port 22. All it ever sees is SSH
ciphertext — it cannot read your password, keys, or session content.

```
browser (wasm ssh client)  --wss://-->  bridge  --tcp-->  sshd:22
```

Run the bridge on the same host as `sshd` (or anywhere that can reach it).

> **One-command shortcut.** `jbcentral mobile` (JetBrains Central CLI) automates
> everything below — it runs the bridge, brings up a Cloudflare quick tunnel with
> a trusted cert, mints a short-lived SSH key, and prints a QR that opens this app
> already connected. Scan and go. The rest of this doc is for running the bridge
> yourself.

## Options (websockify, custom)

[websockify](https://github.com/novnc/websockify) is the reference
implementation and all you need:

```sh
# one-off, no install (recommended for trying it out)
uvx websockify 8022 localhost:22

# or as a persistent tool
pipx install websockify
websockify 8022 localhost:22

# or from distro packages
sudo apt install websockify        # Debian/Ubuntu
websockify 8022 localhost:22
```

This listens on `:8022` for WebSocket connections and forwards each one to
`localhost:22`. Any equivalent ws->tcp proxy works; there is no sshmux-specific
protocol.

### systemd unit

`/etc/systemd/system/ssh-ws-bridge.service`:

```ini
[Unit]
Description=WebSocket to sshd bridge for sshmux
After=network.target

[Service]
ExecStart=/usr/bin/websockify 127.0.0.1:8022 localhost:22
Restart=on-failure
DynamicUser=yes
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now ssh-ws-bridge
```

Note it binds `127.0.0.1` — expose it via one of the TLS options below rather
than directly.

## TLS (wss://) setup

**The page is served over https, so the bridge MUST be reachable over
`wss://` with a certificate the browser trusts.** iOS Safari will not accept
self-signed certificates for WebSockets, and there is no override UI. The one
exception: when developing locally over `http://localhost` (`trunk serve`),
plain `ws://localhost:8022` works fine.

Every option below runs on the **same VM as `sshd`** — websockify forwards to
`localhost:22`, and cloudflared/Tailscale only make outbound connections, so
there is nothing to route inbound and no reason to spin up a separate host.
A dedicated bridge box only helps if you are fronting many SSH targets at once.

Ranked options:

1. **Cloudflare Tunnel (recommended for Cloudflare / Zero Trust setups).**
   Outbound-only: `cloudflared` dials Cloudflare's edge (TCP/UDP 7844), so you
   can block *all* inbound traffic and never expose an IP or port. This is the
   right fit if your VM runs WARP / Zero Trust, which is designed to eliminate
   exactly the kind of inbound port the other options open.

   ```sh
   # 1. install + authenticate (one time)
   #    see https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/
   cloudflared tunnel login
   cloudflared tunnel create sshmux

   # 2. bind websockify to loopback ONLY — cloudflared reaches it locally,
   #    nothing on the network can. (Use the systemd unit above.)
   websockify 127.0.0.1:8022 localhost:22

   # 3. route a hostname to the local websockify port
   cloudflared tunnel route dns sshmux ssh.example.com
   ```

   `~/.cloudflared/config.yml`:

   ```yaml
   tunnel: sshmux
   credentials-file: /root/.cloudflared/sshmux.json
   ingress:
     # http:// here is fine — this hop is loopback-only; Cloudflare terminates
     # the public TLS. cloudflared proxies the WebSocket upgrade automatically.
     - hostname: ssh.example.com
       service: http://127.0.0.1:8022
     - service: http_status:404
   ```

   ```sh
   cloudflared tunnel run sshmux    # or `cloudflared service install` for systemd
   ```

   Point the connect screen at `wss://ssh.example.com`.

   **Add an auth layer.** A bare tunnel is public to anyone with the URL — it
   does *not* authenticate. websockify has no auth of its own, so put a
   **Cloudflare Access** policy (Zero Trust dashboard → Access → Applications)
   on `ssh.example.com` scoped to your email/SSO. The PWA logs in to Access in
   the browser first (SSO redirect sets the `CF_Authorization` cookie), and the
   WebSocket to the same origin rides that cookie. Test this early —
   WebSocket-through-tunnel with Access has [known
   quirks](https://community.cloudflare.com/t/websocket-connections-not-working-through-cloudflare-tunnels/604188);
   if it fights you, you can drop Access and rely on SSH key auth alone and
   still keep the no-inbound-port and hidden-IP benefits.

   > Not the same as Cloudflare's built-in browser SSH (`cloudflared access
   > ssh` / their web terminal): those terminate SSH server-side and render
   > their own terminal, bypassing sshmux. Here the tunnel is only the dumb
   > `wss://` transport — sshmux still does all SSH crypto in your browser and
   > the tunnel carries ciphertext.

2. **Tailscale Serve.** Tailnet-only exposure with automatic,
   publicly-trusted certs and no open ports:

   ```sh
   tailscale serve --bg 8022
   ```

   The bridge is then at `wss://<host>.<tailnet>.ts.net`. Your phone just
   needs to be on the tailnet. Nothing is exposed to the internet. Also
   outbound-only, so it coexists fine with a Zero Trust posture.

3. **Tailscale Funnel.** Same as above but public — use only if you need
   access without the Tailscale app (and ideally with SSH key auth +
   fail2ban-style protections on sshd):

   ```sh
   tailscale funnel --bg 8022
   ```

4. **Caddy + Let's Encrypt.** For a public bridge on your own domain. Note this
   one *does* open an inbound port (443), so it is the least suitable for a
   Zero Trust / WARP host — prefer option 1 there:

   ```
   bridge.example.com {
       reverse_proxy 127.0.0.1:8022
   }
   ```

   Caddy provisions and renews the certificate automatically; WebSocket
   upgrades are proxied out of the box. Point the connect screen at
   `wss://bridge.example.com`.

## Hardening

- The bridge only relays ciphertext, so the security boundary is `sshd`
  itself: prefer key auth, consider a dedicated low-privilege user, and keep
  `sshd_config` tight (e.g. `AllowUsers`, `PasswordAuthentication no` if you
  use key auth in sshmux).
- Bind websockify to `127.0.0.1` and let the front end (Cloudflare
  Tunnel/Tailscale/Caddy) be the only thing that reaches it.
- Prefer an outbound-only, authenticated front end: Cloudflare Tunnel + Access
  (option 1) or tailnet-only Tailscale Serve (option 2). A public bridge with
  no auth in front is a public door to your sshd.
- sshmux pins the SSH host key fingerprint (TOFU) per bridge URL and refuses
  to connect if it changes, so a swapped-out bridge/sshd cannot silently MITM
  you after first use.
