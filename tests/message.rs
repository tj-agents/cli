use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CODEX_CHILD: &str = "019a0000-0000-7000-8000-00000000c0de";
const CLAUDE_PEER: &str = "4d86b5f2-0000-4000-8000-00000000c1a0";

const HOST_VARIABLES: [&str; 9] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_PID",
    "CODEX_THREAD_ID",
    "AGENT_CLI_TAB_TITLE",
    "TJ_CACHE_DIR",
    "TJ_STATE_DIR",
];

struct Sandbox {
    _directory: tempfile::TempDir,
    root: PathBuf,
    stubs: PathBuf,
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
    Sandbox { _directory: directory, root, stubs }
}

impl Sandbox {
    fn sessions(&self, sessions: &[Value]) {
        std::fs::write(
            self.stubs.join("agent-store-sessions.json"),
            json!({"schema_version":1, "ok":true, "data":{"title":"x", "sessions":sessions}}).to_string(),
        )
        .unwrap();
    }

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
            .env("TJ_STATE_DIR", self.root.join("state"))
            .envs(caller.iter().copied());
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

struct NativeAdapter {
    child: Child,
    requests: Receiver<Value>,
}

impl NativeAdapter {
    fn start(sandbox: &Sandbox) -> Self {
        let mut child = sandbox
            .command(&["_native-host", "--session", "claude:c1a0de00"], &FROM_CLAUDE)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let value = serde_json::from_str(&line).unwrap();
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let adapter = Self { child, requests };
        let lease = sandbox.root.join("state/native/claude-c1a0de00/host.lock");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(&lease)
                && matches!(fs4::FileExt::try_lock(&file), Err(fs4::TryLockError::WouldBlock))
            {
                return adapter;
            }
            assert!(Instant::now() < deadline, "native adapter failed to acquire its lease");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn request(&self) -> Value {
        self.requests.recv_timeout(Duration::from_secs(8)).expect("native send was not routed to the adapter")
    }
}

impl Drop for NativeAdapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn native_receipt(sandbox: &Sandbox, request: &Value, result: &Value) {
    let output = sandbox.tj(
        &[
            "_native-receipt",
            "--session",
            "claude:c1a0de00",
            "--request",
            request["id"].as_str().unwrap(),
            "--lease",
            request["lease"].as_str().unwrap(),
            "--result",
            &result.to_string(),
        ],
        &FROM_CLAUDE,
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn claude_routes_through_its_native_adapter_and_retries_keep_the_original_receipt() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
    let adapter = NativeAdapter::start(&sandbox);
    let arguments = ["message", "Peer", "result\nsecond line", "--request-id", "stable"];
    let sending =
        sandbox.command(&arguments, &FROM_CLAUDE).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let request = adapter.request();
    assert_eq!(request["session"], "claude:c1a0de00");
    assert_eq!(request["target"], format!("claude:{CLAUDE_PEER}"));
    assert_eq!(request["text"], "result\nsecond line");
    let native = json!({"isDelivered":true});
    native_receipt(&sandbox, &request, &native);
    let output = sending.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["transport"], "native");
    assert_eq!(receipt["state"], "submitted");
    assert_eq!(receipt["native"], native);
    let retry = sandbox.tj(&arguments, &FROM_CLAUDE);
    assert!(retry.status.success(), "{}", stderr(&retry));
    assert_eq!(serde_json::from_slice::<Value>(&retry.stdout).unwrap(), receipt);
    let changed = sandbox.tj(&["message", "Peer", "changed", "--request-id", "stable"], &FROM_CLAUDE);
    assert_eq!(changed.status.code(), Some(2));
    assert!(stderr(&changed).contains("changes the recorded recipient, transport, content or kind"));
    assert!(adapter.requests.try_recv().is_err());
    assert!(!sandbox.root.join("cache/feed").exists());
}

#[test]
fn native_refusal_and_unknown_results_are_preserved_without_feed_fallback() {
    for native in [
        json!({"isDelivered":false,"reason":"Permission was denied"}),
        json!({"error":"Native connection ended before a result"}),
    ] {
        let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
        let adapter = NativeAdapter::start(&sandbox);
        let sending = sandbox
            .command(&["message", "Peer", "hello"], &FROM_CLAUDE)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        native_receipt(&sandbox, &adapter.request(), &native);
        let output = sending.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["native"], native);
        assert_eq!(receipt["state"], if native["isDelivered"] == false { "not-delivered" } else { "unknown" });
        assert!(!sandbox.root.join("cache/feed").exists());
    }
}

#[test]
fn missing_or_crashed_native_adapters_do_not_resend_or_switch_transport() {
    let sandbox = sandbox(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
    let arguments = ["message", "Peer", "hello", "--request-id", "stable"];
    let missing = sandbox.tj(&arguments, &FROM_CLAUDE);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("native adapter is unavailable"));
    let adapter = NativeAdapter::start(&sandbox);
    let sending =
        sandbox.command(&arguments, &FROM_CLAUDE).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    adapter.request();
    drop(adapter);
    let output = sending.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("delivery is unknown"));
    let retry = sandbox.tj(&arguments, &FROM_CLAUDE);
    assert_eq!(retry.status.code(), Some(2));
    assert!(stderr(&retry).contains("delivery is unknown"));
    sandbox.sessions(&[session("codex", CODEX_CHILD, "Peer", "active")]);
    let redirected = sandbox.tj(&arguments, &FROM_CLAUDE);
    assert_eq!(redirected.status.code(), Some(2));
    assert!(stderr(&redirected).contains("changes the recorded recipient"));
    assert!(!sandbox.root.join("cache/feed").exists());
    sandbox.sessions(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
    let restarted = NativeAdapter::start(&sandbox);
    assert!(restarted.requests.recv_timeout(Duration::from_millis(300)).is_err());
    assert!(!sandbox.root.join("cache/feed").exists());
}

#[test]
fn feed_retries_preserve_one_copy_and_refuse_changed_content() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Peer", "active")]);
    let arguments = ["message", "Peer", "hello", "--request-id", "stable"];
    assert!(sandbox.tj(&arguments, &FROM_CODEX).status.success());
    assert!(sandbox.tj(&arguments, &FROM_CODEX).status.success());
    let changed = sandbox.tj(&["message", "Peer", "changed", "--request-id", "stable"], &FROM_CODEX);
    assert_eq!(changed.status.code(), Some(2));
    assert!(stderr(&changed).contains("changes the recorded recipient, transport, content or kind"));
    assert_eq!(sandbox.posted(&format!("codex-{CODEX_CHILD}.jsonl")).len(), 1);
}

