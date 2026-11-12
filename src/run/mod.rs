pub mod closeout;
pub mod owner;
pub(crate) mod store;
mod tj_agents;
mod turn;
pub mod update;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use tj_cli::process::agent::{self, Agent, Stop};

use crate::host::Host;
use crate::paths;

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
    #[arg(long, hide = true, default_value_t = 2)]
    plugin_grace_seconds: u64,
    #[arg(long, hide = true, default_value_t = 2)]
    input_idle_seconds: u64,
    #[arg(long, hide = true, default_value = update::upstream::MARKETPLACE)]
    update_url: String,
    /// Continues this launch in a replaced tj binary: the launch, its marks and its conversation already exist.
    #[arg(long, hide = true)]
    successor: bool,
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

fn preparation(launch: &Launch, arguments: &[OsString]) -> Result<()> {
    let arguments = arguments[1..]
        .iter()
        .map(|argument| argument.to_str().context("host arguments must be UTF-8 for standards preparation"))
        .collect::<Result<Vec<_>>>()?;
    let input = serde_json::to_vec(&arguments)?;
    let mut child = hidden("tj-prepare")
        .args(["--host", launch.host.name(), "--cwd"])
        .arg(&launch.cwd)
        .arg("--arguments-stdin")
        .current_dir(&launch.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not start tj-prepare")?;
    let written = child
        .stdin
        .take()
        .context("tj-prepare has no input pipe")
        .and_then(|mut stdin| stdin.write_all(&input).context("could not send host arguments to tj-prepare"));
    let output = child.wait_with_output().context("could not wait for tj-prepare")?;
    written?;
    if !output.status.success() {
        bail!("tj-prepare {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn prepare(launch: &Launch, arguments: &[OsString]) {
    if let Err(error) = preparation(launch, arguments) {
        eprintln!("tj run: repository standards preparation failed: {error:#}");
    }
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
    let taken: HashSet<String> = store::launches(None)
        .map(|rows| rows.into_iter().filter(|row| !row.ended).map(|row| row.launch.title).collect())
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
    Done,
    Closing,
}

struct Watch {
    launch: Launch,
    marks: PathBuf,
    poll: Duration,
    idle: Duration,
    next_poll: Instant,
    reported: HashSet<String>,
    asked: bool,
    mark: Option<SystemTime>,
    pinned: Option<turn::Turn>,
    rollout: Option<PathBuf>,
    next_lookup: Instant,
}

impl Watch {
    fn report(&mut self, line: String) {
        if !self.reported.contains(&line) {
            eprintln!("\r\ntj run: {line}\r");
            self.reported.insert(line);
        }
    }

    fn marked(&self) -> Option<SystemTime> {
        self.marks.join("done").metadata().and_then(|meta| meta.modified()).ok()
    }

    fn turn(&mut self, agent: u32) -> (Option<turn::Turn>, u32) {
        match self.launch.host {
            Host::Claude => (turn::claude(agent), turn::shells(agent)),
            Host::Codex => {
                if self.rollout.is_none() && Instant::now() >= self.next_lookup {
                    self.next_lookup = Instant::now() + Duration::from_secs(10);
                    self.rollout =
                        store::bound_session(&self.launch.id).ok().flatten().and_then(|thread| turn::rollout(&thread));
                }
                (self.rollout.as_deref().and_then(turn::codex), 0)
            }
        }
    }

    fn done(&mut self, agent: u32) -> bool {
        let marked = self.marked();
        if marked != self.mark {
            self.mark = marked;
            self.pinned = None;
        }
        let Some(marked) = marked else { return false };
        let (turn, background) = self.turn(agent);
        match turn::judge(marked, SystemTime::now(), turn, &mut self.pinned, background, self.idle) {
            turn::Done::Due => true,
            turn::Done::Waiting => false,
            turn::Done::Withdrawn => {
                let _ = std::fs::remove_file(self.marks.join("done"));
                self.report(
                    "a turn started after `tj done`, which withdrew it; the agent runs it again when finished".into(),
                );
                false
            }
        }
    }

    fn closing(&mut self) -> bool {
        store::closing(&self.launch.id).unwrap_or_else(|error| {
            self.report(format!("{error:#}"));
            false
        })
    }

    fn delivered(&mut self) -> bool {
        closeout::delivered(&self.launch).unwrap_or_else(|error| {
            self.report(format!("{error:#}"));
            false
        })
    }

    fn trigger(&mut self, agent: u32) -> Option<Trigger> {
        if self.done(agent) {
            return Some(Trigger::Done);
        }
        if Instant::now() < self.next_poll {
            return None;
        }
        self.next_poll = Instant::now() + self.poll;
        if self.closing() {
            return Some(Trigger::Closing);
        }
        if !self.asked && self.delivered() {
            let item = self.launch.item.clone().unwrap_or_default();
            match closeout::ask(&self.launch, &item) {
                Ok(()) => self.asked = true,
                Err(error) => self.report(format!("could not ask the agent to close out: {error:#}")),
            }
        }
        None
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

fn successor_arguments(args: &Args, launch: &Launch, agent: Vec<OsString>) -> Vec<OsString> {
    let mut arguments: Vec<OsString> =
        ["run", "--host", launch.host.name(), "--title", &launch.title, "--cwd"].map(OsString::from).to_vec();
    arguments.push(launch.cwd.clone().into_os_string());
    if let Some(item) = &launch.item {
        arguments.extend(["--item", item].map(OsString::from));
    }
    let seconds = [
        ("--poll-seconds", args.poll_seconds),
        ("--idle-seconds", args.idle_seconds),
        ("--grace-seconds", args.grace_seconds),
        ("--restart-grace-seconds", args.restart_grace_seconds),
        ("--plugin-grace-seconds", args.plugin_grace_seconds),
        ("--input-idle-seconds", args.input_idle_seconds),
    ];
    for (name, value) in seconds {
        arguments.extend([OsString::from(name), OsString::from(value.to_string())]);
    }
    arguments.extend(["--launch-id", &launch.id, "--successor", "--"].map(OsString::from));
    arguments.extend(agent);
    arguments
}

/// Stops the agent and hands the tab to the replaced tj binary, resuming the same conversation.
fn restart(agent: &mut Agent, watch: &mut Watch, args: &Args, exe: &Path) -> Result<Option<ExitCode>> {
    let host = watch.launch.host;
    let session = match store::bound_session(&watch.launch.id) {
        Ok(Some(session)) => session,
        Ok(None) => {
            watch.report(format!(
                "not updating tj: no {} session is bound to launch {} yet",
                host.name(),
                watch.launch.id
            ));
            return Ok(None);
        }
        Err(error) => {
            watch.report(format!("not updating tj: {error:#}"));
            return Ok(None);
        }
    };
    eprintln!("\r\ntj run: tj was updated; resuming {}:{session} in the new version\r", host.name());
    stop(agent, host, Duration::from_secs(args.grace_seconds))?;
    let resume = host.resume(&args.agent, &session);
    update::successor::hand_over(exe, &successor_arguments(args, &watch.launch, resume)).map(Some)
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
    let exe = std::env::current_exe().context("could not locate the tj executable")?;
    let launch = launch(&args)?;
    let selected = tj_agents::selected(&launch.cwd);
    let upstream = if selected && !args.successor {
        update::upstream::start(launch.host, &launch.cwd, args.update_url.clone())
    } else {
        None
    };
    let marks = marks(&launch.id)?;
    if !args.successor {
        let _ = std::fs::remove_dir_all(&marks);
    }
    let pid = std::process::id();
    let started_at = rfc3339(agent::started_at(pid)?.context("could not identify this wrapper process")?);
    let environment = [
        ("TJ_RUN_PID", pid.to_string()),
        ("TJ_RUN_STARTED_AT", started_at.clone()),
        ("TJ_RUN_LAUNCH_ID", launch.id.clone()),
        ("AGENT_CLI_TAB_TITLE", launch.title.clone()),
    ];
    if !args.successor {
        record_launch(&launch, args.parent.as_deref(), pid, &started_at);
    }
    if selected {
        prepare(&launch, &args.agent);
    }
    match upstream.map(std::thread::JoinHandle::join) {
        Some(Ok(Ok(Some(line)))) => eprintln!("tj run: {line}"),
        Some(Ok(Err(error))) => {
            eprintln!("tj run: could not check for tj-agents updates: {:#}", anyhow::Error::from(error));
        }
        _ => {}
    }
    let mut agent = Agent::start(&args.agent, &launch.cwd, &environment)
        .with_context(|| format!("could not start {}", args.agent[0].to_string_lossy()))?;
    let poll = Duration::from_secs(args.poll_seconds);
    let grace = Duration::from_secs(args.grace_seconds);
    let graces = update::gate::Graces {
        plugins: Duration::from_secs(args.plugin_grace_seconds),
        restart: Duration::from_secs(args.restart_grace_seconds),
        input: Duration::from_secs(args.input_idle_seconds),
    };
    let mut watcher = update::Watcher::new(exe, update::published(launch.host)?);
    let mut watch = Watch {
        launch,
        marks,
        poll,
        idle: Duration::from_secs(args.idle_seconds),
        next_poll: Instant::now() + poll,
        reported: HashSet::new(),
        asked: false,
        mark: None,
        pinned: None,
        rollout: None,
        next_lookup: Instant::now(),
    };
    let resumed = args.successor;
    let removal = loop {
        if let Some(code) = agent.try_wait()? {
            agent.kill_tree(Duration::from_secs(30))?;
            if resumed && code != 0 {
                eprintln!("\r\ntj run: the resumed agent exited with code {code}\r");
                wait_for_enter();
            }
            let closing = watch.marked().is_some() || watch.closing() || watch.delivered();
            let _ = std::fs::remove_dir_all(&watch.marks);
            if closing {
                let result = closeout::owned_preflight(&watch.launch)
                    .map_err(anyhow::Error::msg)
                    .and_then(|plan| closeout::finish(&watch.launch, plan));
                if let Err(error) = result {
                    eprintln!("tj run: closeout stopped: {error:#}");
                    wait_for_enter();
                    return Ok(ExitCode::FAILURE);
                }
                return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
            }
            if let Err(error) = store::end(&watch.launch.id, "exited") {
                eprintln!("tj run: {error:#}");
            }
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
        if let Some(trigger) = watch.trigger(agent.pid()) {
            match closeout::owned_preflight(&watch.launch) {
                Ok(removal) => break removal,
                Err(reason) => {
                    watch.report(reason);
                    if trigger == Trigger::Done {
                        let _ = std::fs::remove_file(watch.marks.join("done"));
                    }
                }
            }
        }
        let host = update::host(watch.launch.host);
        let due = if watch.marked().is_some() { None } else { update::due(&mut watcher, host, agent.pid(), graces) };
        match due {
            Some(update::gate::Apply::Plugins) => {
                let revision = watcher.revision();
                match host.apply() {
                    Ok(()) => watcher.applied(revision),
                    Err(error) => watch.report(format!("could not update {}: {error}", watch.launch.host.name())),
                }
            }
            Some(update::gate::Apply::Binary) => {
                let exe = watcher.exe().to_path_buf();
                if let Some(code) = restart(&mut agent, &mut watch, &args, &exe)? {
                    return Ok(code);
                }
            }
            None => {}
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

pub fn done() -> Result<ExitCode> {
    let Some(launch_id) = owner::launch() else {
        if launch_id().is_some() {
            bail!("tj done only works from the tab's own agent; this process inherited another tab's tj run");
        }
        bail!("tj done only works inside a tj run session (TJ_RUN_LAUNCH_ID is not set)");
    };
    let marks = marks(&launch_id)?;
    let marker = marks.join("done");
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
