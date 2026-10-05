use std::process::ExitCode;

use anyhow::Result;
use clap::ValueEnum;

#[derive(Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Pwsh,
}

#[derive(clap::Args)]
pub struct Args {
    shell: Shell,
}

// Each platform's shell functions live beside its installer and are compiled in.
const POSIX: &str = include_str!("../linux/tj.sh");
const FISH: &str = include_str!("../linux/tj.fish");
const PWSH: &str = include_str!("../windows/tj.ps1");

pub fn script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => POSIX,
        Shell::Fish => FISH,
        Shell::Pwsh => PWSH,
    }
}

pub fn run(args: Args) -> Result<ExitCode> {
    print!("{}", script(args.shell));
    Ok(ExitCode::SUCCESS)
}
