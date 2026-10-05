use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::parse::{Turn, Who};
use super::text::{printable, truncate, wrap};

const ESC: &str = "\x1b";
const ACCENT: &str = "\x1b[38;5;173m";
const MUTED: &str = "\x1b[38;5;245m";
const DIM: &str = "\x1b[38;5;240m";
const RESET: &str = "\x1b[0m";
const MAX_LINES: usize = 400;

static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());
static SLOT: LazyLock<Regex> = LazyLock::new(|| Regex::new("\x02(\\d+)\x03").unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)\s]+)\)").unwrap());
static BOLD_ITALIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*\*([^*]+)\*\*\*").unwrap());
static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
static ITALIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[^\w*])\*([^*\s][^*]*)\*($|[^\w*])").unwrap());
static UNDERSCORE_BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[^\w_])__([^_]+)__($|[^\w_])").unwrap());
static RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-=_*]{3,}\s*$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)#{1,6}\s+(.+)$").unwrap());
static QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)>\s?(.*)$").unwrap());
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)[-*+]\s+(.*)$").unwrap());
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)(\d+)[.)]\s+(.*)$").unwrap());
static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(```|~~~)").unwrap());
static CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^([^(]+)\((.*)\)$").unwrap());

/// Inline markdown rendered as SGR rather than stripped. Code spans are parked first so the emphasis
/// rules cannot chew through the one span whose contents are meant to stay literal.
fn inline(text: &str) -> String {
    let mut slots = Vec::new();
    let t = CODE.replace_all(text, |c: &Captures| {
        slots.push(format!("{ESC}[38;5;180m{}{ESC}[39m", &c[1]));
        format!("\x02{}\x03", slots.len() - 1)
    });
    let t = LINK.replace_all(&t, format!("{ESC}[4m$1{ESC}[24m {DIM}$2{ESC}[39m").as_str()).into_owned();
    let t = BOLD_ITALIC.replace_all(&t, format!("{ESC}[1;3m$1{ESC}[23;22m").as_str()).into_owned();
    let t = BOLD.replace_all(&t, format!("{ESC}[1m$1{ESC}[22m").as_str()).into_owned();
    let t = ITALIC.replace_all(&t, format!("$1{ESC}[3m$2{ESC}[23m$3").as_str()).into_owned();
    let t = UNDERSCORE_BOLD.replace_all(&t, format!("$1{ESC}[1m$2{ESC}[22m$3").as_str()).into_owned();
    SLOT.replace_all(&t, |c: &Captures| slots[c[1].parse::<usize>().unwrap()].clone()).into_owned()
}

/// Line-level markdown: a heading or list marker becomes the structure it was describing.
fn markdown(line: &str) -> String {
    let l = line.trim_end();
    if RULE.is_match(l) {
        return format!("{DIM}{}{ESC}[39m", "\u{2500}".repeat(12));
    }
    if let Some(c) = HEADING.captures(l) {
        return format!("{}{ESC}[1;97m{}{RESET}", &c[1], inline(&c[2]));
    }
    if let Some(c) = QUOTE.captures(l) {
        return format!("{}{DIM}\u{2502}{ESC}[39m {ESC}[2m{}{ESC}[22m", &c[1], inline(&c[2]));
    }
    if let Some(c) = BULLET.captures(l) {
        return format!("{}{MUTED}\u{2022}{ESC}[39m {}", &c[1], inline(&c[2]));
    }
    if let Some(c) = NUMBERED.captures(l) {
        return format!("{}{MUTED}{}.{ESC}[39m {}", &c[1], &c[2], inline(&c[3]));
    }
    inline(l)
}

/// One turn laid out the way its own CLI lays it out: Claude's orange bullet with the reply running
/// from it, a caret over your input, Codex's labelled speaker with the body beneath. Wrapping happens
/// here rather than in fzf because fzf's wrap indent is one fixed string for the whole pane.
fn turn(t: &Turn, width: usize) -> Vec<String> {
    let (lead, pad) = match t.who {
        Who::You => (format!("{MUTED}>{RESET} "), "  "),
        Who::CodexUser | Who::Codex => (String::new(), "  "),
        Who::Result => (format!("  {MUTED}\u{23BF}  "), "     "),
        Who::CodexTool => (format!("\x1b[38;5;75m\u{2022}{RESET} "), "  "),
        Who::CodexResult => (format!("  {MUTED}\u{2514} "), "    "),
        _ => (format!("{ACCENT}\u{25CF}{RESET} "), "  "),
    };
    let plain = matches!(t.who, Who::You | Who::CodexUser);
    let result = matches!(t.who, Who::Result | Who::CodexResult);
    let verbatim = result || matches!(t.who, Who::Tool | Who::CodexTool);
    let room = width.saturating_sub(pad.len()).max(24);
    let text = printable(&t.text);

    // A tool call and its result are one visual unit, so the call takes no trailing blank line.
    if matches!(t.who, Who::Tool | Who::CodexTool) {
        let (name, arg) = CALL
            .captures(&text)
            .map_or((text.as_str(), ""), |c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()));
        // One line per call: the argument is cut to what is left of the pane after the name.
        let arg = arg.replace('\n', " ");
        let room_for_arg = room.saturating_sub(name.chars().count() + 2).max(8);
        let shown = if arg.chars().count() > room_for_arg {
            format!("{}\u{2026}", truncate(&arg, room_for_arg - 1))
        } else {
            arg
        };
        let mut head = format!("{lead}{ESC}[1m{name}{ESC}[22m");
        if !shown.is_empty() {
            head += &format!("({MUTED}{shown}{ESC}[39m)");
        }
        return vec![head];
    }

    let mut body: Vec<String> = Vec::new();
    let (mut in_code, mut blank) = (false, false);
    for raw in text.split('\n') {
        if !verbatim && FENCE.is_match(raw) {
            in_code = !in_code;
            continue;
        }
        if raw.trim().is_empty() {
            blank = !body.is_empty();
            continue;
        }
        if blank {
            body.push(String::new());
            blank = false;
        }
        for seg in wrap(raw.trim_end(), room) {
            body.push(if result {
                format!("{MUTED}{seg}{RESET}")
            } else if verbatim {
                seg
            } else if in_code {
                format!("\x1b[38;5;109m{seg}{RESET}")
            } else if plain {
                format!("\x1b[38;5;252m{seg}{RESET}")
            } else {
                markdown(&seg)
            });
        }
    }
    if body.is_empty() {
        return body;
    }

    let mut out = Vec::new();
    match t.who {
        Who::CodexUser => out.push(format!("{ESC}[1;38;5;245muser{RESET}")),
        Who::Codex => out.push(format!("{ESC}[1;38;5;75mcodex{RESET}")),
        _ => {}
    }
    let indent = |l: &String| if l.is_empty() { String::new() } else { format!("{pad}{l}") };
    if lead.is_empty() {
        out.extend(body.iter().map(indent));
    } else {
        out.push(format!("{lead}{}", body[0]));
        out.extend(body[1..].iter().map(indent));
    }
    out.push(String::new());
    out
}

/// The pane opened by TAB: the conversation oldest to newest, opening on its end (fzf's `follow`), so
/// the last exchange - the part that says which session this is - is what you land on. Whole turns
/// only, spent newest first, so when the budget runs out the oldest turns are what fall off.
pub fn pane(head: &[Turn], tail: &[Turn], haystack_lines: &[String], width: usize) -> Vec<String> {
    let mut turns: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let tail_start = tail.len().saturating_sub(40);
    for t in head.iter().chain(&tail[tail_start..]) {
        if t.who == Who::Cmd
            || t.text.contains("The messages below were generated by the user while running local commands")
        {
            continue;
        }
        let lines = turn(t, width);
        if !lines.is_empty() {
            turns.entry(t.seq).or_insert(lines);
        }
    }
    let Some(&first_seq) = turns.keys().next() else {
        return haystack_lines.iter().take(6).map(|l| format!("  {l}")).collect();
    };

    let mut kept: Vec<(u32, &Vec<String>)> = Vec::new();
    let mut used = 0;
    for (seq, lines) in turns.iter().rev() {
        if !kept.is_empty() && used + lines.len() > MAX_LINES {
            break;
        }
        used += lines.len();
        kept.push((*seq, lines));
    }
    kept.reverse();

    let mut out = Vec::new();
    if kept[0].0 > first_seq {
        out.push(format!("{DIM}  \u{2026} earlier turns{RESET}"));
        out.push(String::new());
    }
    for (i, (seq, lines)) in kept.iter().enumerate() {
        if i > 0 && *seq > kept[i - 1].0 + 1 {
            out.push(format!("{DIM}  \u{2026} {} turns not shown{RESET}", seq - kept[i - 1].0 - 1));
            out.push(String::new());
        }
        out.extend(lines.iter().cloned());
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(who: Who, seq: u32, text: &str) -> Turn {
        Turn { who, seq, text: text.into() }
    }

    #[test]
    fn code_spans_survive_emphasis() {
        let s = inline("use `a*b*c` and *this*");
        assert!(s.contains("a*b*c"));
        assert!(s.contains("\x1b[3mthis\x1b[23m"));
    }

    #[test]
    fn pane_marks_gaps_and_drops_plumbing() {
        let head = vec![t(Who::You, 1, "hello")];
        let tail = vec![t(Who::Cmd, 2, "/clear"), t(Who::Claude, 5, "bye")];
        let p = pane(&head, &tail, &[], 80).join("\n");
        assert!(p.contains("hello") && p.contains("bye"));
        assert!(!p.contains("/clear"));
        assert!(p.contains("3 turns not shown"));
    }

    #[test]
    fn tool_call_renders_name_and_argument() {
        let lines = turn(&t(Who::Tool, 1, "Read(login.rs)"), 80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Read") && lines[0].contains("login.rs"));
    }
}
