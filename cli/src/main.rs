//! sshmux — one-command connect.
//!
//! Starts a loopback, token-gated ws->tcp relay in front of local sshd, exposes
//! it via a cloudflared quick tunnel, and prints a QR + URL that open the
//! sshmux web app with the connection pre-filled. No inbound port is opened;
//! the public URL reaches sshd only via a 128-bit path token.

use sshmux_cli::{hostkey, relay, trust, tunnel};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use clap::{Parser, Subcommand};
use qrcode::render::unicode;
use qrcode::QrCode;
use rand::RngCore;
use sshmux_link::DeepLink;

/// Drive your tmux agents from your phone: run sshmux, scan the QR.
///
/// With no subcommand, sshmux starts the relay + tunnel and prints the QR/URL.
#[derive(Parser, Debug)]
#[command(name = "sshmux", version)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Command>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
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

#[derive(Subcommand, Debug)]
enum Command {
    /// Trust a browser device: append its public key to ~/.ssh/authorized_keys,
    /// scoped to a loopback source (from="127.0.0.1",restrict). Trust is local
    /// and manual — the relay NEVER installs keys, so a leaked URL can't plant one.
    Trust {
        /// Public key line, or "-" to read one line from stdin (preferred —
        /// keeps the key out of shell history). Generate one in the web app.
        key: String,
        /// Short label recorded in the entry comment (sshmux:<label>). [A-Za-z0-9_.-]
        #[arg(long)]
        label: String,
        /// Also allow the IPv6 loopback source (::1).
        #[arg(long)]
        allow_ipv6: bool,
    },
    /// List sshmux-managed trusted keys (label, type, fingerprint).
    Trusted,
    /// Remove sshmux-managed keys with the given label.
    Untrust {
        /// Label passed to `sshmux trust --label`.
        label: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Some(cmd) => run_command(cmd),
        None => run_serve(args.serve).await,
    }
}

/// Dispatch the `trust`/`trusted`/`untrust` management subcommands. These are
/// synchronous and never touch the relay/tunnel.
fn run_command(cmd: Command) -> Result<()> {
    match cmd {
        Command::Trust {
            key,
            label,
            allow_ipv6,
        } => {
            let key_input = if key == "-" {
                read_stdin_line()?
            } else {
                key
            };
            match trust::add(&key_input, &label, allow_ipv6)? {
                trust::AddOutcome::Added => println!(
                    "sshmux: trusted '{label}' — added to ~/.ssh/authorized_keys \
                     (from=\"127.0.0.1\",restrict). Revoke with: sshmux untrust {label}"
                ),
                trust::AddOutcome::AlreadyTrusted => {
                    println!("sshmux: that key is already trusted (no change)")
                }
            }
        }
        Command::Trusted => {
            let entries = trust::list()?;
            if entries.is_empty() {
                println!("sshmux: no sshmux-managed keys in ~/.ssh/authorized_keys");
            } else {
                for e in entries {
                    println!("  {}\t{}\t{}", e.label, e.algorithm, e.fingerprint);
                }
            }
        }
        Command::Untrust { label } => {
            let n = trust::remove(&label)?;
            println!("sshmux: removed {n} key(s) labelled '{label}'");
        }
    }
    Ok(())
}

/// Read a single line from stdin (used for `sshmux trust -`).
fn read_stdin_line() -> Result<String> {
    use std::io::BufRead as _;
    let mut s = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut s)
        .context("reading public key from stdin")?;
    Ok(s)
}

async fn run_serve(args: ServeArgs) -> Result<()> {
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
