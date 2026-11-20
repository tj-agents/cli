use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::app_server::{Server, native_command};
use crate::paths;
use crate::run::update::observer::SocketGuard;

pub(super) struct Daemon {
    executable: PathBuf,
    pub(super) socket: PathBuf,
    pub(super) guard: SocketGuard,
    home: PathBuf,
    version: String,
}

impl Daemon {
    pub(super) fn ensure(executable: &Path, arguments: &[OsString], cwd: &Path) -> Result<Self> {
        ensure!(executable.is_absolute(), "managed Codex requires its selected absolute native executable");
        require_attachment(arguments)?;
        let home = paths::codex_home()?;
        ensure!(home.is_absolute(), "managed Codex requires an absolute CODEX_HOME");
        let socket = home.join("app-server-control").join("app-server-control.sock");
        let version = match lifecycle(executable, cwd, "version") {
            Ok(reply) => reply,
            Err(probe) => {
                lifecycle(executable, cwd, "start")
                    .with_context(|| format!("initial daemon probe failed: {probe:#}"))?;
                lifecycle(executable, cwd, "version").context("could not verify the started Codex daemon")?
            }
        };
        let reported = version["socketPath"].as_str().context("Codex omitted its daemon socket path")?;
        let selected =
            version["cliVersion"].as_str().filter(|v| !v.is_empty()).context("Codex omitted its CLI version")?;
        ensure!(
            version["status"] == "running" && version["backend"] == "pid",
            "Codex did not confirm a running native-managed daemon at {reported}"
        );
        // Windows AF_UNIX sockets are reparse points that cannot be opened by canonicalize.
        let reported = Path::new(reported);
        ensure!(
            reported.is_absolute()
                && reported.file_name() == socket.file_name()
                && same_path(
                    socket.parent().context("missing control directory")?,
                    reported.parent().context("missing reported control directory")?
                )?,
            "Codex reported another daemon endpoint: {}",
            reported.display()
        );
        ensure!(
            version["appServerVersion"] == selected,
            "selected Codex {selected} and running daemon {} differ at {}",
            version["appServerVersion"],
            reported.display()
        );
        let guard = SocketGuard::open(&socket)?;
        let daemon = Self { executable: executable.into(), socket, guard, home, version: selected.to_owned() };
        daemon.connect()?;
        Ok(daemon)
    }

    pub(super) fn connect(&self) -> Result<Server> {
        let server = Server::connect_socket(&self.executable, &self.socket)
            .with_context(|| format!("could not connect {} to {}", self.executable.display(), self.socket.display()))?;
        let identity = server.identity();
        let home = identity["codexHome"].as_str().context("Codex proxy omitted codexHome")?;
        let version = identity["userAgent"]
            .as_str()
            .and_then(|ua| ua.split_once('/'))
            .and_then(|(_, rest)| rest.split_whitespace().next());
        ensure!(
            version == Some(&self.version) && same_path(&self.home, Path::new(home))?,
            "Codex proxy reached a different version or CODEX_HOME"
        );
        ensure!(
            self.guard.identity()? == super::observer::socket_identity(&self.socket)?,
            "Codex daemon endpoint was replaced"
        );
        Ok(server)
    }
}

fn same_path(expected: &Path, reported: &Path) -> Result<bool> {
    Ok(reported.is_absolute() && std::fs::canonicalize(expected)? == std::fs::canonicalize(reported)?)
}

fn require_attachment(arguments: &[OsString]) -> Result<()> {
    let mut options = arguments.iter().take_while(|value| *value != "--");
    let mut attached = false;
    while let Some(option) = options.next() {
        let option = option.to_string_lossy();
        let target = if option == "--remote" {
            Some(options.next().and_then(|value| value.to_str()).unwrap_or(""))
        } else {
            option.strip_prefix("--remote=")
        };
        if let Some(target) = target {
            ensure!(!attached && target == "unix://", "managed Codex requires one --remote unix:// attachment");
            attached = true;
        } else if matches!(option.as_ref(), "--no-daemon" | "--dangerously-bypass-hook-trust") {
            bail!("{option} is incompatible with managed Codex");
        }
    }
    ensure!(attached, "managed Codex requires explicit --remote unix:// attachment");
    Ok(())
}

fn lifecycle(executable: &Path, cwd: &Path, operation: &str) -> Result<Value> {
    let mut command = native_command(executable);
    command.args(["app-server", "daemon", operation]).current_dir(cwd).stdin(Stdio::null());
    // The service can outlive many launchers. Never seed its hooks with a launch's ownership.
    for name in [
        "TJ_RUN_PID",
        "TJ_RUN_STARTED_AT",
        "TJ_RUN_LAUNCH_ID",
        "AGENT_CLI_TAB_TITLE",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "WT_SESSION",
        "TMUX_PANE",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        command.env_remove(name);
    }
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async { tokio::time::timeout(Duration::from_secs(120), command.output()).await })
        .with_context(|| format!("{} daemon {operation} timed out", executable.display()))??;
    ensure!(
        output.status.success(),
        "{} app-server daemon {operation} exited {}: {}",
        executable.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout).with_context(|| format!("invalid Codex daemon {operation} response"))
}
