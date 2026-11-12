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

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field].as_str().with_context(|| format!("agent-store response has no {field}"))
}

fn active(value: &Value, subject: &str) -> Result<()> {
    if text(value, "state")? != "active" {
        bail!("agent-store {subject} is not active");
    }
    Ok(())
}

fn exact_launch(data: &Value, launch: &Launch, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let row = &data["launch"];
    let cwd = launch.cwd.to_str().context("launch cwd is not UTF-8")?;
    if text(row, "launch_id")? != launch.id
        || text(row, "host")? != launch.host.name()
        || text(row, "title")? != launch.title
        || text(row, "cwd")? != cwd
        || row["wrapper"]["pid"].as_u64() != Some(u64::from(wrapper_pid))
        || text(&row["wrapper"], "started_at")? != wrapper_started_at
    {
        bail!("agent-store launch does not match this wrapper");
    }
    active(row, "launch")
}

fn exact_binding(
    data: &Value,
    launch: &Launch,
    handle: &str,
    wrapper_pid: u32,
    wrapper_started_at: &str,
) -> Result<()> {
    exact_launch(data, launch, wrapper_pid, wrapper_started_at)?;
    let session = &data["session"];
    let native_id = handle.strip_prefix("codex:").context("prebound session is not codex")?;
    if text(&data["launch"], "session")? != handle
        || text(session, "session")? != handle
        || text(session, "native_id")? != native_id
        || text(session, "host")? != launch.host.name()
        || text(session, "launch_id")? != launch.id
    {
        bail!("agent-store session is not reciprocally bound to this launch");
    }
    active(session, "session")
}

fn launch_has_no_foreign_binding(
    data: &Value,
    launch: &Launch,
    handle: &str,
    wrapper_pid: u32,
    wrapper_started_at: &str,
) -> Result<()> {
    exact_launch(data, launch, wrapper_pid, wrapper_started_at)?;
    match &data["launch"]["session"] {
        Value::Null => Ok(()),
        Value::String(session) if session == handle => Ok(()),
        _ => bail!("agent-store launch is bound to another session"),
    }
}

fn register_arguments(launch: &Launch, handle: &str) -> Result<Vec<String>> {
    let cwd = launch.cwd.to_str().context("launch cwd is not UTF-8")?;
    Ok(vec![
        "register".to_owned(),
        "--session".to_owned(),
        handle.to_owned(),
        "--cwd".to_owned(),
        cwd.to_owned(),
        "--launch-id".to_owned(),
        launch.id.clone(),
    ])
}

pub fn prebind(launch: &Launch, handle: &str, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let before = call(&["show", "--launch-id", &launch.id])?;
    launch_has_no_foreign_binding(&before, launch, handle, wrapper_pid, wrapper_started_at)?;
    let arguments = register_arguments(launch, handle)?;
    let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    call(&references)?;
    let by_launch = call(&["show", "--launch-id", &launch.id])?;
    exact_binding(&by_launch, launch, handle, wrapper_pid, wrapper_started_at)?;
    let by_session = call(&["show", handle])?;
    exact_binding(&by_session, launch, handle, wrapper_pid, wrapper_started_at)
}

