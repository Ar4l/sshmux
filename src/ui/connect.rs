use leptos::html::{Input, Textarea};
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

const DEVICE_KEY: &str = "sshmux.device";

/// A generated device identity, persisted SEPARATELY from `SavedForm` so it is
/// never coupled to — or cleared alongside — the ephemeral bridge URL/token.
/// This is what makes every subsequent scan zero-click, not just the first.
#[derive(Clone, Default, Serialize, Deserialize)]
struct DeviceKey {
    private_key: String,
    username: String,
}

#[cfg(target_arch = "wasm32")]
fn load_device() -> Option<DeviceKey> {
    use gloo_storage::Storage as _;
    gloo_storage::LocalStorage::get(DEVICE_KEY).ok()
}
#[cfg(target_arch = "wasm32")]
fn store_device(d: &DeviceKey) {
    use gloo_storage::Storage as _;
    let _ = gloo_storage::LocalStorage::set(DEVICE_KEY, d);
}
#[cfg(target_arch = "wasm32")]
fn clear_device() {
    use gloo_storage::Storage as _;
    gloo_storage::LocalStorage::delete(DEVICE_KEY);
}
#[cfg(not(target_arch = "wasm32"))]
fn load_device() -> Option<DeviceKey> {
    None
}
#[cfg(not(target_arch = "wasm32"))]
fn store_device(_d: &DeviceKey) {}
#[cfg(not(target_arch = "wasm32"))]
fn clear_device() {}

/// Generate a fresh ed25519 keypair in-browser. Returns `(openssh_private_pem,
/// authorized_keys_public_line)`. The private key never leaves the browser; the
/// public line is shown for the user to paste into `sshmux trust`. Seeded from
/// fresh browser entropy — never from the URL (no secret in the deep link).
#[cfg(target_arch = "wasm32")]
fn generate_device_key(label: &str) -> Result<(String, String), String> {
    use russh::keys::ssh_key::{private::Ed25519Keypair, LineEnding};
    use russh::keys::PrivateKey;

    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| e.to_string())?;
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    let mut public = key.public_key().clone();
    public.set_comment(format!("sshmux-device-{}", sanitize_label(label)));
    let authline = public.to_openssh().map_err(|e| e.to_string())?;
    Ok((pem, authline))
}
#[cfg(not(target_arch = "wasm32"))]
fn generate_device_key(_label: &str) -> Result<(String, String), String> {
    Err("key generation is only available in the browser".into())
}

