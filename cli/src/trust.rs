//! `sshmux trust` — pair a browser device by appending its SSH public key to
//! `~/.ssh/authorized_keys`, source-scoped to loopback and revocable.
//!
//! SECURITY — trust is USER-MEDIATED and LOCAL only. The relay
//! (`crate::relay`) is a pure ws→tcp transport and never receives or installs a
//! key. A public key only becomes trusted when a human runs `sshmux trust` on
//! the machine itself and pastes it in. This is deliberate: the relay bearer
//! token lives in the printed URL and is meant to be ephemeral (it dies on
//! Ctrl-C); if a leaked URL could get a key installed into `authorized_keys`,
//! that leak would become a permanent backdoor surviving teardown.
//!
//! Unlike the relay token, a trusted key PERSISTS across Ctrl-C and reboots —
//! that is the point (zero-click reconnect later) but a real departure from the
//! "everything dies on Ctrl-C" model. `sshmux trusted` / `sshmux untrust` are
//! the advertised review/undo.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ssh_key::{Algorithm, HashAlg, PublicKey};

/// Marker prefix written into the key comment so we can list/revoke only the
/// keys sshmux manages, never touching the user's other `authorized_keys` lines.
const MARKER: &str = "sshmux:";

/// Outcome of an `add`.
#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    AlreadyTrusted,
}

/// A listed sshmux-managed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub algorithm: String,
    pub fingerprint: String,
}

// ---- pure validation / composition (no filesystem; unit-testable) ----------

/// Reject empty labels and anything outside `[A-Za-z0-9_.-]`. The label becomes
/// authorized_keys line material (the key comment), so this also guards against
/// newline / option-directive injection via the label.
pub fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() {
        bail!("label must not be empty");
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        bail!("label may only contain letters, digits, '_', '.', '-' (got {label:?})");
    }
    Ok(())
}

/// Strictly validate a pasted public key: single line, parseable OpenSSH public
/// key, on the algorithm allowlist. Returns the parsed key (canonical form) so
/// callers never re-emit the raw user bytes.
///
/// `PublicKey::from_openssh` rejects an options prefix, multiple keys, and
/// private-key PEMs, so this is the core authorized_keys-injection guard.
pub fn validate_pubkey(input: &str) -> Result<PublicKey> {
    let line = input.trim();
    if line.is_empty() {
        bail!("empty public key");
    }
    // Defense in depth: `from_openssh` already refuses multi-line input, but be
    // explicit — an embedded newline is the classic authorized_keys injection.
    if line.contains(['\r', '\n', '\0']) {
        bail!("public key must be a single line (no newlines)");
    }
    let key = PublicKey::from_openssh(line).context("not a valid SSH public key line")?;
    match key.algorithm() {
        Algorithm::Ed25519 => {}
        other => bail!(
            "unsupported key type {:?}; sshmux device keys are ed25519",
            other.as_str()
        ),
    }
    Ok(key)
}

/// Build the hardened authorized_keys line from a validated key + label.
///
/// `from="127.0.0.1"` scopes the key to a loopback source (the relay dials
/// `127.0.0.1:22`, so every browser-originated SSH connection reaches sshd from
/// loopback). `restrict` disables port/agent/X11 forwarding and PTY allocation.
/// NOTE: `restrict` does NOT block command execution, and the web client drives
/// tmux over SSH `exec`, so this key is a full command-execution login as the
/// user, merely source-restricted — not confined to tmux.
pub fn compose_entry(key: &PublicKey, label: &str, allow_ipv6: bool) -> Result<String> {
    validate_label(label)?;
    let mut key = key.clone();
    key.set_comment(format!("{MARKER}{label}"));
    let keyline = key.to_openssh().context("serializing public key")?;
    let from = if allow_ipv6 {
        r#"from="127.0.0.1,::1""#
    } else {
        r#"from="127.0.0.1""#
    };
    Ok(format!("{from},restrict {keyline}"))
}

/// Extract the public key from an authorized_keys line, tolerant of a leading
/// options field. Options precede `<type> <base64> [comment]`; we try to parse
/// the line from each successive token until one parses as a public key. Returns
/// `None` for blanks, comments, and unparseable lines.
fn extract_public_key(line: &str) -> Option<PublicKey> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut rest = line;
    loop {
        if let Ok(pk) = PublicKey::from_openssh(rest) {
            return Some(pk);
        }
        match rest.split_once(char::is_whitespace) {
            Some((_, tail)) => rest = tail.trim_start(),
            None => return None,
        }
    }
}

/// The sshmux label of a parsed key, if it is one we manage.
fn sshmux_label(pk: &PublicKey) -> Option<&str> {
    pk.comment().as_str().ok()?.strip_prefix(MARKER)
}

// ---- filesystem operations against an explicit path (integration-testable) --

