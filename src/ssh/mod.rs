pub mod session;
#[cfg(target_arch = "wasm32")]
pub mod transport;

#[derive(Clone)]
pub struct ConnectOpts {
    pub bridge_url: String,
    pub username: String,
    pub auth: Auth,
    /// Host-key fingerprint delivered out-of-band (via the scanned deep link).
    /// When set and no pin exists yet, it seeds the expected key so first use is
    /// *verified*, not blind: a mismatch fails closed (HostKeyChanged).
    pub expected_host_fingerprint: Option<String>,
}

#[derive(Clone)]
pub enum Auth {
    Password(String),
    /// Pasted OpenSSH PEM, UNENCRYPTED only (MVP).
    PrivateKey(String),
}

/// String payloads are SHA256 fingerprints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostKeyStatus {
    New(String),
    Known,
    Changed { old: String, new: String },
}

/// Cloneable handle to a live SSH connection (internally Rc<...>; wasm is
/// single-threaded).
#[derive(Clone)]
pub struct SshSession {
    #[cfg(target_arch = "wasm32")]
    pub(crate) inner: std::rc::Rc<session::SessionInner>,
}

pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<u32>,
}

/// Raw-stdout variant of ExecOutput: byte-accurate lengths survive (lossy
/// UTF-8 conversion can change them when output is cut mid-character).
pub struct ExecBytes {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub exit_code: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshError {
    Connect(String),
    Auth(String),
    HostKeyChanged { old: String, new: String },
    KeyParse(String),
    Channel(String),
    Disconnected,
}
