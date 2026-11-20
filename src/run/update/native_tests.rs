use super::daemon::Daemon;
use super::refresh;
use crate::app_server::Server;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hook(server: &mut Server, params: Value) -> Result<Value> {
    let inventory = server.request("hooks/list", params)?;
    let hooks = inventory["data"][0]["hooks"].as_array().context("missing hooks")?;
    hooks.iter().find(|hook| hook["pluginId"] == "smoke@fixture").cloned().context("fixture hook missing")
}

fn approve(server: &mut Server, project: &Path, version: &str, reload: bool) -> Result<Value> {
    server.request("skills/list", json!({"cwds":[project],"forceReload":true}))?;
    let hook = hook(server, json!({"cwds":[project]}))?;
    ensure!(hook["command"].as_str().is_some_and(|c| c.ends_with(version)), "stale hook discovery: {hook}");
    let key = hook["key"].as_str().context("missing hook key")?;
    let params = json!({
        "edits": [{"keyPath": "hooks.state", "mergeStrategy": "upsert", "value": {key: {"trusted_hash": hook["currentHash"]}}}],
        "reloadUserConfig": reload
    });
    server.request("config/batchWrite", params)?;
    Ok(hook)
}

fn turn(server: &mut Server, id: &Value, input: Value) -> Result<()> {
    let response = server.request("turn/start", json!({"threadId":id,"input":input}))?;
    let turn = &response["turn"]["id"];
    let deadline = Instant::now() + Duration::from_secs(35);
    while let Some(message) = server.next(deadline)? {
        let params = &message["params"];
        if message["method"] == "turn/completed" && params["threadId"] == *id && params["turn"]["id"] == *turn {
            ensure!(params["turn"]["status"] == "completed", "canned turn failed: {}", params["turn"]["error"]);
            return Ok(());
        }
    }
    anyhow::bail!("canned turn timed out")
}

/// Run through tests/fixtures/native_refresh.py with a disposable home, packaged Codex and loopback provider.
#[test]
#[ignore = "the Windows/Linux CI fixture supplies the pinned native package and canned Responses server"]
fn pinned_native_cold_start() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("TJ_FIXTURE_ROOT").context("fixture root required")?);
    let native = PathBuf::from(std::env::var_os("TJ_FIXTURE_CODEX").context("fixture executable required")?);
    let project = root.join("project");
    let missing = Command::new(&native)
        .arg("--cd")
        .arg(&project)
        .args([
            "resume",
            "123e4567-e89b-12d3-a456-426614174000",
            "--model",
            "gpt-6.2-astra",
            "-c",
            "model_reasoning_effort=high",
            "--remote",
            "unix://",
            "--",
            "--literal",
        ])
        .stdin(Stdio::null())
        .output()?;
    ensure!(
        !missing.status.success()
            && String::from_utf8_lossy(&missing.stderr).contains("requested app-server socket is not available"),
        "unexpected handoff parser result: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let flags = ["--remote".into(), "unix://".into()];
    let daemon = Daemon::ensure(&native, &flags, &project)?;
    drop(daemon);
    Ok(())
}

