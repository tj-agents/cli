#![cfg_attr(windows, windows_subsystem = "windows")]

use std::env;
use std::ffi::OsStr;
use std::io::{self, Write};

fn main() {
    let mut arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == OsStr::new("--")) {
        arguments.remove(0);
    } else if arguments.len() == 1 && arguments[0] == "--version" {
        let _ = writeln!(io::stdout(), "tj_hook {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match tj_cli::process::run(&arguments) {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            let _ = writeln!(io::stderr(), "tj_hook: {error}");
            std::process::exit(127);
        }
    }
}
