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
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(SEP).collect();
        if f.len() < 10 {
            return Err(TmuxError::Parse(format!(
                "expected 10 fields, got {}: {line}",
                f.len()
            )));
        }
        let n = f.len();
        // pane_title is the only free-form field that can plausibly contain
        // the separator; re-join anything between the 6 leading and 3
        // trailing fixed fields.
        let title = f[6..n - 3].join(&SEP.to_string());
        let num = |field: &str, raw: &str| {
            raw.parse::<u32>()
                .map_err(|_| TmuxError::Parse(format!("bad {field} {raw:?} in: {line}")))
        };
        panes.push(Pane {
            id: f[0].to_string(),
            session_name: f[1].to_string(),
            window_index: num("window_index", f[2])?,
            window_name: f[3].to_string(),
            command: f[4].to_string(),
            path: f[5].to_string(),
            title,
            width: num("width", f[n - 3])? as u16,
            height: num("height", f[n - 2])? as u16,
            active: f[n - 1] == "1",
        });
    }
    Ok(panes)
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
    fn too_few_fields_is_parse_error() {
        let out = line(&["%1", "s", "0", "w", "zsh", "/tmp", "t", "80", "24"]);
        assert!(matches!(parse_list_panes(&out), Err(TmuxError::Parse(_))));
    }

    #[test]
    fn non_numeric_field_is_parse_error() {
        let out = line(&["%1", "s", "x", "w", "zsh", "/tmp", "t", "80", "24", "1"]);
        assert!(matches!(parse_list_panes(&out), Err(TmuxError::Parse(_))));
        let out = line(&["%1", "s", "0", "w", "zsh", "/tmp", "t", "wide", "24", "1"]);
        assert!(matches!(parse_list_panes(&out), Err(TmuxError::Parse(_))));
    }
}
