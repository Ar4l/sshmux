# sshmux

**Live: https://aral.cc/sshmux/** (also https://ar4l.github.io/sshmux/)

Mobile-first static web app that is an in-browser SSH client for driving
tmux-hosted Claude Code (and other) agent sessions. Rust + Leptos (CSR),
compiled to WebAssembly, served from GitHub Pages. The SSH protocol runs
entirely in your browser (russh on wasm); a dumb WebSocket->TCP bridge relays
ciphertext to sshd.

## Architecture

```
+--------------------- browser (static wasm app) ---------------------+
|  Leptos UI (connect / panes / pane detail: terminal|chat)           |
|      |                                                              |
|  tmux module (list-panes / capture-pane / send-keys via ssh exec)   |
|  claude module (tail ~/.claude/projects/<slug>/*.jsonl -> chat)     |
|      |                                                              |
|  russh client (crypto IN the browser)                               |
|      |                                                              |
|  WebSocket (wss://)                                                 |
+------|---------------------------------------------------------------+
       v
  ws->tcp bridge (sees only SSH ciphertext)      docs/BRIDGE.md
       |
       v
  sshd on your host --- tmux server --- panes running `claude`, etc.
```

- Pane list is read via `tmux list-panes -a`; the visible pane is polled with
  `capture-pane -e` and rendered through an in-wasm terminal (avt).
- Claude Code panes get a Chat view by tailing the session transcript JSONL
  over SSH `exec` and rendering user/assistant/tool items as bubbles.
- Input goes back through `tmux send-keys`.

## One-command connect (`sshmux` CLI)

Install (Homebrew — pulls in `cloudflared` automatically):

```sh
brew install ar4l/tap/sshmux
# or track main: brew install --HEAD ar4l/tap/sshmux
```

First make sure `sshd` is reachable on the machine you want to control:
- **macOS:** System Settings › General › Sharing › enable **Remote Login**
- **Linux:** `sudo systemctl enable --now ssh`

Then, instead of running a bridge and typing connection details by hand, just
run it on that machine:

```sh
sshmux                 # starts a token-gated relay + cloudflared quick tunnel,
                       # prints a QR + URL to open the web app pre-filled
sshmux --local-only    # loopback relay, NO tunnel — nothing leaves the machine
```

It works the same on a fixed-IP Linux VM (even behind a firewall / WARP) and a
NAT'd MacBook, because the tunnel is outbound-only — no inbound port is opened.
The public URL reaches your sshd only via a 128-bit path token (checked before
any TCP dial), and the URL also carries the host-key fingerprint so first use is
*verified*, not blind TOFU. The CLI lives in `cli/`; the shared deep-link schema
in `link/`. See `docs/BRIDGE.md` for the manual bridge alternative.

## Quickstart

```sh
# prerequisites: rust 1.89 + wasm target, trunk, and (macOS) brew install llvm
rustup target add wasm32-unknown-unknown
cargo install trunk

trunk serve            # dev server
cargo test             # native unit tests (pure parsers)
trunk build --release  # static site in dist/
```

Then run a WebSocket->TCP bridge in front of your sshd (see `docs/BRIDGE.md`)
and point the connect screen at `wss://your-bridge`.

## Security model

- **Crypto in the browser.** The SSH handshake, key exchange, and encryption
  all happen inside the wasm module (russh + ring). Credentials never leave
  the page except inside the SSH protocol.
- **The bridge sees only ciphertext.** It blindly copies bytes between a
  WebSocket and a TCP connection to sshd; compromising it does not reveal
  passwords or session content.
- **TOFU host-key pinning.** The first host key fingerprint seen for a bridge
  URL is stored in localStorage; later mismatches hard-fail until you
  explicitly trust the new key.
- **Unencrypted-key caveat (MVP).** Private-key auth accepts only pasted
  *unencrypted* OpenSSH keys, kept in memory. Prefer a throwaway key
  restricted on the server, or password auth.

## Docs

- `docs/BRIDGE.md` — running the WebSocket->TCP bridge
- `docs/DEPLOY.md` — GitHub Pages deployment
