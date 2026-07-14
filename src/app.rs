use leptos::prelude::*;

use crate::ssh::{ConnectOpts, SshSession};
use crate::tmux::Pane;
use crate::ui::{ConnectScreen, DetailScreen, PanesScreen, UiState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Connect,
    Panes,
    PaneDetail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Terminal,
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// All signals are Copy handles; clone the struct freely.
/// `session` uses LocalStorage because SshSession is Rc-based (wasm is
/// single-threaded).
#[derive(Clone, Copy)]
pub struct AppState {
    pub screen: RwSignal<Screen>,
    pub session: RwSignal<Option<SshSession>, LocalStorage>,
    pub panes: RwSignal<Vec<Pane>>,
    pub active_pane: RwSignal<Option<Pane>>,
    pub view_mode: RwSignal<ViewMode>,
    pub status: RwSignal<ConnStatus>,
    pub error: RwSignal<Option<String>>,
    /// Polling loops (pane list ~5s, capture ~2s, transcript ~2s) capture the
    /// current generation and abort when it changes; only the visible view
    /// polls. Reconnect-with-backoff hooks visibilitychange + socket death.
    pub generation: RwSignal<u64>,
    /// Opts of the last successful connect; reconnect replays them.
    pub connect_opts: RwSignal<Option<ConnectOpts>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            screen: RwSignal::new(Screen::Connect),
            session: RwSignal::new_local(None),
            panes: RwSignal::new(Vec::new()),
            active_pane: RwSignal::new(None),
            view_mode: RwSignal::new(ViewMode::Terminal),
            status: RwSignal::new(ConnStatus::Disconnected),
            error: RwSignal::new(None),
            generation: RwSignal::new(0),
            connect_opts: RwSignal::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state);
    let ui = UiState::new();
    provide_context(ui);

    #[cfg(target_arch = "wasm32")]
    runtime::wire(state, ui);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ui;

    view! {
        <div class="app">
            {move || match state.screen.get() {
                Screen::Connect => view! { <ConnectScreen/> }.into_any(),
                Screen::Panes => view! { <PanesScreen/> }.into_any(),
                Screen::PaneDetail => view! { <DetailScreen/> }.into_any(),
            }}
        </div>
    }
}

/// Polling loops + reconnect. Each loop lives in an Effect that re-runs when
/// its tracked signals (screen/view/generation/...) change: the effect bumps
/// an epoch cell, killing the previously spawned loop, and spawns a fresh one
/// only while its view is visible. Ticks re-check the epoch after every await
/// so stale results are never written into state.
#[cfg(target_arch = "wasm32")]
mod runtime {
    use std::cell::Cell;
    use std::rc::Rc;

    use gloo_timers::future::TimeoutFuture;
    use leptos::prelude::*;
    use leptos::task::spawn_local;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    use super::{AppState, ConnStatus, Screen, ViewMode};
    use crate::claude::{self, ClaudeError, TranscriptRef, TranscriptTail};
    use crate::ssh::{SshError, SshSession};
    use crate::tmux::{self, AgentKind, TmuxError};
    use crate::ui::{ssh_err_text, tmux_err_text, PaneListStatus, UiState};

    const PANES_MS: u32 = 5_000;
    const CAPTURE_MS: u32 = 2_000;
    const TAIL_MS: u32 = 2_000;

    pub fn wire(state: AppState, ui: UiState) {
        pane_list_loop(state, ui);
        capture_loop(state, ui);
        transcripts_fetch(state, ui);
        tail_loop(state, ui);
        visibility_reconnect(state);
    }

    fn claude_err_text(e: &ClaudeError) -> String {
        match e {
            ClaudeError::Ssh(e) => ssh_err_text(e),
            ClaudeError::NotFound => "transcript not found".into(),
            ClaudeError::PermissionDenied => "permission denied reading transcript".into(),
            ClaudeError::Parse(m) => format!("transcript parse error: {m}"),
        }
    }