/// Append `key_input` (a public key line) to the given authorized_keys file,
/// creating it `0600` if missing. Deduplicates by key material.
pub fn add_to_file(
    path: &Path,
    key_input: &str,
    label: &str,
    allow_ipv6: bool,
) -> Result<AddOutcome> {
    let key = validate_pubkey(key_input)?;
    let entry = compose_entry(&key, label, allow_ipv6)?;

    let existing = fs::read_to_string(path).unwrap_or_default();
    for line in existing.lines() {
        if let Some(pk) = extract_public_key(line) {
            if pk.key_data() == key.key_data() {
                return Ok(AddOutcome::AlreadyTrusted);
            }
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    // Don't concatenate onto a keyless final line.
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(entry.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(AddOutcome::Added)
}

/// List sshmux-managed entries in the given file.
pub fn list_file(path: &Path) -> Result<Vec<Entry>> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    for line in content.lines() {
        if let Some(pk) = extract_public_key(line) {
            if let Some(label) = sshmux_label(&pk) {
                out.push(Entry {
                    label: label.to_string(),
                    algorithm: pk.algorithm().as_str().to_string(),
                    fingerprint: pk.fingerprint(HashAlg::Sha256).to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Remove sshmux-managed entries with the given label from the file. Rewrites
/// atomically (temp + rename) preserving `0600` and all non-matching lines.
/// Returns the number of entries removed.
pub fn remove_from_file(path: &Path, label: &str) -> Result<usize> {
    validate_label(label)?;
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(0),
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0usize;
    for line in content.lines() {
        let is_match = extract_public_key(line)
            .as_ref()
            .and_then(sshmux_label)
            .map(|l| l == label)
            .unwrap_or(false);
        if is_match {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    if removed > 0 {
        let mut body = kept.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        let tmp = path.with_file_name(format!(
            "{}.sshmux.tmp",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("authorized_keys")
        ));
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("writing {}", tmp.display()))?;
            f.write_all(body.as_bytes())?;
        }
        fs::rename(&tmp, path)
            .with_context(|| format!("replacing {}", path.display()))?;
    }
    Ok(removed)
}

// ---- $HOME-resolving public entry points -----------------------------------

fn authorized_keys_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("$HOME is not set; cannot locate ~/.ssh/authorized_keys"))?;
    Ok(PathBuf::from(home).join(".ssh").join("authorized_keys"))
}

/// Ensure `~/.ssh` exists (created `0700`); warn but don't widen if it already
/// exists with looser permissions (sshd rejects keys under group/other-accessible dirs).
fn ensure_ssh_dir(ak_path: &Path) -> Result<()> {
    let dir = ak_path
        .parent()
        .ok_or_else(|| anyhow!("authorized_keys path has no parent"))?;
    if dir.exists() {
        if let Ok(meta) = fs::metadata(dir) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "sshmux: warning: {} has mode {:o} (want 700); sshd may ignore keys",
                    dir.display(),
                    mode
                );
            }
        }
    } else {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    Ok(())
}

pub fn add(key_input: &str, label: &str, allow_ipv6: bool) -> Result<AddOutcome> {
    validate_label(label)?;
    let path = authorized_keys_path()?;
    ensure_ssh_dir(&path)?;
    add_to_file(&path, key_input, label, allow_ipv6)
}

pub fn list() -> Result<Vec<Entry>> {
    list_file(&authorized_keys_path()?)
}

pub fn remove(label: &str) -> Result<usize> {
    remove_from_file(&authorized_keys_path()?, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMd8cW6R+pw+ZQiItqbveNlulKVoiAYJ4qBo2PDAynjX test";

    #[test]
    fn label_charset_enforced() {
        assert!(validate_label("my-phone_1.2").is_ok());
        assert!(validate_label("").is_err());
        assert!(validate_label("has space").is_err());
        assert!(validate_label("has\nnewline").is_err());
        assert!(validate_label("quote\"inject").is_err());
    }

    #[test]
    fn rejects_newline_and_multiline_keys() {
        assert!(validate_pubkey(&format!("{ED25519}\ncommand=\"rm -rf /\" {ED25519}")).is_err());
        assert!(validate_pubkey("not a key").is_err());
        assert!(validate_pubkey("").is_err());
    }

    #[test]
    fn rejects_options_prefix_as_key() {
        // An attacker-supplied "key" that smuggles an options directive must not parse.
        assert!(validate_pubkey(&format!("command=\"x\" {ED25519}")).is_err());
    }

    #[test]
    fn composes_scoped_entry() {
        let key = validate_pubkey(ED25519).unwrap();
        let line = compose_entry(&key, "phone", false).unwrap();
        assert!(line.starts_with(r#"from="127.0.0.1",restrict ssh-ed25519 "#));
        assert!(line.ends_with(" sshmux:phone"));
        assert!(!line.contains('\n'));

        let line6 = compose_entry(&key, "phone", true).unwrap();
        assert!(line6.starts_with(r#"from="127.0.0.1,::1",restrict "#));
    }
}
