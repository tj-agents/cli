use std::process::ExitCode;

use serde_json::Value;
use tj_cli::process::agent;

/// Resolve ownership by verified ancestry, or by a remote Codex thread's live reciprocal launch binding.
pub(crate) fn launch() -> Option<String> {
    let chain = agent::ancestry().ok()?;
    if let (Some(launch), Ok(wrapper), Ok(started)) =
        (super::launch_id(), std::env::var("TJ_RUN_PID"), std::env::var("TJ_RUN_STARTED_AT"))
        && let Ok(wrapper) = wrapper.parse::<u32>()
        && agent::started_at(wrapper).ok().flatten().map(super::rfc3339) == Some(started)
        && owns(&chain, wrapper)
    {
        return Some(launch);
    }
    if crate::host::Host::from_ancestry(&chain) != Some(crate::host::Host::Codex) {
        return None;
    }
    let id = std::env::var("CODEX_THREAD_ID").ok()?;
    if !super::canonical_uuid(&id) {
        return None;
    }
    let handle = format!("codex:{id}");
    let session = super::store::call(&["show", &handle]).ok()?;
    let launch_id = session["session"]["launch_id"].as_str()?;
    let binding = super::store::call(&["show", "--launch-id", launch_id]).ok()?;
    remote_binding(&session, &binding, &handle, |pid| agent::started_at(pid).ok()?.map(super::rfc3339))
}

fn remote_binding(
    by_session: &Value,
    binding: &Value,
    handle: &str,
    started_at: impl FnOnce(u32) -> Option<String>,
) -> Option<String> {
    let session = &by_session["session"];
    let launch = &binding["launch"];
    let bound = &binding["session"];
    let launch_id = session["launch_id"].as_str()?.to_owned();
    let native_id = handle.strip_prefix("codex:")?;
    if session["session"].as_str()? != handle
        || session["native_id"].as_str()? != native_id
        || session["host"].as_str()? != "codex"
        || session["state"].as_str()? != "active"
        || launch["launch_id"].as_str()? != launch_id
        || launch["host"].as_str()? != "codex"
        || launch["state"].as_str()? != "active"
        || launch["session"].as_str()? != handle
        || bound["session"].as_str()? != handle
        || bound["native_id"].as_str()? != native_id
        || bound["host"].as_str()? != "codex"
        || bound["state"].as_str()? != "active"
        || bound["launch_id"].as_str()? != launch_id
    {
        return None;
    }
    let pid = u32::try_from(launch["wrapper"]["pid"].as_u64()?).ok()?;
    let recorded = chrono::DateTime::parse_from_rfc3339(launch["wrapper"]["started_at"].as_str()?).ok()?;
    let actual = chrono::DateTime::parse_from_rfc3339(&started_at(pid)?).ok()?;
    (recorded == actual).then_some(launch_id)
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
    if let Some(owner) = crate::host::Host::from_ancestry(&chain[below..=below])
        && crate::host::Host::from_ancestry(&chain[..=below]) != Some(owner)
    {
        return false;
    }
    !chain[..below].iter().any(|(_, image)| image.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{owns, remote_binding};

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
    fn a_different_agent_nested_inside_the_agent_does_not() {
        let nested = chain(&[(6, "tj.exe"), (5, "codex.exe"), (4, "pwsh.exe"), (3, "claude.exe"), (1, "tj.exe")]);
        assert!(!owns(&nested, 1));
    }

    #[test]
    fn the_wrapper_itself_does_not() {
        assert!(!owns(&chain(&[(1, "tj.exe"), (0, "init")]), 1));
    }

    #[test]
    fn remote_session_must_have_a_live_reciprocal_launch() {
        let handle = "codex:123e4567-e89b-12d3-a456-426614174000";
        let session = json!({"session": {"session": handle, "native_id": "123e4567-e89b-12d3-a456-426614174000", "host": "codex", "state": "active", "launch_id": "right"}});
        let binding = json!({"session": session["session"], "launch": {"session": handle, "host": "codex", "state": "active", "launch_id": "right", "wrapper": {"pid": 42, "started_at": "2026-11-11T00:00:00Z"}}});
        let live = |pid| (pid == 42).then(|| "2026-11-11T00:00:00+00:00".to_owned());
        assert_eq!(remote_binding(&session, &binding, handle, live).as_deref(), Some("right"));
        for (path, field, invalid) in [
            ("launch", "state", "ended"),
            ("launch", "session", "codex:foreign"),
            ("session", "launch_id", "foreign"),
            ("session", "state", "ended"),
        ] {
            let mut other = binding.clone();
            other[path][field] = json!(invalid);
            assert!(remote_binding(&session, &other, handle, live).is_none());
        }
        assert!(remote_binding(&session, &binding, handle, |_| None).is_none());
        assert!(remote_binding(&session, &binding, handle, |_| Some("2020-01-01T00:00:00Z".into())).is_none());
    }
}
