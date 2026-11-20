pub mod closeout;
pub mod lifecycle;
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
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use tj_cli::process::agent::{self, Agent, Stop};

use crate::completion::{Record, State, Store};
use crate::domain::item;
use crate::host::Host;
use crate::item_cli;
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
    pub item: Option<item::Reference>,
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
    /// Original invocation directory, so relative native arguments retain their meaning.
    #[arg(long)]
    command_cwd: Option<PathBuf>,
    /// Full work item reference or URL, or a bare id resolved by the working directory's tracker profile.
    #[arg(long)]
    item: Option<String>,
    /// Identifies this launch in agent-store; the launcher waits for the row it names. Generated when omitted.
    #[arg(long)]
    launch_id: Option<String>,
    /// The session that handed this work off, as `host:id`.
    #[arg(long)]
    parent: Option<String>,
    /// Bind a pre-created native Codex session, as `codex:UUID`.
    #[arg(long)]
    session: Option<String>,
    #[arg(long, hide = true, default_value_t = 120)]
    poll_seconds: u64,
    #[arg(long, hide = true, default_value_t = 10)]
    idle_seconds: u64,
    #[arg(long, hide = true, default_value_t = 10)]
    grace_seconds: u64,
    #[arg(long, hide = true, default_value_t = 90)]
    native_exit_seconds: u64,
    #[arg(long, hide = true, default_value_t = 2)]
    plugin_grace_seconds: u64,
    #[arg(long, hide = true, default_value_t = 2)]
    input_idle_seconds: u64,
    #[arg(long, hide = true, default_value = update::upstream::MARKETPLACE)]
    update_url: String,
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

fn preparation(launch: &Launch, directory: &Path, arguments: &[OsString]) -> Result<()> {
    let arguments = arguments[1..]
        .iter()
        .map(|argument| argument.to_str().context("host arguments must be UTF-8 for standards preparation"))
        .collect::<Result<Vec<_>>>()?;
    let input = serde_json::to_vec(&arguments)?;
    let mut child = hidden("tj-prepare")
        .args(["--host", launch.host.name(), "--cwd"])
        .arg(directory)
        .arg("--arguments-stdin")
        .current_dir(directory)
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
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

fn prepare(launch: &Launch, directory: &Path, arguments: &[OsString]) {
    if let Err(error) = preparation(launch, directory, arguments) {
        eprintln!("TJ: repository standards preparation failed: {error:#}");
    }
}

/// The agent's SessionStart registers against this row, so it must exist before the agent starts.
fn record_launch(launch: &Launch, parent: Option<&str>, wrapper_pid: u32, wrapper_started_at: &str) -> Result<()> {
    let mut failure = None;
    for delay in [0, 1, 2, 4].map(Duration::from_secs) {
        std::thread::sleep(delay);
        match store::launch(launch, parent, wrapper_pid, wrapper_started_at) {
            Ok(()) => return Ok(()),
            Err(_) if store::recorded(&launch.id) => return Ok(()),
            Err(error) => failure = Some(error),
        }
    }
    Err(failure.unwrap_or_else(|| anyhow::anyhow!("agent-store did not record launch {}", launch.id)))
}

pub(crate) fn marks(launch_id: &str) -> Result<PathBuf> {
    if launch_id.is_empty() || !launch_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("launch id {launch_id:?} must be letters, digits, '-' or '_'");
    }
    Ok(paths::cache_dir()?.join("run").join(launch_id))
}

fn launch_id() -> Option<String> {
    std::env::var("TJ_RUN_LAUNCH_ID").ok().filter(|id| !id.is_empty())
}

pub(crate) fn generated_id() -> Result<String> {
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
    let item = args.item.as_deref().map(|value| item_cli::reference(value, &cwd, hidden("tj-item"))).transpose()?;
    let program = &args.agent[0];
    let Some(host) = args.host.or_else(|| Host::from_program(program)) else {
        bail!("pass --host: {} is neither claude nor codex", program.to_string_lossy());
    };
    let title = args.title.clone().unwrap_or_else(|| default_title(&cwd));
    let id = args.launch_id.clone().map_or_else(generated_id, Ok)?;
    Ok(Launch { id, host, title, cwd, item })
}

struct PreboundSession {
    handle: String,
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
        })
}

