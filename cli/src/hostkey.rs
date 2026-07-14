//! Read the local sshd host key and compute the SHA256 fingerprint in the same
//! `SHA256:<base64-no-pad>` form russh's `Fingerprint` prints, so the web app
//! can pin it (verified first-use) instead of blind TOFU.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Candidate host-key public files, in russh's rough preference order
/// (ed25519 is the modern default and what the browser client will negotiate
/// first). We embed exactly one fingerprint; if the server ends up presenting a
/// different key the web app falls back to the host-key-changed prompt — safe,
/// never a silent accept.
const HOST_KEY_PUBS: &[&str] = &[
    "/etc/ssh/ssh_host_ed25519_key.pub",
    "/etc/ssh/ssh_host_ecdsa_key.pub",
    "/etc/ssh/ssh_host_rsa_key.pub",
];

/// Best-effort local host-key fingerprint. Returns `None` (not an error) if no
/// readable host key is found — the deep link just omits `fp` and the web app
/// keeps normal TOFU.
pub fn local_fingerprint() -> Option<String> {
    for path in HOST_KEY_PUBS {
        if let Ok(fp) = fingerprint_from_pub_file(path) {
            return Some(fp);
        }
    }
    None
}

fn fingerprint_from_pub_file(path: impl AsRef<Path>) -> Result<String> {
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.as_ref().display()))?;
    // Format: "<type> <base64-blob> [comment]"
    let blob_b64 = contents
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed public key line"))?;
    let blob = STANDARD
        .decode(blob_b64)
        .context("base64-decoding host key blob")?;
    Ok(fingerprint_from_blob(&blob))
}

/// SHA256 fingerprint of an SSH wire-format public key blob, formatted exactly
/// like OpenSSH / russh: `SHA256:` + standard base64 of the digest, no padding.
fn fingerprint_from_blob(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    format!("SHA256:{}", STANDARD_NO_PAD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // Empty blob -> SHA256 of empty string, base64 no-pad. Pins the exact
        // formatting the web app compares against.
        assert_eq!(
            fingerprint_from_blob(b""),
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
    }
}
