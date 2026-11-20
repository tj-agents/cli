use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use fs4::FileExt;
use serde_json::{Map, Value, json};

use super::observer::{sibling, socket_identity};
use crate::app_server::Server;

pub(super) fn threads(server: &mut Server) -> Result<BTreeSet<String>> {
    let mut threads = BTreeSet::new();
    let mut cursors = BTreeSet::new();
    let mut params = json!({});
    loop {
        let page = server.request("thread/loaded/list", params)?;
        for id in page["data"].as_array().context("invalid loaded thread list")? {
            threads.insert(id.as_str().filter(|id| !id.is_empty()).context("invalid loaded thread id")?.to_owned());
        }
        if page["nextCursor"].is_null() {
            return Ok(threads);
        }
        let next = page["nextCursor"].as_str().context("invalid loaded thread cursor")?;
        ensure!(cursors.insert(next.to_owned()), "Codex repeated a loaded thread cursor");
        params = json!({"cursor": next});
    }
}

/// Serialize publication handling across the observer and wrappers attached to this socket.
/// The receipt records runtime hook observations. Agent verifies expected plugin content separately.
pub(super) fn apply(
    server: &mut Server,
    socket: &Path,
    endpoint: (u64, u64),
    revision: Option<&str>,
    loaded: &BTreeSet<String>,
) -> Result<bool> {
    if loaded.is_empty() {
        return Ok(false);
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sibling(socket, ".tj-reload.lock")?)?;
    match FileExt::try_lock(&lock) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    ensure!(socket_identity(socket)? == endpoint, "Codex daemon changed before reload");
    let path = sibling(socket, ".tj-reload.json")?;
    if let Some(revision) = revision
        && let Ok(bytes) = fs::read(&path)
        && let Ok(previous) = serde_json::from_slice::<Value>(&bytes)
        && previous["socket"] == json!(endpoint)
        && previous["revision"] == revision
        && loaded.iter().all(|id| previous["threads"].get(id).is_some())
    {
        return Ok(true);
    }
    let reply = server.request("config/batchWrite", json!({"edits": [], "reloadUserConfig": true}))?;
    ensure!(reply["status"] == "ok", "Codex configuration reload returned {}", reply["status"]);
    let mut evidence = Map::new();
    for id in loaded {
        let result = server
            .request("hooks/list", json!({"threadId": id}))
            .with_context(|| format!("could not read loaded hooks for {id}"))?;
        let [row] = result["data"].as_array().context("invalid hooks/list response")?.as_slice() else {
            anyhow::bail!("Codex omitted the loaded hook inventory for {id}");
        };
        let cwd = row["cwd"].as_str().context("missing hook inventory cwd")?;
        ensure!(
            row["errors"].as_array().is_some_and(Vec::is_empty),
            "Codex reported hook inventory errors for {id}: {}",
            row["errors"]
        );
        let hooks: Vec<_> = row["hooks"].as_array().context("invalid hook inventory")?.iter().map(|hook| {
            json!({"key": hook["key"], "pluginId": hook["pluginId"], "sourcePath": hook["sourcePath"], "currentHash": hook["currentHash"],
                   "enabled": hook["enabled"], "trustStatus": hook["trustStatus"]})
        }).collect();
        evidence.insert(id.clone(), json!({"cwd": cwd, "hooks": hooks, "warnings": row["warnings"]}));
    }
    ensure!(socket_identity(socket)? == endpoint, "Codex daemon changed during reload");
    let receipt = json!({"socket": endpoint, "revision": revision, "threads": evidence});
    let staged = sibling(socket, ".tj-reload.tmp")?;
    fs::write(&staged, serde_json::to_vec(&receipt)?)?;
    fs::rename(staged, path)?;
    Ok(true)
}
