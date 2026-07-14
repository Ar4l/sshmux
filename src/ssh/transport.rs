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
        .with_context(|| format!("websocket connect to {url}"))?;
    Ok(ws.into_io().compat())
}
