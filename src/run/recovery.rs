//! Admission checks for continuing a stopped native conversation in a new launch.

use std::io;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tj_cli::process::agent;

use super::{Launch, hidden, owner, store};
use crate::domain::item;
use crate::host::Host;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    session: String,
    #[arg(long)]
    launch_id: String,
}

#[derive(clap::Args)]
pub struct BindArgs {
    #[arg(long)]
    session: String,
    #[arg(long)]
    launch_id: String,
    #[arg(long)]
    from_launch_id: String,
}

#[derive(Debug)]
pub struct Predecessor {
    pub session: String,
    pub launch_id: String,
    pub wrapper_pid: u32,
    pub wrapper_started_at: String,
    item: Option<item::Reference>,
}

fn predecessor(session: &str, launch_id: &str, current: &Value, bound: &Value) -> Result<Predecessor> {
    let (host, native) = session.split_once(':').context("resume requires an exact native session handle")?;
    if !matches!(host, "claude" | "codex") || native.is_empty() || native.contains(':') {
        bail!("resume requires an exact Claude or Codex session handle");
    }
    let row = &bound["launch"];
    for record in [&current["session"], &bound["session"]] {
        if record["session"].as_str() != Some(session)
            || record["host"].as_str() != Some(host)
            || record["native_id"].as_str() != Some(native)
            || record["launch_id"].as_str() != Some(launch_id)
            || record["wrapper"] != row["wrapper"]
            || !matches!(record["state"].as_str(), Some("active" | "ended"))
        {
            bail!("the predecessor session binding changed or is closing");
        }
    }
    if row["launch_id"].as_str() != Some(launch_id)
        || row["host"].as_str() != Some(host)
        || row["session"].as_str() != Some(session)
        || !matches!(row["state"].as_str(), Some("active" | "ended"))
    {
        bail!("the predecessor launch binding changed or is closing");
    }
    let pid = row["wrapper"]["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .context("the predecessor has no valid wrapper PID")?;
    let started = row["wrapper"]["started_at"].as_str().context("the predecessor has no wrapper creation time")?;
    chrono::DateTime::parse_from_rfc3339(started).context("invalid predecessor wrapper creation time")?;
    Ok(Predecessor {
        session: session.to_owned(),
        launch_id: launch_id.to_owned(),
        wrapper_pid: pid,
        wrapper_started_at: started.to_owned(),
        item: store::item(row)?,
    })
}

fn require_stopped(prior: &Predecessor, inspect: impl FnOnce(u32) -> io::Result<Option<SystemTime>>) -> Result<()> {
    let recorded = chrono::DateTime::parse_from_rfc3339(&prior.wrapper_started_at)?;
    if let Some(started) = inspect(prior.wrapper_pid).context("cannot determine predecessor wrapper liveness")? {
        let started: chrono::DateTime<chrono::Utc> = started.into();
        if (started - recorded.to_utc()).num_seconds().abs() <= 2 {
            bail!("the predecessor wrapper is still running; use its existing conversation");
        }
    }
    Ok(())
}

pub fn inspect(session: &str, launch_id: &str) -> Result<Predecessor> {
    inspect_native(session, launch_id, None)
}

fn inspect_native(session: &str, launch_id: &str, replacement: Option<u32>) -> Result<Predecessor> {
    let current = store::call(&["show", session])?;
    let bound = store::call(&["show", "--launch-id", launch_id])?;
    let prior = predecessor(session, launch_id, &current, &bound)?;
    require_stopped(&prior, agent::started_at)?;
    let executable = std::env::current_exe().context("cannot locate the current TJ executable")?;
    let output = hidden(executable.to_str().context("TJ executable path is not UTF-8")?)
        .args(["session", "liveness", "--session", session, "--launch-id", launch_id])
        .stdin(Stdio::null())
        .output()
        .context("could not verify predecessor native liveness")?;
    if !output.status.success() {
        bail!("predecessor native liveness is unknown: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let native: Value =
        serde_json::from_slice(&output.stdout).context("invalid predecessor native liveness response")?;
    if native["session"].as_str() != Some(session)
        || native["launch_id"].as_str() != Some(launch_id)
        || !native_stopped(&native, replacement)
    {
        bail!("the predecessor native conversation is still running or unverified");
    }
    require_stopped(&prior, agent::started_at)?;
    Ok(prior)
}

fn native_stopped(native: &Value, replacement: Option<u32>) -> bool {
    match native["state"].as_str() {
        Some("dead") => replacement.is_none(),
        Some("live") => replacement.is_some_and(|pid| {
            native["process_ids"].as_array().is_some_and(|processes| {
                !processes.is_empty() && processes.iter().all(|value| value.as_u64() == Some(u64::from(pid)))
            })
        }),
        _ => false,
    }
}

pub fn check(args: Args) -> Result<ExitCode> {
    let prior = inspect(&args.session, &args.launch_id)?;
    println!(
        "{}",
        json!({"session":prior.session, "launch_id":prior.launch_id,
        "wrapper_pid":prior.wrapper_pid, "wrapper_started_at":prior.wrapper_started_at})
    );
    Ok(ExitCode::SUCCESS)
}

pub fn admit(prior: &Predecessor, launch: &Launch, pid: u32, started_at: &str) -> Result<Value> {
    require_stopped(prior, agent::started_at)?;
    if prior.item.is_none() || prior.item != launch.item {
        let mut sources = vec![prior.launch_id.clone()];
        if let Some(item) = prior.item.as_ref() {
            sources.extend(
                store::previous_launches(&prior.launch_id, launch.host, &prior.session, item)?
                    .into_iter()
                    .map(|row| row.launch.id),
            );
        }
        for id in sources {
            if super::closeout::journal::path(&id)?.try_exists()? {
                bail!("launch {id} has a pending cleanup journal; reconcile its cleanup before resuming");
            }
        }
    }
    let response = store::call(&[
        "resume",
        "--session",
        &prior.session,
        "--from-launch-id",
        &prior.launch_id,
        "--launch-id",
        &launch.id,
        "--from-wrapper-pid",
        &prior.wrapper_pid.to_string(),
        "--from-wrapper-started-at",
        &prior.wrapper_started_at,
        "--wrapper-pid",
        &pid.to_string(),
        "--wrapper-started-at",
        started_at,
    ])?;
    // Resume atomically binds the destination and transfers its claims. Registering
    // again would add a failure window after that transaction has already committed.
    store::exact_session(&response["session"], launch, &prior.session)?;
    if response["session"]["wrapper"]["pid"].as_u64() != Some(u64::from(pid))
        || response["session"]["wrapper"]["started_at"].as_str() != Some(started_at)
    {
        bail!("agent-store resume returned a different wrapper");
    }
    Ok(response["session"].clone())
}

/// The native startup hook already runs below the replacement process. Exclude
/// only that verified process when proving the predecessor no longer runs.
pub fn bind(args: BindArgs) -> Result<ExitCode> {
    let (name, native_id) = args.session.split_once(':').context("hook resume requires a native session")?;
    let host = Host::from_name(name).context("unsupported resume host")?;
    if !super::canonical_uuid(native_id) || args.from_launch_id == args.launch_id {
        bail!("hook resume requires a different launch and a canonical native session");
    }
    if owner::launch().as_deref() != Some(args.launch_id.as_str()) {
        bail!("only the replacement native process can bind its launch");
    }
    let target = store::call(&["show", "--launch-id", &args.launch_id])?;
    let row = &target["launch"];
    let pid = row["wrapper"]["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .context("missing replacement wrapper")?;
    let started = row["wrapper"]["started_at"].as_str().context("missing replacement creation time")?;
    let chain = agent::ancestry()?;
    let at =
        chain.iter().position(|(ancestor, _)| *ancestor == pid).context("replacement wrapper is not an ancestor")?;
    let native = at.checked_sub(1).context("the wrapper cannot bind itself")?;
    if row["launch_id"].as_str() != Some(args.launch_id.as_str())
        || row["host"].as_str() != Some(host.name())
        || row["state"].as_str() != Some("active")
        || agent::started_at(pid)?.map(super::rfc3339).as_deref() != Some(started)
    {
        bail!("replacement launch does not match the calling native process");
    }
    let prior = inspect_native(&args.session, &args.from_launch_id, Some(chain[native].0))?;
    let launch = Launch {
        id: args.launch_id,
        host,
        title: row["title"].as_str().context("missing replacement title")?.to_owned(),
        cwd: PathBuf::from(row["cwd"].as_str().context("missing replacement directory")?),
        item: store::item(row)?,
    };
    let session = admit(&prior, &launch, pid, started)?;
    println!("{}", json!({"session":session}));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{Predecessor, native_stopped, predecessor, require_stopped};

    #[test]
    fn hook_admission_excludes_only_its_verified_native_process() {
        assert!(native_stopped(&json!({"state":"dead"}), None));
        assert!(!native_stopped(&json!({"state":"dead"}), Some(42)));
        assert!(native_stopped(&json!({"state":"live", "process_ids":[42]}), Some(42)));
        for reply in [
            json!({"state":"unknown"}),
            json!({"state":"live"}),
            json!({"state":"live", "process_ids":[]}),
            json!({"state":"live", "process_ids":[42,43]}),
            json!({"state":"live", "process_ids":["42"]}),
        ] {
            assert!(!native_stopped(&reply, Some(42)), "{reply}");
        }
        assert!(!native_stopped(&json!({"state":"live", "process_ids":[42]}), None));
    }

    fn prior() -> Predecessor {
        Predecessor {
            session: "codex:thread".into(),
            launch_id: "old".into(),
            wrapper_pid: 42,
            wrapper_started_at: "2026-01-01T00:00:00Z".into(),
            item: None,
        }
    }

    #[test]
    fn absent_or_reused_wrapper_is_stopped_but_unknown_is_not() {
        assert!(require_stopped(&prior(), |_| Ok(None)).is_ok());
        assert!(require_stopped(&prior(), |_| Ok(Some(UNIX_EPOCH))).is_ok());
        assert!(require_stopped(&prior(), |_| Err(io::Error::from(io::ErrorKind::PermissionDenied))).is_err());
    }

    #[test]
    fn a_matching_wrapper_remains_live_at_recorded_second_precision() {
        let time = chrono::DateTime::parse_from_rfc3339(&prior().wrapper_started_at).unwrap();
        let actual = SystemTime::from(time) + Duration::from_millis(800);
        assert!(require_stopped(&prior(), |_| Ok(Some(actual))).is_err());
    }

    #[test]
    fn only_the_current_reciprocal_nonclosing_owner_can_resume() {
        let session = json!({"session":"codex:thread", "host":"codex", "native_id":"thread", "launch_id":"old", "state":"active", "wrapper":{"pid":42,"started_at":"2026-01-01T00:00:00Z"}});
        let current = json!({"session":session});
        let bound = json!({"session":session,"launch":{"launch_id":"old","session":"codex:thread","host":"codex","state":"active","wrapper":session["wrapper"],"item":null}});
        assert!(predecessor("codex:thread", "old", &current, &bound).is_ok());
        for (key, value) in [("launch_id", "new"), ("state", "closing"), ("session", "codex:other")] {
            let mut changed = current.clone();
            changed["session"][key] = json!(value);
            assert!(predecessor("codex:thread", "old", &changed, &bound).is_err());
        }
        let mut changed = bound.clone();
        changed["launch"]["wrapper"]["pid"] = json!(43);
        assert!(predecessor("codex:thread", "old", &current, &changed).is_err());
    }
}
