use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppState, ConnStatus, Screen, ViewMode};
use crate::tmux::{self, AgentKind, Pane, TmuxError};
use crate::ui::{tmux_err_text, ErrorBanner, PaneListStatus, UiState};

/// Panes grouped by tmux session, preserving order of appearance.
fn group_by_session(panes: &[Pane]) -> Vec<(String, Vec<Pane>)> {
    let mut groups: Vec<(String, Vec<Pane>)> = Vec::new();
    for p in panes {
        match groups.iter_mut().find(|(name, _)| *name == p.session_name) {
            Some((_, v)) => v.push(p.clone()),
            None => groups.push((p.session_name.clone(), vec![p.clone()])),
        }
    }
    groups
}

/// Last path component, for compact display.
fn path_tail(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

#[component]
fn AgentBadge(pane: Pane) -> impl IntoView {
    let (class, label) = match pane.agent_kind() {
        AgentKind::Claude => ("badge badge-claude", "claude".to_string()),
        AgentKind::Codex => ("badge badge-codex", "codex".to_string()),
        AgentKind::Other => ("badge badge-other", pane.command.clone()),
    };
    view! { <span class=class>{label}</span> }
}

/// Pane list screen: tmux panes grouped by session/window.
#[component]
pub fn PanesScreen() -> impl IntoView {
    let state = expect_context::<AppState>();
    let ui = expect_context::<UiState>();
    let refreshing = RwSignal::new(false);

    let refresh = move || {
        if refreshing.get_untracked() {
            return;
        }
        let Some(sess) = state.session.get_untracked() else {
            return;
        };
        refreshing.set(true);
        spawn_local(async move {
            match tmux::list_panes(&sess).await {
                Ok(panes) => {
                    state.panes.set(panes);
                    ui.pane_status.set(PaneListStatus::Ready);
                }
                Err(TmuxError::NoTmuxBinary) => ui.pane_status.set(PaneListStatus::NoTmuxBinary),
                Err(TmuxError::NoServer) => ui.pane_status.set(PaneListStatus::NoServer),
                Err(e) => state.error.set(Some(tmux_err_text(&e))),
            }
            refreshing.set(false);
        });
    };

    let open_pane = move |p: Pane| {
        // Reset detail-view signals so stale content never flashes.
        ui.capture.set(None);
        ui.transcripts.set(Vec::new());
        ui.selected_transcript.set(None);
        ui.chat_items.set(Vec::new());
        let mode = if p.agent_kind() == AgentKind::Claude {
            ViewMode::Chat
        } else {
            ViewMode::Terminal
        };
        state.view_mode.set(mode);
        state.active_pane.set(Some(p));
        state.screen.set(Screen::PaneDetail);
    };

    let list = move || {
        match ui.pane_status.get() {
            PaneListStatus::Loading => {
                return view! { <div class="empty-state">"loading panes\u{2026}"</div> }.into_any();
            }
            PaneListStatus::NoTmuxBinary => {
                return view! { <div class="empty-state">"tmux not found on server"</div> }
                    .into_any();
            }
            PaneListStatus::NoServer => {
                return view! { <div class="empty-state">"no tmux server running"</div> }
                    .into_any();
            }
            PaneListStatus::Ready => {}
        }
        let panes = state.panes.get();
        if panes.is_empty() {
            return view! { <div class="empty-state">"no tmux panes"</div> }.into_any();
        }
        group_by_session(&panes)
            .into_iter()
            .map(|(session, panes)| {
                let rows = panes
                    .into_iter()
                    .map(|p| {
                        let subtitle = if p.title.is_empty() || p.title == p.command {
                            path_tail(&p.path).to_string()
                        } else {
                            p.title.clone()
                        };
                        let heading = format!("{}: {}", p.window_index, p.window_name);
                        let active = p.active;
                        let badge = p.clone();
                        view! {
                            <button class="pane-row" on:click=move |_| open_pane(p.clone())>
                                <span class="pane-active" class:on=active></span>
                                <span class="pane-text">
                                    <span class="pane-window">{heading}</span>
                                    <span class="pane-sub">{subtitle}</span>
                                </span>
                                <AgentBadge pane=badge/>
                            </button>
                        }
                    })
                    .collect::<Vec<_>>();
                view! {
                    <section class="session-group">
                        <h2 class="session-name">{session}</h2>
                        {rows}
                    </section>
                }
            })
            .collect::<Vec<_>>()
            .into_any()
    };

    view! {
        <div class="screen screen-panes">
            <header class="topbar">
                <h1 class="topbar-title">"panes"</h1>
                <span
                    class="auto-dot"
                    class:live=move || state.status.get() == ConnStatus::Connected
                    title="auto-refresh"
                ></span>
                <button
                    class="icon-btn"
                    on:click=move |_| refresh()
                    disabled=move || refreshing.get()
                    aria-label="refresh panes"
                >
                    <Show when=move || refreshing.get() fallback=|| "\u{21bb}">
                        <span class="spinner" aria-hidden="true"></span>
                    </Show>
                </button>
            </header>
            <ErrorBanner/>
            <div class="pane-list">{list}</div>
        </div>
    }
}
