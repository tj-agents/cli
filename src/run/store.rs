use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Launch, hidden};
use crate::host::Host;

pub(crate) fn call(arguments: &[&str]) -> Result<Value> {
    let output =
        hidden("agent-store").args(arguments).stdin(Stdio::null()).output().context("could not run agent-store")?;
    let reply: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("agent-store {} printed no JSON", arguments[0]))?;
    if reply["ok"] != Value::Bool(true) {
        bail!("agent-store {}: {}", arguments[0], reply["error"]["message"].as_str().unwrap_or("failed"));
    }
    Ok(reply["data"].clone())
}

pub fn launch(launch: &Launch, parent: Option<&str>, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let cwd = launch.cwd.to_string_lossy();
    let pid = wrapper_pid.to_string();
    let mut arguments = vec![
        "launch",
        "--launch-id",
        &launch.id,
        "--host",
        launch.host.name(),
        "--title",
        &launch.title,
        "--cwd",
        &cwd,
        "--worktree",
        &cwd,
        "--wrapper-pid",
        &pid,
        "--wrapper-started-at",
        wrapper_started_at,
    ];
    if let Some(item) = &launch.item {
        arguments.extend(["--item", item]);
    }
    if let Some(parent) = parent {
        arguments.extend(["--parent", parent]);
    }
    let terminal = terminal();
    if let Some((kind, id)) = &terminal {
        arguments.extend(["--terminal-kind", kind, "--terminal-id", id]);
    }
    call(&arguments).map(drop)
}

pub fn recorded(launch_id: &str) -> bool {
    call(&["show", "--launch-id", launch_id]).is_ok()
}

fn terminal() -> Option<(&'static str, String)> {
    let present = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
    present("WT_SESSION").map(|id| ("windows-terminal", id)).or_else(|| present("TMUX_PANE").map(|id| ("tmux", id)))
}

fn state(row: &Value) -> Option<&str> {
    row["state"].as_str()
}

pub fn closing(launch_id: &str) -> Result<bool> {
    let data = call(&["show", "--launch-id", launch_id])?;
    Ok(state(&data["launch"]) == Some("closing") || state(&data["session"]) == Some("closing"))
}

pub fn bound_session(launch_id: &str) -> Result<Option<String>> {
    let data = call(&["show", "--launch-id", launch_id])?;
    Ok(data["session"]["native_id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned))
}

pub fn worktree(launch: &Launch) -> Result<PathBuf> {
    let data = call(&["show", "--launch-id", &launch.id])?;
    let session = &data["session"];
    let Some(worktree) = session["worktree"].as_str() else {
        return Ok(launch.cwd.clone());
    };
    if data["launch"]["launch_id"].as_str() != Some(launch.id.as_str())
        || session["launch_id"].as_str() != Some(launch.id.as_str())
        || session["host"].as_str() != Some(launch.host.name())
        || session["native_id"].as_str().is_none_or(str::is_empty)
    {
        bail!("session worktree is not bound to {} launch {}", launch.host.name(), launch.id);
    }
    let worktree = PathBuf::from(worktree);
    if !worktree.is_absolute() {
        bail!("bound session worktree is not absolute: {}", worktree.display());
    }
    Ok(worktree)
}

pub fn end(launch_id: &str, reason: &str) -> Result<()> {
    call(&["end", "--launch-id", launch_id, "--reason", reason]).map(drop)
}

pub fn mark_closing(item: &str) -> Result<()> {
    call(&["mark-closing", "--item", item]).map(drop)
}

pub struct Row {
    pub launch: Launch,
    pub closing: bool,
    pub wrapper_pid: u32,
    pub wrapper_started_at: String,
}

pub fn open_launches(item: Option<&str>) -> Result<Vec<Row>> {
    let mut arguments = vec!["launches"];
    if let Some(item) = item {
        arguments.extend(["--item", item]);
    }
    let data = call(&arguments)?;
    let rows = data["launches"].as_array().context("agent-store launches returned no list")?;
    Ok(rows.iter().filter(|row| state(row) != Some("ended")).filter_map(row).collect())
}

fn row(value: &Value) -> Option<Row> {
    let text = |field: &Value| field.as_str().map(str::to_owned);
    Some(Row {
        launch: Launch {
            id: text(&value["launch_id"])?,
            host: Host::from_name(value["host"].as_str()?)?,
            title: text(&value["title"]).unwrap_or_default(),
            cwd: PathBuf::from(text(&value["worktree"]).or_else(|| text(&value["cwd"]))?),
            item: text(&value["item"]["ref"]),
        },
        closing: state(value) == Some("closing"),
        wrapper_pid: u32::try_from(value["wrapper"]["pid"].as_u64()?).ok()?,
        wrapper_started_at: text(&value["wrapper"]["started_at"])?,
    })
}
