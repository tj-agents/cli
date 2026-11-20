#![cfg_attr(windows, windows_subsystem = "windows")]

use std::env;
use std::ffi::OsStr;
use std::io::{self, Write};

#[cfg(windows)]
const BATCH_DISPATCH: &str = "--tj-hook-batch-dispatch";

fn main() {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|argument| argument == "--bounded") {
        let result = bounded(&arguments[1..]);
        match result {
            Ok(code) => std::process::exit(code as i32),
            Err(error) => {
                let _ = writeln!(io::stderr(), "tj_hook: bounded command has no cleanup receipt: {error}");
                std::process::exit(127);
            }
        }
    }
    #[cfg(windows)]
    if arguments.first().is_some_and(|argument| argument == OsStr::new(BATCH_DISPATCH)) {
        let result = if arguments.get(1).is_some_and(|argument| argument == OsStr::new("--")) {
            tj_cli::process::batch_dispatch(&arguments[2..])
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid internal batch dispatch"))
        };
        match result {
            Ok(code) => std::process::exit(code as i32),
            Err(error) => {
                let _ = writeln!(io::stderr(), "tj_hook: {error}");
                std::process::exit(127);
            }
        }
    }
    let mut preserve_descendants = false;
    let mut forwarded = Vec::new();
    let mut options = true;
    let mut separator = false;
    for argument in arguments {
        if options && argument == OsStr::new("--preserve-descendants") {
            preserve_descendants = true;
        } else if options && argument == OsStr::new("--") {
            options = false;
            separator = true;
        } else {
            forwarded.push(argument);
            options = false;
        }
    }
    if !separator && forwarded.len() == 1 && forwarded[0] == "--version" {
        let _ = writeln!(io::stdout(), "tj_hook {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match tj_cli::process::run(&forwarded, preserve_descendants) {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            let _ = writeln!(io::stderr(), "tj_hook: {error}");
            std::process::exit(127);
        }
    }
}

fn bounded(arguments: &[std::ffi::OsString]) -> io::Result<u32> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected --bounded <milliseconds> <receipt> <attempt> -- <command>",
        )
    };
    let [milliseconds, receipt, attempt, separator, command @ ..] = arguments else { return Err(invalid()) };
    let milliseconds: u64 = milliseconds.to_str().ok_or_else(invalid)?.parse().map_err(|_| invalid())?;
    let attempt = attempt
        .to_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .ok_or_else(invalid)?;
    if milliseconds == 0 || separator != "--" || command.is_empty() {
        return Err(invalid());
    }
    let receipt = std::path::Path::new(receipt);
    let outcome =
        tj_cli::process::bounded::run(command, std::time::Duration::from_millis(milliseconds), receipt, attempt);
    outcome.record(receipt, attempt)?;
    Ok(outcome.code())
}
