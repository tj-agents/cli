mod bridge;
mod cr;
mod external;
mod f;
mod fzf;
mod host;
mod init;
mod message;
mod paths;
mod run;
mod subscribe;

use std::ffi::OsString;
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
    /// Run an agent as this tab's root process and close its work item out when it is finished
    Run(run::Args),
    /// Mark this tab's work done; `tj run` closes it out after an idle grace
    Done(run::DoneArgs),
    /// Ask `tj run` to restart this tab's agent once it is idle, resuming the same conversation
    Restart(run::restart::Args),
    /// Mark this tab's agent idle between turns (run by its Stop hook); a no-op outside `tj run`
    Idle(run::restart::IdleArgs),
    /// Close out a work item's sessions, or sweep launches whose tab is gone
    Closeout(run::closeout::Args),
    /// Send a message to another Claude or Codex session, addressed by its tab title
    Message(message::Args),
    /// Get one notice when a Codex child session next goes idle or exits
    Subscribe(subscribe::Args),
    /// Relays `tj message` into this Claude session until it exits (run by the async SessionStart hook)
    #[command(hide = true)]
    Bridge,
    /// Draws cr's preview pane (run by fzf, not by hand)
    #[command(hide = true)]
    CrPreview(cr::PreviewArgs),
    /// Runs `tj-<name>` from PATH, so `tj item` runs `tj-item`
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Cr(args) => cr::run(args),
        Command::F(args) => f::run(args),
        Command::Init(args) => init::run(args),
        Command::Run(args) => run::run(args),
        Command::Done(args) => run::done(args),
        Command::Restart(args) => run::restart::run(args),
        Command::Idle(args) => run::restart::idle(args),
        Command::Closeout(args) => run::closeout::run(args),
        Command::Message(args) => message::run(args),
        Command::Subscribe(args) => subscribe::run(args),
        Command::Bridge => bridge::run(),
        Command::CrPreview(args) => cr::preview(args),
        Command::External(arguments) => external::run(arguments),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("\x1b[31mtj: {e:#}\x1b[0m");
            ExitCode::from(2)
        }
    }
}
