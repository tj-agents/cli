use std::collections::VecDeque;
use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc as async_mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self, Message};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A host CLI run without a console window, so a detached caller never flashes one.
pub(crate) fn host_command(program: &str) -> anyhow::Result<Command> {
    let executable = which::which(program).with_context(|| format!("`{program}` is not on PATH"))?;
    let command = Command::new(executable);
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    Ok(command)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("could not find the host executable")]
    Host(#[source] anyhow::Error),
    #[error("{operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{operation}")]
    WebSocket {
        operation: &'static str,
        #[source]
        source: Box<tungstenite::Error>,
    },
    #[error("{0}: the app-server did not answer")]
    Timeout(String),
    #[error("the app-server connection closed")]
    Closed,
    #[error("the app-server sent invalid JSON")]
    Json(#[source] serde_json::Error),
    #[error("{method}: {message}")]
    Remote { method: String, message: String },
}

/// A WebSocket connection to the shared daemon through the proxy's raw byte pipes.
pub(crate) struct Server {
    outgoing: async_mpsc::UnboundedSender<Value>,
    incoming: Receiver<Result<Value, Error>>,
    pending: VecDeque<Value>,
    shutdown: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    next_id: u64,
    request_timeout: Duration,
}

impl Server {
    pub(crate) fn connect() -> Result<Self, Error> {
        let mut command = host_command("codex").map_err(Error::Host)?;
        command.args(["app-server", "proxy"]);
        Self::connect_command(command, REQUEST_TIMEOUT)
    }

    pub(crate) fn connect_socket(socket: &std::path::Path) -> Result<Self, Error> {
        let mut command = host_command("codex").map_err(Error::Host)?;
        command.args(["app-server", "proxy", "--sock"]).arg(socket);
        Self::connect_command(command, Duration::from_secs(3))
    }

    pub(crate) fn connect_command(command: Command, request_timeout: Duration) -> Result<Self, Error> {
        #[cfg(windows)]
        let command = contained_command(command)?;
        #[cfg(unix)]
        let command = contained_command(command);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| Error::Io { operation: "could not create the app-server runtime", source })?;
        let (outgoing, commands) = async_mpsc::unbounded_channel();
        let (sender, incoming) = mpsc::channel();
        let (shutdown, stopping) = oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("codex-proxy".into())
            .spawn(move || {
                runtime.block_on(async {
                    if let Err(error) = proxy(command, commands, &sender, stopping, request_timeout).await {
                        let _ = sender.send(Err(error));
                    }
                });
                // A broken launcher must not make runtime teardown wait forever on pipe I/O.
                runtime.shutdown_timeout(Duration::from_millis(200));
            })
            .map_err(|source| Error::Io { operation: "could not start the app-server worker", source })?;
        let mut server = Self {
            outgoing,
            incoming,
            pending: VecDeque::new(),
            shutdown: Some(shutdown),
            worker: Some(worker),
            next_id: 0,
            request_timeout,
        };
        // Initialization's deadline also bounds startup and the HTTP upgrade.
        server.request("initialize", json!({"clientInfo": {"name": "tj", "version": env!("CARGO_PKG_VERSION")}}))?;
        server.send(json!({"method": "initialized"}))?;
        Ok(server)
    }

    fn send(&self, message: Value) -> Result<(), Error> {
        self.outgoing.send(message).map_err(|_| Error::Closed)
    }

    pub(crate) fn next(&mut self, deadline: Instant) -> Result<Option<Value>, Error> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(Some(message));
        }
        match self.incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            _ => Err(Error::Closed),
        }
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + self.request_timeout;
        let mut deferred = std::mem::take(&mut self.pending);
        let result = (|| {
            loop {
                if Instant::now() >= deadline {
                    return Err(Error::Timeout(method.into()));
                }
                let Some(reply) = self.next(deadline)? else { return Err(Error::Timeout(method.into())) };
                // Server requests also carry an id; they must remain available to the caller.
                if reply.get("method").is_some() || reply["id"].as_u64() != Some(id) {
                    deferred.push_back(reply);
                    continue;
                }
                if let Some(error) = reply.get("error") {
                    return Err(Error::Remote {
                        method: method.into(),
                        message: error["message"].as_str().unwrap_or("failed").into(),
                    });
                }
                return Ok(reply["result"].clone());
            }
        })();
        self.pending = deferred;
        result
    }
}

