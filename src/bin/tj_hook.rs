#![cfg_attr(windows, windows_subsystem = "windows")]

use std::env;
use std::ffi::OsStr;
use std::io::{self, Write};

fn main() {
    let mut preserve_descendants = false;
    let mut arguments = Vec::new();
    let mut options = true;
    let mut separator = false;
    for argument in env::args_os().skip(1) {
        if options && argument == OsStr::new("--preserve-descendants") {
            preserve_descendants = true;
        } else if options && argument == OsStr::new("--") {
            options = false;
            separator = true;
        } else {
            arguments.push(argument);
            options = false;
        }
    }
    if !separator && arguments.len() == 1 && arguments[0] == "--version" {
        let _ = writeln!(io::stdout(), "tj_hook {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match tj_cli::process::run(&arguments, preserve_descendants) {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            let _ = writeln!(io::stderr(), "tj_hook: {error}");
            std::process::exit(127);
        }
    }
}
