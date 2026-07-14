use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

use crate::app::{AppState, ConnStatus, Screen};
use crate::ssh::{Auth, ConnectOpts, SshError, SshSession};
use crate::ui::{ssh_err_text, ErrorBanner};

const STORAGE_KEY: &str = "sshmux.connect";

#[derive(Clone, Default, Serialize, Deserialize)]
struct SavedForm {
    bridge_url: String,
    username: String,
    use_key: bool,
    password: String,
    private_key: String,
}

#[cfg(target_arch = "wasm32")]
fn load_saved() -> Option<SavedForm> {
    use gloo_storage::Storage as _;
    gloo_storage::LocalStorage::get(STORAGE_KEY).ok()
}

#[cfg(target_arch = "wasm32")]
fn store_saved(form: &SavedForm) {
    use gloo_storage::Storage as _;
    let _ = gloo_storage::LocalStorage::set(STORAGE_KEY, form);
}

#[cfg(target_arch = "wasm32")]
fn clear_saved() {
    use gloo_storage::Storage as _;
    gloo_storage::LocalStorage::delete(STORAGE_KEY);
}

#[cfg(not(target_arch = "wasm32"))]
fn load_saved() -> Option<SavedForm> {
    None
}
#[cfg(not(target_arch = "wasm32"))]
fn store_saved(_form: &SavedForm) {}
#[cfg(not(target_arch = "wasm32"))]
fn clear_saved() {}

