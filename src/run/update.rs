mod claude;
mod codex;
pub(crate) mod gate;
pub(crate) mod successor;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

pub(crate) fn published(host: Host) -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("published").join(host.name()))
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
    /// The host whose plugins were just updated; only its tabs apply the update.
    #[arg(long, value_enum)]
    host: Host,
    /// What was installed, such as the plugin pins. Every publish is a new update, even of the same revision.
    #[arg(long)]
    revision: String,
}

pub fn publish(args: Args) -> Result<ExitCode> {
    let published = published(args.host)?;
    let folder = published.parent().context("the published update has no folder")?;
    std::fs::create_dir_all(folder).context("could not create the published updates folder")?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let staged = folder.join(format!("{}.{}", args.host.name(), std::process::id()));
    std::fs::write(
        &staged,
        format!(
            "{}
{stamp}.{}",
            args.revision,
            std::process::id()
        ),
    )
    .context("could not stage the published update")?;
    std::fs::rename(&staged, &published).context("could not publish the update")?;
    Ok(ExitCode::SUCCESS)
}
