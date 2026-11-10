pub mod closeout;
pub mod restart;
pub(crate) mod store;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use tj_cli::process::agent::{self, Agent, Stop};

use crate::host::Host;
use crate::paths;
use closeout::Prs;

impl Host {
    /// Claude exits cleanly on a console break or SIGINT; Codex has no handler and only quits on the key.
    fn stop(self) -> (Stop, u32) {
        match self {
            Self::Claude => (Stop::Signal, 1),
            Self::Codex => (Stop::Keys, 3),
        }
    }

    fn from_program(program: &OsStr) -> Option<Self> {
        Self::from_name(&Path::new(program).file_stem()?.to_string_lossy().to_lowercase())
    }
}

pub struct Launch {
    pub id: String,
    pub host: Host,
    pub title: String,
    pub cwd: PathBuf,
    pub item: Option<String>,
}

#[derive(clap::Args)]
pub struct Args {
    /// Defaults to the agent executable's name.
    #[arg(long, value_enum)]
    host: Option<Host>,
    /// Tab title, exported to the agent as AGENT_CLI_TAB_TITLE. Defaults to the repository or worktree folder's name.
    #[arg(long)]
    title: Option<String>,
    /// The worktree the agent works in, by default the current folder. Only a linked worktree is removed at closeout.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Work item reference, for example `github:github.com/acme/app:461`.
    #[arg(long)]
    item: Option<String>,
    /// Identifies this launch in agent-store; the launcher waits for the row it names. Generated when omitted.
    #[arg(long)]
    launch_id: Option<String>,
    /// The session that handed this work off, as `host:id`.
    #[arg(long)]
    parent: Option<String>,
    #[arg(long, hide = true, default_value_t = 120)]
    poll_seconds: u64,
    #[arg(long, hide = true, default_value_t = 600)]
    idle_seconds: u64,
    #[arg(long, hide = true, default_value_t = 10)]
    grace_seconds: u64,
    #[arg(long, hide = true, default_value_t = 30)]
    restart_grace_seconds: u64,
    /// The agent's command line.
    #[arg(last = true, required = true)]
    agent: Vec<OsString>,
}

pub fn hidden(program: &str) -> Command {
    let hook = std::env::current_exe().map(|exe| exe.with_file_name("tj_hook.exe")).unwrap_or_default();
    let mut command = agent::hidden(program, &hook);
    command.env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1");
    command
}

/// The agent's SessionStart registers against this row, so it must exist before the agent starts.
fn record_launch(launch: &Launch, parent: Option<&str>, wrapper_pid: u32, wrapper_started_at: &str) {
    let mut failure = None;
    for delay in [0, 1, 2, 4].map(Duration::from_secs) {
        std::thread::sleep(delay);
        match store::launch(launch, parent, wrapper_pid, wrapper_started_at) {
            Ok(()) => return,
            Err(_) if store::recorded(&launch.id) => return,
            Err(error) => failure = Some(error),
        }
    }
    if let Some(error) = failure {
        eprintln!("tj run: launch {} was not recorded: {error:#}", launch.id);
    }
}

fn marks(launch_id: &str) -> Result<PathBuf> {
    if launch_id.is_empty() || !launch_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("launch id {launch_id:?} must be letters, digits, '-' or '_'");
    }
    Ok(paths::cache_dir()?.join("run").join(launch_id))
}

fn launch_id() -> Option<String> {
    std::env::var("TJ_RUN_LAUNCH_ID").ok().filter(|id| !id.is_empty())
}

