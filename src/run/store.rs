use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Launch, hidden};
use crate::domain::item;
use crate::host::Host;
use crate::paths;

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
        arguments.extend(["--item", item.as_str()]);
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

pub fn recorded_for(launch: &Launch, wrapper_pid: u32, wrapper_started_at: &str) -> bool {
    call(&["show", "--launch-id", &launch.id])
        .is_ok_and(|data| exact_launch(&data, launch, wrapper_pid, wrapper_started_at).is_ok())
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

pub(super) fn exact_launch(data: &Value, launch: &Launch, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
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

/// `session` is the session row; agent-store's `show <handle>` returns it without a launch row.
pub(super) fn exact_session(session: &Value, launch: &Launch, handle: &str) -> Result<()> {
    let native_id = handle
        .strip_prefix(&format!("{}:", launch.host.name()))
        .context("prebound session does not match the launch host")?;
    if text(session, "session")? != handle
        || text(session, "native_id")? != native_id
        || text(session, "host")? != launch.host.name()
        || text(session, "launch_id")? != launch.id
    {
        bail!("agent-store session is not reciprocally bound to this launch");
    }
    active(session, "session")
}

fn exact_binding(
    data: &Value,
    launch: &Launch,
    handle: &str,
    wrapper_pid: u32,
    wrapper_started_at: &str,
) -> Result<()> {
    exact_launch(data, launch, wrapper_pid, wrapper_started_at)?;
    if text(&data["launch"], "session")? != handle {
        bail!("agent-store launch is not bound to this session");
    }
    exact_session(&data["session"], launch, handle)
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
    exact_session(&call(&["show", handle])?["session"], launch, handle)
}

pub fn end_not_started(launch: &Launch, handle: &str, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let data = call(&["show", "--launch-id", &launch.id])?;
    launch_has_no_foreign_binding(&data, launch, handle, wrapper_pid, wrapper_started_at)?;
    if matches!(&data["launch"]["session"], Value::String(session) if session == handle) {
        exact_binding(&data, launch, handle, wrapper_pid, wrapper_started_at)?;
        exact_session(&call(&["show", handle])?["session"], launch, handle)?;
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

pub fn bound_session(launch_id: &str) -> Result<Option<String>> {
    let data = call(&["show", "--launch-id", launch_id])?;
    Ok(data["session"]["native_id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Ownership {
    pub session: String,
    pub claims: Vec<Claim>,
    pub location: Option<PathBuf>,
    pub item: Option<item::Reference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub launch_id: String,
    pub path: PathBuf,
}

fn item(row: &Value) -> Result<Option<item::Reference>> {
    let value = row.get("item").context("agent-store launch is missing its item field")?;
    if value.is_null() {
        return Ok(None);
    }
    let reference = text(value, "ref")?;
    item::Reference::parse(reference).map(Some).context("agent-store item has no valid reference")
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
    let claims = claims(&handle, &launch.id)?;
    let location = session["worktree"].as_str().map(PathBuf::from);
    if location.as_ref().is_some_and(|path| !path.is_absolute()) {
        bail!("bound session worktree is not absolute");
    }
    Ok(Ownership { session: handle, claims, location, item: item(&data["launch"])? })
}

pub fn claims(session: &str, launch_id: &str) -> Result<Vec<Claim>> {
    let data = call(&["worktrees", "--session", session, "--launch-id", launch_id])?;
    let rows = data["worktrees"].as_array().context("agent-store worktrees returned no list")?;
    let mut claims: Vec<Claim> = Vec::new();
    for row in rows {
        let path = PathBuf::from(row["path"].as_str().context("owned worktree has no path")?);
        if !path.is_absolute() {
            bail!("owned worktree is not absolute: {}", path.display());
        }
        if row["session"].as_str() != Some(session)
            || row["launch_id"].as_str() != Some(launch_id)
            || claims
                .iter()
                .any(|claim| paths::norm(&claim.path.to_string_lossy()) == paths::norm(&path.to_string_lossy()))
        {
            bail!("worktree ownership does not match launch {launch_id}");
        }
        claims.push(Claim { launch_id: launch_id.to_owned(), path });
    }
    claims.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(claims)
}

pub fn require_managed_claims(session: &str) -> Result<()> {
    // Omitting --launch-id selects NULL ownership, not the session's current launch.
    let data = call(&["worktrees", "--session", session])?;
    let rows = data["worktrees"].as_array().context("agent-store worktrees returned no list")?;
    if !rows.is_empty() {
        bail!("native session has unmanaged worktree claims; recovery/adoption required");
    }
    Ok(())
}

/// Historical launch rows retain their binding when this native session registers a new launch.
pub fn predecessors(launch: &Launch, owner: &Ownership) -> Result<Vec<Row>> {
    let Some(current_item) = owner.item.as_ref() else {
        return Ok(Vec::new());
    };
    let data = call(&["launches"])?;
    let rows = data["launches"].as_array().context("agent-store launches returned no list")?;
    let mut predecessors: Vec<Row> = Vec::new();
    for value in rows {
        if value["launch_id"].as_str() == Some(launch.id.as_str())
            || value["session"].as_str() != Some(owner.session.as_str())
        {
            continue;
        }
        if value["host"].as_str() != Some(launch.host.name()) {
            bail!("historical launch host does not match its session");
        }
        if item(value)?.as_ref() != Some(current_item) {
            continue;
        }
        let predecessor = row(value).context("historical launch identity is incomplete")?;
        if predecessor.launch.id.is_empty() || predecessors.iter().any(|old| old.launch.id == predecessor.launch.id) {
            bail!("historical launch identity is ambiguous");
        }
        predecessors.push(predecessor);
    }
    Ok(predecessors)
}

pub fn release(owner: &Ownership, claim: &Claim) -> Result<()> {
    let path = claim.path.to_string_lossy();
    let data =
        call(&["release-worktree", "--session", &owner.session, "--launch-id", &claim.launch_id, "--path", &path])?;
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

pub struct Row {
    pub launch: Launch,
    pub closing: bool,
    pub ended: bool,
    pub closed: bool,
    pub wrapper_pid: u32,
    pub wrapper_started_at: String,
}

pub fn launches(item: Option<&item::Reference>) -> Result<Vec<Row>> {
    let mut arguments = vec!["launches"];
    if let Some(item) = item {
        arguments.extend(["--item", item.as_str()]);
    }
    let data = call(&arguments)?;
    let rows = data["launches"].as_array().context("agent-store launches returned no list")?;
    Ok(rows.iter().filter_map(row).collect())
}

fn row(value: &Value) -> Option<Row> {
    let text = |field: &Value| field.as_str().map(str::to_owned);
    Some(Row {
        launch: Launch {
            id: text(&value["launch_id"])?,
            host: Host::from_name(value["host"].as_str()?)?,
            title: text(&value["title"]).unwrap_or_default(),
            cwd: PathBuf::from(text(&value["worktree"]).or_else(|| text(&value["cwd"]))?),
            item: value["item"]["ref"].as_str().map(item::Reference::parse).transpose().ok()?,
        },
        closing: state(value) == Some("closing"),
        ended: state(value) == Some("ended"),
        closed: value["end_reason"].as_str() == Some("closed"),
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

    #[test]
    fn canonical_item_distinguishes_unlinked_launches_from_missing_metadata() {
        assert_eq!(super::item(&json!({"item": null})).unwrap(), None);
        let reference = "github:github.com/acme/app:1";
        assert_eq!(
            super::item(&json!({"item": {"ref": reference}}))
                .unwrap()
                .as_ref()
                .map(crate::domain::item::Reference::as_str),
            Some(reference)
        );
        for row in [json!({}), json!({"item": {}}), json!({"item": {"ref": " "}})] {
            assert!(super::item(&row).is_err(), "unknown work item must not be treated as unlinked: {row}");
        }
    }
}