#[test]
fn feed_request_retries_refuse_reused_titles_within_and_across_providers() {
    for initial_host in ["codex", "claude"] {
        for replacement_host in ["codex", "claude"] {
            let original = session(initial_host, "original", "Peer", "active");
            let sandbox = sandbox(std::slice::from_ref(&original));
            let arguments = ["message", "Peer", "private result", "--request-id", "stable"];
            let sent = sandbox.tj(&arguments, &FROM_CODEX);
            assert!(sent.status.success(), "{}", stderr(&sent));
            sandbox.sessions(&[
                session(initial_host, "original", "Peer", "ended"),
                session(replacement_host, "replacement", "Peer", "active"),
            ]);
            let retry = sandbox.tj(&arguments, &FROM_CODEX);
            assert_eq!(retry.status.code(), Some(2), "{}", stdout(&retry));
            assert!(stderr(&retry).contains("changes the recorded recipient"));
            assert!(!sandbox.feed(&format!("{replacement_host}-replacement.jsonl")).exists());
            // A subsequent process can still retry the original binding without duplicating delivery.
            sandbox.sessions(&[original]);
            let retry = sandbox.tj(&arguments, &FROM_CODEX);
            assert!(retry.status.success(), "{}", stderr(&retry));
            assert_eq!(sandbox.posted(&format!("{initial_host}-original.jsonl")).len(), 1);
        }
    }
}