fn generated_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("no OS randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn default_title(cwd: &Path) -> String {
    let toplevel = hidden("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()));
    let folder = toplevel.as_deref().unwrap_or(cwd);
    let base = folder.file_name().map_or_else(|| "agent".to_owned(), |name| name.to_string_lossy().into_owned());
    let taken: HashSet<String> = store::open_launches(None)
        .map(|rows| rows.into_iter().map(|row| row.launch.title).collect())
        .unwrap_or_default();
    std::iter::once(base.clone())
        .chain((2..).map(|number| format!("{base}-{number}")))
        .find(|title| !taken.contains(title))
        .unwrap_or(base)
}

fn launch(args: &Args) -> Result<Launch> {
    let cwd = std::path::absolute(args.cwd.clone().map_or_else(std::env::current_dir, Ok)?)?;
    let program = &args.agent[0];
    let Some(host) = args.host.or_else(|| Host::from_program(program)) else {
        bail!("pass --host: {} is neither claude nor codex", program.to_string_lossy());
    };
    let title = args.title.clone().unwrap_or_else(|| default_title(&cwd));
    let id = args.launch_id.clone().map_or_else(generated_id, Ok)?;
    Ok(Launch { id, host, title, cwd, item: args.item.clone() })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Merged,
    Done,
    Closing,
}

struct Watch {
    launch: Launch,
    environment: [(&'static str, String); 4],
    marks: PathBuf,
    poll: Duration,
    idle: Duration,
    next_poll: Instant,
    reported: HashSet<String>,
}

impl Watch {
    fn report(&mut self, line: String) {
        if !self.reported.contains(&line) {
            eprintln!("\r\ntj run: {line}\r");
            self.reported.insert(line);
        }
    }

    fn trigger(&mut self) -> Option<Trigger> {
        let idle = self
            .marks
            .join("done")
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|marked| SystemTime::now().duration_since(marked).ok());
        if idle.is_some_and(|idle| idle >= self.idle) {
            return Some(Trigger::Done);
        }
        if Instant::now() < self.next_poll {
            return None;
        }
        self.next_poll = Instant::now() + self.poll;
        match store::closing(&self.launch.id) {
            Ok(true) => return Some(Trigger::Closing),
            Ok(false) => {}
            Err(error) => self.report(format!("{error:#}")),
        }
        match closeout::prs(&self.launch) {
            Ok(Prs::AllMerged) => Some(Trigger::Merged),
            Ok(Prs::ClosedUnmerged(urls)) => {
                self.report(format!("closed without merging, staying open: {}", urls.join(" ")));
                None
            }
            Ok(Prs::Pending) => None,
            Err(error) => {
                self.report(format!("{error:#}"));
                None
            }
        }
    }
}

fn stop(agent: &mut Agent, host: Host, grace: Duration) -> Result<()> {
    let (signal, presses) = host.stop();
    let deadline = Instant::now() + grace;
    let mut sent = 0;
    while agent.try_wait()?.is_none() && Instant::now() < deadline {
        if sent < presses {
            if let Err(error) = agent.stop(signal) {
                eprintln!("tj run: could not ask the agent to exit: {error}");
                sent = presses;
            }
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    agent.kill_tree(Duration::from_secs(30)).context("could not end the agent's processes")
}

enum Restarted {
    Kept,
    Resumed(Agent),
}

fn restart(agent: &mut Agent, watch: &mut Watch, command: &[OsString], grace: Duration) -> Result<Restarted> {
    let host = watch.launch.host;
    let session = match store::bound_session(&watch.launch.id) {
        Ok(Some(session)) => session,
        Ok(None) => {
            watch.report(format!(
                "not restarting: no {} session is bound to launch {} yet",
                host.name(),
                watch.launch.id
            ));
            return Ok(Restarted::Kept);
        }
        Err(error) => {
            watch.report(format!("not restarting: {error:#}"));
            return Ok(Restarted::Kept);
        }
    };
    eprintln!("\r\ntj run: restarting to resume {}:{session}\r", host.name());
    stop(agent, host, grace)?;
    let resumed = Agent::start(&host.resume(command, &session), &watch.launch.cwd, &watch.environment)
        .with_context(|| format!("could not resume {}:{session}", host.name()))?;
    Ok(Restarted::Resumed(resumed))
}

fn wait_for_enter() {
    eprintln!("tj run: press Enter to close this tab");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn run(args: Args) -> Result<ExitCode> {
    agent::prepare_wrapper().context("could not prepare the console")?;
    let launch = launch(&args)?;
    let marks = marks(&launch.id)?;
    let _ = std::fs::remove_dir_all(&marks);
    let pid = std::process::id();
    let started_at = rfc3339(agent::started_at(pid).unwrap_or_else(SystemTime::now));
    let environment = [
        ("TJ_RUN_PID", pid.to_string()),
        ("TJ_RUN_STARTED_AT", started_at.clone()),
        ("TJ_RUN_LAUNCH_ID", launch.id.clone()),
        ("AGENT_CLI_TAB_TITLE", launch.title.clone()),
    ];
    record_launch(&launch, args.parent.as_deref(), pid, &started_at);
    let mut agent = Agent::start(&args.agent, &launch.cwd, &environment)
        .with_context(|| format!("could not start {}", args.agent[0].to_string_lossy()))?;
    let poll = Duration::from_secs(args.poll_seconds);
    let grace = Duration::from_secs(args.grace_seconds);
    let request = restart::Request::new(&marks, Duration::from_secs(args.restart_grace_seconds));
    let mut watch = Watch {
        launch,
        environment,
        marks,
        poll,
        idle: Duration::from_secs(args.idle_seconds),
        next_poll: Instant::now() + poll,
        reported: HashSet::new(),
    };
    let mut resumed = false;
    let removal = loop {
        if let Some(code) = agent.try_wait()? {
            agent.kill_tree(Duration::from_secs(30))?;
            if resumed && code != 0 {
                eprintln!("\r\ntj run: the resumed agent exited with code {code}\r");
                wait_for_enter();
            }
            let _ = std::fs::remove_dir_all(&watch.marks);
            if let Err(error) = store::end(&watch.launch.id, "exited") {
                eprintln!("tj run: {error:#}");
            }
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
        if let Some(trigger) = watch.trigger() {
            match closeout::preflight(&watch.launch.cwd, trigger == Trigger::Merged) {
                Ok(removal) => break removal,
                Err(reason) => {
                    watch.report(reason);
                    if trigger == Trigger::Done {
                        let _ = std::fs::remove_file(watch.marks.join("done"));
                    }
                }
            }
        }
        if let Some(reason) = request.due() {
            match restart(&mut agent, &mut watch, &args.agent, grace) {
                Ok(Restarted::Kept) => {}
                Ok(Restarted::Resumed(next)) => {
                    agent = next;
                    resumed = true;
                    request.restarted(&reason);
                }
                Err(error) => {
                    eprintln!("tj run: restart stopped: {error:#}");
                    let _ = std::fs::remove_dir_all(&watch.marks);
                    if let Err(error) = store::end(&watch.launch.id, "exited") {
                        eprintln!("tj run: {error:#}");
                    }
                    wait_for_enter();
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let result = stop(&mut agent, watch.launch.host, grace).and_then(|()| closeout::finish(&watch.launch, removal));
    let _ = std::fs::remove_dir_all(&watch.marks);
    if let Err(error) = result {
        eprintln!("tj run: closeout stopped: {error:#}");
        wait_for_enter();
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(clap::Args)]
pub struct DoneArgs {
    /// Withdraw the mark and the idle mark, because new input arrived. A no-op outside `tj run`.
    #[arg(long)]
    cancel: bool,
}

pub fn done(args: DoneArgs) -> Result<ExitCode> {
    let Some(launch_id) = launch_id() else {
        if args.cancel {
            return Ok(ExitCode::SUCCESS);
        }
        bail!("tj done only works inside a tj run session (TJ_RUN_LAUNCH_ID is not set)");
    };
    let marks = marks(&launch_id)?;
    let marker = marks.join("done");
    if args.cancel {
        for mark in [marker, marks.join(restart::IDLE)] {
            match std::fs::remove_file(&mark) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    std::fs::create_dir_all(marker.parent().context("marker has no folder")?)?;
    std::fs::write(&marker, rfc3339(SystemTime::now()))?;
    println!("Marked done: this session closes out once it has been idle for about 10 minutes. New input cancels.");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use crate::host::Host;

    #[test]
    fn the_host_is_read_from_the_agent_executable_name() {
        assert_eq!(Host::from_program(OsStr::new("claude")), Some(Host::Claude));
        assert_eq!(Host::from_program(Path::new("bin").join("Codex.EXE").as_os_str()), Some(Host::Codex));
        assert_eq!(Host::from_program(OsStr::new("python")), None);
    }
}
