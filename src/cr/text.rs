use std::sync::LazyLock;

use regex::Regex;

static COMMAND_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?(local-)?command-[a-z]+>|</?pasted_content[^>]*>").unwrap());

/// One line, every run of whitespace collapsed to a single space: the shape the search haystack and
/// row labels need, since a TAB or newline inside a field would split an fzf row.
pub fn flat(s: &str) -> String {
    let s = COMMAND_TAG.replace_all(s, " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The same cleanup, but the text keeps its own line breaks for the rendered preview pane.
pub fn block(s: &str) -> String {
    let s = COMMAND_TAG.replace_all(s, "");
    s.replace('\r', "").replace('\t', "    ").trim().to_string()
}

/// Long tool output collapses the way Claude Code collapses it, so one file dump cannot evict the
/// conversation from the pane. Anything short enough to read stays whole.
pub fn collapse_output(s: &str) -> Option<String> {
    let text = block(s);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let mut keep: Vec<String> = lines.iter().take(20).map(|l| l.trim_end().to_string()).collect();
    if lines.len() > 20 {
        keep.push(format!("\u{2026} +{} lines", lines.len() - 20));
    }
    Some(keep.join("\n"))
}

/// At most `max` characters, cut on a character boundary.
pub fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Word wrap for one plain line. A single token wider than the pane (a path, a URL) is still cut,
/// or it would drag the column out and fzf would truncate everything after it.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if line.is_empty() {
            line = word.to_string();
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let mut out = Vec::new();
    for l in lines {
        let chars: Vec<char> = l.chars().collect();
        if chars.len() <= width {
            out.push(l);
        } else {
            out.extend(chars.chunks(width).map(|c| c.iter().collect::<String>()));
        }
    }
    out
}

/// Strips control characters other than newline, which would otherwise be typed straight into the
/// terminal by the preview pane.
pub fn printable(s: &str) -> String {
    s.chars().filter(|c| *c == '\n' || !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_collapses_whitespace_and_command_tags() {
        assert_eq!(flat("<command-name>/x</command-name>\n\tfoo  bar"), "/x foo bar");
    }

    #[test]
    fn collapse_keeps_twenty_lines() {
        let s: String = (1..=25).map(|i| format!("l{i}\n")).collect();
        let c = collapse_output(&s).unwrap();
        assert_eq!(c.lines().count(), 21);
        assert!(c.ends_with("+5 lines"));
        assert_eq!(collapse_output("  \n "), None);
    }

    #[test]
    fn wrap_breaks_words_and_cuts_long_tokens() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("h\u{e9}llo", 2), "h\u{e9}");
    }
}
