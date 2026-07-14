//! cloudflared quick-tunnel backend.
//!
//! Spawns `cloudflared tunnel --url http://127.0.0.1:<port>` (outbound-only,
//! needs no Cloudflare account) and scrapes the `*.trycloudflare.com` hostname
//! from its stderr. The child is killed on drop and on Ctrl-C.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// A running tunnel. Dropping it kills cloudflared.
pub struct Tunnel {
    child: Child,
    /// Public HTTPS origin, e.g. `https://foo-bar.trycloudflare.com`.
    pub public_url: String,
    /// Keeps draining cloudflared's stderr for the tunnel's lifetime. Without
    /// this the pipe fills and cloudflared dies with EPIPE (SIGPIPE) shortly
    /// after we scrape the URL.
    _drain: tokio::task::JoinHandle<()>,
}

impl Tunnel {
    /// The `wss://` base to hand the browser (TLS terminates at Cloudflare).
    pub fn wss_base(&self) -> String {
        self.public_url.replacen("https://", "wss://", 1)
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // Best-effort synchronous kill so no orphaned cloudflared lingers.
        let _ = self.child.start_kill();
    }
}

const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Start a cloudflared quick tunnel to the given local port.
pub async fn start(local_port: u16) -> Result<Tunnel> {
    which_cloudflared()?;

    let mut child = Command::new("cloudflared")
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{local_port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning cloudflared (is it installed? `brew install cloudflared`)")?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("cloudflared stderr not captured"))?;

    // Drain stderr for the whole tunnel lifetime; report the first URL found via
    // a oneshot. Continuing to read past the URL is what keeps cloudflared from
    // getting EPIPE on stderr and dying.
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let drain = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tx = Some(tx);
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(t) = tx.take() {
                match extract_trycloudflare_url(&line) {
                    Some(url) => {
                        let _ = t.send(url);
                    }
                    None => tx = Some(t),
                }
            }
        }
    });

    let public_url = match tokio::time::timeout(READY_TIMEOUT, rx).await {
        Ok(Ok(url)) => url,
        Ok(Err(_)) => bail!("cloudflared exited before reporting a trycloudflare.com URL"),
        Err(_) => bail!("timed out waiting for cloudflared to report a URL"),
    };

    Ok(Tunnel {
        child,
        public_url,
        _drain: drain,
    })
}

/// Pull `https://<sub>.trycloudflare.com` out of a log line, tolerant of the
/// surrounding banner box characters.
fn extract_trycloudflare_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '│')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/').to_string();
    if url.ends_with(".trycloudflare.com") {
        Some(url)
    } else {
        None
    }
}

fn which_cloudflared() -> Result<()> {
    // Cheap PATH probe so we fail with a friendly message before spawning.
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if dir.join("cloudflared").exists() {
            return Ok(());
        }
    }
    bail!("cloudflared not found on PATH — install it (`brew install cloudflared`)")
}

#[cfg(test)]
mod tests {
    use super::extract_trycloudflare_url;

    #[test]
    fn parses_banner_line() {
        let line = "2024-01-01T00:00:00Z INF |  https://happy-cat-1234.trycloudflare.com  |";
        assert_eq!(
            extract_trycloudflare_url(line).as_deref(),
            Some("https://happy-cat-1234.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_other_urls() {
        assert_eq!(
            extract_trycloudflare_url("visit https://developers.cloudflare.com/foo"),
            None
        );
    }

    #[test]
    fn plain_line() {
        assert_eq!(
            extract_trycloudflare_url("https://abc.trycloudflare.com").as_deref(),
            Some("https://abc.trycloudflare.com")
        );
    }
}
