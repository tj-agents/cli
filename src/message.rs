use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::feed::{self, Feed};
use crate::{bridge, host::Host, run::store};

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The recipient: its tab title, or its `host:session-id` handle
    target: String,
    /// The message
    text: String,
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
        Ok([session]) => Ok(session),
        Err(live) if live.is_empty() => bail!("no live session is addressed by {target:?}"),
        Err(live) => {
            let handles: Vec<String> = live.iter().map(Session::handle).collect();
            bail!("{} live sessions are titled {target:?}: {}; address one by handle", live.len(), handles.join(", "))
        }
    }
}

/// The line a Claude session sees in its inbox: who sent it and how to reply, then the text.
fn inbox_text(from: Option<&str>, name: Option<&str>, text: &str) -> String {
    match from {
        Some(handle) => {
            let name = name.unwrap_or(handle);
            format!("[tj message from {name}; reply with: tj message {handle} \"<text>\"] {text}")
        }
        None => format!("[tj message from a terminal] {text}"),
    }
}

/// A Claude target gets it in its native inbox through its session relay; a Codex target, which has no native
/// messaging, gets it in its feed.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let target = resolve(&args.target)?;
    let from = Host::current().and_then(Host::handle);
    let name = env("AGENT_CLI_TAB_TITLE").or_else(|| from.clone());
    match Host::from_name(&target.host) {
        Some(Host::Claude) => {
            bridge::post(&target.id, &inbox_text(from.as_deref(), name.as_deref(), &args.text))
                .with_context(|| format!("could not reach {}", target.name()))?;
            println!("delivered to {}'s inbox", target.name());
        }
        Some(Host::Codex) => {
            let at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            Feed::new(&feed::folder()?, &target.handle())?
                .post(&json!({"at": at, "from": from, "name": name, "text": &args.text}))?;
            println!("posted to {}'s feed; it reads it when it runs tj feed", target.name());
        }
        None => bail!("{} runs on {}, which tj message cannot reach", target.name(), target.host),
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
