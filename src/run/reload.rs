mod claude;
mod codex;
pub(crate) mod gate;
pub(crate) mod successor;
mod watch;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tj_cli::process::agent;

use crate::host::Host;
use crate::paths;
pub(crate) use watch::Updates;

const NOW: &str = "now";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReloadError {
    #[error("could not type into the agent's console")]
    Typing(#[source] std::io::Error),
    #[error("the Codex app-server did not reload")]
    AppServer(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The host's own way to load newly installed plugins, skills, hooks and config into the running session.
pub(crate) trait Reload {
    /// Whether the agent process `agent` is mid-turn, read from the host itself rather than from hooks.
    fn activity(&self, agent: u32) -> gate::Activity;
    fn reload(&self) -> Result<(), ReloadError>;
}

pub(crate) fn host(host: Host) -> &'static dyn Reload {
    match host {
        Host::Claude => &claude::Claude,
        Host::Codex => &codex::Codex,
    }
}

pub(crate) fn published() -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("published"))
}

/// What, if anything, to apply now; consumes a pending `tj refresh`.
pub(crate) fn due(
    updates: &mut Updates,
    marks: &Path,
    host: &dyn Reload,
    agent: u32,
    graces: gate::Graces,
) -> Option<gate::Apply> {
    let now = marks.join(NOW).exists();
    let pending =
        gate::Pending { binary: updates.binary(Duration::from_secs(2)), revision: updates.revision().is_some(), now };
    if !(pending.binary || pending.revision || pending.now) {
        return None;
    }
    let apply = gate::decide(pending, host.activity(agent), agent::input_idle(), graces)?;
    if now {
        let _ = std::fs::remove_file(marks.join(NOW));
    }
    Some(apply)
}

#[derive(clap::Args)]
pub struct Args {
    /// The installed plugin revision, such as their pins. Every running tab reloads once for each new revision.
    #[arg(long)]
    revision: String,
}

pub fn publish(args: Args) -> Result<ExitCode> {
    let published = published()?;
    let folder = published.parent().context("the published revision has no folder")?;
    std::fs::create_dir_all(folder).context("could not create the cache folder")?;
    let staged = folder.join(format!("published.{}", std::process::id()));
    std::fs::write(&staged, &args.revision).context("could not stage the published revision")?;
    std::fs::rename(&staged, &published).context("could not publish the revision")?;
    Ok(ExitCode::SUCCESS)
}

fn step(program: &str, arguments: &[&str]) -> Result<()> {
    let executable = which::which(program).with_context(|| format!("{program} is not on PATH"))?;
    let status =
        Command::new(&executable).args(arguments).status().with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", arguments.join(" "));
    }
    Ok(())
}

pub fn refresh() -> Result<ExitCode> {
    for host in [Host::Claude, Host::Codex] {
        if which::which(host.name()).is_ok() {
            step("tj-plugins", &["update", "--host", host.name()])?;
        }
    }
    step("tj-install", &[])?;
    let Some(launch) = super::owner::launch() else {
        println!("Updated. Tabs under tj run apply it when idle; this one is not, so reload it yourself.");
        return Ok(ExitCode::SUCCESS);
    };
    let folder = super::marks(&launch)?;
    std::fs::create_dir_all(&folder).context("could not create the refresh mark's folder")?;
    std::fs::write(folder.join(NOW), "").context("could not record the refresh")?;
    println!("Updated. Applying it to this session now.");
    Ok(ExitCode::SUCCESS)
}

pub fn launch() -> ExitCode {
    if let Some(launch) = super::owner::launch() {
        println!("{launch}");
    }
    ExitCode::SUCCESS
}
