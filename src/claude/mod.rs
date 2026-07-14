pub mod parse;

use crate::ssh::{SshError, SshSession};
use crate::tmux::shell_quote;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptRef {
    pub path: String,
    /// Rank surrogate from `ls -t` order (higher = newer), not a real epoch.
    pub mtime: u64,
    pub size: u64,
}

fn project_slug(pane_path: &str) -> String {
    pane_path
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Escape for interpolation inside a double-quoted shell word (keeps $HOME
/// expansion outside the slug working).
fn dquote_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '"' | '$' | '`' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// slug = pane_path with '/' and '.' replaced by '-'; list
/// ~/.claude/projects/<slug>/*.jsonl sorted by mtime desc (one ls/find
/// command; tolerate dir-missing -> Ok(vec![])). Skip agent-*.jsonl
/// sidechain files for MVP.
pub async fn find_transcripts(
    s: &SshSession,
    pane_path: &str,
) -> Result<Vec<TranscriptRef>, ClaudeError> {
    let slug = dquote_escape(&project_slug(pane_path));
    let cmd = format!("ls -t \"$HOME/.claude/projects/{slug}\"/*.jsonl 2>/dev/null | head -5");
    let out = s.exec(&cmd).await.map_err(ClaudeError::Ssh)?;
    if out.stderr.contains("Permission denied") {
        return Err(ClaudeError::PermissionDenied);
    }
    let paths: Vec<String> = out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|p| !file_name(p).starts_with("agent-"))
        .map(String::from)
        .collect();
    let total = paths.len() as u64;
    let mut refs = Vec::with_capacity(paths.len());
    for (i, path) in paths.into_iter().enumerate() {
        let out = s
            .exec(&format!("wc -c < {}", shell_quote(&path)))
            .await
            .map_err(ClaudeError::Ssh)?;
        if out.stderr.contains("Permission denied") {
            return Err(ClaudeError::PermissionDenied);
        }
        let size = out.stdout.trim().parse().unwrap_or(0);
        refs.push(TranscriptRef {
            path,
            // ls -t order, descending so index 0 (newest) ranks highest.
            mtime: total - i as u64,
            size,
        });
    }
    Ok(refs)
}

pub struct TranscriptTail {
    pub path: String,
    pub offset: u64,
    /// Trailing incomplete JSONL line carried between polls.
    partial: String,
    /// Started mid-file: discard bytes up to the first newline seen.
    drop_first: bool,
}

impl TranscriptTail {
    /// Start at max(0, size - 200_000) to avoid reading huge files.
    pub fn new_at_end_window(r: &TranscriptRef) -> Self {
        let offset = r.size.saturating_sub(200_000);
        TranscriptTail {
            path: r.path.clone(),
            offset,
            partial: String::new(),
            drop_first: offset > 0,
        }
    }

    /// exec: tail -c +<offset+1> '<path>' | head -c 262144 ; advance offset
    /// by bytes read; parse only complete lines, keep trailing partial in
    /// self.partial; if started mid-file, drop the first partial line.
    pub async fn poll(&mut self, s: &SshSession) -> Result<Vec<ChatItem>, ClaudeError> {
        let cmd = format!(
            "tail -c +{} {} | head -c 262144",
            self.offset + 1,
            shell_quote(&self.path)
        );
        let out = s.exec(&cmd).await.map_err(ClaudeError::Ssh)?;
        if out.stderr.contains("Permission denied") {
            return Err(ClaudeError::PermissionDenied);
        }
        if out.stderr.contains("No such file") {
            return Err(ClaudeError::NotFound);
        }
        // Raw byte count of stdout, before any trimming.
        self.offset += out.stdout.len() as u64;
        Ok(self.ingest(&out.stdout))
    }

    /// Pure line-buffering step, separated from poll for native tests.
    fn ingest(&mut self, chunk: &str) -> Vec<ChatItem> {
        if chunk.is_empty() {
            return Vec::new();
        }
        let mut buf = std::mem::take(&mut self.partial);
        buf.push_str(chunk);
        if self.drop_first {
            match buf.find('\n') {
                Some(i) => {
                    buf.drain(..=i);
                    self.drop_first = false;
                }
                None => return Vec::new(), // still inside the first partial line
            }
        }
        let (complete, partial) = match buf.rfind('\n') {
            Some(i) => (&buf[..i], &buf[i + 1..]),
            None => ("", buf.as_str()),
        };
        let items = complete.split('\n').filter_map(parse::parse_line).collect();
        self.partial = partial.to_string();
        items
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatItem {
    User { text: String },
    AssistantText { text: String },
    Thinking { text: String },
    ToolUse { name: String, summary: String },
    ToolResult { summary: String, is_error: bool },
    Unknown { type_name: String },
}

#[derive(Debug)]
pub enum ClaudeError {
    Ssh(SshError),
    NotFound,
    PermissionDenied,
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_window_clamps_small_files() {
        let r = TranscriptRef {
            path: "/t.jsonl".into(),
            mtime: 0,
            size: 100,
        };
        let t = TranscriptTail::new_at_end_window(&r);
        assert_eq!(t.offset, 0);
        assert!(!t.drop_first);
    }

    #[test]
    fn end_window_offsets_large_files() {
        let r = TranscriptRef {
            path: "/t.jsonl".into(),
            mtime: 0,
            size: 1_000_000,
        };
        let t = TranscriptTail::new_at_end_window(&r);
        assert_eq!(t.offset, 800_000);
        assert!(t.drop_first);
    }

    #[test]
    fn slug_replaces_slash_and_dot() {
        assert_eq!(project_slug("/Users/a.b/proj"), "-Users-a-b-proj");
        assert_eq!(project_slug("/tmp"), "-tmp");
    }

    #[test]
    fn dquote_escape_neutralizes_expansion() {
        assert_eq!(dquote_escape(r#"a"$`\b"#), r#"a\"\$\`\\b"#);
        assert_eq!(dquote_escape("-Users-a-proj"), "-Users-a-proj");
    }

    fn tail(size: u64) -> TranscriptTail {
        TranscriptTail::new_at_end_window(&TranscriptRef {
            path: "/t.jsonl".into(),
            mtime: 0,
            size,
        })
    }

    const USER: &str = r#"{"type":"user","message":{"content":"hi"}}"#;

    #[test]
    fn ingest_buffers_partial_lines_across_chunks() {
        let mut t = tail(0);
        let (head, rest) = USER.split_at(20);
        assert!(t.ingest(&format!("{head}")).is_empty());
        let items = t.ingest(&format!("{rest}\n"));
        assert_eq!(items, vec![ChatItem::User { text: "hi".into() }]);
        assert!(t.partial.is_empty());
    }

    #[test]
    fn ingest_keeps_trailing_partial() {
        let mut t = tail(0);
        let items = t.ingest(&format!("{USER}\n{{\"type\":"));
        assert_eq!(items.len(), 1);
        assert_eq!(t.partial, "{\"type\":");
    }

    #[test]
    fn ingest_drops_first_partial_line_when_started_mid_file() {
        let mut t = tail(1_000_000);
        // no newline yet: everything is still the torn first line
        assert!(t.ingest("age\":{\"cont").is_empty());
        let items = t.ingest(&format!("ent\"}}}}\n{USER}\n"));
        assert_eq!(items, vec![ChatItem::User { text: "hi".into() }]);
        assert!(!t.drop_first);
    }

    #[test]
    fn ingest_empty_chunk_is_noop() {
        let mut t = tail(0);
        t.partial = "abc".into();
        assert!(t.ingest("").is_empty());
        assert_eq!(t.partial, "abc");
    }
}
