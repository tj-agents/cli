use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Stop;

pub struct Agent {
    pid: libc::pid_t,
    exit: Option<i32>,
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn terminal() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

impl Agent {
    pub fn start(argv: &[OsString], cwd: &Path, env: &[(&str, String)]) -> io::Result<Self> {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).current_dir(cwd).envs(env.iter().map(|(name, value)| (name, value)));
        let foreground = terminal();
        unsafe {
            command.pre_exec(move || {
                check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0))?;
                check(libc::setpgid(0, 0))?;
                if foreground {
                    check(libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpid()))?;
                }
                for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU, libc::SIGTTIN] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Self { pid: child.id() as libc::pid_t, exit: None })
    }

    pub fn pid(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// Reaps every exited child, including descendants adopted as subreaper, keeping the agent's status.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
            if pid == self.pid {
                self.exit = Some(if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else {
                    128 + libc::WTERMSIG(status)
                });
            }
        }
        Ok(self.exit)
    }

    pub fn stop(&mut self, stop: Stop) -> io::Result<()> {
        match stop {
            Stop::Signal => check(unsafe { libc::killpg(self.pid, libc::SIGINT) }),
            Stop::Keys => tmux(&["C-c"]),
        }
    }

    pub fn kill_tree(&mut self) -> io::Result<bool> {
        let group_alive = unsafe { libc::killpg(self.pid, libc::SIGKILL) } == 0;
        for pid in adopted_children() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        self.try_wait()?;
        let mut status = 0;
        let none_left = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) } == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
        Ok(!group_alive && none_left)
    }

    pub fn reclaim_terminal(&self) {
        if terminal() {
            unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) };
        }
    }
}

fn tmux(keys: &[&str]) -> io::Result<()> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| io::Error::new(io::ErrorKind::Unsupported, "keys can only be sent inside tmux"))?;
    let status = Command::new("tmux").args(["send-keys", "-t", &pane]).args(keys).status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other("tmux send-keys failed")) }
}

pub fn type_text(text: &str) -> io::Result<()> {
    tmux(&["-l", text])
}

pub fn press_enter() -> io::Result<()> {
    tmux(&["Enter"])
}

pub fn input_idle() -> Option<Duration> {
    None
}

pub fn processes() -> io::Result<std::collections::HashMap<u32, (u32, String)>> {
    let mut table = std::collections::HashMap::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        let Ok(Some(fields)) = stat_fields(libc::pid_t::try_from(pid).map_err(io::Error::other)?) else { continue };
        let parent = fields.get(1).and_then(|parent| parent.parse().ok()).unwrap_or(0);
        let image = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .unwrap_or_default();
        table.insert(pid, (parent, image));
    }
    Ok(table)
}

fn adopted_children() -> Vec<libc::pid_t> {
    let me = std::process::id().to_string();
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<libc::pid_t>().ok())
        .filter(|pid| stat_fields(*pid).ok().flatten().is_some_and(|fields| fields.get(1) == Some(&me)))
        .collect()
}

/// `/proc/<pid>/stat` from the state field on: [state, ppid, pgrp, …, starttime at 19].
fn stat_fields(pid: libc::pid_t) -> io::Result<Option<Vec<String>>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let offset = stat.rfind(')').ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?;
    let rest =
        stat.get(offset + 2..).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process fields"))?;
    Ok(Some(rest.split_whitespace().map(str::to_owned).collect()))
}

pub fn prepare_wrapper() -> io::Result<()> {
    unsafe {
        for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU] {
            libc::signal(signal, libc::SIG_IGN);
        }
        check(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0))
    }
}

pub fn started_at(pid: u32) -> io::Result<Option<SystemTime>> {
    let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
    let Some(fields) = stat_fields(pid)? else { return Ok(None) };
    if fields.first().is_some_and(|state| state == "Z") {
        return Ok(None);
    }
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "cannot read process creation time");
    let ticks: u64 = fields.get(19).ok_or_else(invalid)?.parse().map_err(io::Error::other)?;
    let boot: u64 = std::fs::read_to_string("/proc/stat")?
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())
        .ok_or_else(invalid)?;
    let hertz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|hertz| *hertz > 0)
        .ok_or_else(invalid)?;
    Ok(Some(UNIX_EPOCH + Duration::from_secs(boot) + Duration::from_millis(ticks * 1000 / hertz)))
}

pub fn hidden(program: &str, _hook: &Path) -> Command {
    let mut command = Command::new(program);
    unsafe {
        command.pre_exec(|| check(libc::setsid()).map(|_| ()));
    }
    command
}
