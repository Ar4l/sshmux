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

Ranked options:

1. **Tailscale Serve (recommended).** Tailnet-only exposure with automatic,
   publicly-trusted certs and no open ports:

   ```sh
   tailscale serve --bg 8022
   ```

   The bridge is then at `wss://<host>.<tailnet>.ts.net`. Your phone just
   needs to be on the tailnet. Nothing is exposed to the internet.

2. **Tailscale Funnel.** Same as above but public — use only if you need
   access without the Tailscale app (and ideally with SSH key auth +
   fail2ban-style protections on sshd):

   ```sh
   tailscale funnel --bg 8022
   ```

3. **Caddy + Let's Encrypt.** For a public bridge on your own domain:

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
- Bind websockify to `127.0.0.1` and let the TLS front end (Tailscale/Caddy)
  be the only listener.
- Prefer tailnet-only exposure (option 1); a public bridge is a public door
  to your sshd.
- sshmux pins the SSH host key fingerprint (TOFU) per bridge URL and refuses
  to connect if it changes, so a swapped-out bridge/sshd cannot silently MITM
  you after first use.
