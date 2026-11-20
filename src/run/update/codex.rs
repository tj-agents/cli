use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result};

use super::daemon::Daemon;
use super::gate::Activity;
use super::{ApplyError, Update, refresh};
use crate::host::Host;
use crate::run::Launch;

pub(super) struct Codex(Option<Daemon>);

impl Codex {
    pub(super) fn prepare(arguments: &[OsString], cwd: &Path) -> Result<Self> {
        // The internal supervisor also accepts arbitrary commands with an explicit host label.
        // Only the real native launch contract establishes an addressable engine.
        let native = Host::from_program(&arguments[0]) == Some(Host::Codex);
        Ok(Self(if native { Some(Daemon::ensure(Path::new(&arguments[0]), &arguments[1..], cwd)?) } else { None }))
    }
}

impl Update for Codex {
    fn activity(&self, _agent: u32) -> Activity {
        Activity::Unknown
    }

    fn endpoint(&self) -> Option<&Path> {
        self.0.as_ref().map(|daemon| daemon.socket.as_path())
    }

    fn apply(&self, _launch: &Launch, revision: &str) -> Result<bool, ApplyError> {
        let apply = || {
            let daemon = self.0.as_ref().context("this command has no verified default-daemon attachment")?;
            let mut server = daemon.connect()?;
            let loaded = refresh::threads(&mut server)?;
            refresh::apply(&mut server, &daemon.socket, daemon.guard.identity()?, Some(revision), &loaded)
        };
        apply().map_err(ApplyError::Daemon)
    }
}