#[test]
fn native_receipts_cannot_be_redirected_to_a_feed_by_reusing_a_title() {
    for native in [
        json!({"isDelivered":true}),
        json!({"isDelivered":false,"reason":"Permission was denied"}),
        json!({"error":"Native delivery is unknown"}),
    ] {
        let original = session("claude", CLAUDE_PEER, "Peer", "active");
        let sandbox = sandbox(std::slice::from_ref(&original));
        let adapter = NativeAdapter::start(&sandbox);
        let arguments = ["message", "Peer", "private result", "--request-id", "stable"];
        let sending =
            sandbox.command(&arguments, &FROM_CLAUDE).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        native_receipt(&sandbox, &adapter.request(), &native);
        let output = sending.wait_with_output().unwrap();
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        sandbox.sessions(&[
            session("claude", CLAUDE_PEER, "Peer", "ended"),
            session("codex", CODEX_CHILD, "Peer", "active"),
        ]);
        let retry = sandbox.tj(&arguments, &FROM_CLAUDE);
        assert_eq!(retry.status.code(), Some(2), "{}", stdout(&retry));
        assert!(stderr(&retry).contains("changes the recorded recipient"));
        assert!(!sandbox.root.join("cache/feed").exists());
        // Even after the adapter exits, exact retries return its original receipt rather than resending.
        drop(adapter);
        sandbox.sessions(&[original]);
        let retry = sandbox.tj(&arguments, &FROM_CLAUDE);
        assert_eq!(retry.status.code(), output.status.code(), "{}", stderr(&retry));
        assert_eq!(serde_json::from_slice::<Value>(&retry.stdout).unwrap(), receipt);
    }
}

#[test]
fn a_feed_request_cannot_switch_to_native_when_its_title_changes_provider() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Peer", "active")]);
    let adapter = NativeAdapter::start(&sandbox);
    let arguments = ["message", "Peer", "private result", "--request-id", "stable"];
    let sent = sandbox.tj(&arguments, &FROM_CLAUDE);
    assert!(sent.status.success(), "{}", stderr(&sent));
    sandbox.sessions(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
    // Keep the CLI child bounded even if a regression queues a native send and waits for its receipt.
    let mut retry =
        sandbox.command(&arguments, &FROM_CLAUDE).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    match adapter.requests.recv_timeout(Duration::from_secs(1)) {
        Ok(request) => {
            native_receipt(&sandbox, &request, &json!({"isDelivered":true}));
            let output = retry.wait_with_output().unwrap();
            panic!("retry switched to native: {}", stdout(&output));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(error) => panic!("native adapter ended unexpectedly: {error}"),
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while retry.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = retry.kill();
            let _ = retry.wait();
            panic!("binding refusal did not return");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = retry.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    assert!(stderr(&output).contains("changes the recorded recipient"));
    assert_eq!(sandbox.posted(&format!("codex-{CODEX_CHILD}.jsonl")).len(), 1);
    assert!(adapter.requests.try_recv().is_err());
}

#[test]
fn torn_or_malformed_request_bindings_refuse_dispatch_after_a_process_restart() {
    for damage in ["{\"schema\":1", "{}\n", "not json\n"] {
        let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Peer", "active")]);
        let arguments = ["message", "Peer", "private result", "--request-id", "stable"];
        let sent = sandbox.tj(&arguments, &FROM_CODEX);
        assert!(sent.status.success(), "{}", stderr(&sent));
        let journal = sandbox.root.join("state/messages/bindings.jsonl");
        let intact = std::fs::read_to_string(&journal).unwrap();
        let damaged = format!("{intact}{damage}");
        std::fs::write(&journal, &damaged).unwrap();
        let retry = sandbox.tj(&arguments, &FROM_CODEX);
        assert_eq!(retry.status.code(), Some(2));
        assert!(stderr(&retry).contains("message binding journal"));
        sandbox.sessions(&[session("claude", CLAUDE_PEER, "Peer", "active")]);
        let fresh = sandbox.tj(&["message", "Peer", "another result", "--request-id", "fresh"], &FROM_CODEX);
        assert_eq!(fresh.status.code(), Some(2));
        assert!(stderr(&fresh).contains("message binding journal"));
        assert!(!sandbox.feed(&format!("claude-{CLAUDE_PEER}.jsonl")).exists());
        assert_eq!(sandbox.posted(&format!("codex-{CODEX_CHILD}.jsonl")).len(), 1);
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), damaged);
    }
}