fn prebound_session(args: &Args, launch: &Launch) -> Result<Option<PreboundSession>> {
    let Some(handle) = &args.session else { return Ok(None) };
    let Some(native_id) = handle.strip_prefix("codex:") else {
        bail!("--session must be codex:UUID");
    };
    if !canonical_uuid(native_id) {
        bail!("--session must contain a canonical lowercase UUID");
    }
    if args.host != Some(Host::Codex) || launch.host != Host::Codex {
        bail!("--session requires --host codex");
    }
    let executable = Path::new(&args.agent[0]);
    if !executable.is_absolute()
        || Host::from_program(executable.as_os_str()) != Some(Host::Codex)
        || executable.extension().is_some_and(|extension| !extension.to_string_lossy().eq_ignore_ascii_case("exe"))
    {
        bail!("--session requires an absolute native codex executable");
    }
    let expected = [
        args.agent[0].as_os_str(),
        OsStr::new("--cd"),
        launch.cwd.as_os_str(),
        OsStr::new("resume"),
        OsStr::new(native_id),
    ];
    let mut remaining = args.agent.get(expected.len()..).unwrap_or_default();
    if let [option, model, rest @ ..] = remaining {
        if option == "--model" && !model.is_empty() && !model.to_string_lossy().starts_with('-') {
            remaining = rest;
        }
    }
    if let [option, effort, rest @ ..] = remaining {
        if option == "-c"
            && effort.to_str().is_some_and(|value| {
                value.strip_prefix("model_reasoning_effort=").is_some_and(|value| !value.is_empty())
            })
        {
            remaining = rest;
        }
    }
    let attached = match remaining {
        [option, target] => option == "--remote" && target == "unix://",
        [option, target, delimiter, _] => option == "--remote" && target == "unix://" && delimiter == "--",
        _ => false,
    };
    if !attached || args.agent.iter().take(expected.len()).map(OsString::as_os_str).ne(expected) {
        bail!(
            "--session requires `codex --cd <cwd> resume <UUID> [--model <model>] [-c model_reasoning_effort=<effort>] --remote unix:// [-- <prompt>]`"
        );
    }
    Ok(Some(PreboundSession { handle: handle.clone() }))
}

fn end_not_started(launch: &Launch, prebound: &PreboundSession, wrapper_pid: u32, wrapper_started_at: &str) {
    if let Err(error) = store::end_not_started(launch, &prebound.handle, wrapper_pid, wrapper_started_at) {
        eprintln!("TJ: could not mark unstarted launch {}: {error:#}", launch.id);
    }
}

const REGISTRATION_REPORT_GRACE: Duration = Duration::from_secs(300);

struct Watch {
    launch: Launch,
    marks: PathBuf,
    poll: Duration,
    idle: Duration,
    input_idle: Duration,
    next_poll: Instant,
    reported: HashSet<String>,
    completion: Option<Store>,
    wrapper_started_at: String,
    registration_report_at: Instant,
    revision: Option<u64>,
    ready_since: Option<Instant>,
    pinned: Option<turn::Turn>,
    rollout: Option<PathBuf>,
    next_lookup: Instant,
}

impl Watch {
    fn report(&mut self, line: String) {
        if !self.reported.contains(&line) {
            eprintln!("\r\nTJ: {line}\r");
            self.reported.insert(line);
        }
    }

    fn turn(&mut self, agent: u32) -> Option<turn::Turn> {
        match self.launch.host {
            Host::Claude => turn::claude(agent),
            Host::Codex => {
                if self.rollout.is_none() && Instant::now() >= self.next_lookup {
                    self.next_lookup = Instant::now() + self.poll.max(Duration::from_secs(10));
                    self.rollout =
                        store::bound_session(&self.launch.id).ok().flatten().and_then(|thread| turn::rollout(&thread));
                }
                self.rollout.as_deref().and_then(turn::codex)
            }
        }
    }

    fn record(&mut self) -> Result<Option<Record>> {
        if self.completion.is_none() {
            self.completion = lifecycle::for_startup(&self.launch, &self.wrapper_started_at)?;
            if self.completion.is_none() {
                self.awaiting_registration(Instant::now());
                return Ok(None);
            }
        }
        let store = self.completion.as_ref().context("completion store not bound")?;
        store.load()?.map(|record| lifecycle::deliver(store, record)).transpose()
    }

    fn awaiting_registration(&mut self, now: Instant) {
        if now >= self.registration_report_at {
            self.report("still awaiting native session registration".into());
        }
    }