#[test]
#[ignore = "run tests/fixtures/native_refresh.py with an isolated native daemon"]
fn pinned_native_refresh() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("TJ_FIXTURE_ROOT").context("fixture root required")?);
    let native = PathBuf::from(std::env::var_os("TJ_FIXTURE_CODEX").context("fixture executable required")?);
    let project = root.join("project");
    let flags = ["--remote".into(), "unix://".into()];
    let daemon = Daemon::ensure(&native, &flags, &project)?;
    let mut ui = daemon.connect()?;
    // A second launch verifies the warm service without disturbing the first connection.
    let warm = Daemon::ensure(&native, &flags, &project)?;
    let mut control = warm.connect()?;
    let _ = approve(&mut control, &project, "1.0.0", true)?;
    let params = json!({
        "cwd": project, "model": "gpt-6.2-astra", "modelProvider": "fixture",
        "approvalPolicy": "never", "sandbox": "read-only", "config": {"model_reasoning_effort": "high"}
    });
    let initial = ui.request("thread/start", params)?;
    let id = &initial["thread"]["id"];
    let old = hook(&mut control, json!({"threadId":id}))?;
    assert_eq!(old["trustStatus"], "trusted");
    turn(&mut ui, id, json!([{"type":"text","text":"First synthetic turn."}]))?;
    let installed = Command::new(std::env::var_os("TJ_FIXTURE_PYTHON").context("Python required")?)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_refresh.py"))
        .args(["--install", "2.0.0"])
        .status()?;
    ensure!(installed.success(), "could not install second fixture version");
    let expected = approve(&mut control, &project, "2.0.0", false)?;
    assert_eq!(hook(&mut control, json!({"threadId":id}))?["currentHash"], old["currentHash"]);
    let loaded = refresh::threads(&mut control)?;
    assert!(loaded.contains(id.as_str().context("missing thread ID")?));
    assert!(refresh::apply(&mut control, &warm.socket, warm.guard.identity()?, Some("fixture-v2"), &loaded)?);
    let updated = hook(&mut control, json!({"threadId":id}))?;
    assert_eq!(updated["sourcePath"], expected["sourcePath"]);
    let fresh = expected["currentHash"].as_str().context("missing fresh hash")?;
    assert!(
        updated["currentHash"] == fresh
            || fresh.split_once('/').is_some_and(|(decl, _)| updated["currentHash"] == decl)
    );
    assert_ne!(updated["currentHash"], old["currentHash"]);
    assert_eq!(updated["trustStatus"], "trusted");
    turn(&mut ui, id, json!([{"type":"text","text":"Second synthetic turn."}]))?;
    let list = control.request("skills/list", json!({"cwds":[project]}))?;
    let skill = list["data"][0]["skills"]
        .as_array()
        .context("missing skills")?
        .iter()
        .find(|s| s["name"] == "smoke:marker")
        .context("fixture skill missing")?;
    turn(
        &mut ui,
        id,
        json!([{"type":"skill","name":skill["name"],"path":skill["path"]},{"type":"text","text":"Read the fixture skill."}]),
    )?;
    // Same path and hook declaration, new bundle content. The loaded declaration alone
    // cannot distinguish these two versions; prove what the next request actually reads.
    let path = Path::new(skill["path"].as_str().context("missing skill path")?);
    let contents = std::fs::read_to_string(path)?;
    std::fs::write(path, format!("{contents}\nSame-path fixture edit.\n"))?;
    let changed = approve(&mut control, &project, "2.0.0", false)?;
    assert_ne!(changed["currentHash"], expected["currentHash"]);
    assert_eq!(changed["sourcePath"], expected["sourcePath"]);
    assert!(refresh::apply(&mut control, &warm.socket, warm.guard.identity()?, Some("same-path"), &loaded)?);
    turn(
        &mut ui,
        id,
        json!([
            {"type":"skill","name":skill["name"],"path":path},
            {"type":"text","text":"Read the changed fixture skill."}
        ]),
    )?;
    std::fs::write(root.join("thread-id"), id.as_str().context("missing thread ID")?)?;
    let resumed = control.request("thread/resume", json!({"threadId":id}))?;
    for key in ["model", "modelProvider", "cwd", "approvalPolicy", "sandbox", "reasoningEffort"] {
        assert_eq!(initial[key], resumed[key], "{key}");
    }
    Ok(())
}

#[test]
#[ignore = "run with the Linux tmux draft fixture"]
fn refresh_thread_with_draft() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("TJ_FIXTURE_ROOT").context("fixture root required")?);
    let native = PathBuf::from(std::env::var_os("TJ_FIXTURE_CODEX").context("fixture executable required")?);
    let project = root.join("project");
    let id = std::fs::read_to_string(root.join("thread-id"))?;
    let daemon = Daemon::ensure(&native, &["--remote".into(), "unix://".into()], &project)?;
    let mut server = daemon.connect()?;
    let loaded = refresh::threads(&mut server)?;
    ensure!(loaded.contains(&id), "TUI did not load the prebound thread");
    let expected = approve(&mut server, &project, "3.0.0", false)?;
    let old = hook(&mut server, json!({"threadId":id}))?;
    assert_ne!(old["sourcePath"], expected["sourcePath"]);
    assert!(refresh::apply(&mut server, &daemon.socket, daemon.guard.identity()?, Some("unsent-draft"), &loaded)?);
    let updated = hook(&mut server, json!({"threadId":id}))?;
    assert_eq!(updated["sourcePath"], expected["sourcePath"]);
    assert_eq!(updated["trustStatus"], "trusted");
    Ok(())
}
