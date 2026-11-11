pub mod closeout;
pub mod restart;
pub(crate) mod store;

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
    /// Bind a pre-created native Codex session, as `codex:UUID`.
    #[arg(long)]
    session: Option<String>,
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
    let prompt_is_delimited = match args.agent.get(expected.len()..) {
        Some([]) => true,
        Some([delimiter, _]) => delimiter == OsStr::new("--"),
        _ => false,
    };
    if !prompt_is_delimited || args.agent.iter().take(expected.len()).map(OsString::as_os_str).ne(expected) {
        bail!("--session requires `codex --cd <cwd> resume <UUID> [-- <prompt>]`");
    }
    Ok(Some(PreboundSession { handle: handle.clone() }))
}

fn end_not_started(launch: &Launch, prebound: &PreboundSession, wrapper_pid: u32, wrapper_started_at: &str) {
    if let Err(error) = store::end_not_started(launch, &prebound.handle, wrapper_pid, wrapper_started_at) {
        eprintln!("tj run: could not mark unstarted launch {}: {error:#}", launch.id);
    }
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
    prepare(&watch.launch, command);
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
    let prebound = prebound_session(&args, &launch)?;
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
    if let Err(error) = record_launch(&launch, args.parent.as_deref(), pid, &started_at) {
        if let Some(prebound) = &prebound {
            end_not_started(&launch, prebound, pid, &started_at);
            return Err(error);
        }
        eprintln!("tj run: launch {} was not recorded: {error:#}", launch.id);
    }
    if let Some(prebound) = &prebound {
        if let Err(error) = store::prebind(&launch, &prebound.handle, pid, &started_at) {
            end_not_started(&launch, prebound, pid, &started_at);
            return Err(error);
        }
    }
    prepare(&launch, &args.agent);
    let mut agent = match Agent::start(&args.agent, &launch.cwd, &environment)
        .with_context(|| format!("could not start {}", args.agent[0].to_string_lossy()))
    {
        Ok(agent) => agent,
        Err(error) => {
            if let Some(prebound) = &prebound {
                end_not_started(&launch, prebound, pid, &started_at);
            }
            return Err(error);
        }
    };
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
            watch.next_poll = Instant::now();
            let closing = watch.trigger().is_some();
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
        if let Some(trigger) = watch.trigger() {
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
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    use crate::host::Host;

    use super::{Args, Launch, canonical_uuid, prebound_session};

    fn prebound_args(agent: Vec<OsString>) -> Args {
        Args {
            host: Some(Host::Codex),
            title: Some("Tab".to_owned()),
            cwd: None,
            item: None,
            launch_id: Some("launch".to_owned()),
            parent: None,
            session: Some("codex:123e4567-e89b-12d3-a456-426614174000".to_owned()),
            poll_seconds: 120,
            idle_seconds: 600,
            grace_seconds: 10,
            restart_grace_seconds: 30,
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
            OsString::from("--"),
            OsString::from("Prompt"),
        ]);
        assert_eq!(prebound_session(&with_prompt, &launch).unwrap().unwrap().handle, with_prompt.session.unwrap());
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
                OsString::from("--"),
                OsString::from(prompt),
            ]);
            assert!(prebound_session(&args, &launch).is_ok());
        }
    }

    #[test]
    fn prebound_session_rejects_undelimited_or_extra_arguments() {
        let launch = launch();
        let bare_prompt = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("Prompt"),
        ]);
        assert!(prebound_session(&bare_prompt, &launch).is_err());

        let bare_option = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--model=other"),
        ]);
        assert!(prebound_session(&bare_option, &launch).is_err());

        let bare_delimiter = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
            OsString::from("--"),
        ]);
        assert!(prebound_session(&bare_delimiter, &launch).is_err());

        let extra = prebound_args(vec![
            executable(),
            OsString::from("--cd"),
            launch.cwd.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("123e4567-e89b-12d3-a456-426614174000"),
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
