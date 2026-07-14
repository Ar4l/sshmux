use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

use crate::app::{AppState, ConnStatus, Screen};
use crate::ssh::{Auth, ConnectOpts, SshError, SshSession};
use crate::ui::{ssh_err_text, ErrorBanner};

const STORAGE_KEY: &str = "sshmux.connect";

/// Connection parameters delivered via the URL fragment by `jbcentral mobile`
/// (scanned from a QR): `#v=1&bridge=<wss>&user=<login>&key=<hex ed25519 seed>`.
#[cfg(target_arch = "wasm32")]
struct DeepLink {
    bridge: String,
    user: String,
    key_pem: String,
}

/// Read (and consume) a deep link from the URL fragment. The payload lives in
/// the fragment, not the query, so the key material never reached a web server.
/// On success the fragment is scrubbed so a reload can't silently reconnect and
/// the key isn't left in the address bar / PWA restore state.
#[cfg(target_arch = "wasm32")]
fn take_deeplink() -> Option<DeepLink> {
    use wasm_bindgen::JsValue;

    let win = web_sys::window()?;
    let loc = win.location();
    let hash = loc.hash().ok()?;
    let frag = hash.strip_prefix('#').unwrap_or(&hash);
    if frag.is_empty() {
        return None;
    }
    let params = web_sys::UrlSearchParams::new_with_str(frag).ok()?;
    if params.get("v").as_deref() != Some("1") {
        return None;
    }
    let bridge = params.get("bridge")?;
    let user = params.get("user")?;
    let seed_hex = params.get("key")?;
    let key_pem = seed_hex_to_openssh_pem(&seed_hex)?;

    if let Ok(history) = win.history() {
        let path = loc.pathname().unwrap_or_default();
        let search = loc.search().unwrap_or_default();
        let clean = format!("{path}{search}");
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&clean));
    }

    Some(DeepLink {
        bridge,
        user,
        key_pem,
    })
}

/// Rebuild an unencrypted OpenSSH private key PEM from a 32-byte ed25519 seed
/// (hex). Only the seed travels in the QR (keeps it small); the full key is
/// reconstructed here and fed to the existing private-key auth path.
#[cfg(target_arch = "wasm32")]
fn seed_hex_to_openssh_pem(seed_hex: &str) -> Option<String> {
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::PrivateKey;

    let seed = decode_hex_32(seed_hex)?;
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    let pem = key.to_openssh(LineEnding::LF).ok()?;
    Some(pem.as_str().to_owned())
}

#[cfg(any(target_arch = "wasm32", test))]
fn decode_hex_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (b[2 * i] as char).to_digit(16)?;
        let lo = (b[2 * i + 1] as char).to_digit(16)?;
        *slot = (hi * 16 + lo) as u8;
    }
    Some(out)
}

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

/// Connect screen: bridge URL + username + password/key form.
#[component]
pub fn ConnectScreen() -> impl IntoView {
    let state = expect_context::<AppState>();

    let saved = load_saved();
    let remember = RwSignal::new(saved.is_some());
    let saved = saved.unwrap_or_default();
    let bridge_url = RwSignal::new(saved.bridge_url);
    let username = RwSignal::new(saved.username);
    let use_key = RwSignal::new(saved.use_key);
    let password = RwSignal::new(saved.password);
    let private_key = RwSignal::new(saved.private_key);

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

    // A QR deep link (from `jbcentral mobile`) prefills the form and connects
    // immediately. Its credentials are never persisted unless the user later
    // opts in.
    #[cfg(target_arch = "wasm32")]
    if let Some(dl) = take_deeplink() {
        bridge_url.set(dl.bridge);
        username.set(dl.user);
        use_key.set(true);
        private_key.set(dl.key_pem);
        remember.set(false);
        do_connect(false);
    }

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

#[cfg(test)]
mod tests {
    use super::decode_hex_32;

    #[test]
    fn decodes_valid_seed() {
        let hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let out = decode_hex_32(hex).expect("valid");
        assert_eq!(out[0], 0x00);
        assert_eq!(out[15], 0x0f);
        assert_eq!(out[31], 0x1f);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(decode_hex_32("00").is_none());
        assert!(decode_hex_32(&"ab".repeat(31)).is_none()); // 62 chars
        assert!(decode_hex_32(&"ab".repeat(33)).is_none()); // 66 chars
    }

    #[test]
    fn rejects_non_hex() {
        let bad = "zz".to_string() + &"ab".repeat(31); // 64 chars, first pair invalid
        assert!(decode_hex_32(&bad).is_none());
    }
}
