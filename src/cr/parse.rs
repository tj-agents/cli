use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::text::{block, collapse_output, flat, truncate};

const HAYSTACK_CAP: usize = 300_000;
const TAIL_CAP: usize = 100_000;
const HEAD_TURNS: usize = 6;
/// Codex lines this long are bulk payloads (images, giant outputs); parsing them buys nothing.
const CODEX_LINE_CAP: usize = 200_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Who {
    /// Claude: you, Claude, a tool call, its result, and slash-command plumbing.
    You,
    Claude,
    Tool,
    Result,
    Cmd,
    /// Codex: you, Codex, a tool call, its result.
    CodexUser,
    Codex,
    CodexTool,
    CodexResult,
}

#[derive(Clone, Debug)]
pub struct Turn {
    pub who: Who,
    pub seq: u32,
    pub text: String,
}

/// What the index keeps from one transcript.
#[derive(Default, Debug)]
pub struct Session {
    pub preview: String,
    pub story: String,
    pub branch: String,
    pub cwd: String,
    pub haystack: String,
    pub head: Vec<Turn>,
    pub tail: Vec<Turn>,
}

/// The rolling state both parsers share. The haystack is every message's text on one line so the
/// search bar can match anything that was said; tool traffic never reaches it, or one file dump
/// would eat the whole cap. The tail is its own buffer of the newest turns because a long session
/// blows the cap hours before it ends, and how it ended is what identifies it.
#[derive(Default)]
struct Builder {
    seq: u32,
    head: Vec<Turn>,
    tail: VecDeque<Turn>,
    tail_len: usize,
    hay: String,
}

impl Builder {
    fn push(&mut self, who: Who, text: String) {
        self.seq += 1;
        let turn = Turn { who, seq: self.seq, text };
        if who != Who::Cmd && self.head.len() < HEAD_TURNS {
            self.head.push(turn.clone());
        }
        self.tail_len += turn.text.len();
        self.tail.push_back(turn);
        while self.tail_len > TAIL_CAP && self.tail.len() > 1 {
            self.tail_len -= self.tail.pop_front().map_or(0, |t| t.text.len());
        }
    }

    fn said(&mut self, text: &str) {
        if self.hay.len() < HAYSTACK_CAP {
            self.hay.push_str(text);
            self.hay.push(' ');
        }
    }

    fn finish(self, mut s: Session) -> Session {
        let mut hay = flat(&self.hay);
        // Past the cap the head stops growing, so the newest turns are appended: without them nothing
        // said in the back half of a long session would be searchable at all.
        if self.hay.len() >= HAYSTACK_CAP {
            let tail: Vec<String> = self
                .tail
                .iter()
                .filter(|t| !matches!(t.who, Who::Tool | Who::Result | Who::CodexTool | Who::CodexResult))
                .map(|t| flat(&t.text))
                .collect();
            hay = format!("{hay} ... {}", tail.join(" ")).trim().to_string();
        }
        s.haystack = hay;
        s.preview = truncate(&flat(&s.preview), 78).to_string();
        if s.preview.is_empty() {
            s.preview = "(no prompt)".into();
        }
        let blocked = |t: &Turn| Turn { text: block(&t.text), ..t.clone() };
        s.head = self.head.iter().map(blocked).collect();
        s.tail = self.tail.iter().map(blocked).filter(|t| !t.text.is_empty()).collect();
        s
    }
}

fn lines(path: &Path) -> impl Iterator<Item = String> {
    // A session still open in another window is being appended to; reading it is fine on every OS
    // (Rust opens with shared read/write on Windows), and a torn last line just fails to parse.
    File::open(path).ok().into_iter().flat_map(|f| BufReader::new(f).lines().map_while(Result::ok))
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

static STORY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_workitems/edit/(\d{4,7})|AB#(\d{4,7})|workitem=(\d{4,7})").unwrap());
static BRANCH_STORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^AB#\d+$").unwrap());
static ABS_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[A-Za-z]:[\\/]|[\\/])\S*$").unwrap());

