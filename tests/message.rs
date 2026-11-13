use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CODEX_CHILD: &str = "019a0000-0000-7000-8000-00000000c0de";
const CLAUDE_PEER: &str = "4d86b5f2-0000-4000-8000-00000000c1a0";
const TOKEN: &str = "test-token";

const HOST_VARIABLES: [&str; 8] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_PID",
    "CODEX_THREAD_ID",
    "AGENT_CLI_TAB_TITLE",
    "TJ_CACHE_DIR",
];

struct Sandbox {
    _directory: tempfile::TempDir,
    root: PathBuf,
    stubs: PathBuf,
    background: Vec<Child>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for child in &mut self.background {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn agent_store_stub(directory: &Path) {
    #[cfg(windows)]
    std::fs::write(
        directory.join("agent-store.cmd"),
        "@echo off\r\necho %*>>\"%~dp0agent-store.log\"\r\ntype \"%~dp0agent-store-%1.json\"\r\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("agent-store");
        std::fs::write(
            &path,
            "#!/bin/sh\nhere=\"$(dirname \"$0\")\"\necho \"$*\" >> \"$here/agent-store.log\"\ncat \"$here/agent-store-$1.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn session(host: &str, id: &str, title: &str, state: &str) -> Value {
    json!({"session": format!("{host}:{id}"), "host": host, "native_id": id, "title": title, "state": state})
}

fn sandbox(sessions: &[Value]) -> Sandbox {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let stubs = root.join("stubs");
    std::fs::create_dir(&stubs).unwrap();
    agent_store_stub(&stubs);
    let reply = |data: Value| json!({"schema_version": 1, "ok": true, "data": data}).to_string();
    std::fs::write(stubs.join("agent-store-sessions.json"), reply(json!({"title": "x", "sessions": sessions})))
        .unwrap();
    if let Some(first) = sessions.first() {
        std::fs::write(stubs.join("agent-store-show.json"), reply(json!({"session": first}))).unwrap();
    }
    Sandbox { _directory: directory, root, stubs, background: Vec::new() }
}

impl Sandbox {
    fn command(&self, arguments: &[&str], caller: &[(&str, &str)]) -> Command {
        let path = std::env::join_paths(
            std::iter::once(self.stubs.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
        command.args(arguments);
        for name in HOST_VARIABLES {
            command.env_remove(name);
        }
        command.env("PATH", path).env("TJ_CACHE_DIR", self.root.join("cache")).envs(caller.iter().copied());
        command
    }

    fn tj(&self, arguments: &[&str], caller: &[(&str, &str)]) -> Output {
        self.command(arguments, caller).stdin(Stdio::null()).output().unwrap()
    }

    fn feed(&self, file: &str) -> PathBuf {
        self.root.join("cache").join("feed").join(file)
    }

    fn posted(&self, file: &str) -> Vec<Value> {
        std::fs::read_to_string(self.feed(file))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn inbox_address(&self) -> String {
        if cfg!(windows) {
            format!(r"\\.\pipe\LOCAL\tj-test-{}", self.root.file_name().unwrap().to_string_lossy())
        } else {
            self.root.join("inbox.sock").to_string_lossy().into_owned()
        }
    }

    fn start_inbox(&mut self) -> String {
        let address = self.inbox_address();
        let ready = self.root.join("inbox.ready");
        let child = Command::new(python())
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("fake_inbox.py"))
            .args([address.as_str(), &self.root.join("inbox.capture").to_string_lossy(), &ready.to_string_lossy()])
            .spawn()
            .unwrap();
        self.background.push(child);
        wait_for("the fake inbox", || ready.exists());
        address
    }

    fn inbox(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("inbox.capture"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn endpoint(&self) -> PathBuf {
        self.root.join("cache").join("bridge").join(format!("{CLAUDE_PEER}.json"))
    }

    fn start_bridge(&mut self, inbox: &str) {
        let child = self
            .command(
                &["bridge"],
                &[
                    ("CLAUDE_CODE_MESSAGING_SOCKET", inbox),
                    ("CLAUDE_CODE_MESSAGING_TOKEN", TOKEN),
                    ("CLAUDE_CODE_SESSION_ID", CLAUDE_PEER),
                ],
            )
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        self.background.push(child);
        let endpoint = self.endpoint();
        wait_for("the relay endpoint", || endpoint.exists());
    }

    /// Sends `text` to the relay with the secret `secret` picks from its endpoint record, and returns the reply.
    fn relay(&self, secret: impl Fn(&Value) -> Value, text: &str) -> String {
        let endpoint: Value = serde_json::from_slice(&std::fs::read(self.endpoint()).unwrap()).unwrap();
        let port = u16::try_from(endpoint["port"].as_u64().unwrap()).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(stream, "{}", json!({"secret": secret(&endpoint), "text": text})).unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        reply
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const FROM_CLAUDE: [(&str, &str); 3] =
    [("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "c1a0de00"), ("AGENT_CLI_TAB_TITLE", "cli 82")];
const FROM_CODEX: [(&str, &str); 2] = [("CODEX_THREAD_ID", "019a-parent"), ("AGENT_CLI_TAB_TITLE", "agent 92")];
const AS_CLAUDE_PEER: [(&str, &str); 2] = [("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", CLAUDE_PEER)];
const AS_CODEX_CHILD: [(&str, &str); 1] = [("CODEX_THREAD_ID", CODEX_CHILD)];

#[test]
fn a_message_to_a_codex_session_lands_in_its_feed_with_the_sender() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", "Phase 3b", "rebase on main"], &FROM_CLAUDE);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("posted to Phase 3b's feed; it reads it when it runs tj feed"));
    let posted = sandbox.posted(&format!("codex-{CODEX_CHILD}.jsonl"));
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["from"], "claude:c1a0de00");
    assert_eq!(posted[0]["name"], "cli 82");
    assert_eq!(posted[0]["text"], "rebase on main");
}

#[test]
fn a_message_to_a_claude_session_goes_to_its_feed_from_either_host() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "tj-agents-c2", "active")]);
    for caller in [&FROM_CLAUDE[..], &FROM_CODEX[..]] {
        let output = sandbox.tj(&["message", "tj-agents-c2", "PR 56 is ready"], caller);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let posted = sandbox.posted(&format!("claude-{CLAUDE_PEER}.jsonl"));
    let senders: Vec<&str> = posted.iter().map(|message| message["from"].as_str().unwrap()).collect();
    assert_eq!(senders, ["claude:c1a0de00", "codex:019a-parent"]);
}

#[test]
fn a_handle_is_looked_up_with_show() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", &format!("codex:{CODEX_CHILD}"), "done"], &FROM_CODEX);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(std::fs::read_to_string(sandbox.stubs.join("agent-store.log")).unwrap().contains("show codex:"));
}

#[test]
fn feed_shows_unread_messages_once_with_a_reply_route() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "tj-agents-c2", "active")]);
    sandbox.tj(&["message", "tj-agents-c2", "PR 56 is ready"], &FROM_CODEX);
    let first = sandbox.tj(&["feed"], &AS_CLAUDE_PEER);
    assert!(first.status.success(), "{}", stderr(&first));
    let shown = stdout(&first);
    assert!(shown.contains("agent 92 (codex:019a-parent): PR 56 is ready"), "{shown}");
    assert!(shown.contains("reply: tj message codex:019a-parent \"<text>\""), "{shown}");
    assert_eq!(stdout(&sandbox.tj(&["feed"], &AS_CLAUDE_PEER)).trim(), "no unread messages");
    sandbox.tj(&["message", "tj-agents-c2", "merged"], &FROM_CODEX);
    let next = stdout(&sandbox.tj(&["feed"], &AS_CLAUDE_PEER));
    assert!(next.contains("merged") && !next.contains("PR 56"), "{next}");
}

#[test]
fn a_message_from_a_terminal_has_no_reply_route() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    sandbox.tj(&["message", "Phase 3b", "stop for lunch"], &[]);
    let shown = stdout(&sandbox.tj(&["feed"], &AS_CODEX_CHILD));
    assert!(shown.contains("a terminal: stop for lunch") && !shown.contains("reply:"), "{shown}");
}

#[test]
fn a_partly_written_line_waits_for_its_newline() {
    let sandbox = sandbox(&[]);
    let feed = sandbox.feed(&format!("codex-{CODEX_CHILD}.jsonl"));
    std::fs::create_dir_all(feed.parent().unwrap()).unwrap();
    std::fs::write(&feed, "{\"at\":\"t\",\"from\":null,\"text\":\"wh").unwrap();
    assert_eq!(stdout(&sandbox.tj(&["feed"], &AS_CODEX_CHILD)).trim(), "no unread messages");
    std::fs::write(&feed, "{\"at\":\"t\",\"from\":null,\"text\":\"whole\"}\n").unwrap();
    assert!(stdout(&sandbox.tj(&["feed"], &AS_CODEX_CHILD)).contains("a terminal: whole"));
}

#[test]
fn feed_wait_returns_when_a_message_arrives() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let waiting = sandbox
        .command(&["feed", "--wait", "--timeout", "60"], &AS_CODEX_CHILD)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    sandbox.tj(&["message", "Phase 3b", "finished: PR 12"], &FROM_CLAUDE);
    let started = Instant::now();
    let output = waiting.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(stdout(&output).contains("cli 82 (claude:c1a0de00): finished: PR 12"));
}

