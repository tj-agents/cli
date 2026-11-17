use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::feed::{self, Feed};
use crate::{host::Host, paths, run::store};

mod binding;
pub(crate) mod bridge;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The recipient: its tab title, or its `host:session-id` handle
    target: String,
    /// The message
    text: String,
    /// Reuse this identity only to inspect/retry the exact same message safely.
    #[arg(long)]
    request_id: Option<String>,
}

struct Session {
    host: String,
    id: String,
    title: Option<String>,
}

impl Session {
    fn handle(&self) -> String {
        format!("{}:{}", self.host, self.id)
    }

    fn name(&self) -> String {
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

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn is_handle(target: &str) -> bool {
    target
        .split_once(':')
        .is_some_and(|(host, id)| ["claude", "codex"].contains(&host) && !id.is_empty() && !id.contains(' '))
}

fn resolve(target: &str) -> Result<Session> {
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
        Ok([session]) if is_handle(target) && session.handle() != target => {
            bail!("agent-store resolved {target:?} to a different session: {}", session.handle());
        }
        Ok([session]) => Ok(session),
        Err(live) if live.is_empty() => bail!("no live session is addressed by {target:?}"),
        Err(live) => {
            let handles: Vec<String> = live.iter().map(Session::handle).collect();
            bail!("{} live sessions are titled {target:?}: {}; address one by handle", live.len(), handles.join(", "))
        }
    }
}

pub(crate) fn send(
    target: &str,
    from: Option<&str>,
    name: Option<&str>,
    text: &str,
    event: &Value,
    kind: &str,
) -> Result<Value> {
    let target = resolve(target)?;
    if Host::from_name(&target.host).is_none() {
        bail!("{} runs on {}, which tj message cannot reach", target.name(), target.host);
    }
    crate::completion::Identity::new("message", target.handle())?;
    let native = target.host == "claude" && from.is_some_and(|source| source.starts_with("claude:"));
    let transport = if native { "native" } else { "feed" };
    binding::bind(
        &paths::state_dir()?.join("messages"),
        &json!({
            "schema":1, "source":from, "event":event, "target":target.handle(),
            "transport":transport, "text":text, "kind":kind,
        }),
    )?;
    if native {
        return bridge::send(from.context("native message needs a source session")?, &target.handle(), text, event);
    }
    let at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    Feed::new(&feed::folder()?, &target.handle())?
        .post_once(event, &json!({"at":at, "from":from, "name":name, "text":text, "kind":kind}))?;
    Ok(json!({"transport":"feed", "state":"stored", "target":target.handle(), "name":target.name(), "event":event}))
}

/// All callers use the same route. Native refusal or uncertainty never falls back to a feed.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let from = Host::current().and_then(Host::handle);
    let name = env("AGENT_CLI_TAB_TITLE").or_else(|| from.clone());
    let id = args.request_id.clone().map(Ok).unwrap_or_else(crate::run::generated_id)?;
    let event = json!({"kind":"message", "source":from, "id":id});
    let receipt = send(&args.target, from.as_deref(), name.as_deref(), &args.text, &event, "message")?;
    if receipt["transport"] == "feed" {
        println!(
            "posted to {}'s feed; it reads it when it runs tj feed",
            receipt["name"].as_str().unwrap_or("recipient")
        );
    } else {
        println!("{receipt}");
    }
    Ok(if matches!(receipt["state"].as_str(), Some("stored" | "submitted")) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
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
