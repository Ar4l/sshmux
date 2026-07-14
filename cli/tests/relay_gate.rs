//! Security invariant: the relay never opens a connection to the SSH target
//! unless the WebSocket upgrade path carries the exact token. A wrong/missing
//! token must yield an HTTP error and ZERO dials to the target.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "correct-horse-battery-staple-token";

/// A mock SSH target that counts accepted TCP connections and greets each with
/// some bytes (so we can also prove piping works on the happy path).
async fn spawn_mock_target() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            c.fetch_add(1, Ordering::SeqCst);
            let _ = sock.write_all(b"SSH-2.0-mock").await;
            // Hold the socket open briefly so the relay pipe stays alive.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    (addr, count)
}

async fn spawn_relay(target: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(sshmux_cli::relay::serve(
        listener,
        TOKEN.to_string(),
        target,
        8,
    ));
    port
}

#[tokio::test]
async fn wrong_and_missing_tokens_never_dial_target() {
    let (target_addr, dials) = spawn_mock_target().await;
    let port = spawn_relay(target_addr).await;

    // Root path, wrong token, and a prefix of the real token: all must fail.
    for bad in ["/", "/nope", "/wrong-token", &format!("/{}x", TOKEN), &format!("/{}", &TOKEN[..10])] {
        let url = format!("ws://127.0.0.1:{port}{bad}");
        let res = connect_async(&url).await;
        assert!(res.is_err(), "bad path {bad:?} should be rejected, got Ok");
    }

    // Give any (erroneous) dial a chance to land before asserting.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        dials.load(Ordering::SeqCst),
        0,
        "target must NOT be dialed for any bad token"
    );
}

#[tokio::test]
async fn correct_token_connects_and_pipes() {
    let (target_addr, dials) = spawn_mock_target().await;
    let port = spawn_relay(target_addr).await;

    let url = format!("ws://127.0.0.1:{port}/{TOKEN}");
    let (mut ws, _resp) = connect_async(&url).await.expect("correct token must connect");

    // The mock target greets on connect; the relay must pipe those bytes to us.
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for piped bytes")
        .expect("stream ended")
        .expect("ws error");
    let bytes = match msg {
        Message::Binary(b) => b.to_vec(),
        other => panic!("expected binary, got {other:?}"),
    };
    assert_eq!(&bytes, b"SSH-2.0-mock");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "exactly one dial on happy path");
}
