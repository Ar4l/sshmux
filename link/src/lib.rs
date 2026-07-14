//! Shared deep-link payload for sshmux.
//!
//! The CLI encodes a [`DeepLink`] into the URL **fragment** (`…/#c=<base64url>`)
//! and the web app decodes it to prefill the connect form. The fragment never
//! leaves the browser, so it is not logged by the app origin — but note `b`
//! embeds the relay bearer token, so the whole URL is sensitive (treat it like
//! a password; the web app clears it from the address bar after reading it).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Non-secret-by-design connection coordinates. Field names are short to keep
/// the encoded fragment compact. Deliberately carries **no** SSH credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepLink {
    /// Bridge WebSocket URL, e.g. `wss://<rand>.trycloudflare.com/<token>`.
    /// Includes the relay bearer token as its path — sensitive.
    pub b: String,
    /// SSH username to prefill.
    pub u: String,
    /// Expected SSH host-key fingerprint (`SHA256:<base64>`), for verified
    /// first-use pinning. `None` if the CLI could not read a host key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
}

impl DeepLink {
    /// Encode to the `c` fragment value: `base64url(json)` (no padding).
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("DeepLink serializes");
        URL_SAFE_NO_PAD.encode(json)
    }

    /// Decode from the `c` fragment value. Returns `None` on any malformed
    /// input (bad base64, bad JSON) so the web app can fall back to a blank
    /// form instead of erroring.
    pub fn decode(c: &str) -> Option<DeepLink> {
        let bytes = URL_SAFE_NO_PAD.decode(c.trim()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_fp() {
        let dl = DeepLink {
            b: "wss://abc-def.trycloudflare.com/Zm9vYmFy".into(),
            u: "aral".into(),
            fp: Some("SHA256:abc123".into()),
        };
        assert_eq!(DeepLink::decode(&dl.encode()), Some(dl));
    }

    #[test]
    fn round_trip_without_fp() {
        let dl = DeepLink {
            b: "wss://h/tok".into(),
            u: "u".into(),
            fp: None,
        };
        let enc = dl.encode();
        // No padding chars, URL-safe alphabet only.
        assert!(!enc.contains('='));
        assert!(!enc.contains('+') && !enc.contains('/'));
        assert_eq!(DeepLink::decode(&enc), Some(dl));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(DeepLink::decode("!!!not base64!!!"), None);
        assert_eq!(DeepLink::decode(&URL_SAFE_NO_PAD.encode(b"not json")), None);
        assert_eq!(DeepLink::decode(""), None);
    }
}