#[test]
fn feed_wait_gives_up_at_its_timeout() {
    let sandbox = sandbox(&[]);
    let output = sandbox.tj(&["feed", "--wait", "--timeout", "1"], &AS_CODEX_CHILD);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "no message arrived within 1 seconds");
}

#[test]
fn a_torn_line_is_passed_over_and_the_messages_after_it_still_arrive() {
    let sandbox = sandbox(&[]);
    let feed = sandbox.feed(&format!("codex-{CODEX_CHILD}.jsonl"));
    std::fs::create_dir_all(feed.parent().unwrap()).unwrap();
    let whole = "{\"at\":\"t\",\"from\":null,\"text\":\"after\"}";
    std::fs::write(
        &feed,
        format!(
            "{{\"at\":\"t\",\"te{whole}
{whole}
"
        ),
    )
    .unwrap();
    let shown = stdout(&sandbox.tj(&["feed"], &AS_CODEX_CHILD));
    assert!(shown.contains("passed over 1 line(s) in the feed that were not JSON"), "{shown}");
    assert_eq!(shown.matches("a terminal: after").count(), 1, "{shown}");
    assert_eq!(stdout(&sandbox.tj(&["feed"], &AS_CODEX_CHILD)).trim(), "no unread messages");
}

#[cfg(unix)]
#[test]
fn only_the_owner_can_read_a_feed() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    sandbox.tj(&["message", "Phase 3b", "private"], &FROM_CLAUDE);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let feed = sandbox.feed(&format!("codex-{CODEX_CHILD}.jsonl"));
    assert_eq!(mode(&feed), 0o600);
    assert_eq!(mode(feed.parent().unwrap()), 0o700);
}