/// A tool call the way Claude Code prints it: the name and the one argument that says what it acted on.
fn claude_call(block: &Value) -> String {
    let name = str_at(block, "name");
    let input = block.get("input").unwrap_or(&Value::Null);
    let keys =
        ["command", "file_path", "pattern", "notebook_path", "path", "url", "skill", "query", "description", "prompt"];
    let mut arg = keys.iter().find_map(|k| input.get(k).and_then(Value::as_str)).map(flat).unwrap_or_default();
    if ABS_PATH.is_match(&arg) {
        arg = arg.rsplit(['/', '\\']).next().unwrap_or(&arg).to_string();
    }
    format!("{name}({arg})")
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            items.iter().map(|i| str_at(i, "text")).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

const PLUMBING: &[&str] =
    &["<local-command-stdout>", "<task-notification>", "<bash-input>", "<bash-stdout>", "<system-reminder>"];

pub fn claude(path: &Path) -> Session {
    let mut s = Session::default();
    let mut b = Builder::default();
    let mut cmd_preview = String::new();

    for line in lines(path) {
        let Ok(o) = serde_json::from_str::<Value>(&line) else { continue };
        if s.branch.is_empty() {
            s.branch = str_at(&o, "gitBranch").to_string();
        }
        if s.cwd.is_empty() {
            s.cwd = str_at(&o, "cwd").to_string();
        }
        let kind = str_at(&o, "type");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let content = o.get("message").and_then(|m| m.get("content")).unwrap_or(&Value::Null);
        let blocks: &[Value] = content.as_array().map(Vec::as_slice).unwrap_or(&[]);

        let results: Vec<&Value> = blocks.iter().filter(|x| str_at(x, "type") == "tool_result").collect();
        if !results.is_empty() {
            for r in results {
                if let Some(text) = collapse_output(&result_text(r.get("content").unwrap_or(&Value::Null))) {
                    b.push(Who::Result, text);
                }
            }
            continue;
        }

        let said: Vec<String> = match content {
            Value::String(t) => vec![t.clone()],
            _ => blocks.iter().filter(|x| str_at(x, "type") == "text").map(|x| str_at(x, "text").to_string()).collect(),
        };
        let calls: Vec<String> = blocks.iter().filter(|x| str_at(x, "type") == "tool_use").map(claude_call).collect();
        if said.is_empty() {
            for c in calls {
                b.push(Who::Tool, c);
            }
            continue;
        }
        // Injected context is not something that was said.
        if o.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }

        let turn = said.join(" ");
        let is_cmd = turn.contains("<command-name>") || turn.contains("<command-message>");
        let is_user = kind == "user";
        if s.preview.is_empty() && is_user {
            if is_cmd {
                if cmd_preview.is_empty() {
                    cmd_preview = said[0].clone();
                }
            } else if !said[0].starts_with("<local-command-") {
                s.preview = said[0].clone();
            }
        }
        // A slash command and its echoed output stay searchable (`cr /techdebt` finds them) but identify
        // no session, so the pane leaves them out.
        let plumbing = is_cmd || PLUMBING.iter().any(|p| turn.contains(p));
        let who = if plumbing {
            Who::Cmd
        } else if is_user {
            Who::You
        } else {
            Who::Claude
        };
        b.push(who, turn);
        for c in calls {
            b.push(Who::Tool, c);
        }
        for t in &said {
            b.said(t);
        }
    }

    if s.preview.is_empty() {
        s.preview = cmd_preview;
    }
    if let Some(m) = STORY.captures(&s.preview) {
        s.story = (1..=3).find_map(|i| m.get(i)).map(|g| format!("AB#{}", g.as_str())).unwrap_or_default();
    } else if BRANCH_STORY.is_match(&s.branch) {
        s.story = s.branch.clone();
    }
    b.finish(s)
}

/// The first line of a Codex rollout: its resumable id, where it ran, and whether it is a subagent
/// (a guardian reviewer, say) whose transcript belongs to some other session.
#[derive(Default, Debug)]
pub struct CodexMeta {
    pub session_id: String,
    pub cwd: String,
    pub subagent: bool,
}

pub fn codex_meta(path: &Path) -> CodexMeta {
    let Some(first) = lines(path).next() else { return CodexMeta::default() };
    let Ok(o) = serde_json::from_str::<Value>(&first) else { return CodexMeta::default() };
    let p = o.get("payload").unwrap_or(&Value::Null);
    CodexMeta {
        session_id: str_at(p, "session_id").to_string(),
        cwd: str_at(p, "cwd").to_string(),
        subagent: p.get("source").and_then(|s| s.get("subagent")).is_some(),
    }
}

// Codex re-injects AGENTS.md and environment context as ordinary user messages, and the approval
// reviewer's traffic rides the same transcript, so both are recognised by content.
static CODEX_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"<INSTRUCTIONS>|<environment_context|^# AGENTS\.md instructions|<skills_instructions>|<recommended_plugins>",
        r"|>>> (APPROVAL REQUEST|TRANSCRIPT)|^Planned action JSON:|^Assess the exact planned action",
        r"|^Reviewed Codex session id:|^Some conversation entries were omitted|^The Codex agent has requested",
        r#"|^\{"risk_level"|\[\d+\] (tool \w+ (call|result)|user:|assistant:)|"risk_level":"#,
        r"|The following is the Codex agent history|<no retained transcript delta entries>",
    ))
    .unwrap()
});

