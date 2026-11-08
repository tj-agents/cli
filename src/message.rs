use std::ffi::OsStr;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{bridge, run::store};

/// Exit code when nothing was sent because the caller must use its host's native tool.
const NATIVE: u8 = 3;

#[derive(clap::Args)]
pub struct Args {
    /// The recipient: its tab title, or its `host:session-id` handle
    target: String,
    /// The message
    text: String,
}

pub struct Session {
    pub host: String,
    pub id: String,
    pub title: Option<String>,
}

impl Session {
    pub fn handle(&self) -> String {
        format!("{}:{}", self.host, self.id)
    }

    pub fn name(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.handle())
    }

    fn from_row(row: &Value) -> Option<Self> {
        Some(Self {
            host: row["host"].as_str()?.to_owned(),
            id: row["native_id"].as_str()?.to_owned(),
            title: row["title"].as_str().filter(|title| !title.is_empty()).map(str::to_owned),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Claude,
    Codex,
    Shell,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn caller() -> Caller {
    if env("CLAUDECODE").as_deref() == Some("1") {
        Caller::Claude
    } else if env("CODEX_THREAD_ID").is_some() {
        Caller::Codex
    } else {
        Caller::Shell
    }
}

fn caller_handle(caller: Caller) -> Option<String> {
    match caller {
        Caller::Claude => env("CLAUDE_CODE_SESSION_ID").map(|id| format!("claude:{id}")),
        Caller::Codex => env("CODEX_THREAD_ID").map(|id| format!("codex:{id}")),
        Caller::Shell => None,
    }
}

fn header(caller: Caller) -> String {
    let Some(handle) = caller_handle(caller) else {
        return "[tj message from a terminal]".to_owned();
    };
    let name = env("AGENT_CLI_TAB_TITLE").unwrap_or_else(|| handle.clone());
    format!("[tj message from {name}; reply with: tj message {handle} \"<text>\"]")
}

fn is_handle(target: &str) -> bool {
    target
        .split_once(':')
        .is_some_and(|(host, id)| ["claude", "codex"].contains(&host) && !id.is_empty() && !id.contains(' '))
}

pub fn resolve(target: &str) -> Result<Session> {
    let rows = if is_handle(target) {
        vec![store::call(&["show", target])?["session"].clone()]
    } else {
        store::call(&["sessions", "--title", target])?["sessions"]
            .as_array()
            .context("agent-store sessions returned no list")?
            .clone()
    };
    let live: Vec<Session> =
        rows.iter().filter(|row| row["state"].as_str() != Some("ended")).filter_map(Session::from_row).collect();
    match <[Session; 1]>::try_from(live) {
        Ok([session]) => Ok(session),
        Err(live) if live.is_empty() => bail!("no live session is addressed by {target:?}"),
        Err(live) => {
            let handles: Vec<String> = live.iter().map(Session::handle).collect();
            bail!("{} live sessions are titled {target:?}: {}; address one by handle", live.len(), handles.join(", "))
        }
    }
}

/// A host CLI run without a console window, so a detached caller never flashes one.
pub fn host_command(program: &str) -> Result<Command> {
    let executable = which::which(program).with_context(|| format!("`{program}` is not on PATH"))?;
    let mut command = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

pub fn queue(thread: &str, text: &str) -> Result<()> {
    let output = host_command("codex")?
        .args([OsStr::new("queue"), "--thread".as_ref(), thread.as_ref(), "--message".as_ref(), text.as_ref()])
        .stdin(Stdio::null())
        .output()
        .context("could not run codex queue")?;
    if !output.status.success() {
        bail!("codex queue: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

pub fn run(args: Args) -> Result<ExitCode> {
    let target = resolve(&args.target)?;
    let from = caller();
    let name = target.name();
    let text = format!("{} {}", header(from), args.text);
    match (target.host.as_str(), from) {
        ("codex", _) => {
            queue(&target.id, &text)?;
            println!("queued for {name}; delivered at the target's next idle point");
        }
        ("claude", Caller::Claude) => {
            println!("not sent: {name} is a Claude session; send it with the native SendMessage tool, to: {name}");
            return Ok(ExitCode::from(NATIVE));
        }
        ("claude", _) => {
            bridge::post(&target.id, &text).with_context(|| format!("could not reach {name}"))?;
            println!("delivered to {name}'s inbox; it starts a turn if idle, or reads it between tool calls");
        }
        (host, _) => bail!("{name} runs on {host}, which tj message cannot reach"),
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::is_handle;

    #[test]
    fn handles_are_host_prefixed_ids_and_titles_are_everything_else() {
        assert!(is_handle("codex:019a-77"));
        assert!(is_handle("claude:4d86b5f2"));
        assert!(!is_handle("Postgres sweep: Search"));
        assert!(!is_handle("codex: spaced"));
        assert!(!is_handle("cli 82"));
    }
}
