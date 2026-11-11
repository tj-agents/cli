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

    pub fn pid(&self) -> u32 {
        self.0.pid()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.0.try_wait()
    }

    pub fn stop(&mut self, stop: Stop) -> io::Result<()> {
        self.0.stop(stop)
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
pub fn started_at(pid: u32) -> io::Result<Option<SystemTime>> {
    os::started_at(pid)
}

/// The calling process and its live ancestors, nearest first, as (pid, executable file name).
pub fn ancestry() -> io::Result<Vec<(u32, String)>> {
    let mut table = os::processes()?;
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    while let Some((parent, image)) = table.remove(&pid) {
        chain.push((pid, image));
        pid = parent;
    }
    Ok(chain)
}

/// Executable file names of every live process below `pid`.
pub fn descendants(pid: u32) -> io::Result<Vec<String>> {
    let table = os::processes()?;
    let mut found = Vec::new();
    let mut parents = vec![pid];
    while let Some(parent) = parents.pop() {
        for (child, (_, image)) in table.iter().filter(|(child, (of, _))| *of == parent && **child != parent) {
            if !found.iter().any(|(seen, _)| seen == child) {
                found.push((*child, image.clone()));
                parents.push(*child);
            }
        }
    }
    Ok(found.into_iter().map(|(_, image)| image).collect())
}

/// Types into the console the agent shares with this process, as if from the keyboard.
pub fn type_text(text: &str) -> io::Result<()> {
    os::type_text(text)
}

pub fn press_enter() -> io::Result<()> {
    os::press_enter()
}

/// How long since the last keyboard or mouse input to this desktop, where the platform can tell.
pub fn input_idle() -> Option<Duration> {
    os::input_idle()
}

/// Keeps a helper off the agent's console: through `tj_hook` on Windows, in its own session on Linux.
pub fn hidden(program: &str, hook: &Path) -> Command {
    os::hidden(program, hook)
}

#[cfg(test)]
mod tests {
    use super::started_at;

    #[test]
    fn current_process_identity_is_stable() {
        let pid = std::process::id();
        let first = started_at(pid).unwrap().unwrap();
        assert_eq!(started_at(pid).unwrap(), Some(first));
    }

    #[test]
    fn a_missing_process_is_distinct_from_an_inspection_error() {
        assert_eq!(started_at(0).unwrap(), None);
    }
}