#[test]
fn concurrent_request_retries_deliver_one_copy_per_source() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Peer", "active")]);
    let children: Vec<_> = (0..8)
        .map(|index| {
            let caller = if index % 2 == 0 { &FROM_CODEX[..] } else { &FROM_CLAUDE[..] };
            sandbox
                .command(&["message", "Peer", "private result", "--request-id", "stable"], caller)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let posted = sandbox.posted(&format!("codex-{CODEX_CHILD}.jsonl"));
    assert_eq!(posted.len(), 2);
    for source in ["codex:019a-parent", "claude:c1a0de00"] {
        assert_eq!(posted.iter().filter(|message| message["from"] == source).count(), 1);
    }
}

#[test]
fn routes_without_a_native_sender_use_the_feed_preserving_the_sender() {
    for (host, id, recipient) in
        [("claude", CLAUDE_PEER, &AS_CLAUDE_PEER[..]), ("codex", CODEX_CHILD, &AS_CODEX_CHILD[..])]
    {
        let sandbox = sandbox(&[session(host, id, "Peer", "active")]);
        let mut expected = Vec::new();
        for caller in [&FROM_CLAUDE[..], &FROM_CODEX[..]] {
            if host == "claude" && caller == FROM_CLAUDE {
                continue;
            }
            let output = sandbox.tj(&["message", "Peer", "PR 56 is ready"], caller);
            assert!(output.status.success(), "{}", stderr(&output));
            assert!(stdout(&output).contains("posted to Peer's feed; it reads it when it runs tj feed"));
            expected.push(if caller == FROM_CLAUDE {
                ("claude:c1a0de00", "cli 82")
            } else {
                ("codex:019a-parent", "agent 92")
            });
        }
        let posted = sandbox.posted(&format!("{host}-{id}.jsonl"));
        assert_eq!(posted.len(), expected.len());
        for (message, &(from, name)) in posted.iter().zip(&expected) {
            assert_eq!(message["from"], from);
            assert_eq!(message["name"], name);
            assert_eq!(message["text"], "PR 56 is ready");
        }
        let read = sandbox.tj(&["feed"], recipient);
        assert!(read.status.success(), "{}", stderr(&read));
        let shown = stdout(&read);
        for (from, name) in expected {
            assert!(shown.contains(&format!("{name} ({from}): PR 56 is ready")), "{shown}");
        }
        assert!(!sandbox.root.join("cache").join("bridge").exists());
    }
}

#[test]
fn a_handle_is_looked_up_with_show() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", &format!("codex:{CODEX_CHILD}"), "done"], &FROM_CODEX);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(std::fs::read_to_string(sandbox.stubs.join("agent-store.log")).unwrap().contains("show codex:"));
}

#[test]
fn a_handle_cannot_be_redirected_by_an_inconsistent_store_reply() {
    let sandbox = sandbox(&[session("codex", CODEX_CHILD, "Phase 3b", "active")]);
    let output = sandbox.tj(&["message", "codex:someone-else", "private result"], &FROM_CODEX);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("to a different session"));
    assert!(!sandbox.root.join("cache/feed").exists());
}

