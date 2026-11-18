mod app_server;
mod completion;
mod cr;
mod domain;
mod external;
mod f;
mod feed;
mod fzf;
mod host;
mod init;
mod item_cli;
mod message;
mod paths;
mod run;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tj", version, about = "Managed Claude Code and Codex sessions", propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a managed Claude Code session with native arguments and permissions
    #[command(disable_help_flag = true, disable_version_flag = true)]
    Claude(external::HostArgs),
    /// Open a managed Codex session with native arguments and permissions
    #[command(disable_help_flag = true, disable_version_flag = true)]
    Codex(external::HostArgs),
    /// Search Claude Code / Codex sessions by anything said in them, and resume one
    Cr(cr::Args),
    /// Fuzzy-find a project folder and print its path (the `f` shell function cds into it)
    F(f::Args),
    /// Print the shell functions to eval from your shell's startup file
    Init(init::Args),
    /// Internal root process shared by all managed launch paths
    #[command(name = "_session", hide = true)]
    Session(Box<run::Args>),
    /// Request completion after this native turn delivers its final response
    Finish,
    /// Record a genuine pending answer, child, CI, or approval
    Wait(run::lifecycle::WaitArgs),
    /// Keep this managed session open across turns, or release that hold
    Hold(run::lifecycle::HoldArgs),
    /// Inspect a durable completion record and its result receipts
    Receipt(run::lifecycle::ReceiptArgs),
    #[command(name = "_lifecycle", hide = true)]
    Lifecycle,
    /// Announce installed plugin updates; managed tabs apply them once idle (run by Agent)
    #[command(hide = true)]
    Update(run::update::Args),
    /// Observe plugin updates for an existing Codex daemon (run by Agent)
    #[command(hide = true)]
    ObserveCodexUpdates(run::update::observer::Args),
    /// Prints the managed launch this process belongs to, if any (run by the agent's SessionStart hook)
    #[command(hide = true)]
    Launch,
    /// Close out a work item's sessions, or sweep launches whose tab is gone
    Closeout(run::closeout::Args),
    /// Message another session through its supported native or feed route
    Message(message::Args),
    #[command(name = "_message-host", hide = true)]
    MessageHost(message::bridge::HostArgs),
    #[command(name = "_message-receipt", hide = true)]
    MessageReceipt(message::bridge::ReceiptArgs),
    /// Read this session's unread feed messages, optionally waiting for the next one
    Feed(feed::Args),
    /// Draws cr's preview pane (run by fzf, not by hand)
    #[command(hide = true)]
    CrPreview(cr::PreviewArgs),
    /// Fast-forwards a clean, behind default branch and reports anything else stale (run before a plain host)
    #[command(name = "_checkout-freshness", hide = true)]
    CheckoutFreshness(run::freshness::Args),
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
        Command::Claude(args) => external::host(host::Host::Claude, args),
        Command::Codex(args) => external::host(host::Host::Codex, args),
        Command::Session(args) => run::run(*args),
        Command::Finish => run::lifecycle::finish(),
        Command::Wait(args) => run::lifecycle::wait(args),
        Command::Hold(args) => run::lifecycle::hold(args),
        Command::Receipt(args) => run::lifecycle::receipt(args),
        Command::Lifecycle => run::lifecycle::native_event(),
        Command::Update(args) => run::update::publish(args),
        Command::ObserveCodexUpdates(args) => run::update::observer::run(args),
        Command::Launch => Ok(run::owner::print()),
        Command::Closeout(args) => run::closeout::run(args),
        Command::Message(args) => message::run(&args),
        Command::MessageHost(args) => message::bridge::host(args),
        Command::MessageReceipt(args) => message::bridge::receipt(args),
        Command::Feed(args) => feed::run(&args),
        Command::CrPreview(args) => cr::preview(args),
        Command::CheckoutFreshness(args) => run::freshness::run(args),
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::Parser;

    use super::{Cli, Command};

    #[test]
    fn provider_flags_subcommands_and_prompts_are_not_consumed_by_tj() {
        for host in ["claude", "codex"] {
            for arguments in [
                vec![],
                vec!["--help"],
                vec!["--version"],
                vec!["login"],
                vec!["resume", "thread-id", "a prompt with spaces"],
                vec!["--model", "native-model", "--", "--literal prompt"],
            ] {
                let input = [vec!["tj", host], arguments.clone()].concat();
                let parsed = Cli::try_parse_from(input).expect("provider arguments must pass through");
                let (Command::Claude(parsed) | Command::Codex(parsed)) = parsed.command else {
                    panic!("wrong provider command");
                };
                assert_eq!(parsed.arguments, arguments.into_iter().map(OsString::from).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn the_low_level_wrapper_is_not_a_public_run_command() {
        assert!(matches!(
            Cli::try_parse_from(["tj", "_session", "--host", "codex", "--", "codex"]).unwrap().command,
            Command::Session(_)
        ));
        assert!(matches!(Cli::try_parse_from(["tj", "run"]).unwrap().command, Command::External(_)));
    }
}
