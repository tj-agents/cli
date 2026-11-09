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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
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

fn codex_stub(directory: &Path) {
    let script = fixture("fake_codex.py");
    #[cfg(windows)]
    std::fs::write(directory.join("codex.cmd"), format!("@echo off\r\npython \"{}\" %*\r\n", script.display()))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("codex");
        std::fs::write(&path, format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", script.display())).unwrap();
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
    codex_stub(&stubs);
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
        command
            .env("PATH", path)
            .env("TJ_CACHE_DIR", self.root.join("cache"))
            .env("FAKE_CODEX_LOG", self.root.join("codex.log"))
            .envs(caller.iter().copied());
        command
    }

    fn tj(&self, arguments: &[&str], caller: &[(&str, &str)]) -> Output {
        self.command(arguments, caller).stdin(Stdio::null()).output().unwrap()
    }

    fn codex_log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("codex.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn queued(&self) -> Vec<Value> {
        self.codex_log().into_iter().filter_map(|entry| entry.get("queue").cloned()).collect()
    }

    fn wait_for(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
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
            .arg(fixture("fake_inbox.py"))
            .args([address.as_str(), &self.root.join("inbox.capture").to_string_lossy(), &ready.to_string_lossy()])
            .spawn()
            .unwrap();
        self.background.push(child);
        self.wait_for("the fake inbox", || ready.exists());
        address
    }

    fn inbox(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("inbox.capture"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
        let endpoint = self.root.join("cache").join("bridge").join(format!("{CLAUDE_PEER}.json"));
        self.wait_for("the relay endpoint", || endpoint.exists());
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

#[test]
fn claude_to_codex_queues_with_a_reply_route() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", "Phase 3b", "rebase on main"], &FROM_CLAUDE);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("queued for Phase 3b; delivered at the target's next idle point"));
    let queued = sandbox.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["--thread"], CODEX_CHILD);
    assert_eq!(
        queued[0]["--message"],
        "[tj message from cli 82; reply with: tj message claude:c1a0de00 \"<text>\"] rebase on main"
    );
}

#[test]
fn codex_to_codex_queues_by_handle() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", &format!("codex:{CODEX_CHILD}"), "done"], &FROM_CODEX);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(std::fs::read_to_string(sandbox.stubs.join("agent-store.log")).unwrap().contains("show codex:"));
    let queued = sandbox.queued();
    assert_eq!(
        queued[0]["--message"],
        "[tj message from agent 92; reply with: tj message codex:019a-parent \"<text>\"] done"
    );
}

#[test]
fn a_failed_queue_is_reported_not_claimed() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output =
        sandbox.command(&["message", "Phase 3b", "hi"], &FROM_CODEX).env("FAKE_QUEUE_FAIL", "1").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("No active session found"));
    assert!(!stdout(&output).contains("queued"));
}

#[test]
fn claude_to_claude_defers_to_native_send_message() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "tj-agents-c2", "active")]);
    let output = sandbox.tj(&["message", "tj-agents-c2", "hi"], &FROM_CLAUDE);
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stdout(&output)
            .contains("not sent: tj-agents-c2 is a Claude session; send it with the native SendMessage tool")
    );
    assert!(sandbox.codex_log().is_empty());
}

#[test]
fn codex_to_claude_goes_through_the_session_relay_with_its_token() {
    let mut sandbox = sandbox(&[session("claude", CLAUDE_PEER, "tj-agents-c2", "active")]);
    let inbox = sandbox.start_inbox();
    sandbox.start_bridge(&inbox);
    let output = sandbox.tj(&["message", "tj-agents-c2", "PR 56 is ready"], &FROM_CODEX);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("delivered to tj-agents-c2's inbox"));
    sandbox.wait_for("the inbox to record the message", || sandbox.inbox().len() == 2);
    let lines = sandbox.inbox();
    assert_eq!(lines[0], json!({"type": "auth", "token": TOKEN}));
    assert_eq!(lines[1]["type"], "user");
    assert_eq!(
        lines[1]["message"]["content"],
        "[tj message from agent 92; reply with: tj message codex:019a-parent \"<text>\"] PR 56 is ready"
    );
}

#[test]
fn a_relay_refuses_a_caller_without_its_secret() {
    let mut sandbox = sandbox(&[]);
    let inbox = sandbox.start_inbox();
    sandbox.start_bridge(&inbox);
    let endpoint: Value = serde_json::from_slice(
        &std::fs::read(sandbox.root.join("cache").join("bridge").join(format!("{CLAUDE_PEER}.json"))).unwrap(),
    )
    .unwrap();
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", endpoint["port"].as_u64().unwrap() as u16)).unwrap();
    use std::io::{BufRead, Write};
    writeln!(stream, "{}", json!({"secret": "guess", "text": "hi"})).unwrap();
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply).unwrap();
    assert!(reply.contains("wrong relay secret"));
    assert!(sandbox.inbox().is_empty());
}