    fn exited(&mut self, agent: u32, code: i32) -> Result<Option<Record>> {
        if self.completion.is_none() {
            self.completion = Some(
                lifecycle::for_launch(&self.launch.id).context("native exited without a verified session binding")?,
            );
        }
        if code == 0 { self.natural_exit(agent) } else { Ok(None) }
    }

    fn ready(&mut self) -> Result<Option<Record>> {
        let Some(record) = self.record()? else { return Ok(None) };
        if self.revision != Some(record.revision()) {
            self.revision = Some(record.revision());
            self.pinned = None;
            self.ready_since = None;
            self.report(lifecycle::message(&record));
        }
        if record.held_open()
            || !matches!(
                record.state(),
                State::Ready { .. } | State::Closing { .. } | State::Attention { outcome: Some(_), .. }
            )
        {
            return Ok(None);
        }
        Ok(Some(record))
    }

    fn natural_exit(&mut self, agent: u32) -> Result<Option<Record>> {
        let Some(record) = self.ready()? else { return Ok(None) };
        let turn = self.turn(agent);
        if turn.is_some_and(|turn| turn.idle) && self.pinned.is_none_or(|pinned| Some(pinned) == turn) {
            return Ok(Some(record));
        }
        let reason = "Native activity after the final response is changed or unverified; the work remains open";
        let store = self.completion.as_ref().context("completion store not bound")?.clone();
        lifecycle::Closing::begin(store, &record)?.withdraw(reason)?;
        self.report(reason.into());
        Ok(None)
    }

    fn trigger(&mut self, agent: u32) -> Option<lifecycle::Closing> {
        let ready = self.ready();
        match ready {
            Ok(Some(record)) => {
                let turn = self.turn(agent);
                if let Some(turn) = turn.filter(|turn| turn.idle) {
                    if self.pinned.is_some_and(|pinned| pinned != turn) {
                        self.report(
                            "Native activity changed after the final response; closeout needs a new verified turn."
                                .into(),
                        );
                        return None;
                    }
                    self.pinned = Some(turn);
                    let since = *self.ready_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= self.idle
                        && turn::shells(agent) == 0
                        && agent::input_idle().is_none_or(|idle| idle >= self.input_idle)
                    {
                        return match lifecycle::Closing::begin(self.completion.as_ref()?.clone(), &record) {
                            Ok(closing) => Some(closing),
                            Err(error) => {
                                self.report(format!("{error:#}"));
                                None
                            }
                        };
                    }
                } else {
                    self.ready_since = None;
                }
            }
            Ok(None) => {}
            Err(error) => self.report(format!("completion remains unverified: {error:#}")),
        }
        if Instant::now() < self.next_poll {
            return None;
        }
        self.next_poll = Instant::now() + self.poll;
        if let Err(error) = closeout::notify(&self.launch) {
            self.report(format!("could not record merge notification: {error:#}"));
        }
        None
    }
}

