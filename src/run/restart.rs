use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tj_cli::process::agent::Agent;

use crate::host::Host;

const CLAUDE_VALUES: &[&str] = &[
    "--add-dir",
    "--agent",
    "--allowed-tools",
    "--allowedTools",
    "--append-system-prompt",
    "--disallowed-tools",
    "--disallowedTools",
    "--effort",
    "--fallback-model",
    "--mcp-config",
    "--model",
    "--name",
    "-n",
    "--permission-mode",
    "--plugin-dir",
    "--setting-sources",
    "--settings",
    "--system-prompt",
    "--tools",
];
const CLAUDE_SESSION_FLAGS: &[&str] = &["--continue", "-c", "--fork-session"];
const CLAUDE_SESSION_VALUES: &[&str] = &["--resume", "-r", "--session-id", "--from-pr"];
const CODEX_VALUES: &[&str] = &[
    "--add-dir",
    "--ask-for-approval",
    "-a",
    "--cd",
    "-C",
    "--config",
    "-c",
    "--disable",
    "--enable",
    "--image",
    "-i",
    "--local-provider",
    "--model",
    "-m",
    "--profile",
    "-p",
    "--remote",
    "--remote-auth-token-env",
    "--sandbox",
    "-s",
];
const CODEX_SESSION_COMMANDS: &[&str] = &["resume", "fork"];

fn is_option(argument: &OsStr) -> bool {
    argument.to_string_lossy().starts_with('-')
}

fn named(argument: &OsStr, names: &[&str]) -> bool {
    let argument = argument.to_string_lossy();
    names.iter().any(|name| argument == *name || argument.strip_prefix(name).is_some_and(|rest| rest.starts_with('=')))
}

impl Host {
    fn takes_value(self, option: &OsStr) -> bool {
        let values = match self {
            Self::Claude => CLAUDE_VALUES,
            Self::Codex => CODEX_VALUES,
        };
        values.iter().any(|value| option == *value)
    }

    /// The original command line pointed at `session`: its options kept, its prompt and session choice dropped.
    pub(super) fn resume(self, agent: &[OsString], session: &str) -> Vec<OsString> {
        let mut kept = vec![agent[0].clone()];
        let mut arguments = agent[1..].iter().peekable();
        while let Some(argument) = arguments.next() {
            let follows_value = kept.len() > 1 && self.takes_value(&kept[kept.len() - 1]);
            match self {
                Self::Claude if named(argument, CLAUDE_SESSION_FLAGS) => continue,
                Self::Claude if named(argument, CLAUDE_SESSION_VALUES) => {
                    if !argument.to_string_lossy().contains('=') {
                        arguments.next_if(|value| !is_option(value));
                    }
                    continue;
                }
                Self::Codex if !follows_value && named(argument, CODEX_SESSION_COMMANDS) => break,
                _ => kept.push(argument.clone()),
            }
        }
        let prompt = kept.len() > 1
            && !is_option(&kept[kept.len() - 1])
            && !(kept.len() > 2 && self.takes_value(&kept[kept.len() - 2]));
        if prompt {
            kept.pop();
        }
        let resume = match self {
            Self::Claude => "--resume",
            Self::Codex => "resume",
        };
        kept.extend([OsString::from(resume), OsString::from(session)]);
        kept
    }
}

/// CPU the agent may use across a whole window and still count as waiting at its prompt.
struct Quiet {
    window: Duration,
    budget: Duration,
    samples: VecDeque<(Instant, Duration)>,
}

impl Quiet {
    fn new(window: Duration) -> Self {
        Self { window, budget: window / 20, samples: VecDeque::new() }
    }

    fn observe(&mut self, now: Instant, cpu: Duration) -> bool {
        self.samples.push_back((now, cpu));
        while self.samples.get(1).is_some_and(|(at, _)| now.duration_since(*at) >= self.window) {
            self.samples.pop_front();
        }
        let (Some((first, before)), Some((_, after))) = (self.samples.front(), self.samples.back()) else {
            return false;
        };
        now.duration_since(*first) >= self.window && after.saturating_sub(*before) <= self.budget
    }
}

pub(super) struct Request {
    folder: PathBuf,
    quiet: Option<Quiet>,
    window: Duration,
}

impl Request {
    pub(super) fn new(folder: &Path, window: Duration) -> Self {
        Self { folder: folder.to_path_buf(), quiet: None, window }
    }

