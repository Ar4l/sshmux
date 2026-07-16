//! Browser WebSocket -> tokio AsyncRead/AsyncWrite adapter (wasm only).
//! Proven by the compile spike; do not change the layering.

use anyhow::{Context as _, Result};
use async_io_stream::IoStream;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use ws_stream_wasm::{WsMeta, WsStreamIo};

/// A browser WebSocket exposed as a tokio `AsyncRead + AsyncWrite` stream,
/// suitable for `russh::client::connect_stream`.
///
/// Layers: WebSocket (browser) -> WsStream (Sink/Stream of WsMessage)
/// -> IoStream (futures-io AsyncRead/AsyncWrite over binary messages)
/// -> Compat (tokio AsyncRead/AsyncWrite).
///
/// ws_stream_wasm wraps all JS handles in `SendWrapper`, so this type is
/// `Send` and satisfies `connect_stream`'s bounds (single-threaded wasm).
pub type WsTransport = Compat<IoStream<WsStreamIo, Vec<u8>>>;

/// Open a WebSocket to the SSH bridge and adapt it to a tokio io stream.
pub async fn connect(url: &str) -> Result<WsTransport> {
    let (_meta, ws) = WsMeta::connect(url, None)
        .await
        // Redact the path: it carries the relay bearer token, and this context
        // is rendered into a visible DOM error banner on connect failure. The
        // deep-link fragment is scrubbed from the address bar for the same
        // reason — don't reintroduce the secret through an error message.
        .with_context(|| format!("websocket connect to {}", redact_token(url)))?;
    Ok(ws.into_io().compat())
}

/// Drop the path component (the bearer token) from a bridge URL, keeping only
/// scheme + host for diagnostics: `wss://host/<token>` -> `wss://host/…`.
fn redact_token(url: &str) -> String {
    if let Some(after_scheme) = url.find("://").map(|i| i + 3) {
        if let Some(rel) = url[after_scheme..].find('/') {
            return format!("{}/…", &url[..after_scheme + rel]);
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::redact_token;

    #[test]
    fn strips_token_path() {
        assert_eq!(
            redact_token("wss://foo-bar.trycloudflare.com/SECRETTOKEN"),
            "wss://foo-bar.trycloudflare.com/…"
        );
        // No path: nothing to redact.
        assert_eq!(redact_token("wss://host"), "wss://host");
    }
}
