use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::app_server::Server;
use crate::bridge;
use crate::host::Host;
use crate::message;

const LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(clap::Args)]
pub struct Args {
    /// The child: its tab title, or its `host:session-id` handle
    target: String,
    #[arg(long, hide = true)]
    watch: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Idle,
    Exited,
    Expired,
}

fn event(thread: &str, message: &Value) -> Option<Event> {
    if message["params"]["threadId"].as_str() != Some(thread) {
        return None;
    }
    match message["method"].as_str()? {
        "thread/closed" => Some(Event::Exited),
        "thread/status/changed" => status(&message["params"]["status"]),
        _ => None,
    }
}

fn status(status: &Value) -> Option<Event> {
    match status["type"].as_str()? {
        "idle" => Some(Event::Idle),
        "notLoaded" => Some(Event::Exited),
        _ => None,
    }
}

fn wait(server: &mut Server, thread: &str, lifetime: Duration) -> Result<Event> {
    let resumed = server.request("thread/resume", json!({"threadId": thread, "excludeTurns": true}))?;
    if let Some(now) = status(&resumed["thread"]["status"]) {
        return Ok(now);
    }
    let deadline = Instant::now() + lifetime;
    loop {
        let Some(message) = server.next(deadline)? else { return Ok(Event::Expired) };
        if let Some(event) = event(thread, &message) {
            let _ = server.request("thread/unsubscribe", json!({"threadId": thread}));
            return Ok(event);
        }
    }
}

/// Inherited pipes would keep the caller's tool call open until the watcher exits.
fn detach(command: &mut Command) -> Result<()> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().context("could not start a background process")?;
    Ok(())
}

fn notify(caller: Host, text: &str) -> Result<()> {
    match caller {
        Host::Claude => bridge::post_own(text),
        Host::Codex => message::queue(&caller.session_id().context("CODEX_THREAD_ID is not set")?, text),
    }
}

pub fn run(args: Args) -> Result<ExitCode> {
    let Some(caller) = Host::current() else {
        bail!("tj subscribe needs a Claude or Codex parent session to notify");
    };
    let child = message::resolve(&args.target)?;
    if child.host != "codex" {
        bail!(
            "{} is a Claude session: a Claude parent subscribes with SendMessage notify_when_idle, and a Claude child \
             reports to a Codex parent with tj message",
            child.name()
        );
    }
    if args.watch {
        let mut server = Server::connect()?;
        let outcome = wait(&mut server, &child.id, LIFETIME);
        let what = match outcome {
            Ok(Event::Idle) => "is idle".to_owned(),
            Ok(Event::Exited) => "has exited".to_owned(),
            Ok(Event::Expired) => "sent no idle or exit event within 12 hours; the subscription expired".to_owned(),
            Err(error) => format!("can no longer be watched: {error:#}"),
        };
        notify(caller, &format!("[tj subscribe] {} ({}) {what}", child.name(), child.handle()))?;
        return Ok(ExitCode::SUCCESS);
    }
    if !Server::connect()?.loaded(&child.id)? {
        bail!(
            "{} is not loaded in the shared Codex app-server (it runs embedded), so its events cannot be watched",
            child.name()
        );
    }
    detach(Command::new(std::env::current_exe()?).args(["subscribe", &child.handle(), "--watch"]))?;
    println!("subscribed: one notice arrives when {} next goes idle or exits", child.name());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Event, event};

    #[test]
    fn only_the_watched_threads_idle_and_close_events_count() {
        let changed = |thread: &str, kind: &str| json!({"method": "thread/status/changed", "params": {"threadId": thread, "status": {"type": kind}}});
        assert_eq!(event("t", &changed("t", "idle")), Some(Event::Idle));
        assert_eq!(event("t", &changed("t", "notLoaded")), Some(Event::Exited));
        assert_eq!(event("t", &changed("t", "active")), None);
        assert_eq!(event("t", &changed("other", "idle")), None);
        assert_eq!(event("t", &json!({"method": "thread/closed", "params": {"threadId": "t"}})), Some(Event::Exited));
    }
}
