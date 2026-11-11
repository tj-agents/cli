use std::process::ExitCode;

use tj_cli::process::agent;

/// The launch this process speaks for. A tab, shell or agent started from inside a session inherits `TJ_RUN_*`,
/// so the caller must also descend from the wrapper through its agent, with no second copy of that agent between.
pub(crate) fn launch() -> Option<String> {
    let launch = super::launch_id()?;
    let wrapper: u32 = std::env::var("TJ_RUN_PID").ok()?.parse().ok()?;
    let started = std::env::var("TJ_RUN_STARTED_AT").ok()?;
    if agent::started_at(wrapper).ok()?.map(super::rfc3339) != Some(started) {
        return None;
    }
    owns(&agent::ancestry().ok()?, wrapper).then_some(launch)
}

pub fn print() -> ExitCode {
    if let Some(launch) = launch() {
        println!("{launch}");
    }
    ExitCode::SUCCESS
}

fn owns(chain: &[(u32, String)], wrapper: u32) -> bool {
    let Some(below) = chain.iter().position(|(pid, _)| *pid == wrapper).and_then(|at| at.checked_sub(1)) else {
        return false;
    };
    let host = &chain[below].1;
    !chain[..below].iter().any(|(_, image)| image.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::owns;

    fn chain(processes: &[(u32, &str)]) -> Vec<(u32, String)> {
        processes.iter().map(|(pid, image)| (*pid, (*image).to_owned())).collect()
    }

    #[test]
    fn a_hook_of_the_wrappers_own_agent_owns_the_launch() {
        let hook = chain(&[(5, "tj.exe"), (4, "python.exe"), (3, "tj_hook.exe"), (2, "claude.exe"), (1, "tj.exe")]);
        assert!(owns(&hook, 1));
    }

    #[test]
    fn a_session_started_in_another_tab_does_not() {
        let tab = chain(&[(5, "tj.exe"), (4, "claude.exe"), (3, "pwsh.exe"), (9, "WindowsTerminal.exe")]);
        assert!(!owns(&tab, 1));
    }

    #[test]
    fn an_agent_nested_inside_the_agent_does_not() {
        let nested = chain(&[(6, "tj.exe"), (5, "claude.exe"), (4, "bash.exe"), (3, "Claude.EXE"), (1, "tj.exe")]);
        assert!(!owns(&nested, 1));
    }

    #[test]
    fn the_wrapper_itself_does_not() {
        assert!(!owns(&chain(&[(1, "tj.exe"), (0, "init")]), 1));
    }
}
