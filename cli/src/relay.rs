//! Token-gated WebSocket -> TCP relay.
//!
//! Security invariant: a TCP connection to the SSH target is opened **only
//! after** a WebSocket upgrade succeeds, and the upgrade succeeds **only** when
//! the request path equals `/<token>` (constant-time compare). A wrong or
//! missing token yields HTTP 404 and **no** dial to the target. The listener
//! binds loopback only; the tunnel is the sole reachable front end.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Accept loop. Runs until the process exits. `token` is the bearer secret; it
/// is never logged.
pub async fn serve(
    listener: TcpListener,
    token: String,
    target: String,
    max_conns: usize,
) -> Result<()> {
    let expected_path = format!("/{token}");
    let sem = Arc::new(Semaphore::new(max_conns));

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("relay: accept error: {e}");
                continue;
            }
        };
        let expected_path = expected_path.clone();
        let target = target.clone();
        let sem = Arc::clone(&sem);

        tokio::spawn(async move {
            // Bound concurrency; drop the connection if we're at capacity.
            let permit = match sem.try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("relay: at capacity, refusing connection");
                    return;
                }
            };
            if let Err(e) = handle(stream, &expected_path, &target).await {
                // Never interpolate the token; `e` may include the request path
                // on a rejected upgrade, so keep messages generic.
                eprintln!("relay: connection from {peer} ended: {e}");
            }
            drop(permit);
        });
    }
}

async fn handle(stream: TcpStream, expected_path: &str, target: &str) -> Result<()> {
    // Gate on the token DURING the upgrade, before touching the SSH target.
    let mut path_ok = false;
    let callback = |req: &Request, resp: Response| -> std::result::Result<Response, ErrorResponse> {
        if ct_eq(req.uri().path().as_bytes(), expected_path.as_bytes()) {
            path_ok = true;
            Ok(resp)
        } else {
            // Denied: log without revealing the expected token.
            eprintln!("relay: rejected upgrade with bad token path");
            let err = ErrorResponse::new(Some("not found".to_string()));
            let (mut parts, body) = err.into_parts();
            parts.status = StatusCode::NOT_FOUND;
            Err(ErrorResponse::from_parts(parts, body))
        }
    };

    let ws = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_hdr_async(stream, callback),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => return Err(e.into()), // includes rejected-token case
        Err(_) => anyhow::bail!("handshake timed out"),
    };
    debug_assert!(path_ok, "upgrade only succeeds when the token path matched");

    // Only now do we dial the SSH target.
    let tcp = TcpStream::connect(target).await?;
    pipe(ws, tcp).await
}

/// Bidirectional copy: WS binary frames <-> raw TCP bytes.
async fn pipe(
    ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    tcp: TcpStream,
) -> Result<()> {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();

    // WS -> TCP
    let ws_to_tcp = async {
        while let Some(msg) = ws_rx.next().await {
            match msg? {
                Message::Binary(data) => tcp_wr.write_all(&data).await?,
                Message::Text(t) => tcp_wr.write_all(t.as_bytes()).await?,
                Message::Close(_) => break,
                // Ping/Pong handled by tungstenite internally on the next read.
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    // TCP -> WS
    let tcp_to_ws = async {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = tcp_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_tx.send(Message::Binary(buf[..n].to_vec().into())).await?;
        }
        ws_tx.send(Message::Close(None)).await.ok();
        Ok::<(), anyhow::Error>(())
    };

    // First half to finish tears the connection down.
    tokio::select! {
        r = ws_to_tcp => r?,
        r = tcp_to_ws => r?,
    }
    Ok(())
}

/// Constant-time byte compare (length is not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_matches_only_equal() {
        assert!(ct_eq(b"/abc123", b"/abc123"));
        assert!(!ct_eq(b"/abc123", b"/abc124"));
        assert!(!ct_eq(b"/abc", b"/abc123"));
        assert!(!ct_eq(b"/", b"/abc123"));
    }
}
