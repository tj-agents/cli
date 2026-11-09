use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as os;
#[cfg(windows)]
use windows as os;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Signal,
    Keys,
}

pub struct Agent(os::Agent);

impl Agent {
    pub fn start(argv: &[OsString], cwd: &Path, env: &[(&str, String)]) -> io::Result<Self> {
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no agent command given"));
        }
        os::Agent::start(argv, cwd, env).map(Self)
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.0.try_wait()
    }

    pub fn stop(&mut self, stop: Stop) -> io::Result<()> {
        self.0.stop(stop)
    }

    /// CPU time the agent's own process has used; a host redraws its spinner while a turn runs.
    pub fn cpu_time(&self) -> io::Result<Duration> {
        self.0.cpu_time()
    }

    pub fn kill_tree(&mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.0.kill_tree()? {
                self.0.reclaim_terminal();
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the agent's processes did not all exit"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Clears an inherited ignore-Ctrl+C flag, and on Linux adopts orphaned descendants so the tree can be reaped.
pub fn prepare_wrapper() -> io::Result<()> {
    os::prepare_wrapper()
}

/// When a still-running process started, for telling a live wrapper from a reused pid.
pub fn started_at(pid: u32) -> Option<SystemTime> {
    os::started_at(pid)
}

/// Keeps a helper off the agent's console: through `tj_hook` on Windows, in its own session on Linux.
pub fn hidden(program: &str, hook: &Path) -> Command {
    os::hidden(program, hook)
}
