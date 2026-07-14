pub mod parse;

use crate::ssh::{ExecOutput, SshError, SshSession};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    /// tmux pane id, e.g. "%3".
    pub id: String,
    pub session_name: String,
    pub window_index: u32,
    pub window_name: String,
    pub command: String,
    pub path: String,
    pub title: String,
    pub width: u16,
    pub height: u16,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
    Other,
}

impl Pane {
    pub fn agent_kind(&self) -> AgentKind {
        match self.command.as_str() {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            _ => AgentKind::Other,
        }
    }
}

#[derive(Debug)]
pub enum TmuxError {
    /// exec exit code 127.
    NoTmuxBinary,
    /// "no server running" in stderr.
    NoServer,
    Ssh(SshError),
    Parse(String),
}

async fn run_tmux(s: &SshSession, cmd: &str) -> Result<ExecOutput, TmuxError> {
    let out = s.exec(cmd).await.map_err(TmuxError::Ssh)?;
    if out.exit_code == Some(127) {
        return Err(TmuxError::NoTmuxBinary);
    }
    if out.stderr.contains("no server") {
        return Err(TmuxError::NoServer);
    }
    match out.exit_code {
        Some(0) | None => Ok(out),
        Some(_) => Err(TmuxError::Parse(out.stderr.trim().to_string())),
    }
}

/// tmux list-panes -a -F with U+241E field separator; pane IDs, not names.
pub async fn list_panes(s: &SshSession) -> Result<Vec<Pane>, TmuxError> {
    let fmt = [
        "#{pane_id}",
        "#{session_name}",
        "#{window_index}",
        "#{window_name}",
        "#{pane_current_command}",
        "#{pane_current_path}",
        "#{pane_title}",
        "#{pane_width}",
        "#{pane_height}",
        "#{pane_active}",
    ]
    .join(&parse::SEP.to_string());
    let cmd = format!("tmux list-panes -a -F {}", shell_quote(&fmt));
    let out = run_tmux(s, &cmd).await?;
    parse::parse_list_panes(&out.stdout)
}

/// capture-pane -e -p -t '%N' (visible screen).
pub async fn capture_pane(s: &SshSession, pane: &Pane) -> Result<String, TmuxError> {
    let cmd = format!("tmux capture-pane -e -p -t {}", shell_quote(&pane.id));
    Ok(run_tmux(s, &cmd).await?.stdout)
}

/// send-keys -l -- (single-quote shell escaping).
pub async fn send_literal_text(s: &SshSession, pane_id: &str, text: &str) -> Result<(), TmuxError> {
    let cmd = format!(
        "tmux send-keys -t {} -l -- {}",
        shell_quote(pane_id),
        shell_quote(text)
    );
    run_tmux(s, &cmd).await.map(|_| ())
}

/// NO -l; key names: Escape, C-c, Tab, Up, Down, Enter.
pub async fn send_key(s: &SshSession, pane_id: &str, key: &str) -> Result<(), TmuxError> {
    let cmd = format!(
        "tmux send-keys -t {} {}",
        shell_quote(pane_id),
        shell_quote(key)
    );
    run_tmux(s, &cmd).await.map(|_| ())
}

/// Literal text, ~150ms TimeoutFuture, then Enter key (Claude Code TUI
/// swallows combined Enter).
pub async fn send_submit(s: &SshSession, pane_id: &str, text: &str) -> Result<(), TmuxError> {
    send_literal_text(s, pane_id, text).await?;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(150).await;
    send_key(s, pane_id, "Enter").await
}

/// POSIX single-quote shell escaping: wrap in ', turning each embedded '
/// into '\'' .
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_quote_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("''"), r"''\'''\'''");
    }

    #[test]
    fn shell_quote_metachars_inert() {
        assert_eq!(
            shell_quote("$(rm -rf /) `x` \"y\""),
            "'$(rm -rf /) `x` \"y\"'"
        );
    }

    #[test]
    fn shell_quote_newlines_and_unicode() {
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        assert_eq!(shell_quote("héllo ✳"), "'héllo ✳'");
    }

    #[test]
    fn agent_kind_from_command() {
        let mut p = Pane {
            id: "%1".into(),
            session_name: "main".into(),
            window_index: 0,
            window_name: "w".into(),
            command: "claude".into(),
            path: "/tmp".into(),
            title: "t".into(),
            width: 80,
            height: 24,
            active: true,
        };
        assert_eq!(p.agent_kind(), AgentKind::Claude);
        p.command = "codex".into();
        assert_eq!(p.agent_kind(), AgentKind::Codex);
        p.command = "zsh".into();
        assert_eq!(p.agent_kind(), AgentKind::Other);
    }
}