/// Constrain a label to `[A-Za-z0-9_.-]` (matching the CLI's `--label`), so the
/// suggested `sshmux trust` command is safe to paste. Defaults to "device".
fn sanitize_label(label: &str) -> String {
    let s: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .collect();
    if s.is_empty() {
        "device".to_string()
    } else {
        s
    }
}

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
    // not persisted unless the user opts into "remember". Arriving via a link is
    // what drives the "no-click" flow below (auto-connect / autofocus).
    let deeplink = read_deeplink();
    let arrived_via_link = deeplink.is_some();
    let remember = RwSignal::new(saved.is_some() && deeplink.is_none());
    let saved = saved.unwrap_or_default();
    let (dl_bridge, dl_user, dl_fp) = match deeplink {
        Some(dl) => (Some(dl.b), Some(dl.u), dl.fp),
        None => (None, None, None),
    };
    // A generated device key lives in its own store; it takes precedence and
    // drives the key-auth flow, and is never cleared by the SavedForm/remember
    // path (that path still governs only the bridge/password form).
    let device = load_device();
    let bridge_url = RwSignal::new(dl_bridge.unwrap_or(saved.bridge_url));
    let username = RwSignal::new(
        dl_user
            .or_else(|| {
                device
                    .as_ref()
                    .map(|d| d.username.clone())
                    .filter(|u| !u.is_empty())
            })
            .unwrap_or(saved.username),
    );
    let use_key = RwSignal::new(saved.use_key || device.is_some());
    let password = RwSignal::new(saved.password);
    let private_key = RwSignal::new(
        device
            .as_ref()
            .map(|d| d.private_key.clone())
            .unwrap_or(saved.private_key),
    );
    // Expected host-key fingerprint from the deep link (verified first use).
    let expected_fp = RwSignal::new(dl_fp);

    let connecting = RwSignal::new(false);
    // Set when connect fails with HostKeyChanged: (old, new) fingerprints.
    let key_changed = RwSignal::new(None::<(String, String)>);

    // Focus targets for the "one field, no button" flow: when a link supplies
    // everything but the secret, jump the cursor straight into it.
    let password_ref: NodeRef<Input> = NodeRef::new();
    let key_ref: NodeRef<Textarea> = NodeRef::new();

    // Device-key generation UI state. `device_pubkey` holds the freshly generated
    // authorized_keys line (empty until "generate" is clicked); it is NOT persisted.
    let device_label = RwSignal::new(String::new());
    let device_pubkey = RwSignal::new(String::new());

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

    // No-click flow, only when we arrived via a `#c=` deep link (never on a
    // blank/manual visit). If the selected auth mode already has its secret
    // (e.g. a remembered private key or password), connect automatically — the
    // link supplied everything needed, so no click is required. Otherwise focus
    // the empty secret field so it's "one field, no button": the user types the
    // password and presses Enter (see `submit_on_enter`). Runs once; on failure
    // the error shows as usual and we do NOT retry, so no tight loop.
    if arrived_via_link {
        let has_secret = if use_key.get_untracked() {
            !private_key.get_untracked().trim().is_empty()
        } else {
            !password.get_untracked().is_empty()
        };
        if has_secret {
            // Everything needed is present: connect once, on next tick so the
            // view is mounted (ConnStatus::Connecting drives the spinner).
            Effect::new(move |prev: Option<()>| {
                if prev.is_none() {
                    do_connect(false);
                }
            });
        } else {
            // Secret missing: focus its field. `.get()` tracks the NodeRef, so
            // this re-runs and focuses once the element is actually mounted.
            Effect::new(move |_| {
                let focused = if use_key.get() {
                    key_ref.get().map(|el| el.focus())
                } else {
                    password_ref.get().map(|el| el.focus())
                };
                let _ = focused;
            });
        }
    }

    // Enter in a secret field submits the connect, matching the "no button"
    // intent. Guarded by `connecting` inside `do_connect`.
    let submit_on_enter = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" {
            ev.prevent_default();
            do_connect(false);
        }
    };

    // Generate a throwaway ed25519 key in-browser: the private key is persisted
    // in its own device store; the public line is shown to paste into
    // `sshmux trust` on the target machine. Never touches the URL.
    let on_generate = move |_| match generate_device_key(&device_label.get_untracked()) {
        Ok((pem, authline)) => {
            private_key.set(pem.clone());
            use_key.set(true);
            store_device(&DeviceKey {
                private_key: pem,
                username: username.get_untracked(),
            });
            device_pubkey.set(authline);
            state.error.set(None);
        }
        Err(e) => state.error.set(Some(format!("key generation failed: {e}"))),
    };
    let on_forget = move |_| {
        clear_device();
        private_key.set(String::new());
        device_pubkey.set(String::new());
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
                                    enterkeyhint="go"
                                    node_ref=password_ref
                                    prop:value=move || password.get()
                                    on:input:target=move |ev| password.set(ev.target().value())
                                    on:keydown=submit_on_enter
                                />
                            </label>
                        }
                    }
                >
                    <div class="key-pane">
                        <div class="device-key">
                            <label class="field">
                                <span class="field-label">"device label"</span>
                                <input
                                    type="text"
                                    autocapitalize="off"
                                    spellcheck="false"
                                    placeholder="my-phone"
                                    prop:value=move || device_label.get()
                                    on:input:target=move |ev| device_label.set(ev.target().value())
                                />
                            </label>
                            <div class="device-actions">
                                <button class="btn btn-secondary" type="button" on:click=on_generate>
                                    "generate device key"
                                </button>
                                <Show when=move || !private_key.get().is_empty()>
                                    <button class="btn btn-ghost" type="button" on:click=on_forget>
                                        "forget this device"
                                    </button>
                                </Show>
                            </div>
                            <Show when=move || !device_pubkey.get().is_empty()>
                                <p class="field-hint">
                                    "On the machine you're connecting to, run this once, then scan again:"
                                </p>
                                <textarea
                                    class="key-input"
                                    rows="3"
                                    readonly
                                    prop:value=move || {
                                        format!(
                                            "echo '{}' | sshmux trust - --label {}",
                                            device_pubkey.get(),
                                            sanitize_label(&device_label.get()),
                                        )
                                    }
                                ></textarea>
                            </Show>
                        </div>
                        <label class="field">
                            <span class="field-label">"private key (OpenSSH PEM)"</span>
                            <textarea
                                class="key-input"
                                rows="6"
                                autocapitalize="off"
                                spellcheck="false"
                                node_ref=key_ref
                                placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                                prop:value=move || private_key.get()
                                on:input:target=move |ev| private_key.set(ev.target().value())
                            ></textarea>
                            <span class="field-hint">
                                "generate a device key above, or paste an unencrypted key (prefer a throwaway, server-restricted key)"
                            </span>
                        </label>
                    </div>
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
                <Show when=move || use_key.get() && !private_key.get().is_empty()>
                    <p class="field-hint warn">
                        "a device key is stored unencrypted in this browser. Revoke with "
                        <code>"sshmux untrust <label>"</code>
                        " on the server, or \"forget this device\" above."
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
