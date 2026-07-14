use leptos::html::Div;
use leptos::prelude::*;

use crate::claude::ChatItem;

/// Chat view: renders parsed transcript items as message bubbles. All remote
/// content goes through Leptos text nodes — never inner_html. Auto-scrolls to
/// the bottom on new items unless the user scrolled up (then a jump-to-bottom
/// pill appears).
#[component]
pub fn ChatView(#[prop(into)] items: Signal<Vec<ChatItem>>) -> impl IntoView {
    let scroll_ref: NodeRef<Div> = NodeRef::new();
    let stuck = RwSignal::new(true);

    let scroll_to_bottom = move || {
        if let Some(el) = scroll_ref.get_untracked() {
            el.set_scroll_top(el.scroll_height());
        }
    };

    Effect::new(move |_| {
        items.track();
        if stuck.get_untracked() {
            // Defer one tick so the new items are in the DOM before scrolling.
            #[cfg(target_arch = "wasm32")]
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(30).await;
                scroll_to_bottom();
            });
        }
    });

    let on_scroll = move |_| {
        if let Some(el) = scroll_ref.get_untracked() {
            let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 40;
            if stuck.get_untracked() != at_bottom {
                stuck.set(at_bottom);
            }
        }
    };

    view! {
        <div class="chat-wrap">
            <div class="chat" node_ref=scroll_ref on:scroll=on_scroll>
                {move || render_items(&items.get())}
            </div>
            <Show when=move || !stuck.get()>
                <button
                    class="jump-pill"
                    on:click=move |_| {
                        stuck.set(true);
                        scroll_to_bottom();
                    }
                >
                    "\u{2193} latest"
                </button>
            </Show>
        </div>
    }
}

/// Renders items, collapsing runs of `Unknown` into one aggregated dim row.
fn render_items(items: &[ChatItem]) -> Vec<AnyView> {
    let mut out = Vec::new();
    let mut unknowns = 0usize;
    let flush = |out: &mut Vec<AnyView>, unknowns: &mut usize| {
        if *unknowns > 0 {
            let label = if *unknowns == 1 {
                "1 unsupported entry".to_string()
            } else {
                format!("{unknowns} unsupported entries")
            };
            out.push(view! { <div class="chat-unknown">{label}</div> }.into_any());
            *unknowns = 0;
        }
    };
    for item in items {
        match item {
            ChatItem::Unknown { .. } => unknowns += 1,
            other => {
                flush(&mut out, &mut unknowns);
                out.push(render_item(other));
            }
        }
    }
    flush(&mut out, &mut unknowns);
    out
}

fn render_item(item: &ChatItem) -> AnyView {
    match item {
        ChatItem::User { text } => {
            let text = text.clone();
            view! {
                <div class="msg-row right">
                    <div class="bubble bubble-user">{text}</div>
                </div>
            }
            .into_any()
        }
        ChatItem::AssistantText { text } => {
            let text = text.clone();
            view! {
                <div class="msg-row">
                    <div class="bubble bubble-assistant">{text}</div>
                </div>
            }
            .into_any()
        }
        ChatItem::Thinking { text } => {
            let text = text.clone();
            view! {
                <details class="thinking">
                    <summary>"thinking\u{2026}"</summary>
                    <div class="thinking-body">{text}</div>
                </details>
            }
            .into_any()
        }
        ChatItem::ToolUse { name, summary } => {
            let name = name.clone();
            let brief = summary.clone();
            let full = summary.clone();
            view! {
                <details class="tool-use">
                    <summary>
                        <span class="tool-name">{name}</span>
                        <span class="tool-brief">{brief}</span>
                    </summary>
                    <div class="tool-body">{full}</div>
                </details>
            }
            .into_any()
        }
        ChatItem::ToolResult { summary, is_error } => {
            let summary = summary.clone();
            let class = if *is_error {
                "tool-result error"
            } else {
                "tool-result"
            };
            view! { <div class=class>{summary}</div> }.into_any()
        }
        ChatItem::Unknown { .. } => {
            // Aggregated by render_items; kept for match exhaustiveness.
            view! { <div class="chat-unknown">"1 unsupported entry"</div> }.into_any()
        }
    }
}
