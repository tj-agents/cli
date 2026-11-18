use std::collections::HashMap;
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

/// Session identities a wrapper can inherit from the agent that launched its tab. The launched host sets its own;
/// an inherited one would make the new session look like two sessions at once.
pub(crate) const INHERITED_SESSION: [&str; 4] =
    ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "CODEX_SESSION_ID"];

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

/// Runs `closed` when the user closes the wrapper's tab, but not when the OS session is logging off or shutting
/// down. Windows only; elsewhere the tab's hangup ends the wrapper unrecorded.
pub fn on_tab_closed(closed: Box<dyn Fn() + Send + Sync>) {
    os::on_tab_closed(closed);
}

/// Whether the OS session is logging off or shutting down, which ends the tab without the user closing it.
pub fn session_ending() -> bool {
    os::session_ending()
}

/// When a still-running process started, for telling a live wrapper from a reused pid.
pub fn started_at(pid: u32) -> io::Result<Option<SystemTime>> {
    os::started_at(pid)
}

/// The calling process and its verified ancestors, nearest first, as (pid, executable file name).
/// A reused or unobservable parent ends the chain before it can establish ownership.
pub fn ancestry() -> io::Result<Vec<(u32, String)>> {
    Ok(ancestry_from(os::processes()?, std::process::id(), os::started_at))
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChildIdentity {
    pub pid: u32,
    pub image: String,
    pub started_at: SystemTime,
}

pub fn direct_children(pid: u32) -> io::Result<Vec<ChildIdentity>> {
    children_from(os::processes()?, pid, os::started_at)
}

fn children_from(
    table: HashMap<u32, (u32, String)>,
    pid: u32,
    mut read_started: impl FnMut(u32) -> io::Result<Option<SystemTime>>,
) -> io::Result<Vec<ChildIdentity>> {
    let parent_started =
        read_started(pid)?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "parent process is not running"))?;
    let mut children = Vec::new();
    for (child, (parent, image)) in table {
        if parent != pid || child == pid {
            continue;
        }
        if let Some(started_at) = read_started(child)?
            && started_at >= parent_started
        {
            children.push(ChildIdentity { pid: child, image, started_at });
        }
    }
    if read_started(pid)? != Some(parent_started) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "parent process identity changed"));
    }
    Ok(children)
}

fn ancestry_from(
    mut table: HashMap<u32, (u32, String)>,
    mut pid: u32,
    mut read_started: impl FnMut(u32) -> io::Result<Option<SystemTime>>,
) -> Vec<(u32, String)> {
    let mut chain = Vec::new();
    let mut child_started = None;
    while let Some((parent, image)) = table.remove(&pid) {
        let Ok(Some(created)) = read_started(pid) else { break };
        // Windows can retain an exited parent's PID after that PID has been reused.
        if child_started.is_some_and(|child| created > child) {
            break;
        }
        chain.push((pid, image));
        child_started = Some(created);
        pid = parent;
    }
    chain
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
    use super::{ancestry, ancestry_from, children_from, started_at};
    use std::io;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn a_reused_parent_pid_is_not_an_ancestor() {
        let table = [
            (9, (5, "tj.exe".to_owned())),
            (5, (1, "shell.exe".to_owned())),
            (1, (0, "unrelated-wrapper.exe".to_owned())),
        ]
        .into_iter()
        .collect();
        let chain = ancestry_from(table, 9, |pid| {
            let seconds = match pid {
                9 => 30,
                5 => 20,
                1 => 40,
                _ => return Ok(None),
            };
            Ok(Some(UNIX_EPOCH + Duration::from_secs(seconds)))
        });
        assert_eq!(chain, [(9, "tj.exe".to_owned()), (5, "shell.exe".to_owned())]);
    }

    #[test]
    fn valid_ancestors_remain_available() {
        let table =
            [(9, (5, "tj.exe".to_owned())), (5, (1, "agent.exe".to_owned())), (1, (0, "wrapper.exe".to_owned()))]
                .into_iter()
                .collect();
        let chain = ancestry_from(table, 9, |pid| Ok(Some(UNIX_EPOCH + Duration::from_secs(u64::from(pid)))));
        assert_eq!(chain.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [9, 5, 1]);
    }

    #[test]
    fn an_unobservable_parent_cannot_supply_an_ownership_link() {
        for denied in [false, true] {
            let table = [(9, (5, "tj.exe".to_owned())), (5, (1, "agent.exe".to_owned()))].into_iter().collect();
            let chain = ancestry_from(table, 9, |pid| {
                if pid == 9 {
                    Ok(Some(UNIX_EPOCH + Duration::from_secs(30)))
                } else if denied {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "unobservable parent"))
                } else {
                    Ok(None)
                }
            });
            assert_eq!(chain, [(9, "tj.exe".to_owned())]);
        }
    }

    #[test]
    fn the_current_process_starts_its_verified_ancestry() {
        assert_eq!(ancestry().unwrap().first().map(|(pid, _)| *pid), Some(std::process::id()));
    }

    #[test]
    fn current_process_identity_is_stable() {
        let pid = std::process::id();
        let first = started_at(pid).unwrap().unwrap();
        assert_eq!(started_at(pid).unwrap(), Some(first));
    }

    #[test]
    fn direct_children_exclude_reused_parent_links_and_exited_processes() {
        let table = [
            (2, (1, "old.exe".to_owned())),
            (3, (1, "codex.exe".to_owned())),
            (4, (3, "shell.exe".to_owned())),
            (5, (1, "exited.exe".to_owned())),
        ]
        .into_iter()
        .collect();
        let children = children_from(table, 1, |pid| {
            Ok(match pid {
                1 => Some(UNIX_EPOCH + Duration::from_secs(20)),
                2 => Some(UNIX_EPOCH + Duration::from_secs(10)),
                3 => Some(UNIX_EPOCH + Duration::from_nanos(30_123_456_700)),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].pid, 3);
        assert_eq!(children[0].started_at, UNIX_EPOCH + Duration::from_nanos(30_123_456_700));
    }

    #[test]
    fn direct_children_refuse_unobservable_children_or_a_changed_parent() {
        let table = || [(3, (1, "codex.exe".to_owned()))].into_iter().collect();
        assert!(
            children_from(table(), 1, |pid| {
                if pid == 3 {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "unobservable child"))
                } else {
                    Ok(Some(UNIX_EPOCH))
                }
            })
            .is_err()
        );
        let mut reads = 0;
        assert!(
            children_from(table(), 1, |pid| {
                if pid == 1 {
                    reads += 1;
                }
                Ok((reads < 2).then_some(UNIX_EPOCH))
            })
            .is_err()
        );
    }

    #[test]
    fn a_missing_process_is_distinct_from_an_inspection_error() {
        assert_eq!(started_at(0).unwrap(), None);
    }
}
