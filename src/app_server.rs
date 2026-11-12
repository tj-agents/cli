use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::message;

/// A connection to the shared Codex app-server through `codex app-server proxy`.
pub(crate) struct Server {
    child: Child,
    input: ChildStdin,
    lines: Receiver<Value>,
    next_id: u64,
}

impl Server {
    pub(crate) fn connect() -> Result<Self> {
        let mut child = message::host_command("codex")?
            .args(["app-server", "proxy"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not run codex app-server proxy")?;
        let input = child.stdin.take().context("no proxy stdin")?;
        let output = child.stdout.take().context("no proxy stdout")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && sender.send(value).is_err()
                {
                    break;
                }
            }
        });
        let mut server = Self { child, input, lines, next_id: 0 };
        server.request("initialize", json!({"clientInfo": {"name": "tj", "version": env!("CARGO_PKG_VERSION")}}))?;
        server.send(&json!({"method": "initialized"}))?;
        Ok(server)
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        writeln!(self.input, "{message}").context("the app-server proxy closed")?;
        Ok(self.input.flush()?)
    }

    pub(crate) fn next(&self, deadline: Instant) -> Result<Option<Value>> {
        match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(value) => Ok(Some(value)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("the app-server connection closed"),
        }
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let Some(reply) = self.next(deadline)? else { bail!("{method}: the app-server did not answer") };
            if reply["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                bail!("{method}: {}", error["message"].as_str().unwrap_or("failed"));
            }
            return Ok(reply["result"].clone());
        }
    }

    pub(crate) fn loaded(&mut self, thread: &str) -> Result<bool> {
        let result = self.request("thread/loaded/list", json!({}))?;
        Ok(result["data"].as_array().is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(thread))))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