#[cfg(windows)]
fn contained_command(command: Command) -> Result<Command, Error> {
    let mut launcher = host_command("tj_hook").map_err(Error::Host)?;
    launcher.arg("--").arg(command.get_program()).args(command.get_args());
    if let Some(directory) = command.get_current_dir() {
        launcher.current_dir(directory);
    }
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            launcher.env(name, value);
        } else {
            launcher.env_remove(name);
        }
    }
    Ok(launcher)
}

#[cfg(unix)]
fn contained_command(mut command: Command) -> Command {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    command
}

#[cfg(unix)]
fn terminate_group(pid: u32) {
    if let Ok(raw) = i32::try_from(pid)
        && raw > 1
        && let Some(group) = rustix::process::Pid::from_raw(raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

async fn proxy(
    command: Command,
    mut outgoing: async_mpsc::UnboundedReceiver<Value>,
    incoming: &mpsc::Sender<Result<Value, Error>>,
    mut stopping: oneshot::Receiver<()>,
    request_timeout: Duration,
) -> Result<(), Error> {
    let mut child = tokio::process::Command::from(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| Error::Io { operation: "could not run codex app-server proxy", source })?;
    // Both pipes are guaranteed by Stdio::piped above.
    let stream = tokio::io::join(
        child.stdout.take().expect("proxy stdout is piped"),
        child.stdin.take().expect("proxy stdin is piped"),
    );
    let result = async {
        // The URI supplies the HTTP request target and Host header; the transport remains the pipes.
        let upgrade = tokio_tungstenite::client_async("ws://localhost/", stream);
        let (mut socket, _) = timeout(request_timeout, upgrade)
            .await
            .map_err(|_| Error::Timeout("WebSocket upgrade".into()))?
            .map_err(|source| Error::WebSocket {
                operation: "the app-server WebSocket upgrade failed",
                source: Box::new(source),
            })?;
        loop {
            tokio::select! {
                message = outgoing.recv() => {
                    let Some(message) = message else { return Ok(()) };
                    timeout(request_timeout, socket.send(Message::text(message.to_string())))
                        .await
                        .map_err(|_| Error::Timeout("WebSocket write".into()))?
                        .map_err(|source| Error::WebSocket { operation: "the app-server WebSocket write failed", source: Box::new(source) })?;
                }
                message = socket.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let value = serde_json::from_str(&text).map_err(Error::Json)?;
                            incoming.send(Ok(value)).map_err(|_| Error::Closed)?;
                        }
                        Some(Ok(Message::Ping(_))) => {
                            // Tungstenite queues the pong; flush it promptly.
                            timeout(request_timeout, socket.flush()).await
                                .map_err(|_| Error::Timeout("WebSocket control reply".into()))?
                                .map_err(|source| Error::WebSocket { operation: "the app-server WebSocket control reply failed", source: Box::new(source) })?;
                        }
                        Some(Ok(Message::Close(_))) => {
                            // Reply to the close, then fail without waiting for the proxy's EOF.
                            let _ = timeout(Duration::from_millis(100), socket.flush()).await;
                            return Err(Error::Closed);
                        }
                        Some(Ok(_)) => {}
                        Some(Err(source)) => return Err(Error::WebSocket { operation: "the app-server WebSocket read failed", source: Box::new(source) }),
                        None => return Err(Error::Closed),
                    }
                }
            }
        }
    };
    let outcome = tokio::select! {
        _ = &mut stopping => Ok(()),
        result = result => result,
    };
    let peer_closed = matches!(&outcome, Err(Error::Closed));
    #[cfg(unix)]
    let proxy_group = child.id();
    // Notify a waiting request before process cleanup, including when the peer sent Close.
    if let Err(error) = outcome {
        let _ = incoming.send(Err(error));
    }
    if peer_closed {
        // Let the peer consume the flushed Close reply before terminating its proxy.
        // A peer that keeps its pipes open still has a bounded cleanup deadline.
        let _ = timeout(Duration::from_secs(1), child.wait()).await;
    }
    // The Windows helper owns a job; Unix launches in a dedicated process group.
    // Terminate descendants before waiting for Tokio's pipe readers to finish.
    #[cfg(unix)]
    if let Some(pid) = proxy_group {
        terminate_group(pid);
    }
    let _ = child.start_kill();
    let _ = timeout(Duration::from_secs(1), child.wait()).await;
    Ok(())
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(log: &std::path::Path, scenario: &str) -> Command {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let mut command = host_command(python).unwrap();
        command
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.py"))
            .args(["app-server", "proxy"])
            .env("FAKE_CODEX_LOG", log)
            .env("FAKE_CODEX_SCENARIO", scenario);
        command
    }

    fn connect(log: &std::path::Path) -> Server {
        Server::connect_command(fixture(log, "normal"), Duration::from_secs(5)).unwrap()
    }

    fn next(server: &mut Server) -> Value {
        server.next(Instant::now() + Duration::from_secs(5)).unwrap().unwrap()
    }

    fn assert_proxy_stopped(log: &std::path::Path) {
        let events = std::fs::read_to_string(log).unwrap();
        let pid = serde_json::from_str::<Value>(events.lines().next().unwrap()).unwrap()["pid"].as_u64().unwrap();
        assert!(tj_cli::process::agent::started_at(pid as u32).unwrap().is_none(), "proxy {pid} was left running");
    }

    #[test]
    fn websocket_upgrade_masking_fragmentation_and_async_messages() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("proxy.jsonl");
        let mut server = connect(&log);
        let payload = json!({"text": "é".repeat(70_000)});
        assert_eq!(server.request("test/echo", payload.clone()).unwrap(), payload);
        assert_eq!(next(&mut server)["method"], "test/early");
        let request = next(&mut server);
        assert_eq!(request["id"], 1);
        assert_eq!(request["method"], "test/server-request");
        assert_eq!(next(&mut server)["method"], "test/idle");
        assert!(server.next(Instant::now() + Duration::from_millis(30)).unwrap().is_none());
        let result = server.request("test/error", json!({}));
        assert!(
            matches!(result, Err(Error::Remote { method, message }) if method == "test/error" && message == "fixture rejected request")
        );
        drop(server);
        assert_proxy_stopped(&log);
        let events = std::fs::read_to_string(log).unwrap();
        assert!(events.contains(r#""websocket": "upgrade""#));
        assert_eq!(events.matches(r#""websocket": "masked-pong""#).count(), 3);
    }

    #[test]
    fn an_idle_connection_delivers_notifications_without_a_request() {
        let directory = tempfile::tempdir().unwrap();
        let mut server = connect(&directory.path().join("proxy.jsonl"));
        assert_eq!(next(&mut server)["method"], "test/early");
        assert_eq!(next(&mut server)["method"], "test/server-request");
        assert_eq!(next(&mut server)["method"], "test/idle");
    }

    #[test]
    fn a_stalled_upgrade_or_initialize_times_out_and_reaps_the_proxy() {
        for scenario in ["silent-upgrade", "silent-initialize"] {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("proxy.jsonl");
            let started = Instant::now();
            let result = Server::connect_command(fixture(&log, scenario), Duration::from_secs(1));
            assert!(matches!(result, Err(Error::Timeout(_))), "{scenario}: unexpected result");
            assert!(started.elapsed() < Duration::from_secs(4), "{scenario}: cleanup blocked after timeout");
            assert_proxy_stopped(&log);
        }
    }

    #[test]
    fn rejected_or_invalid_websocket_upgrades_fail_and_reap_the_proxy() {
        for scenario in ["reject-upgrade", "bad-accept"] {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("proxy.jsonl");
            let result = Server::connect_command(fixture(&log, scenario), Duration::from_secs(5));
            assert!(matches!(result, Err(Error::WebSocket { .. })), "{scenario}: invalid upgrade was accepted");
            assert_proxy_stopped(&log);
        }
    }

    #[test]
    fn a_stalled_request_keeps_its_deadline_and_drop_cancels_the_worker() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("proxy.jsonl");
        let mut server = connect(&log);
        server.request_timeout = Duration::from_millis(100);
        let result = server.request("test/timeout", json!({}));
        assert!(matches!(result, Err(Error::Timeout(method)) if method == "test/timeout"));
        let started = Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(2), "drop blocked on an idle pipe");
        assert_proxy_stopped(&log);
    }

    #[test]
    fn close_eof_and_invalid_json_reach_the_waiting_request() {
        for method in ["test/close", "test/close-delayed", "test/abrupt", "test/malformed"] {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("proxy.jsonl");
            let mut server = connect(&log);
            let result = server.request(method, json!({}));
            assert!(
                matches!(result, Err(Error::Closed | Error::WebSocket { .. } | Error::Json(_))),
                "{method}: unexpected result"
            );
            drop(server);
            assert_proxy_stopped(&log);
            if matches!(method, "test/close" | "test/close-delayed") {
                assert!(std::fs::read_to_string(&log).unwrap().contains("close-acknowledged"));
            }
        }
    }

    #[test]
    fn a_close_frame_fails_promptly_even_when_the_peer_keeps_its_pipes_open() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("proxy.jsonl");
        let mut server = connect(&log);
        let started = Instant::now();
        let result = server.request("test/close-hold", json!({}));
        assert!(matches!(result, Err(Error::Closed)), "expected immediate close, got {result:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "close waited for the peer's EOF");
        drop(server);
        assert_proxy_stopped(&log);
    }

    #[test]
    fn a_stuck_launcher_and_its_pipe_holding_descendant_are_terminated() {
        const PROBE: &str = "TJ_TEST_STUCK_PROXY_LOG";
        if let Some(log) = std::env::var_os(PROBE) {
            let log = std::path::PathBuf::from(log);
            #[cfg(windows)]
            let command = {
                let python = which::which("python").unwrap();
                let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.py");
                let batch = log.with_extension("cmd");
                std::fs::write(
                    &batch,
                    format!("@echo off\r\n\"{}\" \"{}\" %*\r\n", python.display(), script.display()),
                )
                .unwrap();
                let mut command = Command::new(batch);
                command
                    .args(["app-server", "proxy"])
                    .env("FAKE_CODEX_LOG", &log)
                    .env("FAKE_CODEX_SCENARIO", "stuck-launcher");
                command
            };
            #[cfg(unix)]
            let command = fixture(&log, "stuck-launcher");
            let result = Server::connect_command(command, Duration::from_secs(1));
            assert!(matches!(result, Err(Error::Timeout(_))), "stuck launcher did not time out");
            return;
        }

        // Run the potentially hanging regression in its own contained process so a
        // failure can be stopped without affecting this test runner or other sessions.
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("proxy.jsonl");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "app_server::tests::a_stuck_launcher_and_its_pipe_holding_descendant_are_terminated"])
            .env(PROBE, &log);
        #[cfg(windows)]
        let mut command = contained_command(command).unwrap();
        #[cfg(unix)]
        let mut command = contained_command(command);
        let mut child = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                #[cfg(unix)]
                terminate_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                panic!("stuck proxy kept the worker alive after its deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "contained proxy probe failed: {status}");
        let events = std::fs::read_to_string(&log).unwrap();
        let pids: Vec<u32> = events
            .lines()
            .filter_map(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                event.get("pid").or_else(|| event.get("descendant")).and_then(Value::as_u64).map(|pid| pid as u32)
            })
            .collect();
        assert_eq!(pids.len(), 2, "expected launcher and descendant: {events}");
        for pid in pids {
            assert!(
                tj_cli::process::agent::started_at(pid).unwrap().is_none(),
                "proxy process {pid} survived cancellation"
            );
        }
    }
}
