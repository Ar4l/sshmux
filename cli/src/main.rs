//! sshmux — one-command connect.
//!
//! Starts a loopback, token-gated ws->tcp relay in front of local sshd, exposes
//! it via a cloudflared quick tunnel, and prints a QR + URL that open the
//! sshmux web app with the connection pre-filled. No inbound port is opened;
//! the public URL reaches sshd only via a 128-bit path token.

use sshmux_cli::{hostkey, relay, tunnel};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use clap::Parser;
use qrcode::render::unicode;
use qrcode::QrCode;
use rand::RngCore;
use sshmux_link::DeepLink;

/// Drive your tmux agents from your phone: run sshmux, scan the QR.
#[derive(Parser, Debug)]
#[command(name = "sshmux", version)]
struct Args {
    /// SSH username to pre-fill (default: $USER).
    #[arg(long, env = "USER")]
    user: Option<String>,

    /// Local SSH target the relay forwards to.
    #[arg(long, default_value = "127.0.0.1:22")]
    target: String,

    /// sshmux web app base URL.
    #[arg(long, default_value = "https://aral.cc/sshmux/")]
    app_url: String,

    /// Local relay port (0 = pick a free one).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Max simultaneous relayed connections.
    #[arg(long, default_value_t = 8)]
    max_conns: usize,

    /// Bind the relay to loopback and DO NOT start a tunnel. Nothing is exposed
    /// to the internet — for local testing/dev only. Emits a ws:// URL.
    #[arg(long)]
    local_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let user = match args.user.as_deref().map(str::trim) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => bail!("could not determine username; pass --user"),
    };

    // 128-bit+ URL-safe bearer token. This is a secret: it gates the relay.
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let token = URL_SAFE_NO_PAD.encode(raw);

    // Bind loopback FIRST so the relay is up before anything is exposed.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{}", args.port))?;
    let port = listener.local_addr()?.port();
    eprintln!("sshmux: relay on 127.0.0.1:{port} -> {} (loopback only)", args.target);

    preflight_target(&args.target).await;

    let fp = hostkey::local_fingerprint();
    if fp.is_none() {
        eprintln!("sshmux: warning: no local host key readable; host key will not be pre-pinned");
    }

    // Launch the relay.
    let relay_token = token.clone();
    let target = args.target.clone();
    let max_conns = args.max_conns.max(1);
    tokio::spawn(async move {
        if let Err(e) = relay::serve(listener, relay_token, target, max_conns).await {
            eprintln!("sshmux: relay stopped: {e}");
        }
    });

    // Determine the public (or local) bridge base.
    let mut tunnel = None;
    let wss_base = if args.local_only {
        eprintln!("sshmux: --local-only: NOT starting a tunnel; nothing is exposed to the internet");
        format!("ws://127.0.0.1:{port}")
    } else {
        eprintln!("sshmux: starting cloudflared quick tunnel…");
        let t = tunnel::start(port).await?;
        let base = t.wss_base();
        tunnel = Some(t);
        base
    };

    let bridge_url = format!("{wss_base}/{token}");
    let link = DeepLink { b: bridge_url, u: user, fp };
    let url = format!("{}#c={}", args.app_url, link.encode());

    print_qr_and_url(&url)?;

    // Run until Ctrl-C (or the tunnel dies).
    match tunnel.as_mut() {
        Some(t) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => eprintln!("\nsshmux: shutting down…"),
                s = t.wait() => eprintln!("\nsshmux: cloudflared exited ({s:?}); shutting down…"),
            }
        }
        None => {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("\nsshmux: shutting down…");
        }
    }
    // Dropping `tunnel` kills cloudflared.
    Ok(())
}

/// Warn early (don't fail) if the SSH target isn't accepting connections.
async fn preflight_target(target: &str) {
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(target),
    )
    .await
    {
        Ok(Ok(_)) => {}
        _ => eprintln!(
            "sshmux: warning: cannot reach {target} — is sshd running? \
             (macOS: enable Remote Login in Settings › General › Sharing)"
        ),
    }
}

fn print_qr_and_url(url: &str) -> Result<()> {
    let code = QrCode::new(url.as_bytes()).context("building QR code")?;
    let rendered = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();

    println!("\n{rendered}\n");
    println!("  Scan the QR with your phone, or open:\n");
    println!("  {url}\n");
    println!("  This URL contains a one-time access token — treat it like a password.");
    println!("  Press Ctrl-C to stop and tear everything down.\n");
    Ok(())
}
