use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

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
    pub(crate) fn resume(self, agent: &[OsString], session: &str) -> Vec<OsString> {
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

/// Runs `arguments` with the replaced executable in this tab. Linux replaces this process, keeping its pid; Windows
/// Terminal closes the tab when its root process exits, so the root waits for its successor instead.
pub(crate) fn hand_over(exe: &Path, arguments: &[OsString]) -> Result<ExitCode> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(exe).args(arguments).exec();
        Err(error).with_context(|| format!("could not run {}", exe.display()))
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new(exe)
            .args(arguments)
            .status()
            .with_context(|| format!("could not run {}", exe.display()))?;
        Ok(ExitCode::from(status.code().map_or(1, |code| u8::try_from(code).unwrap_or(1))))
    }
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
        assert_eq!(resume(Host::Codex, &["codex"]), ["codex", "resume", "id"]);
    }
}
