use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Launch, hidden, store};
use crate::paths;

#[derive(Debug, PartialEq, Eq)]
pub enum Prs {
    AllMerged,
    ClosedUnmerged(Vec<String>),
    Pending,
}

pub fn prs(launch: &Launch) -> Result<Prs> {
    let Some(item) = &launch.item else { return Ok(Prs::Pending) };
    let output = hidden("tj-item")
        .args(["prs", item])
        .current_dir(&launch.cwd)
        .stdin(Stdio::null())
        .output()
        .context("could not run tj item prs")?;
    if !output.status.success() {
        bail!("tj item prs {item}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let reply: Value = serde_json::from_slice(&output.stdout).context("tj item prs printed no JSON")?;
    if reply["all_merged"] == Value::Bool(true) {
        return Ok(Prs::AllMerged);
    }
    let prs = reply["prs"].as_array().map(Vec::as_slice).unwrap_or_default();
    let state = |pr: &Value| pr["state"].as_str().unwrap_or_default().to_owned();
    if !prs.is_empty() && prs.iter().all(|pr| state(pr) != "open") {
        let closed = prs.iter().filter(|pr| state(pr) == "closed");
        return Ok(Prs::ClosedUnmerged(closed.filter_map(|pr| pr["url"].as_str().map(str::to_owned)).collect()));
    }
    Ok(Prs::Pending)
}

pub struct Removal {
    primary: PathBuf,
    worktree: PathBuf,
}

pub fn owned_preflight(launch: &Launch, delivered: bool) -> Result<Option<Removal>, String> {
    let worktree = store::worktree(launch).map_err(|error| format!("{error:#}"))?;
    preflight(&worktree, delivered)
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let output =
        hidden("git").args(arguments).current_dir(cwd).stdin(Stdio::null()).output().context("could not run git")?;
    if !output.status.success() {
        bail!("git {}: {}", arguments.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs while the agent is still alive, so a refusal leaves the session open. A primary checkout is never removed.
pub fn preflight(cwd: &Path, delivered: bool) -> Result<Option<Removal>, String> {
    let fail = |error: anyhow::Error| format!("{error:#}");
    let toplevel = PathBuf::from(git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).map_err(fail)?);
    let common = PathBuf::from(git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).map_err(fail)?);
    if paths::norm(&common.to_string_lossy()) == paths::norm(&toplevel.join(".git").to_string_lossy()) {
        return Ok(None);
    }
    if !git(cwd, &["status", "--porcelain"]).map_err(fail)?.is_empty() {
        return Err("worktree has uncommitted changes: commit them and open a PR, or discard them".into());
    }
    if !delivered {
        let default = git(cwd, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).map_err(fail)?;
        if git(cwd, &["rev-list", "--count", &format!("{default}..HEAD")]).map_err(fail)? != "0" {
            return Err("worktree has unmerged commits: open a PR or discard them".into());
        }
    }
    let hook = hidden("git-wt")
        .args(["hook", "pre-remove"])
        .current_dir(&toplevel)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run git-wt: {error}"))?;
    if !hook.status.success() {
        let reason = String::from_utf8_lossy(&hook.stderr);
        return Err(format!("pre-remove refused: {}", reason.trim().lines().last().unwrap_or("no reason given")));
    }
    let primary = common.parent().context("git common dir has no parent").map_err(fail)?.to_path_buf();
    Ok(Some(Removal { primary, worktree: toplevel }))
}

fn remove(removal: &Removal) -> Result<()> {
    let worktree = removal.worktree.to_string_lossy();
    let mut last = String::new();
    for _ in 0..2 {
        let output = hidden("git-wt")
            .args(["remove", "--foreground", &worktree])
            .current_dir(&removal.primary)
            .stdin(Stdio::null())
            .output()
            .context("could not run git-wt")?;
        if output.status.success() {
            break;
        }
        last = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        std::thread::sleep(Duration::from_secs(1));
    }
    if removal.worktree.exists() {
        std::fs::remove_dir(&removal.worktree)
            .with_context(|| format!("{} is still there: {last}", removal.worktree.display()))?;
    }
    Ok(())
}

pub fn finish(launch: &Launch, removal: Option<Removal>) -> Result<()> {
    let repository = removal.as_ref().map_or(launch.cwd.clone(), |removal| removal.primary.clone());
    if let Some(removal) = &removal {
        remove(removal)?;
    }
    if let Some(item) = &launch.item {
        let output = hidden("tj-item")
            .args(["progress", item, "--state", "Done"])
            .current_dir(&repository)
            .stdin(Stdio::null())
            .output()
            .context("could not run tj item progress")?;
        if !output.status.success() {
            bail!("tj item progress {item}: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
    }
    store::end(&launch.id, "closed")
}

fn wrapper_alive(row: &store::Row) -> bool {
    let Ok(recorded) = chrono::DateTime::parse_from_rfc3339(&row.wrapper_started_at) else { return false };
    let Some(started) = tj_cli::process::agent::started_at(row.wrapper_pid) else { return false };
    let started: chrono::DateTime<chrono::Utc> = started.into();
    (started - recorded.to_utc()).num_seconds().abs() <= 2
}

#[derive(clap::Args)]
pub struct Args {
    /// Work item whose sessions to close. Without it, only launches whose tab is already gone are swept.
    item: Option<String>,
}

pub fn run(args: Args) -> Result<ExitCode> {
    if let Some(item) = &args.item {
        store::mark_closing(item)?;
    }
    let mut failed = false;
    for row in store::open_launches(args.item.as_deref())? {
        if wrapper_alive(&row) {
            continue;
        }
        let launch = &row.launch;
        let delivered = match prs(launch) {
            Ok(prs) => prs == Prs::AllMerged,
            Err(error) => {
                eprintln!("{}: {error:#}", launch.id);
                false
            }
        };
        if args.item.is_none() && !row.closing && !delivered {
            continue;
        }
        let result = store::worktree(launch).and_then(|worktree| {
            let removal =
                if worktree.exists() { preflight(&worktree, delivered).map_err(anyhow::Error::msg)? } else { None };
            finish(launch, removal)
        });
        match result {
            Ok(()) => println!("closed     {} ({})", launch.title, launch.cwd.display()),
            Err(error) => {
                failed = true;
                println!("kept       {} ({}): {error:#}", launch.title, launch.cwd.display());
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}
