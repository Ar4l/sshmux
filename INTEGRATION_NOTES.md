<!-- Cross-agent integration notes. Implementer agents: record missing
     dependencies, contract deviations, and anything the Integrate agent
     must reconcile. Scaffold left this empty on purpose. -->

## SSH implementer (src/ssh/)

- `SshSession` in mod.rs gained a wasm-only field: `pub(crate) inner: std::rc::Rc<session::SessionInner>` (contract said "internally Rc"; struct literal construction happens only inside src/ssh). Native builds have a zero-field SshSession; connect/exec return `SshError::Connect("wasm only")`, is_alive() -> false.
- Host-key status slot is `Arc<std::sync::Mutex<Option<HostKeyStatus>>>`, not `Rc<RefCell<..>>`: russh's `Handler` bound requires `Send`. Equivalent on single-threaded wasm.
- `keepalive_interval` deliberately left None: it compiles, but russh drives keepalive with `tokio::time::sleep`, which panics at runtime on wasm (no tokio timer). Liveness instead via `Handle::is_closed()` inside `is_alive()`; app-level reconnect logic should poll that.
- russh's `Handle` is not Clone, so it sits behind `tokio::sync::Mutex`; the lock is held only while opening a channel, so concurrent exec() calls interleave fine. `is_alive()` uses try_lock and treats "locked" (exec in flight) as alive.
- RSA auth: `best_supported_rsa_hash()` result is `.flatten().or(Some(HashAlg::Sha256))` — on wasm russh skips the 1s EXT_INFO wait, so if the server's server-sig-algs hasn't arrived we still sign with rsa-sha2-256 instead of legacy ssh-rsa.
- Encrypted key detection matches `russh::keys::Error::KeyIsEncrypted` (covers OpenSSH-format keys; PKCS#5/8-encrypted PEMs surface as generic KeyParse with the decoder's message).
- exec() collects stdout (Data), stderr (ExtendedData ext==1), exit code (ExitStatus); loops until Close/channel end rather than breaking on Eof so a late exit-status isn't dropped.
- localStorage key format: `sshmux:hostkey:<bridge_url>` via gloo-storage (JSON-serialized String).

## tmux+claude implementer
- No new dependencies needed; no contract signature changes.
- `TranscriptTail` gained a private field `drop_first: bool` (set by `new_at_end_window` when starting mid-file); public API unchanged.
- `TranscriptRef.mtime` is an `ls -t` rank surrogate (higher = newer), not a real epoch — per spec; do not display as a date.
- `find_transcripts` filters `agent-*.jsonl` after `head -5`, so it can return fewer than 5 refs.
- tmux commands with nonzero exit (other than 127 / "no server") map to `TmuxError::Parse(stderr)` — closest available variant.
- claude/mod.rs uses `crate::tmux::shell_quote` (cross-module, already pub).

## UI implementer (src/ui/, style.css, docs/)

Wiring the Integrate agent must do in app.rs:

- **Provide `crate::ui::UiState` context.** `App()` must call
  `provide_context(crate::ui::UiState::new())` right after `provide_context(state)`.
  Every screen `expect_context::<UiState>()`s it. Fields (all `RwSignal`, Copy):
  `pane_status: PaneListStatus{Loading,Ready,NoTmuxBinary,NoServer}`,
  `capture: Option<String>`, `transcripts: Vec<TranscriptRef>`,
  `selected_transcript: Option<String>` (path), `chat_items: Vec<ChatItem>`.
- **Polling loops write into UiState:**
  - pane list (~5s): on Ok set `state.panes` + `ui.pane_status = Ready`; map
    `TmuxError::NoTmuxBinary/NoServer` to the matching `pane_status`; other errors to
    `state.error`.
  - capture (~2s, when detail screen visible and Terminal is showing — note the
    Chat no-transcript fallback also shows the terminal, i.e. when
    `view_mode == Chat && ui.selected_transcript.get().is_none()`): write raw
    `capture_pane` output into `ui.capture` (Some(text)).
  - transcripts: on entering a Claude pane run `find_transcripts(pane.path)`, set
    `ui.transcripts` and default `ui.selected_transcript` to the newest path
    (`None` if empty — UI then shows the "no transcript found" fallback).
  - transcript tail (~2s, Chat visible + transcript selected): append
    `TranscriptTail::poll` results to `ui.chat_items` (`update(|v| v.extend(..))`).
    When `ui.selected_transcript` changes (the picker clears `ui.chat_items` and
    sets the new path), restart the tail via `new_at_end_window` for that path.
- **UI already does itself** (one-shot commands; no wiring needed): connect button
  (sets `state.session/status/screen`, bumps `state.generation` on success, handles
  HostKeyChanged retry with trust_changed_key=true), manual pane refresh
  (panes.rs calls `list_panes` directly), send_submit/send_key (detail.rs), and the
  detail-signal reset when opening a pane (capture/transcripts/selected/chat_items
  cleared in panes.rs `open_pane`). Reconnect/backoff + visibilitychange remain
  app.rs's job; on reconnect, set `state.session` and bump `state.generation`.

Contract deviations / notes:

- `TerminalView` prop `text` changed from `String` to `#[prop(into)] Signal<String>`
  and `ChatView` prop `items` from `Vec<ChatItem>` to
  `#[prop(into)] Signal<Vec<ChatItem>>` — needed so poll updates don't recreate the
  DOM subtree (scroll position/auto-scroll state live in the component).
- connect.rs stores the remembered form (JSON `SavedForm`) under localStorage key
  `"sshmux.connect"` — distinct from the ssh module's `sshmux:hostkey:<url>` keys.
  Secrets (password/private key) are stored only when "remember" is checked; a
  warning is shown in the UI.
- gloo (storage/timers) usage in ui/ is `#[cfg(target_arch = "wasm32")]`-gated with
  native no-ops so `cargo test` keeps compiling; no new dependencies added.
- No `is_alive()`-based indicator in the UI yet; the panes-screen auto-refresh dot
  reflects `state.status` (`ConnStatus::Connected`). Keep `state.status` accurate
  from the reconnect loop.

## Integrate agent

- `ConnectOpts` and `Auth` now `#[derive(Clone)]` (needed so reconnect can replay
  the last opts; no field changes).
- `AppState` gained `pub connect_opts: RwSignal<Option<ConnectOpts>>`; connect.rs
  sets it on successful connect. Reconnect (backoff 1s/2s/4s, 3 tries) replays it
  with trust_changed_key=false — a host key that changes mid-session lands the user
  back on the Connect screen, where the existing HostKeyChanged UI handles retrust.
- Polling loops live in `src/app.rs` `mod runtime` (wasm-only): each is a Leptos
  Effect that bumps an Rc<Cell<u64>> epoch on re-run (killing the previous loop)
  and re-checks the epoch after every await so stale results are never written.
- On reconnect (generation bump) the transcripts fetch re-runs and resets
  `chat_items` + `selected_transcript` (fresh end-window tail), avoiding duplicate
  chat items from re-reading the tail window.
- Deps added to Cargo.toml (wasm): `js-sys` (visibilitychange Closure cast) and
  web-sys feature `"EventTarget"`.

## Fix agent (review findings)

Contract changes (all callers updated in-repo):

- `SshSession` gained `exec_bytes(&self, cmd) -> Result<ExecBytes, SshError>`
  (`ExecBytes { stdout: Vec<u8>, stderr: String, exit_code }` in ssh/mod.rs);
  `exec` is now a lossy-String wrapper over it. `TranscriptTail::poll` uses
  exec_bytes so its file offset advances by raw bytes (from_utf8_lossy can
  change byte length when `head -c` tears a multi-byte UTF-8 char);
  `TranscriptTail.partial` is now `Vec<u8>`.
- wasm exec is raced against a 20s `gloo_timers` TimeoutFuture; on timeout it
  marks the session dead and returns `SshError::Disconnected` so reconnect
  fires on half-open transports (mobile blackhole, no SSH keepalive on wasm).
- `tmux::capture_pane` now returns `(u16, u16, String)` — current pane
  width/height (one extra `display-message` in the same exec) + capture text —
  so the detail screen tracks remote pane resizes; capture_loop updates
  `state.active_pane` dims when they change.
- `run_tmux` treats exit_code `None` with a dead session as
  `TmuxError::Ssh(Disconnected)` instead of success (empty output no longer
  flashes "no tmux panes" when the connection died mid-exec).
- `parse_list_panes` skips malformed lines (SEP/newline inside free-form
  fields) instead of failing the whole list.
- `project_slug` now maps every non-alphanumeric char to '-' (matches Claude
  Code's `cwd.replace(/[^a-zA-Z0-9]/g, '-')`; previously only '/' and '.').

Known gap deferred to a follow-up (needs index.html, off-limits here):
keyboard occlusion — add `interactive-widget=resizes-content` to the viewport
meta (Android) and a `window.visualViewport` resize handler that offsets
`.bottom-bar` while the on-screen keyboard is open (iOS Safari's 100dvh does
not shrink for the keyboard).
