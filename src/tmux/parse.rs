//! Pure parsers for tmux command output. Unit-test with #[cfg(test)]
//! fixtures; must compile natively for `cargo test`.

use super::{Pane, TmuxError};

/// Field separator used in the list-panes -F format string.
pub const SEP: char = '\u{241E}';

/// Parse `tmux list-panes -a -F` output (one pane per line, SEP-delimited
/// fields in the order of the `Pane` struct).
pub fn parse_list_panes(out: &str) -> Result<Vec<Pane>, TmuxError> {
    let mut panes = Vec::new();
    for line in out.lines() {
        // Free-form fields (session/window names, title, path) can contain
        // SEP or newlines and shift/split a record; skip malformed lines
        // rather than failing the whole list on every poll.
        if let Some(pane) = parse_pane_line(line) {
            panes.push(pane);
        }
    }
    Ok(panes)
}

fn parse_pane_line(line: &str) -> Option<Pane> {
    if line.is_empty() {
        return None;
    }
    let f: Vec<&str> = line.split(SEP).collect();
    if f.len() < 10 {
        return None;
    }
    let n = f.len();
    // pane_title is the only free-form field that can plausibly contain
    // the separator; re-join anything between the 6 leading and 3
    // trailing fixed fields.
    let title = f[6..n - 3].join(&SEP.to_string());
    let num = |raw: &str| raw.parse::<u32>().ok();
    Some(Pane {
        id: f[0].to_string(),
        session_name: f[1].to_string(),
        window_index: num(f[2])?,
        window_name: f[3].to_string(),
        command: f[4].to_string(),
        path: f[5].to_string(),
        title,
        width: num(f[n - 3])? as u16,
        height: num(f[n - 2])? as u16,
        active: f[n - 1] == "1",
    })
}

/// Parse `capture_pane` output: first line is "#{pane_width} #{pane_height}"
/// from display-message, the remainder is the capture text.
pub fn parse_sized_capture(out: &str) -> Result<(u16, u16, String), TmuxError> {
    let (first, rest) = out.split_once('\n').unwrap_or((out, ""));
    let mut it = first.split_whitespace();
    let mut dim = || {
        it.next()
            .and_then(|v| v.parse::<u16>().ok())
            .ok_or_else(|| TmuxError::Parse(format!("bad pane size line: {first:?}")))
    };
    let w = dim()?;
    let h = dim()?;
    Ok((w, h, rest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        fields.join(&SEP.to_string())
    }

    #[test]
    fn sep_is_record_separator_symbol() {
        assert_eq!(SEP, '\u{241E}');
    }

    #[test]
    fn parses_two_panes() {
        let out = format!(
            "{}\n{}\n",
            line(&["%0", "main", "0", "shell", "zsh", "/Users/a", "host", "80", "24", "1"]),
            line(&[
                "%3",
                "work",
                "2",
                "agent",
                "claude",
                "/Users/a/proj",
                "✳ claude",
                "120",
                "40",
                "0"
            ]),
        );
        let panes = parse_list_panes(&out).unwrap();
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].id, "%0");
        assert_eq!(panes[0].session_name, "main");
        assert_eq!(panes[0].window_index, 0);
        assert_eq!(panes[0].width, 80);
        assert_eq!(panes[0].height, 24);
        assert!(panes[0].active);
        assert_eq!(panes[1].command, "claude");
        assert_eq!(panes[1].title, "✳ claude");
        assert_eq!(panes[1].path, "/Users/a/proj");
        assert!(!panes[1].active);
    }

    #[test]
    fn empty_output_is_no_panes() {
        assert!(parse_list_panes("").unwrap().is_empty());
        assert!(parse_list_panes("\n\n").unwrap().is_empty());
    }

    #[test]
    fn sep_inside_title_folds_back_into_title() {
        let out = line(&[
            "%1", "s", "0", "w", "zsh", "/tmp", "a", "b", "80", "24", "0",
        ]);
        let panes = parse_list_panes(&out).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].title, format!("a{SEP}b"));
        assert_eq!(panes[0].width, 80);
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        // too few fields (e.g. a record split by a newline in a name)
        let short = line(&["%1", "s", "0", "w", "zsh", "/tmp", "t", "80", "24"]);
        assert!(parse_list_panes(&short).unwrap().is_empty());
        // non-numeric fixed fields (e.g. SEP inside window_name shifted them)
        let bad_idx = line(&["%1", "s", "x", "w", "zsh", "/tmp", "t", "80", "24", "1"]);
        assert!(parse_list_panes(&bad_idx).unwrap().is_empty());
        // good lines around a bad one still parse
        let good = line(&["%0", "s", "0", "w", "zsh", "/tmp", "t", "80", "24", "1"]);
        let out = format!("{bad_idx}\n{good}\n");
        let panes = parse_list_panes(&out).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].id, "%0");
    }

    #[test]
    fn sized_capture_splits_dimensions_and_text() {
        let (w, h, text) = parse_sized_capture("120 40\nline1\nline2").unwrap();
        assert_eq!((w, h), (120, 40));
        assert_eq!(text, "line1\nline2");
        let (w, h, text) = parse_sized_capture("80 24\n").unwrap();
        assert_eq!((w, h, text.as_str()), (80, 24, ""));
    }

    #[test]
    fn sized_capture_bad_first_line_is_parse_error() {
        assert!(matches!(
            parse_sized_capture("garbage\nx"),
            Err(TmuxError::Parse(_))
        ));
        assert!(matches!(
            parse_sized_capture(""),
            Err(TmuxError::Parse(_))
        ));
    }
}
