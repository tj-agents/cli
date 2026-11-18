//! Admission checks for continuing a stopped native conversation in a new launch.

use std::io;
use std::process::{ExitCode, Stdio};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tj_cli::process::agent;

use super::{Launch, hidden, store};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    session: String,
    #[arg(long)]
    launch_id: String,
}

#[derive(Debug)]
pub struct Predecessor {
    pub session: String,
    pub launch_id: String,
    pub wrapper_pid: u32,
    pub wrapper_started_at: String,
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
        || native["state"].as_str() != Some("dead")
    {
        bail!("the predecessor native conversation is still running or unverified");
    }
    require_stopped(&prior, agent::started_at)?;
    Ok(prior)
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

pub fn admit(prior: &Predecessor, launch: &Launch, pid: u32, started_at: &str) -> Result<()> {
    require_stopped(prior, agent::started_at)?;
    store::call(&[
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{Predecessor, predecessor, require_stopped};

    fn prior() -> Predecessor {
        Predecessor {
            session: "codex:thread".into(),
            launch_id: "old".into(),
            wrapper_pid: 42,
            wrapper_started_at: "2026-01-01T00:00:00Z".into(),
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
        let bound = json!({"session":session,"launch":{"launch_id":"old","session":"codex:thread","host":"codex","state":"active","wrapper":session["wrapper"]}});
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
