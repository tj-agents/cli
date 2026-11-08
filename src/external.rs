use std::ffi::OsString;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

pub fn run(arguments: Vec<OsString>) -> Result<ExitCode> {
    let Some((name, rest)) = arguments.split_first() else {
        bail!("missing subcommand");
    };
    let program = format!("tj-{}", name.to_string_lossy());
    let Ok(executable) = which::which(&program) else {
        bail!("`tj {}` is not a tj command and no `{program}` is on PATH", name.to_string_lossy());
    };
    let status = Command::new(&executable)
        .args(rest)
        .status()
        .with_context(|| format!("could not run {}", executable.display()))?;
    Ok(ExitCode::from(status.code().map_or(1, |code| u8::try_from(code).unwrap_or(1))))
}
