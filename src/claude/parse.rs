//! Pure, tolerant JSONL transcript-line parser (serde_json::Value based).
//! Unknown shapes become ChatItem::Unknown, never Err. Must compile natively
//! for `cargo test`.

use super::ChatItem;
use serde_json::Value;

/// Whitespace-collapse and truncate to at most `max` chars (plus ellipsis).
fn compact(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut last_ws = false;
    for c in s.chars() {
        let c = if c.is_whitespace() {
            if last_ws || n == 0 {
                continue;
            }
            last_ws = true;
            ' '
        } else {
            last_ws = false;
            c
        };
        if n == max {
            out.push('…');
            break;
        }
        out.push(c);
        n += 1;
    }
    out.truncate(out.trim_end().len());
    out
}

fn block_type(b: &Value) -> Option<&str> {
    b.get("type").and_then(Value::as_str)
}

/// tool_result content is a string OR an array of {type:"text",...} blocks.
fn summarize_content(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => compact(s, 200),
        Some(Value::Array(blocks)) => {
            let text = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            compact(&text, 200)
        }
        _ => String::new(),
    }
}

fn parse_user(v: &Value) -> Option<ChatItem> {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => Some(ChatItem::User { text: s.clone() }),
        Some(Value::Array(blocks)) => {
            // A user turn carrying tool results renders as the result.
            for b in blocks {
                if block_type(b) == Some("tool_result") {
                    return Some(ChatItem::ToolResult {
                        summary: summarize_content(b.get("content")),
                        is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    });
                }
            }
            let text = blocks
                .iter()
                .filter(|b| block_type(b) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                Some(ChatItem::Unknown {
                    type_name: "user".into(),
                })
            } else {
                Some(ChatItem::User { text })
            }
        }
        _ => Some(ChatItem::Unknown {
            type_name: "user".into(),
        }),
    }
}

fn parse_assistant(v: &Value) -> Option<ChatItem> {
    let Some(blocks) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return Some(ChatItem::Unknown {
            type_name: "assistant".into(),
        });
    };
    // Assistant lines hold one block in practice; take the first we know.
    for b in blocks {
        match block_type(b) {
            Some("text") => {
                let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    return Some(ChatItem::AssistantText {
                        text: text.to_string(),
                    });
                }
            }
            Some("thinking") => {
                return Some(ChatItem::Thinking {
                    text: b
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
            Some("tool_use") => {
                let input = b
                    .get("input")
                    .map(|i| serde_json::to_string(i).unwrap_or_default())
                    .unwrap_or_default();
                return Some(ChatItem::ToolUse {
                    name: b
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    summary: compact(&input, 100),
                });
            }
            _ => {}
        }
    }
    Some(ChatItem::Unknown {
        type_name: "assistant".into(),
    })
}

/// Parse one complete transcript JSONL line into zero-or-one chat items.
///
/// - type=="user": message.content is string OR array (text blocks -> User;
///   tool_result blocks -> ToolResult with content summarized, is_error flag)
/// - type=="assistant": message.content array -> text -> AssistantText;
///   thinking -> Thinking; tool_use -> ToolUse{name, summary: compact first
///   ~100 chars of input JSON}
/// - skip (None) when isMeta==true or isSidechain==true or type in
///   {"summary","file-history-snapshot","queue-operation","attachment","system"}
/// - anything else -> Some(Unknown{type_name})
pub fn parse_line(line: &str) -> Option<ChatItem> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(ChatItem::Unknown {
                type_name: "unparseable".into(),
            })
        }
    };
    if v.get("isMeta").and_then(Value::as_bool) == Some(true)
        || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    match v.get("type").and_then(Value::as_str) {
        Some("summary" | "file-history-snapshot" | "queue-operation" | "attachment" | "system") => {
            None
        }
        Some("user") => parse_user(&v),
        Some("assistant") => parse_assistant(&v),
        Some(other) => Some(ChatItem::Unknown {
            type_name: other.to_string(),
        }),
        None => Some(ChatItem::Unknown {
            type_name: "<missing type>".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_string_content() {
        let line =
            r#"{"type":"user","message":{"role":"user","content":"fix the bug"},"uuid":"u1"}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::User {
                text: "fix the bug".into()
            })
        );
    }

    #[test]
    fn user_array_text_content() {
        let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::User {
                text: "hello\nworld".into()
            })
        );
    }

    #[test]
    fn tool_result_array_content_with_error() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":[{"type":"text","text":"command\nfailed:  exit 1"}]}]}}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::ToolResult {
                summary: "command failed: exit 1".into(),
                is_error: true
            })
        );
    }

    #[test]
    fn tool_result_string_content() {
        let line =
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::ToolResult {
                summary: "ok".into(),
                is_error: false
            })
        );
    }

    #[test]
    fn assistant_text() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Sure, here's the plan."}]}}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::AssistantText {
                text: "Sure, here's the plan.".into()
            })
        );
    }

    #[test]
    fn assistant_thinking() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm let me think","signature":"sig"}]}}"#;
        assert_eq!(
            parse_line(line),
            Some(ChatItem::Thinking {
                text: "hmm let me think".into()
            })
        );
    }

    #[test]
    fn assistant_tool_use_summary_truncated() {
        let long: String = "x".repeat(300);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"{long}"}}}}]}}}}"#
        );
        let Some(ChatItem::ToolUse { name, summary }) = parse_line(&line) else {
            panic!("expected ToolUse");
        };
        assert_eq!(name, "Bash");
        assert!(summary.starts_with(r#"{"command":"#));
        assert!(summary.chars().count() <= 101); // 100 + ellipsis
    }

    #[test]
    fn meta_and_sidechain_skipped() {
        assert_eq!(
            parse_line(r#"{"type":"user","isMeta":true,"message":{"content":"x"}}"#),
            None
        );
        assert_eq!(
            parse_line(r#"{"type":"user","isSidechain":true,"message":{"content":"x"}}"#),
            None
        );
    }

    #[test]
    fn noise_types_skipped() {
        for ty in [
            "summary",
            "file-history-snapshot",
            "queue-operation",
            "attachment",
            "system",
        ] {
            assert_eq!(parse_line(&format!(r#"{{"type":"{ty}"}}"#)), None, "{ty}");
        }
    }

    #[test]
    fn unknown_type_is_unknown() {
        assert_eq!(
            parse_line(r#"{"type":"progress","data":1}"#),
            Some(ChatItem::Unknown {
                type_name: "progress".into()
            })
        );
        assert_eq!(
            parse_line(r#"{"foo":"bar"}"#),
            Some(ChatItem::Unknown {
                type_name: "<missing type>".into()
            })
        );
    }

    #[test]
    fn garbage_and_truncated_lines_never_err() {
        assert_eq!(
            parse_line("not json at all"),
            Some(ChatItem::Unknown {
                type_name: "unparseable".into()
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"assistant","message":{"content":[{"type":"te"#),
            Some(ChatItem::Unknown {
                type_name: "unparseable".into()
            })
        );
        assert_eq!(parse_line("   "), None);
    }

    #[test]
    fn user_weird_shapes_become_unknown() {
        assert_eq!(
            parse_line(r#"{"type":"user","message":{"content":42}}"#),
            Some(ChatItem::Unknown {
                type_name: "user".into()
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"user"}"#),
            Some(ChatItem::Unknown {
                type_name: "user".into()
            })
        );
    }

    #[test]
    fn compact_collapses_and_truncates() {
        assert_eq!(compact("  a\n\n  b\tc  ", 200), "a b c");
        assert_eq!(compact("abcdef", 3), "abc…");
    }
}