#[test]
fn feed_shows_unread_messages_once_with_a_reply_route() {
    for (host, id, recipient) in
        [("claude", CLAUDE_PEER, &AS_CLAUDE_PEER[..]), ("codex", CODEX_CHILD, &AS_CODEX_CHILD[..])]
    {
        let sandbox = sandbox(&[session(host, id, "Phase 3b", "active")]);
        let sent = sandbox.tj(&["message", "Phase 3b", "PR 56 is ready"], &FROM_CODEX);
        assert!(sent.status.success(), "{}", stderr(&sent));
        let first = sandbox.tj(&["feed"], recipient);
        assert!(first.status.success(), "{}", stderr(&first));
        let shown = stdout(&first);
        assert!(shown.contains("agent 92 (codex:019a-parent): PR 56 is ready"), "{shown}");
        assert!(shown.contains("reply: tj message codex:019a-parent \"<text>\""), "{shown}");
        assert_eq!(stdout(&sandbox.tj(&["feed"], recipient)).trim(), "no unread messages");
        let sent = sandbox.tj(&["message", "Phase 3b", "merged"], &FROM_CODEX);
        assert!(sent.status.success(), "{}", stderr(&sent));
        let next = stdout(&sandbox.tj(&["feed"], recipient));
        assert!(next.contains("merged") && !next.contains("PR 56"), "{next}");
    }
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
    for (host, id, recipient) in
        [("claude", CLAUDE_PEER, &AS_CLAUDE_PEER[..]), ("codex", CODEX_CHILD, &AS_CODEX_CHILD[..])]
    {
        let sandbox = sandbox(&[session(host, id, "Phase 3b", "active")]);
        let mut waiting = sandbox
            .command(&["feed", "--wait", "--timeout", "60"], recipient)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let (sender, expected) = if host == "claude" {
            (&FROM_CODEX[..], "agent 92 (codex:019a-parent)")
        } else {
            (&FROM_CLAUDE[..], "cli 82 (claude:c1a0de00)")
        };
        let sent = sandbox.tj(&["message", "Phase 3b", "finished: PR 12"], sender);
        if !sent.status.success() {
            let _ = waiting.kill();
            let _ = waiting.wait();
            panic!("{}", stderr(&sent));
        }
        let started = Instant::now();
        let output = waiting.wait_with_output().unwrap();
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains(&format!("{expected}: finished: PR 12")));
    }
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
    let bindings = sandbox.root.join("state/messages/bindings.jsonl");
    assert_eq!(mode(&bindings), 0o600);
    assert_eq!(mode(bindings.parent().unwrap()), 0o700);
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
fn the_bridge_command_is_removed() {
    let sandbox = sandbox(&[]);
    let output = sandbox.command(&["bridge"], &FROM_CODEX).env("PATH", &sandbox.stubs).output().unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("`tj bridge` is not a tj command"), "{}", stderr(&output));
    assert!(!sandbox.root.join("cache").join("bridge").exists());
}

#[test]
fn concurrent_senders_preserve_every_message_after_a_torn_line() {
    for (host, id, recipient) in
        [("claude", CLAUDE_PEER, &AS_CLAUDE_PEER[..]), ("codex", CODEX_CHILD, &AS_CODEX_CHILD[..])]
    {
        let sandbox = sandbox(&[session(host, id, "Peer", "active")]);
        let feed = sandbox.feed(&format!("{host}-{id}.jsonl"));
        std::fs::create_dir_all(feed.parent().unwrap()).unwrap();
        std::fs::write(&feed, "{\"text\":\"torn").unwrap();
        let messages: Vec<_> =
            (0..12).map(|number| format!("message {number}: {}\nsecond line", "payload ".repeat(1024))).collect();
        let children: Vec<_> = messages
            .iter()
            .enumerate()
            .map(|(number, text)| {
                let sender = if host != "claude" && number % 2 == 0 { &FROM_CLAUDE[..] } else { &FROM_CODEX[..] };
                sandbox
                    .command(&["message", "Peer", text], sender)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{}", stderr(&output));
        }
        let lines = std::fs::read_to_string(&feed).unwrap();
        assert_eq!(lines.lines().count(), messages.len() + 1);
        let posted: Vec<Value> = lines.lines().skip(1).map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(posted.len(), messages.len());
        for (number, text) in messages.iter().enumerate() {
            let matches: Vec<_> = posted.iter().filter(|message| message["text"] == text.as_str()).collect();
            assert_eq!(matches.len(), 1, "message {number} was lost or duplicated");
            assert_eq!(
                matches[0]["from"],
                if host != "claude" && number % 2 == 0 { "claude:c1a0de00" } else { "codex:019a-parent" }
            );
        }
        let output = sandbox.tj(&["feed"], recipient);
        assert!(output.status.success(), "{}", stderr(&output));
        let shown = stdout(&output);
        assert!(shown.contains("passed over 1 line(s) in the feed that were not JSON"));
        for number in 0..messages.len() {
            assert_eq!(shown.matches(&format!("message {number}: ")).count(), 1);
        }
        assert_eq!(stdout(&sandbox.tj(&["feed"], recipient)).trim(), "no unread messages");
    }
}
