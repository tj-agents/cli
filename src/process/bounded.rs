//! A finite command owned by a dedicated tj_hook process, including its descendants.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CLEANUP_LIMIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Outcome {
    Exited(u32),
    TimedOut,
    Cancelled,
    Failed(io::Error),
    Unsettled(io::Error),
}

impl Outcome {
    pub fn code(&self) -> u32 {
        match self {
            Self::Exited(code) => *code,
            Self::TimedOut => 124,
            Self::Cancelled => 125,
            Self::Failed(_) | Self::Unsettled(_) => 127,
        }
    }

    pub fn record(&self, path: &Path, attempt: &str) -> io::Result<()> {
        let (state, error) = match self {
            Self::Exited(_) => ("exited", None),
            Self::TimedOut => ("timed_out", None),
            Self::Cancelled => ("cancelled", None),
            Self::Failed(error) => ("failed", Some(error.to_string())),
            Self::Unsettled(error) => ("unsettled", Some(error.to_string())),
        };
        let value = serde_json::json!({
            "attempt": attempt, "state": state, "code": self.code(), "error": error,
            "settled": !matches!(self, Self::Unsettled(_)),
        });
        let staged = path.with_extension(format!("{attempt}.tmp"));
        std::fs::write(&staged, value.to_string())?;
        std::fs::rename(staged, path)
    }
}

pub fn cancel_path(receipt: &Path) -> PathBuf {
    receipt.with_extension("cancel")
}

/// Only a dedicated helper may call this: Linux cleanup owns all children of that helper.
pub fn run(arguments: &[OsString], lifetime: Duration, receipt: &Path, attempt: &str) -> Outcome {
    let cancelled = || std::fs::read_to_string(cancel_path(receipt)).is_ok_and(|value| value == attempt);
    if arguments.is_empty() {
        return Outcome::Failed(io::Error::new(io::ErrorKind::InvalidInput, "an executable is required"));
    }
    #[cfg(windows)]
    {
        super::windows::run_bounded(arguments, lifetime, cancelled)
    }
    #[cfg(unix)]
    {
        unix(arguments, lifetime, cancelled)
    }
}

#[cfg(unix)]
fn unix(arguments: &[OsString], lifetime: Duration, cancelled: impl Fn() -> bool) -> Outcome {
    use super::agent;
    use std::time::Instant;

    // Reuse the native wrapper's subreaper and process-group cleanup, isolated in this helper.
    if let Err(error) = agent::prepare_wrapper() {
        return Outcome::Failed(error);
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return Outcome::Failed(error),
    };
    let inherited: Vec<_> = agent::INHERITED_SESSION
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (*name, value)))
        .collect();
    let mut process = match agent::Agent::start(arguments, &cwd, &inherited) {
        Ok(process) => process,
        Err(error) => return Outcome::Failed(error),
    };
    let deadline = Instant::now() + lifetime;
    let outcome = loop {
        match process.try_wait() {
            Ok(Some(code)) => break Outcome::Exited(code as u32),
            Err(error) => break Outcome::Failed(error),
            Ok(None) => {}
        }
        if cancelled() {
            break Outcome::Cancelled;
        }
        if Instant::now() >= deadline {
            break Outcome::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    match process.kill_tree(CLEANUP_LIMIT) {
        Ok(()) => outcome,
        Err(error) => Outcome::Unsettled(error),
    }
}
