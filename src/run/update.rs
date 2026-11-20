mod claude;
mod codex;
mod daemon;
pub(crate) mod gate;
#[cfg(test)]
mod native_tests;
pub(crate) mod observer;
mod refresh;
pub(crate) mod upstream;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tj_cli::process::agent;

use crate::host::Host;
use crate::native_bridge;
use crate::paths;
use crate::run::Launch;
pub(crate) use watch::Watcher;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ApplyError {
    #[error("could not resolve the managed session for native reload")]
    Session(#[source] anyhow::Error),
    #[error(transparent)]
    Native(#[from] native_bridge::ReloadError),
    #[error("could not refresh the managed Codex daemon")]
    Daemon(#[source] anyhow::Error),
}

/// The host's own way to load newly installed plugins, skills, hooks and config into the running session.
pub(crate) trait Update {
    fn endpoint(&self) -> Option<&std::path::Path> {
        None
    }
    /// Whether the agent process `agent` is mid-turn, read from the host itself rather than from hooks.
    fn activity(&self, agent: u32) -> gate::Activity;
    fn apply(&self, launch: &Launch, revision: &str) -> Result<bool, ApplyError>;
}

pub(crate) fn host(host: Host, arguments: &[std::ffi::OsString], cwd: &std::path::Path) -> Result<Box<dyn Update>> {
    Ok(match host {
        Host::Claude => Box::new(claude::Claude),
        Host::Codex => Box::new(codex::Codex::prepare(arguments, cwd)?),
    })
}

pub(crate) fn published(host: Host) -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("published").join(host.name()))
}

pub(crate) fn plugins_due(host: &dyn Update, agent: u32, graces: gate::Graces) -> bool {
    gate::decide(host.activity(agent), agent::input_idle(), graces)
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