#[test]
fn a_session_on_an_unknown_host_is_refused() {
    let sandbox = sandbox(&[session("gemini", "g1", "elsewhere", "active")]);
    let output = sandbox.tj(&["message", "elsewhere", "hi"], &FROM_CODEX);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("elsewhere runs on gemini, which tj message cannot reach"));
    assert!(!sandbox.root.join("cache").join("feed").exists());
}

#[test]
fn feed_needs_a_calling_session() {
    let output = sandbox(&[]).tj(&["feed"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("tj feed reads the calling session's own feed"));
}

#[test]
fn ended_and_ambiguous_titles_are_refused() {
    let ended = sandbox(&[session("codex", CODEX_CHILD, "old", "ended")]);
    assert!(
        stderr(&ended.tj(&["message", "old", "hi"], &FROM_CODEX)).contains("no live session is addressed by \"old\"")
    );
    let twice = sandbox(&[session("codex", "a", "dup", "active"), session("claude", "b", "dup", "closing")]);
    let output = twice.tj(&["message", "dup", "hi"], &FROM_CODEX);
    assert!(stderr(&output).contains("2 live sessions are titled \"dup\": codex:a, claude:b"));
    assert!(!twice.root.join("cache").join("feed").exists());
}

#[test]
fn the_relay_posts_to_its_session_inbox_with_its_token() {
    let mut sandbox = sandbox(&[]);
    let inbox = sandbox.start_inbox();
    sandbox.start_bridge(&inbox);
    let reply = sandbox.relay(|endpoint| endpoint["secret"].clone(), "[tj run] every PR has merged");
    assert!(reply.contains(r#""ok":true"#), "{reply}");
    wait_for("the inbox to record the notice", || sandbox.inbox().len() == 2);
    let lines = sandbox.inbox();
    assert_eq!(lines[0], json!({"type": "auth", "token": TOKEN}));
    assert_eq!(lines[1]["type"], "user");
    assert_eq!(lines[1]["message"]["content"], "[tj run] every PR has merged");
}

#[test]
fn a_relay_refuses_a_caller_without_its_secret() {
    let mut sandbox = sandbox(&[]);
    let inbox = sandbox.start_inbox();
    sandbox.start_bridge(&inbox);
    let reply = sandbox.relay(|_| json!("guess"), "hi");
    assert!(reply.contains("wrong relay secret"));
    assert!(sandbox.inbox().is_empty());
}

#[test]
fn the_bridge_hook_is_a_no_op_outside_claude() {
    let sandbox = sandbox(&[]);
    let output = sandbox.tj(&["bridge"], &FROM_CODEX);
    assert!(output.status.success());
    assert!(!sandbox.root.join("cache").join("bridge").exists());
}