    /// Session for a poll tick; triggers reconnect when the socket died.
    fn live_session(state: AppState) -> Option<SshSession> {
        let sess = state.session.get_untracked()?;
        if sess.is_alive() {
            Some(sess)
        } else {
            schedule_reconnect(state);
            None
        }
    }

    /// Reconnect with saved opts: backoff 1s/2s/4s, max 3 tries, then back to
    /// the Connect screen with an error. Bumps `generation` on success so all
    /// polling loops restart against the new session.
    fn schedule_reconnect(state: AppState) {
        if matches!(
            state.status.get_untracked(),
            ConnStatus::Connecting | ConnStatus::Reconnecting
        ) {
            return;
        }
        let Some(opts) = state.connect_opts.get_untracked() else {
            state.status.set(ConnStatus::Disconnected);
            state.screen.set(Screen::Connect);
            return;
        };
        state.status.set(ConnStatus::Reconnecting);
        spawn_local(async move {
            let mut last = "gave up".to_string();
            for delay in [1_000u32, 2_000, 4_000] {
                TimeoutFuture::new(delay).await;
                match SshSession::connect(opts.clone(), false).await {
                    Ok(sess) => {
                        state.session.set(Some(sess));
                        state.status.set(ConnStatus::Connected);
                        state.error.set(None);
                        state.generation.update(|g| *g += 1);
                        return;
                    }
                    Err(e) => last = ssh_err_text(&e),
                }
            }
            state.session.set(None);
            state.status.set(ConnStatus::Disconnected);
            state.error.set(Some(format!("reconnect failed: {last}")));
            state.screen.set(Screen::Connect);
        });
    }

