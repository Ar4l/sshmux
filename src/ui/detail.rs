use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{AppState, Screen, ViewMode};
use crate::tmux::{self, AgentKind};
use crate::ui::chat::ChatView;
use crate::ui::terminal::TerminalView;
use crate::ui::{tmux_err_text, ErrorBanner, UiState};

const QUICK_KEYS: [(&str, &str); 6] = [
    ("esc", "Escape"),
    ("^C", "C-c"),
    ("tab", "Tab"),
    ("\u{2191}", "Up"),
    ("\u{2193}", "Down"),
    ("\u{23ce}", "Enter"),
];

/// Last path component of a transcript path, minus the .jsonl suffix.
fn transcript_label(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.trim_end_matches(".jsonl").to_string()
}

/// Pane detail screen: Terminal|Chat view toggle + input bar + quick keys.
#[component]
pub fn DetailScreen() -> impl IntoView {
    let state = expect_context::<AppState>();
    let ui = expect_context::<UiState>();

    let input = RwSignal::new(String::new());
    let sending = RwSignal::new(false);

    let is_claude = move || {
        state.active_pane.with(|p| {
            p.as_ref()
                .is_some_and(|p| p.agent_kind() == AgentKind::Claude)
        })
    };

    let go_back = move |_| {
        state.active_pane.set(None);
        state.screen.set(Screen::Panes);
    };

    let send_key = move |key: &'static str| {
        let Some(sess) = state.session.get_untracked() else {
            return;
        };
        let Some(pane_id) = state
            .active_pane
            .with_untracked(|p| p.as_ref().map(|p| p.id.clone()))
        else {
            return;
        };
        spawn_local(async move {
            if let Err(e) = tmux::send_key(&sess, &pane_id, key).await {
                state.error.set(Some(tmux_err_text(&e)));
            }
        });
    };

    let send_input = move || {
        let text = input.get_untracked();
        if text.is_empty() || sending.get_untracked() {
            return;
        }
        let Some(sess) = state.session.get_untracked() else {
            return;
        };
        let Some(pane_id) = state
            .active_pane
            .with_untracked(|p| p.as_ref().map(|p| p.id.clone()))
        else {
            return;
        };
        sending.set(true);
        spawn_local(async move {
            match tmux::send_submit(&sess, &pane_id, &text).await {
                Ok(()) => input.set(String::new()),
                Err(e) => state.error.set(Some(tmux_err_text(&e))),
            }
            sending.set(false);
        });
    };

    let title = move || {
        state.active_pane.with(|p| {
            p.as_ref()
                .map(|p| format!("{}:{} {}", p.session_name, p.window_index, p.window_name))
                .unwrap_or_else(|| "pane".into())
        })
    };

    // Transcript picker: only in Chat mode with >1 candidate.
    let picker = move || {
        if state.view_mode.get() != ViewMode::Chat {
            return None;
        }
        let transcripts = ui.transcripts.get();
        if transcripts.len() < 2 {
            return None;
        }
        let selected = ui.selected_transcript.get();
        let options = transcripts
            .iter()
            .map(|t| {
                let sel = selected.as_deref() == Some(t.path.as_str());
                view! {
                    <option value=t.path.clone() selected=sel>
                        {transcript_label(&t.path)}
                    </option>
                }
            })
            .collect::<Vec<_>>();
        Some(view! {
            <select
                class="transcript-picker"
                aria-label="transcript"
                on:change:target=move |ev| {
                    ui.chat_items.set(Vec::new());
                    ui.selected_transcript.set(Some(ev.target().value()));
                }
            >
                {options}
            </select>
        })
    };

    let capture_text = Signal::derive(move || ui.capture.get().unwrap_or_default());

    let body = move || {
        let Some(pane) = state.active_pane.get() else {
            return view! { <div class="empty-state">"no pane selected"</div> }.into_any();
        };
        let (w, h) = (pane.width, pane.height);
        match state.view_mode.get() {
            ViewMode::Terminal => {
                view! { <TerminalView text=capture_text width=w height=h/> }.into_any()
            }
            ViewMode::Chat => {
                if ui.selected_transcript.get().is_none() {
                    view! {
                        <div class="chat-fallback">
                            <div class="hint-box">
                                "no transcript found \u{2014} showing terminal"
                            </div>
                            <TerminalView text=capture_text width=w height=h/>
                        </div>
                    }
                    .into_any()
                } else {
                    view! { <ChatView items=ui.chat_items/> }.into_any()
                }
            }
        }
    };

    view! {
        <div class="screen screen-detail">
            <header class="topbar">
                <button class="icon-btn back-btn" on:click=go_back aria-label="back to panes">
                    "\u{2039}"
                </button>
                <h1 class="topbar-title detail-title">{title}</h1>
                {picker}
                <div class="segmented view-toggle">
                    <button
                        class="seg-btn"
                        class:selected=move || state.view_mode.get() == ViewMode::Terminal
                        on:click=move |_| state.view_mode.set(ViewMode::Terminal)
                    >
                        "term"
                    </button>
                    <button
                        class="seg-btn"
                        class:selected=move || state.view_mode.get() == ViewMode::Chat
                        disabled=move || !is_claude()
                        on:click=move |_| {
                            if is_claude() {
                                state.view_mode.set(ViewMode::Chat);
                            }
                        }
                    >
                        "chat"
                    </button>
                </div>
            </header>
            <ErrorBanner/>
            <div class="detail-body">{body}</div>
            <div class="bottom-bar detail-input">
                <div class="input-row">
                    <input
                        type="text"
                        class="msg-input"
                        placeholder="send to pane\u{2026}"
                        autocapitalize="off"
                        enterkeyhint="send"
                        prop:value=move || input.get()
                        on:input:target=move |ev| input.set(ev.target().value())
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                send_input();
                            }
                        }
                    />
                    <button
                        class="btn btn-primary send-btn"
                        on:click=move |_| send_input()
                        disabled=move || sending.get()
                        aria-label="send"
                    >
                        <Show when=move || sending.get() fallback=|| "\u{2191}">
                            <span class="spinner" aria-hidden="true"></span>
                        </Show>
                    </button>
                </div>
                <div class="quick-keys">
                    {QUICK_KEYS
                        .into_iter()
                        .map(|(label, key)| {
                            view! {
                                <button class="qk-btn" on:click=move |_| send_key(key)>
                                    {label}
                                </button>
                            }
                        })
                        .collect::<Vec<_>>()}
                </div>
            </div>
        </div>
    }
}