    /// The pending request's reason, once the agent has stayed quiet for a whole window after it arrived.
    pub(super) fn due(&mut self, agent: &Agent) -> Option<String> {
        let path = self.folder.join(REQUEST);
        if !path.exists() {
            self.quiet = None;
            return None;
        }
        let cpu = agent.cpu_time().ok()?;
        let quiet = self.quiet.get_or_insert_with(|| Quiet::new(self.window));
        if !quiet.observe(Instant::now(), cpu) {
            return None;
        }
        self.quiet = None;
        let reason = std::fs::read_to_string(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        Some(reason)
    }

    pub(super) fn restarted(&self, reason: &str) {
        let _ = std::fs::write(self.folder.join(RESTARTED), reason);
    }
}

const REQUEST: &str = "restart";
const RESTARTED: &str = "restarted";

#[derive(clap::Args)]
pub struct Args {
    /// What the restart is for, such as the plugin build to load. A reason this tab already restarted for is refused.
    #[arg(long, default_value = "")]
    reason: String,
}

pub fn run(args: Args) -> Result<ExitCode> {
    let Some(launch_id) = std::env::var("TJ_RUN_LAUNCH_ID").ok().filter(|id| !id.is_empty()) else {
        bail!("tj restart only works inside a tj run session (TJ_RUN_LAUNCH_ID is not set)");
    };
    let folder = super::marks(&launch_id)?;
    if std::fs::read_to_string(folder.join(RESTARTED)).is_ok_and(|done| done == args.reason) {
        bail!("this tab already restarted for {:?}; restarting again would not change it", args.reason);
    }
    std::fs::create_dir_all(&folder).context("could not create the restart request's folder")?;
    std::fs::write(folder.join(REQUEST), &args.reason).context("could not record the restart request")?;
    println!("Restart requested: tj run resumes this conversation in a fresh process once the agent is idle.");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::{Duration, Instant};

    use super::{Host, Quiet};

    fn resume(host: Host, agent: &[&str]) -> Vec<String> {
        let agent: Vec<OsString> = agent.iter().map(OsString::from).collect();
        host.resume(&agent, "id").iter().map(|argument| argument.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_launched_claude_keeps_its_flags_and_drops_its_prompt() {
        let agent =
            ["claude", "--dangerously-skip-permissions", "--name", "T", "--model", "m", "--effort", "x", "Do it"];
        assert_eq!(
            resume(Host::Claude, &agent),
            [
                "claude",
                "--dangerously-skip-permissions",
                "--name",
                "T",
                "--model",
                "m",
                "--effort",
                "x",
                "--resume",
                "id"
            ]
        );
    }

    #[test]
    fn a_trailing_option_value_is_not_taken_for_a_prompt() {
        assert_eq!(resume(Host::Claude, &["claude", "--model", "m"]), ["claude", "--model", "m", "--resume", "id"]);
        assert_eq!(
            resume(Host::Codex, &["codex", "--cd", "d", "--model", "m"]),
            ["codex", "--cd", "d", "--model", "m", "resume", "id"]
        );
    }

    #[test]
    fn an_earlier_session_choice_is_replaced() {
        assert_eq!(resume(Host::Claude, &["claude", "-c"]), ["claude", "--resume", "id"]);
        assert_eq!(
            resume(Host::Claude, &["claude", "--resume", "old", "--fork-session"]),
            ["claude", "--resume", "id"]
        );
        assert_eq!(resume(Host::Claude, &["claude", "--resume=old"]), ["claude", "--resume", "id"]);
        assert_eq!(resume(Host::Codex, &["codex", "--yolo", "resume", "old"]), ["codex", "--yolo", "resume", "id"]);
    }

    #[test]
    fn a_launched_codex_drops_its_prompt() {
        assert_eq!(
            resume(Host::Codex, &["codex", "--cd", "d", "--model", "m", "Do it"]),
            ["codex", "--cd", "d", "--model", "m", "resume", "id"]
        );
        assert_eq!(resume(Host::Codex, &["codex"]), ["codex", "resume", "id"]);
    }

    #[test]
    fn quiet_needs_a_whole_window_under_budget() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let mut quiet = Quiet::new(Duration::from_secs(20));
        assert!(!quiet.observe(start, Duration::ZERO));
        assert!(!quiet.observe(start + 10 * second, Duration::from_millis(500)));
        assert!(quiet.observe(start + 20 * second, Duration::from_millis(900)));
        assert!(!quiet.observe(start + 21 * second, Duration::from_millis(2000)));
        assert!(!quiet.observe(start + 40 * second, Duration::from_millis(2500)));
        assert!(quiet.observe(start + 41 * second, Duration::from_millis(2500)));
    }
}