/// What a Codex shell call ran, not the JS or JSON wrapping it.
fn codex_arg(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        for key in ["cmd", "command"] {
            match v.get(key) {
                Some(Value::String(s)) => return flat(s),
                Some(Value::Array(a)) => return a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                _ => {}
            }
        }
    }
    static CMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bcmd"?\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap());
    if let Some(m) = CMD.captures(raw) {
        let quoted = format!("\"{}\"", &m[1]);
        return flat(&serde_json::from_str::<String>(&quoted).unwrap_or_else(|_| m[1].to_string()));
    }
    truncate(&flat(raw), 300).to_string()
}

pub fn codex(path: &Path) -> Session {
    let mut s = Session::default();
    let mut b = Builder::default();

    for line in lines(path).skip(1) {
        if line.len() >= CODEX_LINE_CAP || !line.contains("\"response_item\"") {
            continue;
        }
        let Ok(o) = serde_json::from_str::<Value>(&line) else { continue };
        let p = o.get("payload").unwrap_or(&Value::Null);
        match str_at(p, "type") {
            "function_call" | "custom_tool_call" => {
                let name = Some(str_at(p, "name")).filter(|n| !n.is_empty()).unwrap_or("tool");
                let raw = p.get("arguments").or_else(|| p.get("input")).and_then(Value::as_str).unwrap_or("");
                b.push(Who::CodexTool, format!("{name}({})", codex_arg(raw)));
            }
            "local_shell_call" => {
                let cmd = p.pointer("/action/command").and_then(Value::as_array);
                let arg =
                    cmd.map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                b.push(Who::CodexTool, format!("shell({arg})"));
            }
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                if let Some(text) = collapse_output(&result_text(p.get("output").unwrap_or(&Value::Null))) {
                    b.push(Who::CodexResult, text);
                }
            }
            "message" => {
                let role = str_at(p, "role");
                if role != "user" && role != "assistant" {
                    continue;
                }
                let is_user = role == "user";
                for item in p.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
                    let text = str_at(item, "text");
                    if text.is_empty() || CODEX_NOISE.is_match(text) {
                        continue;
                    }
                    if s.preview.is_empty() && is_user {
                        s.preview = text.to_string();
                    }
                    b.push(if is_user { Who::CodexUser } else { Who::Codex }, text.to_string());
                    b.said(text);
                }
            }
            _ => {}
        }
    }
    b.finish(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn claude_transcript() {
        let s = claude(&fixture("claude.jsonl"));
        assert_eq!(s.preview, "fix the AB#12345 login bug");
        assert_eq!(s.story, "AB#12345");
        assert_eq!(s.branch, "main");
        assert_eq!(s.cwd, "/work/repo");
        assert!(s.haystack.contains("fix the AB#12345 login bug"));
        assert!(s.haystack.contains("The login handler"));
        assert!(!s.haystack.contains("secret file body"), "tool output must not reach the haystack");
        assert!(!s.haystack.contains("injected context"), "isMeta lines are skipped");
        let whos: Vec<Who> = s.tail.iter().map(|t| t.who).collect();
        assert_eq!(whos, [Who::Cmd, Who::You, Who::Claude, Who::Tool, Who::Result, Who::Claude]);
        assert_eq!(s.tail[3].text, "Read(login.rs)");
    }

    #[test]
    fn codex_transcript() {
        let path = fixture("codex.jsonl");
        let m = codex_meta(&path);
        assert_eq!(m.session_id, "abc-123");
        assert_eq!(m.cwd, "/work/repo");
        assert!(!m.subagent);
        let s = codex(&path);
        assert_eq!(s.preview, "why is the build red");
        assert!(!s.haystack.contains("AGENTS.md"), "injected instructions are filtered");
        let tool = s.tail.iter().find(|t| t.who == Who::CodexTool).unwrap();
        assert_eq!(tool.text, "exec_command(cargo test)");
        assert!(s.tail.iter().any(|t| t.who == Who::CodexResult && t.text.contains("test result: FAILED")));
    }

    #[test]
    fn codex_subagents_are_flagged() {
        assert!(codex_meta(&fixture("codex-guardian.jsonl")).subagent);
    }
}
