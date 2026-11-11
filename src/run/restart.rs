use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

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

pub(super) const IDLE: &str = "idle";
const REQUEST: &str = "restart";
const RESTARTED: &str = "restarted";

pub(super) struct Request {
    folder: PathBuf,
    grace: Duration,
}

impl Request {
    pub(super) fn new(folder: &Path, grace: Duration) -> Self {
        Self { folder: folder.to_path_buf(), grace }
    }

    /// The pending request's reason, once the agent has sat idle with no background work for the grace.
    pub(super) fn due(&self) -> Option<String> {
        let reason = std::fs::read_to_string(self.folder.join(REQUEST)).ok()?;
        let idle = self.folder.join(IDLE);
        let background = std::fs::read_to_string(&idle).ok()?;
        let since = idle.metadata().and_then(|meta| meta.modified()).ok()?;
        let waited = SystemTime::now().duration_since(since).ok()?;
        if background.trim().parse::<u32>() != Ok(0) || waited < self.grace {
            return None;
        }
        let _ = std::fs::remove_file(self.folder.join(REQUEST));
        let _ = std::fs::remove_file(&idle);
        Some(reason)
    }

    pub(super) fn restarted(&self, reason: &str) {
        let _ = std::fs::write(self.folder.join(RESTARTED), reason);
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// What the restart is for, such as the plugin build to load. A reason this tab already restarted for is refused.
    #[arg(long, default_value = "")]
    reason: String,
}

pub fn run(args: Args) -> Result<ExitCode> {
    let Some(launch_id) = super::launch_id() else {
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

fn background(text: &str) -> Result<String, String> {
    if text == "unknown" || text.parse::<u32>().is_ok() {
        return Ok(text.to_owned());
    }
    Err(format!("{text:?} is neither a count nor `unknown`"))
}

#[derive(clap::Args)]
pub struct IdleArgs {
    /// Background work the host still has in flight (shells, subagents, watches, timers), or `unknown`.
    /// Only 0 lets a requested restart happen.
    #[arg(long, value_parser = background)]
    background: String,
}

pub fn idle(args: IdleArgs) -> Result<ExitCode> {
    let Some(launch_id) = super::launch_id() else { return Ok(ExitCode::SUCCESS) };
    let folder = super::marks(&launch_id)?;
    std::fs::create_dir_all(&folder).context("could not create the idle mark's folder")?;
    std::fs::write(folder.join(IDLE), args.background).context("could not record the idle mark")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::Host;

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
        assert_eq!(
            resume(Host::Codex, &["codex", "--cd", "d", "resume", "old", "--", "--model=other"]),
            ["codex", "--cd", "d", "resume", "id"]
        );
        assert_eq!(resume(Host::Codex, &["codex"]), ["codex", "resume", "id"]);
    }
}