#[test]
fn a_claude_session_without_a_relay_is_an_error() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "tj-agents-c2", "active")]);
    let output = sandbox.tj(&["message", "tj-agents-c2", "hi"], &FROM_CODEX);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("has no message relay"));
}

#[test]
fn the_bridge_hook_is_a_no_op_outside_claude() {
    let sandbox = sandbox(&[]);
    let output = sandbox.tj(&["bridge"], &FROM_CODEX);
    assert!(output.status.success());
    assert!(!sandbox.root.join("cache").join("bridge").exists());
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
    assert!(twice.codex_log().is_empty());
}

fn subscribe(sandbox: &Sandbox, caller: &[(&str, &str)], loaded: &str, status: &str, events: &Value) -> Output {
    sandbox
        .command(&["subscribe", "Phase 3b"], caller)
        .env("FAKE_LOADED", loaded)
        .env("FAKE_STATUS", status)
        .env("FAKE_EVENTS", events.to_string())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn status_changed(thread: &str, kind: &str) -> Value {
    json!({"method": "thread/status/changed", "params": {"threadId": thread, "status": {"type": kind}}})
}

#[test]
fn a_codex_parent_hears_once_when_its_child_goes_idle() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let events = json!([
        status_changed("other", "idle"),
        status_changed(CODEX_CHILD, "active"),
        status_changed(CODEX_CHILD, "idle")
    ]);
    let output = subscribe(&sandbox, &FROM_CODEX, CODEX_CHILD, "active", &events);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("subscribed: one notice arrives when Phase 3b next goes idle or exits"));
    sandbox.wait_for("the idle notice", || !sandbox.queued().is_empty());
    let queued = sandbox.queued();
    assert_eq!(queued[0]["--thread"], "019a-parent");
    assert_eq!(queued[0]["--message"], format!("[tj subscribe] Phase 3b (codex:{CODEX_CHILD}) is idle"));
    let resumed = sandbox.codex_log().into_iter().find(|entry| entry["proxy"] == "thread/resume").unwrap();
    assert_eq!(resumed["params"], json!({"threadId": CODEX_CHILD, "excludeTurns": true}));
}

#[test]
fn a_codex_parent_hears_when_its_child_exits() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let events = json!([{"method": "thread/closed", "params": {"threadId": CODEX_CHILD}}]);
    let output = subscribe(&sandbox, &FROM_CODEX, CODEX_CHILD, "active", &events);
    assert!(output.status.success(), "{}", stderr(&output));
    sandbox.wait_for("the exit notice", || !sandbox.queued().is_empty());
    assert_eq!(sandbox.queued()[0]["--message"], format!("[tj subscribe] Phase 3b (codex:{CODEX_CHILD}) has exited"));
}

#[test]
fn a_claude_parent_hears_through_its_own_inbox() {
    let mut sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let inbox = sandbox.start_inbox();
    let caller = [
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_SESSION_ID", "c1a0de00"),
        ("CLAUDE_CODE_MESSAGING_SOCKET", inbox.as_str()),
        ("CLAUDE_CODE_MESSAGING_TOKEN", TOKEN),
    ];
    let output = subscribe(&sandbox, &caller, CODEX_CHILD, "idle", &json!([]));
    assert!(output.status.success(), "{}", stderr(&output));
    sandbox.wait_for("the idle notice", || sandbox.inbox().len() == 2);
    let lines = sandbox.inbox();
    assert_eq!(lines[0]["token"], TOKEN);
    assert_eq!(lines[1]["message"]["content"], format!("[tj subscribe] Phase 3b (codex:{CODEX_CHILD}) is idle"));
}

#[test]
fn an_embedded_child_is_refused_before_anything_is_watched() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = subscribe(&sandbox, &FROM_CODEX, "", "active", &json!([]));
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("is not loaded in the shared Codex app-server"));
    assert!(sandbox.codex_log().iter().all(|entry| entry["proxy"] != "thread/resume"));
}

#[test]
fn a_claude_child_is_not_subscribable_from_tj() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "Phase 3b", "active")]);
    let output = subscribe(&sandbox, &FROM_CODEX, "", "active", &json!([]));
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("a Claude parent subscribes with SendMessage notify_when_idle"));
}
