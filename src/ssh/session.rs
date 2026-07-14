use super::{ConnectOpts, ExecBytes, ExecOutput, SshError, SshSession};

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::SessionInner;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use russh::client;
    use russh::keys::{decode_secret_key, HashAlg, PrivateKeyWithHashAlg};
    use russh::ChannelMsg;

    use super::super::{
        transport, Auth, ConnectOpts, ExecBytes, HostKeyStatus, SshError, SshSession,
    };

    pub struct SessionInner {
        // russh's Handle is not Clone; async Mutex serializes channel opens
        // while letting each channel's I/O proceed lock-free afterwards.
        handle: tokio::sync::Mutex<client::Handle<PinningHandler>>,
        alive: Cell<bool>,
    }

    fn storage_key(bridge_url: &str) -> String {
        format!("sshmux:hostkey:{bridge_url}")
    }

    /// TOFU host-key handler. `Handler` must be `Send`, so the output slot is
    /// Arc<Mutex<..>> rather than the Rc<RefCell<..>> sketched in the contract
    /// (equivalent on single-threaded wasm).
    struct PinningHandler {
        expected: Option<String>,
        trust_changed: bool,
        status: Arc<Mutex<Option<HostKeyStatus>>>,
    }

    impl client::Handler for PinningHandler {
        type Error = russh::Error;

        async fn check_server_key(
            &mut self,
            server_public_key: &russh::keys::PublicKey,
        ) -> Result<bool, Self::Error> {
            // Display of Fingerprint is "SHA256:<base64>".
            let fp = server_public_key.fingerprint(HashAlg::Sha256).to_string();
            let status = match &self.expected {
                None => HostKeyStatus::New(fp),
                Some(known) if *known == fp => HostKeyStatus::Known,
                Some(known) => HostKeyStatus::Changed {
                    old: known.clone(),
                    new: fp,
                },
            };
            let accept = !matches!(status, HostKeyStatus::Changed { .. }) || self.trust_changed;
            *self.status.lock().unwrap() = Some(status);
            Ok(accept)
        }
    }

    pub async fn connect(
        opts: ConnectOpts,
        trust_changed_key: bool,
    ) -> Result<SshSession, SshError> {
        use gloo_storage::{LocalStorage, Storage};

        let stream = transport::connect(&opts.bridge_url)
            .await
            .map_err(|e| SshError::Connect(format!("{e:#}")))?;

        let key = storage_key(&opts.bridge_url);
        // A stored pin wins (normal TOFU). Absent one, seed from the deep-link
        // fingerprint so first use is verified against the QR-delivered key; a
        // server key differing from it becomes HostKeyStatus::Changed and is
        // rejected (fail closed).
        let expected: Option<String> = LocalStorage::get(&key)
            .ok()
            .or_else(|| opts.expected_host_fingerprint.clone());
        let status: Arc<Mutex<Option<HostKeyStatus>>> = Arc::new(Mutex::new(None));
        let handler = PinningHandler {
            expected,
            trust_changed: trust_changed_key,
            status: Arc::clone(&status),
        };

        // Default config: keepalive_interval stays None — russh drives it with
        // tokio::time::sleep, which panics on wasm (no tokio timer runtime).
        let config = Arc::new(client::Config::default());
        let handshake = client::connect_stream(config, stream, handler).await;

        let seen = status.lock().unwrap().clone();
        let mut handle = match handshake {
            Ok(h) => h,
            Err(e) => {
                if let Some(HostKeyStatus::Changed { old, new }) = seen {
                    if !trust_changed_key {
                        return Err(SshError::HostKeyChanged { old, new });
                    }
                }
                return Err(SshError::Connect(format!("ssh handshake: {e}")));
            }
        };
        // Pin (or re-pin) the fingerprint after a successful handshake.
        match seen {
            Some(HostKeyStatus::New(fp)) | Some(HostKeyStatus::Changed { new: fp, .. }) => {
                let _ = LocalStorage::set(&key, fp);
            }
            _ => {}
        }

        let auth_result = match &opts.auth {
            Auth::Password(pw) => handle
                .authenticate_password(opts.username.clone(), pw.clone())
                .await
                .map_err(|e| SshError::Auth(e.to_string()))?,
            Auth::PrivateKey(pem) => {
                let key = decode_secret_key(pem, None).map_err(|e| match e {
                    russh::keys::Error::KeyIsEncrypted => SshError::KeyParse(
                        "encrypted keys not supported — paste an unencrypted key".into(),
                    ),
                    other => SshError::KeyParse(other.to_string()),
                })?;
                let hash_alg = if key.algorithm().is_rsa() {
                    // EXT_INFO may not have arrived yet on wasm (no 1s wait);
                    // fall back to rsa-sha2-256 rather than legacy ssh-rsa.
                    handle
                        .best_supported_rsa_hash()
                        .await
                        .map_err(|e| SshError::Auth(e.to_string()))?
                        .flatten()
                        .or(Some(HashAlg::Sha256))
                } else {
                    None
                };
                handle
                    .authenticate_publickey(
                        opts.username.clone(),
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
                    )
                    .await
                    .map_err(|e| SshError::Auth(e.to_string()))?
            }
        };
        if !auth_result.success() {
            return Err(SshError::Auth("authentication failed".into()));
        }

        Ok(SshSession {
            inner: Rc::new(SessionInner {
                handle: tokio::sync::Mutex::new(handle),
                alive: Cell::new(true),
            }),
        })
    }

    const EXEC_TIMEOUT_MS: u32 = 20_000;

    /// exec raced against a timeout: on a half-open transport (mobile network
    /// blackhole; SSH keepalive is deliberately off on wasm) channel.wait()
    /// would otherwise pend forever while is_alive() keeps reporting true.
    pub async fn exec(session: &SshSession, cmd: &str) -> Result<ExecBytes, SshError> {
        use futures::future::{select, Either};
        let work = exec_inner(session, cmd);
        futures::pin_mut!(work);
        let timeout = gloo_timers::future::TimeoutFuture::new(EXEC_TIMEOUT_MS);
        futures::pin_mut!(timeout);
        match select(work, timeout).await {
            Either::Left((res, _)) => res,
            Either::Right(_) => {
                session.inner.alive.set(false);
                Err(SshError::Disconnected)
            }
        }
    }

    async fn exec_inner(session: &SshSession, cmd: &str) -> Result<ExecBytes, SshError> {
        let inner = &session.inner;
        let channel = {
            let handle = inner.handle.lock().await;
            if handle.is_closed() {
                inner.alive.set(false);
                return Err(SshError::Disconnected);
            }
            match handle.channel_open_session().await {
                Ok(c) => c,
                Err(e) => {
                    if handle.is_closed() {
                        inner.alive.set(false);
                        return Err(SshError::Disconnected);
                    }
                    return Err(SshError::Channel(e.to_string()));
                }
            }
        };
        let mut channel = channel;
        channel
            .exec(true, cmd)
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = None;
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::ExtendedData { ref data, ext: 1 } => stderr.extend_from_slice(data),
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }

        Ok(ExecBytes {
            stdout,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
        })
    }

    pub fn is_alive(session: &SshSession) -> bool {
        let inner = &session.inner;
        if !inner.alive.get() {
            return false;
        }
        match inner.handle.try_lock() {
            Ok(handle) => {
                if handle.is_closed() {
                    inner.alive.set(false);
                    false
                } else {
                    true
                }
            }
            // Lock held by an in-flight exec -> connection was alive moments ago.
            Err(_) => true,
        }
    }
}