/// Read a deep link from the URL fragment (`…/#c=<base64url>`), left by the
/// `sshmux` CLI's QR/URL. On success the fragment is cleared from the address
/// bar so the embedded relay token isn't retained in history or leaked via a
/// later Referer.
#[cfg(target_arch = "wasm32")]
fn read_deeplink() -> Option<sshmux_link::DeepLink> {
    let win = web_sys::window()?;
    let hash = win.location().hash().ok()?;
    let hash = hash.strip_prefix('#').unwrap_or(&hash);
    let c = hash.strip_prefix("c=")?;
    let dl = sshmux_link::DeepLink::decode(c)?;

    if let Ok(history) = win.history() {
        let clean = win
            .location()
            .href()
            .ok()
            .and_then(|h| h.split('#').next().map(str::to_string))
            .unwrap_or_default();
        let _ = history.replace_state_with_url(
            &wasm_bindgen::JsValue::NULL,
            "",
            Some(&clean),
        );
    }
    Some(dl)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_deeplink() -> Option<sshmux_link::DeepLink> {
    None
}

/// Connect screen: bridge URL + username + password/key form.
#[component]
pub fn ConnectScreen() -> impl IntoView {
    let state = expect_context::<AppState>();

    let saved = load_saved();
    // A deep link (scanned QR / clicked URL) overrides saved coordinates but is
    // not persisted unless the user opts into "remember".
    let deeplink = read_deeplink();
    let remember = RwSignal::new(saved.is_some() && deeplink.is_none());
    let saved = saved.unwrap_or_default();
    let (dl_bridge, dl_user, dl_fp) = match deeplink {
        Some(dl) => (Some(dl.b), Some(dl.u), dl.fp),
        None => (None, None, None),
    };
    let bridge_url = RwSignal::new(dl_bridge.unwrap_or(saved.bridge_url));
    let username = RwSignal::new(dl_user.unwrap_or(saved.username));
    let use_key = RwSignal::new(saved.use_key);
    let password = RwSignal::new(saved.password);
    let private_key = RwSignal::new(saved.private_key);
    // Expected host-key fingerprint from the deep link (verified first use).
    let expected_fp = RwSignal::new(dl_fp);

    let connecting = RwSignal::new(false);
    // Set when connect fails with HostKeyChanged: (old, new) fingerprints.
    let key_changed = RwSignal::new(None::<(String, String)>);

    let do_connect = move |trust_changed_key: bool| {
        if connecting.get_untracked() {
            return;
        }
        let url = bridge_url.get_untracked().trim().to_string();
        let user = username.get_untracked().trim().to_string();
        if !(url.starts_with("wss://") || url.starts_with("ws://")) {
            state.error.set(Some(
                "bridge URL must start with wss:// (or ws:// for local dev)".into(),
            ));
            return;
        }
        if user.is_empty() {
            state.error.set(Some("username is required".into()));
            return;
        }
        let auth = if use_key.get_untracked() {
            Auth::PrivateKey(private_key.get_untracked())
        } else {
            Auth::Password(password.get_untracked())
        };
        let exp_fp = expected_fp.get_untracked();

        if remember.get_untracked() {
            let key_mode = use_key.get_untracked();
            store_saved(&SavedForm {
                bridge_url: url.clone(),
                username: user.clone(),
                use_key: key_mode,
                // never persist the secret of the unselected auth mode
                password: if key_mode {
                    String::new()
                } else {
                    password.get_untracked()
                },
                private_key: if key_mode {
                    private_key.get_untracked()
                } else {
                    String::new()
                },
            });
        } else {
            clear_saved();
        }

        state.error.set(None);
        key_changed.set(None);
        connecting.set(true);
        state.status.set(ConnStatus::Connecting);

        spawn_local(async move {
            let opts = ConnectOpts {
                bridge_url: url,
                username: user,
                auth,
                expected_host_fingerprint: exp_fp,
            };
            let saved_opts = opts.clone();
            match SshSession::connect(opts, trust_changed_key).await {
                Ok(sess) => {
                    state.connect_opts.set(Some(saved_opts));
                    state.session.set(Some(sess));
                    state.status.set(ConnStatus::Connected);
                    state.generation.update(|g| *g += 1);
                    state.screen.set(Screen::Panes);
                }
                Err(SshError::HostKeyChanged { old, new }) => {
                    state.status.set(ConnStatus::Disconnected);
                    key_changed.set(Some((old, new)));
                }
                Err(e) => {
                    state.status.set(ConnStatus::Disconnected);
                    state.error.set(Some(ssh_err_text(&e)));
                }
            }
            connecting.set(false);
        });
    };

    view! {
        <div class="screen screen-connect">
            <div class="connect-scroll">
                <h1 class="brand">"sshmux"</h1>
                <p class="tagline">"drive your tmux agents over ssh, from your phone"</p>

                <ErrorBanner/>

                <Show when=move || key_changed.get().is_some()>
                    <div class="hostkey-banner" role="alert">
                        <strong>"host key changed!"</strong>
                        <p>"the bridge's SSH host key does not match the pinned one. Only trust it if you expected this (e.g. reinstalled server)."</p>
                        {move || {
                            key_changed
                                .get()
                                .map(|(old, new)| {
                                    view! {
                                        <div class="fingerprints">
                                            <div class="fp-row">
                                                <span class="fp-label">"old"</span>
                                                <code class="fp">{old}</code>
                                            </div>
                                            <div class="fp-row">
                                                <span class="fp-label">"new"</span>
                                                <code class="fp">{new}</code>
                                            </div>
                                        </div>
                                    }
                                })
                        }}
                        <button
                            class="btn btn-danger"
                            on:click=move |_| do_connect(true)
                            disabled=move || connecting.get()
                        >
                            "Trust new key and connect"
                        </button>
                    </div>
                </Show>

                <label class="field">
                    <span class="field-label">"bridge URL"</span>
                    <input
                        type="url"
                        inputmode="url"
                        autocapitalize="off"
                        spellcheck="false"
                        placeholder="wss://ssh-bridge.example.com"
                        prop:value=move || bridge_url.get()
                        on:input:target=move |ev| bridge_url.set(ev.target().value())
                    />
                </label>

                <label class="field">
                    <span class="field-label">"username"</span>
                    <input
                        type="text"
                        autocapitalize="off"
                        spellcheck="false"
                        placeholder="user"
                        prop:value=move || username.get()
                        on:input:target=move |ev| username.set(ev.target().value())
                    />
                </label>

                <div class="segmented auth-toggle" role="tablist">
                    <button
                        class="seg-btn"
                        class:selected=move || !use_key.get()
                        on:click=move |_| use_key.set(false)
                    >
                        "password"
                    </button>
                    <button
                        class="seg-btn"
                        class:selected=move || use_key.get()
                        on:click=move |_| use_key.set(true)
                    >
                        "private key"
                    </button>
                </div>

                <Show
                    when=move || use_key.get()
                    fallback=move || {
                        view! {
                            <label class="field">
                                <span class="field-label">"password"</span>
                                <input
                                    type="password"
                                    prop:value=move || password.get()
                                    on:input:target=move |ev| password.set(ev.target().value())
                                />
                            </label>
                        }
                    }
                >
                    <label class="field">
                        <span class="field-label">"private key (OpenSSH PEM)"</span>
                        <textarea
                            class="key-input"
                            rows="6"
                            autocapitalize="off"
                            spellcheck="false"
                            placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                            prop:value=move || private_key.get()
                            on:input:target=move |ev| private_key.set(ev.target().value())
                        ></textarea>
                        <span class="field-hint">
                            "unencrypted keys only (MVP) — prefer a throwaway key restricted on the server"
                        </span>
                    </label>
                </Show>

                <label class="remember-row">
                    <input
                        type="checkbox"
                        prop:checked=move || remember.get()
                        on:change:target=move |ev| {
                            let checked = ev.target().checked();
                            remember.set(checked);
                            if !checked {
                                // drop stored plaintext secrets immediately,
                                // not only on the next connect
                                clear_saved();
                            }
                        }
                    />
                    <span>"remember on this device"</span>
                </label>
                <Show when=move || remember.get()>
                    <p class="field-hint warn">
                        "credentials are saved unencrypted in this browser's localStorage"
                    </p>
                </Show>
            </div>

            <div class="bottom-bar">
                <button
                    class="btn btn-primary btn-connect"
                    on:click=move |_| do_connect(false)
                    disabled=move || connecting.get()
                >
                    <Show when=move || connecting.get() fallback=|| "connect">
                        <span class="spinner" aria-hidden="true"></span>
                        "connecting\u{2026}"
                    </Show>
                </button>
            </div>
        </div>
    }
}
