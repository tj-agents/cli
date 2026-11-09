pub mod closeout;
pub(crate) mod store;

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
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
    #[arg(long, value_enum)]
    host: Host,
    /// Tab title, exported to the agent as AGENT_CLI_TAB_TITLE.
    #[arg(long)]
    title: String,
    /// The worktree the agent works in. Only a linked worktree is removed at closeout.
    #[arg(long)]
    cwd: PathBuf,
    /// Work item reference, for example `github:github.com/acme/app:461`.
    #[arg(long)]
    item: Option<String>,
    /// Identifies this launch in agent-store; the launcher waits for the row it names.
    #[arg(long)]
    launch_id: String,
    /// The session that handed this work off, as `host:id`.
    #[arg(long)]
    parent: Option<String>,
    #[arg(long, hide = true, default_value_t = 120)]
    poll_seconds: u64,
    #[arg(long, hide = true, default_value_t = 600)]
    idle_seconds: u64,
    #[arg(long, hide = true, default_value_t = 10)]
    grace_seconds: u64,
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

fn marker(launch_id: &str) -> Result<PathBuf> {
    if launch_id.is_empty() || !launch_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("launch id {launch_id:?} must be letters, digits, '-' or '_'");
    }
    Ok(paths::cache_dir()?.join("run").join(launch_id).join("done"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Merged,
    Done,
    Closing,
}

struct Watch {
    launch: Launch,
    marker: PathBuf,
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
            .marker
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

fn wait_for_enter() {
    eprintln!("tj run: press Enter to close this tab");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn run(args: Args) -> Result<ExitCode> {
    agent::prepare_wrapper().context("could not prepare the console")?;
    let launch = Launch {
        id: args.launch_id,
        host: args.host,
        title: args.title,
        cwd: std::path::absolute(&args.cwd)?,
        item: args.item,
    };
    let marker = marker(&launch.id)?;
    let _ = std::fs::remove_file(&marker);
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
    let mut watch = Watch {
        launch,
        marker,
        poll,
        idle: Duration::from_secs(args.idle_seconds),
        next_poll: Instant::now() + poll,
        reported: HashSet::new(),
    };
    let removal = loop {
        if let Some(code) = agent.try_wait()? {
            agent.kill_tree(Duration::from_secs(30))?;
            let _ = std::fs::remove_dir_all(watch.marker.parent().unwrap_or(&watch.marker));
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
                        let _ = std::fs::remove_file(&watch.marker);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let result = stop(&mut agent, watch.launch.host, Duration::from_secs(args.grace_seconds))
        .and_then(|()| closeout::finish(&watch.launch, removal));
    let _ = std::fs::remove_dir_all(watch.marker.parent().unwrap_or(&watch.marker));
    if let Err(error) = result {
        eprintln!("tj run: closeout stopped: {error:#}");
        wait_for_enter();
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(clap::Args)]
pub struct DoneArgs {
    /// Withdraw the mark, because new input arrived. A no-op outside `tj run`.
    #[arg(long)]
    cancel: bool,
}

pub fn done(args: DoneArgs) -> Result<ExitCode> {
    let Some(launch_id) = std::env::var("TJ_RUN_LAUNCH_ID").ok().filter(|id| !id.is_empty()) else {
        if args.cancel {
            return Ok(ExitCode::SUCCESS);
        }
        bail!("tj done only works inside a tj run session (TJ_RUN_LAUNCH_ID is not set)");
    };
    let marker = marker(&launch_id)?;
    if args.cancel {
        match std::fs::remove_file(&marker) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => return Ok(ExitCode::SUCCESS),
        }
    }
    std::fs::create_dir_all(marker.parent().context("marker has no folder")?)?;
    std::fs::write(&marker, rfc3339(SystemTime::now()))?;
    println!("Marked done: this session closes out once it has been idle for about 10 minutes. New input cancels.");
    Ok(ExitCode::SUCCESS)
}