fn stop(agent: &mut Agent, host: Host, grace: Duration, mut allowed: impl FnMut(u32) -> Result<bool>) -> Result<bool> {
    let (signal, presses) = host.stop();
    let deadline = Instant::now() + grace;
    let mut sent = 0;
    while agent.try_wait()?.is_none() && Instant::now() < deadline {
        if !allowed(agent.pid())? {
            return Ok(false);
        }
        if sent < presses {
            if let Err(error) = agent.stop(signal) {
                eprintln!("TJ: could not ask the agent to exit: {error}");
                sent = presses;
            }
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    if !allowed(agent.pid())? {
        return Ok(false);
    }
    agent.kill_tree(Duration::from_secs(30)).context("could not end the agent's processes")?;
    Ok(true)
}

fn wait_for_enter() {
    eprintln!("TJ: press Enter to close this tab");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

static OPEN_RECORD: Mutex<()> = Mutex::new(());

fn mark_open(launch_id: &str, open: bool) {
    let _record = OPEN_RECORD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Err(error) = store::set_open(launch_id, open) {
        eprintln!("TJ: {error:#}");
    }
}

/// Holds the record lock until the process exits, so no later write can mark the closed tab open again.
fn mark_closed(launch_id: &str) {
    std::mem::forget(OPEN_RECORD.lock().unwrap_or_else(PoisonError::into_inner));
    let _ = store::set_open(launch_id, false);
}

pub fn run(args: Args) -> Result<ExitCode> {
    let launch = launch(&args)?;
    let prebound = prebound_session(&args, &launch)?;
    let id = launch.id.clone();
    let closed = launch.id.clone();
    agent::on_tab_closed(Box::new(move || mark_closed(&closed)));
    let result = supervise(&args, launch, prebound.as_ref());
    if !agent::session_ending() {
        mark_open(&id, false);
    }
    result
}

fn supervise(args: &Args, launch: Launch, prebound: Option<&PreboundSession>) -> Result<ExitCode> {
    let command_cwd = args.command_cwd.as_deref().unwrap_or(&launch.cwd);
    let exe = std::env::current_exe().context("could not locate the tj executable")?;
    let mut watcher = update::Watcher::new(exe, update::published(launch.host)?);
    let selected = tj_agents::selected(&launch.cwd);
    let marks = marks(&launch.id)?;
    let _ = std::fs::remove_dir_all(&marks);
    let pid = std::process::id();
    let started_at = rfc3339(agent::started_at(pid)?.context("could not identify this wrapper process")?);
    let environment = [
        ("TJ_RUN_PID", pid.to_string()),
        ("TJ_RUN_STARTED_AT", started_at.clone()),
        ("TJ_RUN_LAUNCH_ID", launch.id.clone()),
        ("AGENT_CLI_TAB_TITLE", launch.title.clone()),
    ];
    if launch.host == Host::Codex && Host::from_program(&args.agent[0]) == Some(Host::Codex) {
        // A maintenance command can also be a direct codex child. A pending record keeps it
        // out of spawned receipts until the wrapper records the actual TUI's birth.
        crate::process_command::record_transport(&marks, &launch.id, pid, &started_at, None)?;
    }
    if let Err(error) = record_launch(&launch, args.parent.as_deref(), pid, &started_at) {
        if let Some(prebound) = prebound {
            end_not_started(&launch, prebound, pid, &started_at);
            return Err(error);
        }
        return Err(error).context("managed launch was not recorded; the native host was not started");
    }
    if let Some(prebound) = prebound {
        if let Err(error) = store::prebind(&launch, &prebound.handle, pid, &started_at) {
            end_not_started(&launch, prebound, pid, &started_at);
            return Err(error);
        }
    }
    // Ensure the native service before Linux adopts or Windows owns TUI descendants.
    let prepare_host = || {
        let host = update::host(launch.host, &args.agent, command_cwd)?;
        agent::prepare_wrapper().context("could not prepare the console")?;
        Ok(host)
    };
    let host = match prepare_host() {
        Ok(host) => host,
        Err(error) => {
            if let Some(prebound) = prebound {
                end_not_started(&launch, prebound, pid, &started_at);
            } else {
                let _ = store::end_not_started_unbound(&launch, pid, &started_at);
            }
            return Err(error);
        }
    };
    let upstream =
        if selected { update::upstream::start(launch.host, &launch.cwd, args.update_url.clone()) } else { None };
    mark_open(&launch.id, true);
    if selected {
        prepare(&launch, command_cwd, &args.agent);
    }
    match upstream.map(std::thread::JoinHandle::join) {
        Some(Ok(Ok(Some(line)))) => eprintln!("TJ: {line}"),
        Some(Ok(Err(error))) => {
            eprintln!("TJ: could not check for tj-agents updates: {:#}", anyhow::Error::from(error));
        }
        _ => {}
    }
    let mut agent = match Agent::start(&args.agent, command_cwd, &environment)
        .with_context(|| format!("could not start {}", args.agent[0].to_string_lossy()))
    {
        Ok(agent) => agent,
        Err(error) => {
            if let Some(prebound) = prebound {
                end_not_started(&launch, prebound, pid, &started_at);
            }
            return Err(error);
        }
    };
    if let Some(endpoint) = host.endpoint() {
        let record = agent::started_at(agent.pid()).map_err(anyhow::Error::from).and_then(|born| {
            if let Some(born) = born {
                crate::process_command::record_transport(
                    &marks,
                    &launch.id,
                    pid,
                    &started_at,
                    Some((agent.pid(), born, endpoint)),
                )?;
            }
            Ok(())
        });
        if let Err(error) = record {
            agent.kill_tree(Duration::from_secs(30))?;
            return Err(error).context("could not record the native Codex transport");
        }
    }
    let poll = Duration::from_secs(args.poll_seconds);
    let grace = Duration::from_secs(args.grace_seconds);
    let graces = update::gate::Graces {
        plugins: Duration::from_secs(args.plugin_grace_seconds),
        input: Duration::from_secs(args.input_idle_seconds),
    };
    let mut watch = Watch {
        launch,
        marks,
        poll,
        idle: Duration::from_secs(args.idle_seconds),
        input_idle: Duration::from_secs(args.input_idle_seconds),
        next_poll: Instant::now() + poll,
        reported: HashSet::new(),
        completion: None,
        wrapper_started_at: started_at,
        registration_report_at: Instant::now() + REGISTRATION_REPORT_GRACE,
        revision: None,
        ready_since: None,
        pinned: None,
        rollout: None,
        next_lookup: Instant::now(),
    };
    // The shared timestamp limits requests even when tabs start at different times.
    let mut next_upstream = Instant::now() + poll;
    let mut upstream = None;
    loop {
        if let Some(code) = agent.try_wait()? {
            agent.kill_tree(Duration::from_secs(30))?;
            // A clean process exit is not a final result. Only the exact native receipt can close.
            let ready = match watch.exited(agent.pid(), code) {
                Ok(ready) => ready,
                Err(error) => {
                    watch.report(format!("completion remains unverified: {error:#}"));
                    None
                }
            };
            if let Some(record) = ready {
                let store = watch.completion.as_ref().context("completion store not bound")?.clone();
                let closing = lifecycle::Closing::begin(store, &record)?;
                let result =
                    closeout::await_native_exit(&watch.launch, &closing, Duration::from_secs(args.native_exit_seconds))
                        .and_then(|()| {
                            closing.attempt(crate::completion::Step::Cleanup, || {
                                closeout::owned_preflight(&watch.launch).map_err(anyhow::Error::msg)
                            })
                        })
                        .and_then(|plan| closeout::finish(&watch.launch, plan, &closing));
                if let Err(error) = result {
                    eprintln!("TJ: closeout stopped: {error:#}");
                    wait_for_enter();
                    return Ok(ExitCode::FAILURE);
                }
                return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
            }
            if let Err(error) = store::end(&watch.launch.id, "exited") {
                eprintln!("TJ: {error:#}");
            }
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
        if let Some(closing) = watch.trigger(agent.pid()) {
            match closeout::owned_preflight(&watch.launch) {
                Ok(removal) => {
                    // Preflight can take time. Recheck input and native activity before ending the host.
                    if closing.check().is_err() {
                        watch.report("Closeout withdrawn while checking ownership; the session remains open.".into());
                        continue;
                    }
                    if watch.turn(agent.pid()) != watch.pinned || turn::shells(agent.pid()) != 0 {
                        closing.withdraw("Native activity changed during closeout preflight")?;
                        continue;
                    }
                    let stopped = stop(&mut agent, watch.launch.host, grace, |pid| {
                        if closing.check().is_err() {
                            return Ok(false);
                        }
                        // The native status may disappear once the host exits. Its durable token
                        // still rejects a final hook that recorded new input during shutdown.
                        match watch.turn(pid) {
                            Some(turn) if Some(turn) == watch.pinned => Ok(true),
                            None if agent::started_at(pid)?.is_none() => Ok(true),
                            _ => {
                                let _ = closing.withdraw("Native activity changed while stopping the host");
                                Ok(false)
                            }
                        }
                    })?;
                    if !stopped {
                        watch.report("Closeout withdrawn during shutdown; no further stop signal was sent.".into());
                        continue;
                    }
                    let result = closeout::await_native_exit(
                        &watch.launch,
                        &closing,
                        Duration::from_secs(args.native_exit_seconds),
                    )
                    .and_then(|()| closeout::finish(&watch.launch, removal, &closing));
                    let _ = std::fs::remove_dir_all(&watch.marks);
                    if let Err(error) = result {
                        eprintln!("tj: closeout stopped: {error:#}");
                        wait_for_enter();
                        return Ok(ExitCode::FAILURE);
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                Err(reason) => {
                    let _ = closing.fail(crate::completion::Step::Cleanup, &reason);
                    watch.report(reason);
                }
            }
        }
        if upstream.as_ref().is_some_and(std::thread::JoinHandle::is_finished) {
            match upstream.take().map(std::thread::JoinHandle::join) {
                Some(Ok(Ok(Some(line)))) => eprintln!("TJ: {line}"),
                Some(Ok(Err(error))) => watch.report(format!("could not check for tj-agents updates: {error}")),
                _ => {}
            }
        }
        if selected && upstream.is_none() && Instant::now() >= next_upstream {
            next_upstream = Instant::now() + poll;
            upstream = update::upstream::start(watch.launch.host, &watch.launch.cwd, args.update_url.clone());
        }
        let finishing = watch.completion.as_ref().and_then(|store| store.load().ok().flatten()).is_some_and(|record| {
            matches!(
                record.state(),
                State::Active { intent: Some(_), .. } | State::Ready { .. } | State::Closing { .. }
            )
        });
        if watcher.binary_notice(Duration::from_secs(2)) {
            watch.report("A new tj binary is installed. Your next launch will use it.".into());
        }
        if !finishing
            && let Some(revision) = watcher.revision()
            && update::plugins_due(host.as_ref(), agent.pid(), graces)
        {
            match host.apply(&watch.launch, &revision) {
                Ok(true) => watcher.applied(Some(revision)),
                Ok(false) => {}
                Err(error) => watch.report(format!(
                    "could not update {}: {:#}",
                    watch.launch.host.name(),
                    anyhow::Error::new(error)
                )),
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    use crate::host::Host;

    use super::{Args, Launch, canonical_uuid, prebound_session};

    fn prebound_args(agent: Vec<OsString>) -> Args {
        Args {
            host: Some(Host::Codex),
            title: Some("Tab".to_owned()),
            cwd: None,
            command_cwd: None,
            item: None,
            launch_id: Some("launch".to_owned()),
            parent: None,
            session: Some("codex:123e4567-e89b-12d3-a456-426614174000".to_owned()),
            poll_seconds: 120,
            idle_seconds: 600,
            grace_seconds: 10,
            native_exit_seconds: 90,
            plugin_grace_seconds: 2,
            input_idle_seconds: 2,
            update_url: String::new(),
            agent,
        }
    }

    fn cwd() -> PathBuf {
        if cfg!(windows) { PathBuf::from("C:/worktree") } else { PathBuf::from("/worktree") }
    }

    fn executable() -> OsString {
        if cfg!(windows) { OsString::from("C:/bin/codex.exe") } else { OsString::from("/bin/codex") }
    }

    fn launch() -> Launch {
        Launch { id: "launch".to_owned(), host: Host::Codex, title: "Tab".to_owned(), cwd: cwd(), item: None }
    }

    #[test]
    fn registration_waiting_is_reported_once_only_after_its_grace() {
        let start = std::time::Instant::now();
        let deadline = start + super::REGISTRATION_REPORT_GRACE;
        let mut watch = super::Watch {
            launch: launch(),
            marks: PathBuf::new(),
            poll: std::time::Duration::ZERO,
            idle: std::time::Duration::ZERO,
            input_idle: std::time::Duration::ZERO,
            next_poll: start,
            reported: std::collections::HashSet::new(),
            completion: None,
            wrapper_started_at: String::new(),
            registration_report_at: deadline,
            revision: None,
            ready_since: None,
            pinned: None,
            rollout: None,
            next_lookup: start,
        };
        watch.awaiting_registration(start);
        watch.awaiting_registration(deadline - std::time::Duration::from_nanos(1));
        assert!(watch.reported.is_empty());
        watch.awaiting_registration(deadline);
        watch.awaiting_registration(deadline + super::REGISTRATION_REPORT_GRACE);
        assert_eq!(watch.reported.len(), 1);
        assert!(watch.reported.contains("still awaiting native session registration"));
        assert!(watch.completion.is_none());
    }

    #[test]
    fn the_host_is_read_from_the_agent_executable_name() {
        assert_eq!(Host::from_program(OsStr::new("claude")), Some(Host::Claude));
        assert_eq!(Host::from_program(Path::new("bin").join("Codex.EXE").as_os_str()), Some(Host::Codex));
        assert_eq!(Host::from_program(OsStr::new("python")), None);
    }

    #[test]
    fn prebound_session_admits_only_the_native_codex_resume_shape() {
        let launch = launch();
        let without_prompt = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
        ]);
        assert_eq!(
            prebound_session(&without_prompt, &launch).unwrap().unwrap().handle,
            without_prompt.session.unwrap()
        );

        let with_prompt = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
            OsString::from("--"),
            OsString::from("Prompt"),
        ]);
        assert_eq!(prebound_session(&with_prompt, &launch).unwrap().unwrap().handle, with_prompt.session.unwrap());
    }

    #[test]
    fn prebound_session_carries_only_model_and_reasoning_overrides() {
        let launch = launch();
        for overrides in [
            vec!["--model", "chosen"],
            vec!["-c", "model_reasoning_effort=xhigh"],
            vec!["--model", "chosen", "-c", "model_reasoning_effort=high"],
            vec![],
        ] {
            let mut agent = vec![
                executable(),
                "--cd".into(),
                launch.cwd.clone().into_os_string(),
                "resume".into(),
                "123e4567-e89b-12d3-a456-426614174000".into(),
            ];
            agent.extend(overrides.into_iter().map(OsString::from));
            agent.extend(["--remote".into(), "unix://".into(), "--".into(), "--model=literal-prompt".into()]);
            assert!(prebound_session(&prebound_args(agent), &launch).is_ok());
        }
        for overrides in [
            vec!["--model", ""],
            vec!["--model", "--last"],
            vec!["-c", "model_reasoning_effort="],
            vec!["-c", "approval_policy=never"],
            vec!["--last"],
            vec!["--model", "chosen", "--model", "other"],
            vec!["--remote", "unix://"],
            vec!["--remote", "ws://localhost:9000"],
            vec!["--remote", "unix:///another-daemon.sock"],
        ] {
            let mut agent = vec![
                executable(),
                "--cd".into(),
                launch.cwd.clone().into_os_string(),
                "resume".into(),
                "123e4567-e89b-12d3-a456-426614174000".into(),
            ];
            agent.extend(overrides.into_iter().map(OsString::from));
            agent.extend(["--remote".into(), "unix://".into()]);
            assert!(prebound_session(&prebound_args(agent), &launch).is_err());
        }
    }

    #[test]
    fn prebound_session_admits_only_a_delimited_literal_prompt() {
        let launch = launch();
        for prompt in ["--help", "--model=other", "-", " spaced 世界"] {
            let args = prebound_args(vec![
                executable(),
                OsString::from("--cd"),
                launch.cwd.clone().into_os_string(),
                OsString::from("resume"),
                OsString::from("123e4567-e89b-12d3-a456-426614174000"),
                OsString::from("--remote"),
                OsString::from("unix://"),
                OsString::from("--"),
                OsString::from(prompt),
            ]);
            assert!(prebound_session(&args, &launch).is_ok());
        }
    }

    #[test]
    fn prebound_session_rejects_undelimited_or_extra_arguments() {
        let launch = launch();
        let missing_attachment = prebound_args(vec![
            executable(),
            "--cd".into(),
            launch.cwd.clone().into_os_string(),
            "resume".into(),
            "123e4567-e89b-12d3-a456-426614174000".into(),
        ]);
        assert!(prebound_session(&missing_attachment, &launch).is_err());
        let bare_prompt = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
            OsString::from("Prompt"),
        ]);
        assert!(prebound_session(&bare_prompt, &launch).is_err());

        let bare_option = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
            OsString::from("--model=other"),
        ]);
        assert!(prebound_session(&bare_option, &launch).is_err());

        let bare_delimiter = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
            OsString::from("--"),
        ]);
        assert!(prebound_session(&bare_delimiter, &launch).is_err());

        let extra = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--remote"),
            OsString::from("unix://"),
            OsString::from("--"),
            OsString::from("Prompt"),
            OsString::from("extra"),
        ]);
        assert!(prebound_session(&extra, &launch).is_err());

        let reordered = prebound_args(vec![
            executable(),
            OsString::from("resume"),
            launch.cwd.clone().into_os_string(),
            OsString::from("--cd"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
        ]);
        assert!(prebound_session(&reordered, &launch).is_err());
        assert!(!canonical_uuid("123E4567-e89b-12d3-a456-426614174000"));

        let mut wrong_host = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
        ]);
        wrong_host.host = Some(Host::Claude);
        assert!(prebound_session(&wrong_host, &launch).is_err());

        let wrong_cwd = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            OsString::from("/other"),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
        ]);
        assert!(prebound_session(&wrong_cwd, &launch).is_err());
    }
}
