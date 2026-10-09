use std::{
    io,
    process::{Command, ExitCode},
};

use clap::Args as ClapArgs;
use thiserror::Error;

use crate::{live, watch};

#[derive(Debug, Error)]
pub enum RecoverError {
    #[error(transparent)]
    Watch(#[from] watch::WatchError),
    #[error(transparent)]
    Live(#[from] live::LiveError),
    #[error("cannot start {program}: {source}")]
    Launch { program: String, source: io::Error },
    #[cfg(not(windows))]
    #[error("cannot determine the tj state directory: {source}")]
    StateDirectory { source: anyhow::Error },
    #[cfg(not(windows))]
    #[error("cannot write kitty session file at {path}: {source}")]
    WriteKittySession { path: std::path::PathBuf, source: io::Error },
}

#[derive(ClapArgs)]
pub struct RecoverArgs {
    /// Print the selected sessions and commands without opening a terminal.
    #[arg(long)]
    list: bool,
}

#[derive(Debug)]
pub struct Recovery {
    pub sessions: Vec<live::Session>,
    pub missing_directories: Vec<live::Session>,
}

pub fn run(args: RecoverArgs) -> Result<ExitCode, RecoverError> {
    let running = live::sessions()?;
    let recovery = match watch::load_current()? {
        Some(state) => select(&state, &running),
        None => {
            eprintln!("tj: no watcher state; Codex sessions cannot be determined.");
            let sessions = live::dead_claude_sessions()?
                .into_iter()
                .filter(|session| !running.iter().any(|running_session| same_session(running_session, session)))
                .collect();
            split_missing_directories(sessions)
        }
    };

    for session in &recovery.missing_directories {
        eprintln!(
            "tj: not restoring {} {}: working directory {} no longer exists",
            session.agent.command(),
            session.id,
            session.cwd.display()
        );
    }

    if recovery.sessions.is_empty() {
        println!("No stopped sessions are available to restore.");
        return Ok(ExitCode::SUCCESS);
    }

    for session in &recovery.sessions {
        println!("{}", format_command(session));
    }

    if !args.list {
        launch(&recovery.sessions)?;
    }

    Ok(ExitCode::SUCCESS)
}

pub fn select(state: &watch::State, running: &[live::Session]) -> Recovery {
    let candidates = if let Some(abrupt_stop) = &state.abrupt_stop {
        state
            .records
            .iter()
            .filter(|record| {
                record.last_seen_boot == abrupt_stop.boot && record.last_seen_tick == abrupt_stop.final_tick
            })
            .collect::<Vec<_>>()
    } else {
        let latest = state
            .records
            .iter()
            .filter_map(|record| record.ended_tick.zip(record.ended_boot))
            .max_by_key(|(tick, boot)| (*boot, *tick));

        latest.map_or_else(Vec::new, |(tick, boot)| {
            let burst = state
                .records
                .iter()
                .filter(|record| record.ended_boot == Some(boot) && record.ended_tick == Some(tick))
                .collect::<Vec<_>>();
            if burst.len() >= 2 { burst } else { Vec::new() }
        })
    };

    let mut sessions = Vec::new();
    for record in candidates {
        let session = record.session.clone();
        if running.iter().any(|running_session| same_session(running_session, &session)) {
            continue;
        }
        sessions.push(session);
    }

    split_missing_directories(sessions)
}

fn split_missing_directories(candidates: Vec<live::Session>) -> Recovery {
    let mut sessions = Vec::new();
    let mut missing_directories = Vec::new();
    for session in candidates {
        if session.cwd.is_dir() {
            sessions.push(session);
        } else {
            missing_directories.push(session);
        }
    }
    Recovery { sessions, missing_directories }
}

pub fn resume_command(session: &live::Session) -> (String, Vec<String>) {
    match session.agent {
        live::Agent::Claude => {
            let mut arguments = vec!["--resume".to_owned(), session.id.clone()];
            if !session.name.is_empty() {
                arguments.push("--name".to_owned());
                arguments.push(session.name.clone());
            }
            ("claude".to_owned(), arguments)
        }
        live::Agent::Codex => {
            let mut arguments = vec!["resume".to_owned()];
            if !session.model.is_empty() {
                arguments.push("--model".to_owned());
                arguments.push(session.model.clone());
            }
            if !session.effort.is_empty() {
                arguments.push("-c".to_owned());
                arguments.push(format!("model_reasoning_effort={}", session.effort));
            }
            arguments.push(session.id.clone());
            ("codex".to_owned(), arguments)
        }
    }
}

pub fn format_command(session: &live::Session) -> String {
    let (program, arguments) = resume_command(session);
    std::iter::once(program).chain(arguments).map(|argument| shell_quote(&argument)).collect::<Vec<_>>().join(" ")
}

fn same_session(left: &live::Session, right: &live::Session) -> bool {
    left.agent == right.agent && left.id == right.id
}

fn shell_quote(value: &str) -> String {
    if value.chars().all(|character| character.is_ascii_alphanumeric() || "-_=.:/".contains(character)) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn launch(sessions: &[live::Session]) -> Result<(), RecoverError> {
    #[cfg(windows)]
    return launch_windows_terminal(sessions);

    #[cfg(not(windows))]
    launch_linux_terminal(sessions)
}

#[cfg(windows)]
fn launch_windows_terminal(sessions: &[live::Session]) -> Result<(), RecoverError> {
    let mut command = Command::new("wt.exe");
    command.args(windows_terminal_arguments(sessions));
    command.spawn().map(|_| ()).map_err(|source| RecoverError::Launch { program: "wt.exe".to_owned(), source })
}

#[cfg(windows)]
fn windows_terminal_arguments(sessions: &[live::Session]) -> Vec<String> {
    let mut arguments = vec!["-w".to_owned(), "new".to_owned()];
    for (index, session) in sessions.iter().enumerate() {
        if index != 0 {
            arguments.push(";".to_owned());
        }
        let (program, resume_arguments) = resume_command(session);
        arguments.extend([
            "new-tab".to_owned(),
            "--title".to_owned(),
            wt_escape(session_title(session)),
            "-d".to_owned(),
        ]);
        arguments.push(wt_escape(&session.cwd.to_string_lossy()));
        arguments.push(wt_escape(&program));
        arguments.extend(resume_arguments.iter().map(|argument| wt_escape(argument)));
    }
    arguments
}

#[cfg(windows)]
fn wt_escape(value: &str) -> String {
    value.replace(';', "\\;")
}

#[cfg(not(windows))]
fn launch_linux_terminal(sessions: &[live::Session]) -> Result<(), RecoverError> {
    let Some(terminal) = terminal_program() else { return Ok(()) };
    if matches!(terminal, LinuxTerminal::Kitty) {
        return launch_kitty_session(sessions);
    }
    for session in sessions {
        let (program, arguments) = resume_command(session);
        let mut command = Command::new(terminal.program());
        match &terminal {
            LinuxTerminal::Tmux => {
                command.args(["new-window", "-c"]);
                command.arg(&session.cwd);
                command.args(["-n", session_title(session), "--"]);
            }
            LinuxTerminal::KittyRemote => {
                command.args(["@", "launch", "--type=tab", "--cwd"]);
                command.arg(&session.cwd);
                command.args(["--tab-title", session_title(session), "--"]);
            }
            LinuxTerminal::WezTerm => {
                command.args(["cli", "spawn", "--cwd"]);
                command.arg(&session.cwd);
                command.arg("--");
            }
            LinuxTerminal::Program(name) if name == "gnome-terminal" => {
                command.arg(format!("--working-directory={}", session.cwd.display()));
                command.arg(format!("--title={}", session_title(session)));
                command.arg("--");
            }
            LinuxTerminal::Program(name) if name == "konsole" => {
                command.arg("--workdir").arg(&session.cwd).arg("-e");
            }
            LinuxTerminal::Program(name) if name == "alacritty" => {
                command.arg("--working-directory").arg(&session.cwd).arg("-e");
            }
            LinuxTerminal::Program(name) if name == "foot" => {
                command.arg("--working-directory").arg(&session.cwd);
            }
            LinuxTerminal::Program(name) if name == "ghostty" => {
                command.arg(format!("--working-directory={}", session.cwd.display()));
                command.arg("-e");
            }
            LinuxTerminal::Program(_) => {
                command.args(["-e", "sh", "-lc"]);
                command.arg(format!(
                    "cd {} && exec {}",
                    shell_quote(&session.cwd.to_string_lossy()),
                    std::iter::once(program.clone())
                        .chain(arguments.iter().cloned())
                        .map(|argument| shell_quote(&argument))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                command
                    .spawn()
                    .map(|_| ())
                    .map_err(|source| RecoverError::Launch { program: terminal.program().to_owned(), source })?;
                continue;
            }
        }
        command.arg(program);
        command.args(arguments);
        command.spawn().map_err(|source| RecoverError::Launch { program: terminal.program().to_owned(), source })?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_kitty_session(sessions: &[live::Session]) -> Result<(), RecoverError> {
    let directory = crate::paths::state_dir().map_err(|source| RecoverError::StateDirectory { source })?;
    std::fs::create_dir_all(&directory)
        .map_err(|source| RecoverError::WriteKittySession { path: directory.clone(), source })?;
    let path = directory.join(format!(
        "recover-{}.session",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
    ));
    let content = sessions
        .iter()
        .map(|session| {
            let (program, arguments) = resume_command(session);
            format!(
                "new_tab {}\ncd {}\nlaunch {}",
                shell_quote(session_title(session)),
                shell_quote(&session.cwd.to_string_lossy()),
                std::iter::once(program)
                    .chain(arguments)
                    .map(|argument| shell_quote(&argument))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, content).map_err(|source| RecoverError::WriteKittySession { path: path.clone(), source })?;
    Command::new("kitty")
        .args(["--detach", "--session"])
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|source| RecoverError::Launch { program: "kitty".to_owned(), source })
}

#[cfg(not(windows))]
enum LinuxTerminal {
    Tmux,
    KittyRemote,
    Kitty,
    WezTerm,
    Program(String),
}

#[cfg(not(windows))]
impl LinuxTerminal {
    fn program(&self) -> &str {
        match self {
            Self::Tmux => "tmux",
            Self::KittyRemote | Self::Kitty => "kitty",
            Self::WezTerm => "wezterm",
            Self::Program(program) => program,
        }
    }
}

#[cfg(not(windows))]
fn terminal_program() -> Option<LinuxTerminal> {
    if std::env::var_os("TMUX").is_some() {
        return Some(LinuxTerminal::Tmux);
    }
    if std::env::var_os("KITTY_LISTEN_ON").is_some() {
        return Some(LinuxTerminal::KittyRemote);
    }
    if which::which("kitty").is_ok() {
        return Some(LinuxTerminal::Kitty);
    }
    if which::which("wezterm").is_ok() {
        return Some(LinuxTerminal::WezTerm);
    }
    if let Ok(terminal) = std::env::var("TJ_TERMINAL") {
        return which::which(&terminal).is_ok().then_some(LinuxTerminal::Program(terminal));
    }
    ["alacritty", "foot", "ghostty", "konsole", "gnome-terminal", "xterm"]
        .iter()
        .find(|program| which::which(program).is_ok())
        .map(|program| LinuxTerminal::Program((*program).to_owned()))
}

fn session_title(session: &live::Session) -> &str {
    if session.name.is_empty() { &session.id } else { &session.name }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, last_seen_tick: u64) -> watch::Record {
        watch::Record {
            session: live::Session {
                agent: live::Agent::Claude,
                id: id.to_owned(),
                cwd: std::env::temp_dir(),
                name: format!("{id} name"),
                model: String::new(),
                effort: String::new(),
            },
            first_seen_boot: 1,
            first_seen_tick: 1,
            last_seen_boot: 1,
            last_seen_tick,
            ended_boot: Some(1),
            ended_tick: Some(last_seen_tick + 1),
            missing_polls: 2,
        }
    }

    #[test]
    fn chooses_all_sessions_seen_at_an_abrupt_stop() {
        let mut state = watch::State {
            boot: 2,
            tick: 1,
            abrupt_stop: Some(watch::AbruptStop { boot: 1, final_tick: 4 }),
            records: vec![record("first", 4), record("second", 4), record("older", 3)],
        };
        state.records[1].session.agent = live::Agent::Codex;
        let recovery = select(&state, &[]);
        assert_eq!(recovery.sessions.len(), 2);
    }

    #[test]
    fn does_not_restore_a_single_clean_closure() {
        let state = watch::State { boot: 1, tick: 4, abrupt_stop: None, records: vec![record("one", 2)] };
        assert!(select(&state, &[]).sessions.is_empty());
    }

    #[test]
    fn reports_a_missing_working_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().to_path_buf();
        drop(directory);
        let mut record = record("one", 4);
        record.session.cwd = missing;
        let state = watch::State {
            boot: 2,
            tick: 1,
            abrupt_stop: Some(watch::AbruptStop { boot: 1, final_tick: 4 }),
            records: vec![record],
        };
        let recovery = select(&state, &[]);
        assert!(recovery.sessions.is_empty());
        assert_eq!(recovery.missing_directories.len(), 1);
    }

    #[test]
    fn preserves_claude_names_in_the_resume_command() {
        let session = record("first", 1).session;
        assert_eq!(format_command(&session), "claude --resume first --name 'first name'");
    }

    #[test]
    fn preserves_codex_model_and_effort_in_the_resume_command() {
        let mut session = record("thread", 1).session;
        session.agent = live::Agent::Codex;
        session.model = "gpt-5.3-codex".to_owned();
        session.effort = "xhigh".to_owned();
        assert_eq!(
            format_command(&session),
            "codex resume --model gpt-5.3-codex -c model_reasoning_effort=xhigh thread"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_terminal_keeps_semicolons_inside_arguments() {
        let mut first = record("first", 1).session;
        first.name = "first; name".to_owned();
        let arguments = windows_terminal_arguments(&[first, record("second", 1).session]);
        assert_eq!(arguments.iter().filter(|argument| argument.as_str() == ";").count(), 1);
        assert!(arguments.iter().any(|argument| argument == "first\\; name"));
    }
}