impl SshSession {
    /// TOFU pinning: fingerprint stored in localStorage keyed by bridge_url;
    /// New -> store+proceed; Known -> proceed; Changed ->
    /// Err(SshError::HostKeyChanged{..}) unless trust_changed_key (then
    /// re-store).
    pub async fn connect(
        opts: ConnectOpts,
        trust_changed_key: bool,
    ) -> Result<SshSession, SshError> {
        #[cfg(target_arch = "wasm32")]
        {
            wasm::connect(opts, trust_changed_key).await
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (opts, trust_changed_key);
            Err(SshError::Connect("wasm only".into()))
        }
    }

    /// One session channel per call.
    pub async fn exec(&self, cmd: &str) -> Result<ExecOutput, SshError> {
        let out = self.exec_bytes(cmd).await?;
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: out.stderr,
            exit_code: out.exit_code,
        })
    }

    /// Raw-stdout exec for byte-accurate offset tracking (transcript tail).
    pub async fn exec_bytes(&self, cmd: &str) -> Result<ExecBytes, SshError> {
        #[cfg(target_arch = "wasm32")]
        {
            wasm::exec(self, cmd).await
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = cmd;
            Err(SshError::Connect("wasm only".into()))
        }
    }

    pub fn is_alive(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            wasm::is_alive(self)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    }
}
