mod cr;
mod f;
mod fzf;
mod init;
mod paths;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tj", version, about = "Shell tools for agentic programming", propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search Claude Code / Codex sessions by anything said in them, and resume one
    Cr(cr::Args),
    /// Fuzzy-find a project folder and print its path (the `f` shell function cds into it)
    F(f::Args),
    /// Print the shell functions to eval from your shell's startup file
    Init(init::Args),
    /// Draws cr's preview pane (run by fzf, not by hand)
    #[command(hide = true)]
    CrPreview(cr::PreviewArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Cr(args) => cr::run(args),
        Command::F(args) => f::run(args),
        Command::Init(args) => init::run(args),
        Command::CrPreview(args) => cr::preview(args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("\x1b[31mtj: {e:#}\x1b[0m");
            ExitCode::from(2)
        }
    }
}
