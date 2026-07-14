pub mod chat;
pub mod connect;
pub mod detail;
pub mod panes;
pub mod terminal;

pub use connect::ConnectScreen;
pub use detail::DetailScreen;
pub use panes::PanesScreen;

use leptos::prelude::*;

use crate::app::AppState;
use crate::claude::{ChatItem, TranscriptRef};
use crate::ssh::SshError;
use crate::tmux::TmuxError;

/// Outcome of the last pane-list fetch; drives the panes-screen empty states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneListStatus {
    Loading,
    Ready,
    NoTmuxBinary,
    NoServer,
}

/// UI-owned shared signals. `App` must `provide_context(UiState::new())`
/// alongside `AppState`; the integrate agent's polling loops write into these
/// (see INTEGRATION_NOTES.md). Components read them and fire one-shot
/// commands (connect, refresh, send-keys) themselves.
#[derive(Clone, Copy)]
pub struct UiState {
    pub pane_status: RwSignal<PaneListStatus>,
    /// Latest `capture-pane -e` output for the active pane.
    pub capture: RwSignal<Option<String>>,
    /// Transcript candidates for the active pane, mtime desc.
    pub transcripts: RwSignal<Vec<TranscriptRef>>,
    /// Path of the transcript being tailed; None => none found.
    pub selected_transcript: RwSignal<Option<String>>,
    /// Chat items accumulated from the transcript tail.
    pub chat_items: RwSignal<Vec<ChatItem>>,
}

impl UiState {
    pub fn new() -> Self {
        UiState {
            pane_status: RwSignal::new(PaneListStatus::Loading),
            capture: RwSignal::new(None),
            transcripts: RwSignal::new(Vec::new()),
            selected_transcript: RwSignal::new(None),
            chat_items: RwSignal::new(Vec::new()),
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn ssh_err_text(e: &SshError) -> String {
    match e {
        SshError::Connect(m) => format!("connection failed: {m}"),
        SshError::Auth(m) => format!("authentication failed: {m}"),
        SshError::HostKeyChanged { .. } => "host key changed".into(),
        SshError::KeyParse(m) => format!("could not parse private key: {m}"),
        SshError::Channel(m) => format!("channel error: {m}"),
        SshError::Disconnected => "disconnected".into(),
    }
}

pub(crate) fn tmux_err_text(e: &TmuxError) -> String {
    match e {
        TmuxError::NoTmuxBinary => "tmux not found on server".into(),
        TmuxError::NoServer => "no tmux server running".into(),
        TmuxError::Ssh(e) => ssh_err_text(e),
        TmuxError::Parse(m) => format!("tmux parse error: {m}"),
    }
}

/// Dismissable banner bound to `AppState.error`.
#[component]
pub fn ErrorBanner() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show when=move || state.error.get().is_some()>
            <div class="error-banner" role="alert">
                <span class="error-text">{move || state.error.get().unwrap_or_default()}</span>
                <button class="error-dismiss" on:click=move |_| state.error.set(None)>
                    "\u{00d7}"
                </button>
            </div>
        </Show>
    }
}
