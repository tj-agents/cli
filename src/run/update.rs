mod claude;
mod codex;
mod daemon;
pub(crate) mod gate;
#[cfg(test)]
mod native_tests;
pub(crate) mod observer;
pub(crate) mod publication;
mod refresh;
pub(crate) mod upstream;
mod watch;

use std::process::ExitCode;

use anyhow::Result;
use tj_cli::process::agent;

use crate::host::Host;
use crate::native_bridge;
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
    /// Publish only when the recorded revision differs, as after an update that converged without changes.
    #[arg(long)]
    if_stale: bool,
}

pub fn publish(args: Args) -> Result<ExitCode> {
    let publication = publication::Publication::of(args.host)?;
    if !(args.if_stale && publication.revision().as_deref() == Some(args.revision.as_str())) {
        publication.announce(&args.revision)?;
    }
    Ok(ExitCode::SUCCESS)
}