    /// visibilitychange -> visible: if the session died while backgrounded,
    /// reconnect immediately instead of waiting for a poll tick.
    fn visibility_reconnect(state: AppState) {
        let cb = Closure::<dyn FnMut()>::new(move || {
            let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                return;
            };
            if doc.visibility_state() != web_sys::VisibilityState::Visible {
                return;
            }
            if state.screen.get_untracked() == Screen::Connect {
                return;
            }
            let alive = state
                .session
                .get_untracked()
                .map(|s| s.is_alive())
                .unwrap_or(false);
            if !alive {
                schedule_reconnect(state);
            }
        });
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback(
                "visibilitychange",
                cb.as_ref().unchecked_ref::<js_sys::Function>(),
            );
        }
        cb.forget(); // listener lives for the app's lifetime
    }

    /// Pane list, ~5s, only while the Panes screen is visible.
    fn pane_list_loop(state: AppState, ui: UiState) {
        let epoch = Rc::new(Cell::new(0u64));
        Effect::new(move |_| {
            epoch.set(epoch.get() + 1);
            let token = epoch.get();
            state.generation.track();
            if state.screen.get() != Screen::Panes {
                return;
            }
            let epoch = Rc::clone(&epoch);
            spawn_local(async move {
                loop {
                    if let Some(sess) = live_session(state) {
                        let res = tmux::list_panes(&sess).await;
                        if epoch.get() != token {
                            break;
                        }
                        match res {
                            Ok(panes) => {
                                state.panes.set(panes);
                                ui.pane_status.set(PaneListStatus::Ready);
                            }
                            Err(TmuxError::NoTmuxBinary) => {
                                ui.pane_status.set(PaneListStatus::NoTmuxBinary)
                            }
                            Err(TmuxError::NoServer) => {
                                ui.pane_status.set(PaneListStatus::NoServer)
                            }
                            Err(TmuxError::Ssh(SshError::Disconnected)) => {
                                schedule_reconnect(state)
                            }
                            Err(e) => state.error.set(Some(tmux_err_text(&e))),
                        }
                    }
                    TimeoutFuture::new(PANES_MS).await;
                    if epoch.get() != token {
                        break;
                    }
                }
            });
        });
    }

    /// Screen capture, ~2s, while the detail screen shows the terminal —
    /// either Terminal mode or the Chat no-transcript fallback.
    fn capture_loop(state: AppState, ui: UiState) {
        let epoch = Rc::new(Cell::new(0u64));
        Effect::new(move |_| {
            epoch.set(epoch.get() + 1);
            let token = epoch.get();
            state.generation.track();
            if state.screen.get() != Screen::PaneDetail {
                return;
            }
            let showing_terminal = state.view_mode.get() == ViewMode::Terminal
                || ui.selected_transcript.get().is_none();
            if !showing_terminal {
                return;
            }
            let Some(pane) = state.active_pane.get() else {
                return;
            };
            let epoch = Rc::clone(&epoch);
            spawn_local(async move {
                loop {
                    if let Some(sess) = live_session(state) {
                        let res = tmux::capture_pane(&sess, &pane).await;
                        if epoch.get() != token {
                            break;
                        }
                        match res {
                            Ok(text) => ui.capture.set(Some(text)),
                            Err(TmuxError::Ssh(SshError::Disconnected)) => {
                                schedule_reconnect(state)
                            }
                            Err(e) => state.error.set(Some(tmux_err_text(&e))),
                        }
                    }
                    TimeoutFuture::new(CAPTURE_MS).await;
                    if epoch.get() != token {
                        break;
                    }
                }
            });
        });
    }

    /// One-shot on entering a Claude pane (and after reconnect): list
    /// transcripts, select the newest, reset accumulated chat items.
    fn transcripts_fetch(state: AppState, ui: UiState) {
        Effect::new(move |_| {
            state.generation.track();
            if state.screen.get() != Screen::PaneDetail {
                return;
            }
            let Some(pane) = state.active_pane.get() else {
                return;
            };
            if pane.agent_kind() != AgentKind::Claude {
                return;
            }
            spawn_local(async move {
                let Some(sess) = live_session(state) else {
                    return;
                };
                match claude::find_transcripts(&sess, &pane.path).await {
                    Ok(refs) => {
                        ui.chat_items.set(Vec::new());
                        ui.transcripts.set(refs.clone());
                        ui.selected_transcript
                            .set(refs.first().map(|r| r.path.clone()));
                    }
                    Err(ClaudeError::Ssh(SshError::Disconnected)) => schedule_reconnect(state),
                    Err(e) => state.error.set(Some(claude_err_text(&e))),
                }
            });
        });
    }

    /// Transcript tail, ~2s, while Chat is visible with a selected transcript.
    /// Restarts (fresh end-window) when the selection changes.
    fn tail_loop(state: AppState, ui: UiState) {
        let epoch = Rc::new(Cell::new(0u64));
        Effect::new(move |_| {
            epoch.set(epoch.get() + 1);
            let token = epoch.get();
            state.generation.track();
            if state.screen.get() != Screen::PaneDetail || state.view_mode.get() != ViewMode::Chat {
                return;
            }
            let Some(path) = ui.selected_transcript.get() else {
                return;
            };
            let tref = ui
                .transcripts
                .get_untracked()
                .into_iter()
                .find(|t| t.path == path)
                .unwrap_or(TranscriptRef {
                    path,
                    mtime: 0,
                    size: 0,
                });
            let mut tail = TranscriptTail::new_at_end_window(&tref);
            let epoch = Rc::clone(&epoch);
            spawn_local(async move {
                loop {
                    if let Some(sess) = live_session(state) {
                        let res = tail.poll(&sess).await;
                        if epoch.get() != token {
                            break;
                        }
                        match res {
                            Ok(items) => {
                                if !items.is_empty() {
                                    ui.chat_items.update(|v| v.extend(items));
                                }
                            }
                            Err(ClaudeError::Ssh(SshError::Disconnected)) => {
                                schedule_reconnect(state)
                            }
                            Err(e) => {
                                // NotFound/PermissionDenied won't heal; stop.
                                state.error.set(Some(claude_err_text(&e)));
                                break;
                            }
                        }
                    }
                    TimeoutFuture::new(TAIL_MS).await;
                    if epoch.get() != token {
                        break;
                    }
                }
            });
        });
    }
}