pub fn end_not_started(launch: &Launch, handle: &str, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let data = call(&["show", "--launch-id", &launch.id])?;
    launch_has_no_foreign_binding(&data, launch, handle, wrapper_pid, wrapper_started_at)?;
    if matches!(&data["launch"]["session"], Value::String(session) if session == handle) {
        exact_binding(&data, launch, handle, wrapper_pid, wrapper_started_at)?;
        let by_session = call(&["show", handle])?;
        exact_binding(&by_session, launch, handle, wrapper_pid, wrapper_started_at)?;
    }
    end(&launch.id, "not_started")
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

#[derive(Debug, PartialEq, Eq)]
pub struct Ownership {
    pub session: String,
    pub paths: Vec<PathBuf>,
    pub location: Option<PathBuf>,
}

pub fn ownership(launch: &Launch) -> Result<Ownership> {
    let data = call(&["show", "--launch-id", &launch.id])?;
    let session = &data["session"];
    let native_id = session["native_id"].as_str().filter(|id| !id.trim().is_empty());
    let handle = format!("{}:{}", launch.host.name(), native_id.unwrap_or_default());
    if data["launch"]["launch_id"].as_str() != Some(launch.id.as_str())
        || data["launch"]["host"].as_str() != Some(launch.host.name())
        || data["launch"]["session"].as_str() != Some(handle.as_str())
        || session["session"].as_str() != Some(handle.as_str())
        || session["launch_id"].as_str() != Some(launch.id.as_str())
        || session["host"].as_str() != Some(launch.host.name())
        || native_id.is_none()
    {
        bail!("session is not bound to {} launch {}", launch.host.name(), launch.id);
    }
    let data = call(&["worktrees", "--session", &handle, "--launch-id", &launch.id])?;
    let rows = data["worktrees"].as_array().context("agent-store worktrees returned no list")?;
    let mut paths = Vec::new();
    for row in rows {
        let path = PathBuf::from(row["path"].as_str().context("owned worktree has no path")?);
        if !path.is_absolute() {
            bail!("owned worktree is not absolute: {}", path.display());
        }
        if row["session"].as_str() != Some(handle.as_str())
            || row["launch_id"].as_str() != Some(launch.id.as_str())
            || paths.contains(&path)
        {
            bail!("worktree ownership does not match launch {}", launch.id);
        }
        paths.push(path);
    }
    paths.sort();
    let location = session["worktree"].as_str().map(PathBuf::from);
    if location.as_ref().is_some_and(|path| !path.is_absolute()) {
        bail!("bound session worktree is not absolute");
    }
    Ok(Ownership { session: handle, paths, location })
}

pub fn release(launch: &Launch, owner: &Ownership, path: &std::path::Path) -> Result<()> {
    let path = path.to_string_lossy();
    let data = call(&["release-worktree", "--session", &owner.session, "--launch-id", &launch.id, "--path", &path])?;
    if data["released"] != Value::Bool(true) {
        bail!("worktree ownership was not released: {path}");
    }
    Ok(())
}

pub fn sessions() -> Result<Vec<Value>> {
    let data = call(&["sessions"])?;
    data["sessions"].as_array().cloned().context("agent-store sessions returned no list")
}

pub fn end(launch_id: &str, reason: &str) -> Result<()> {
    call(&["end", "--launch-id", launch_id, "--reason", reason]).map(drop)
}

pub fn set_open(launch_id: &str, open: bool) -> Result<()> {
    call(&["set-open", "--launch-id", launch_id, "--open", if open { "true" } else { "false" }]).map(drop)
}

pub fn mark_closing(item: &str) -> Result<()> {
    call(&["mark-closing", "--item", item]).map(drop)
}

pub struct Row {
    pub launch: Launch,
    pub closing: bool,
    pub ended: bool,
    pub wrapper_pid: u32,
    pub wrapper_started_at: String,
}

pub fn launches(item: Option<&str>) -> Result<Vec<Row>> {
    let mut arguments = vec!["launches"];
    if let Some(item) = item {
        arguments.extend(["--item", item]);
    }
    let data = call(&arguments)?;
    let rows = data["launches"].as_array().context("agent-store launches returned no list")?;
    Ok(rows.iter().filter(|row| row["end_reason"].as_str() != Some("closed")).filter_map(row).collect())
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
        ended: state(value) == Some("ended"),
        wrapper_pid: u32::try_from(value["wrapper"]["pid"].as_u64()?).ok()?,
        wrapper_started_at: text(&value["wrapper"]["started_at"])?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{Host, Launch, exact_binding, register_arguments};

    fn launch() -> Launch {
        Launch {
            id: "launch".to_owned(),
            host: Host::Codex,
            title: "Tab".to_owned(),
            cwd: PathBuf::from("C:/worktree"),
            item: None,
        }
    }

    fn bound() -> serde_json::Value {
        json!({
            "launch": {
                "launch_id": "launch",
                "host": "codex",
                "title": "Tab",
                "cwd": "C:/worktree",
                "session": "codex:123e4567-e89b-12d3-a456-426614174000",
                "state": "active",
                "wrapper": {"pid": 4, "started_at": "2026-11-11T00:00:00Z"}
            },
            "session": {
                "session": "codex:123e4567-e89b-12d3-a456-426614174000",
                "native_id": "123e4567-e89b-12d3-a456-426614174000",
                "host": "codex",
                "launch_id": "launch",
                "state": "active"
            }
        })
    }

    #[test]
    fn registration_has_only_the_prebound_session_and_launch_fields() {
        assert_eq!(
            register_arguments(&launch(), "codex:123e4567-e89b-12d3-a456-426614174000").unwrap(),
            [
                "register",
                "--session",
                "codex:123e4567-e89b-12d3-a456-426614174000",
                "--cwd",
                "C:/worktree",
                "--launch-id",
                "launch"
            ]
        );
    }

    #[test]
    fn reciprocal_binding_rejects_a_foreign_native_id() {
        let mut data = bound();
        data["session"]["native_id"] = json!("123e4567-e89b-12d3-a456-426614174001");
        assert!(
            exact_binding(&data, &launch(), "codex:123e4567-e89b-12d3-a456-426614174000", 4, "2026-11-11T00:00:00Z")
                .is_err()
        );
    }
}
