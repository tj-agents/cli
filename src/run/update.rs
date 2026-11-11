mod claude;
mod codex;
pub(crate) mod gate;
pub(crate) mod successor;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use tj_cli::process::agent;

use crate::host::Host;
use crate::paths;
pub(crate) use watch::Watcher;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ApplyError {
    #[error("could not type into the agent's console")]
    Typing(#[source] std::io::Error),
    #[error("the Codex app-server did not take the update")]
    AppServer(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The host's own way to load newly installed plugins, skills, hooks and config into the running session.
pub(crate) trait Update {
    /// Whether the agent process `agent` is mid-turn, read from the host itself rather than from hooks.
    fn activity(&self, agent: u32) -> gate::Activity;
    fn apply(&self) -> Result<(), ApplyError>;
}

pub(crate) fn host(host: Host) -> &'static dyn Update {
    match host {
        Host::Claude => &claude::Claude,
        Host::Codex => &codex::Codex,
    }
}

pub(crate) fn published() -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("published"))
}

/// The update to apply now, if any.
pub(crate) fn due(watcher: &mut Watcher, host: &dyn Update, agent: u32, graces: gate::Graces) -> Option<gate::Apply> {
    let pending =
        gate::Pending { binary: watcher.binary(Duration::from_secs(2)), plugins: watcher.revision().is_some() };
    if !(pending.binary || pending.plugins) {
        return None;
    }
    gate::decide(pending, host.activity(agent), agent::input_idle(), graces)
}

#[derive(clap::Args)]
pub struct Args {
    /// The installed plugin revision, such as their pins. Every running tab applies each new revision once.
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
