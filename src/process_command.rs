use std::io;
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tj_cli::process::agent;

use crate::{host::Host, run};

#[derive(Args)]
pub(crate) struct ProcessArgs {
    #[command(subcommand)]
    command: ProcessCommand,
}

#[derive(Subcommand)]
enum ProcessCommand {
    Owner {
        #[arg(long)]
        launch_id: String,
    },
    Spawned {
        #[arg(long)]
        launch_id: String,
        #[arg(long)]
        session: String,
    },
    Inspect,
}

struct Identity {
    pid: u32,
    started_at: String,
}

struct Inspection {
    pid: u32,
    started_at: String,
    alive: bool,
}

pub(crate) fn run(args: ProcessArgs) -> Result<ExitCode> {
    match args.command {
        ProcessCommand::Owner { launch_id } => {
            ensure!(run::owner::launch().as_deref() == Some(&launch_id), "launch owner could not be verified");
            let data = run::store::call(&["show", "--launch-id", &launch_id])?;
            let chain = agent::ancestry().context("cannot inspect process ancestry")?;
            let identity = owner(&launch_id, &data, &chain, agent::started_at)?;
            println!("{}", json!({"pid": identity.pid, "started_at": identity.started_at}));
        }
        ProcessCommand::Spawned { launch_id, session } => {
            let data = run::store::call(&["show", "--launch-id", &launch_id])?;
            let wrapper = verified_wrapper(&launch_id, &data, &agent::started_at)?;
            let children = agent::direct_children(wrapper).context("cannot inspect native children")?;
            let observed = spawned(&launch_id, &session, &data, &children, agent::started_at)?;
            println!("{}", observed.unwrap_or(Value::Null));
        }
        ProcessCommand::Inspect => {
            let input: Value = serde_json::from_reader(io::stdin().lock()).context("invalid identities")?;
            let input = parse_identities(&input)?;
            let output = inspect(input, agent::started_at)?;
            let rows: Vec<_> = output
                .into_iter()
                .map(|row| json!({"pid": row.pid, "started_at": row.started_at, "alive": row.alive}))
                .collect();
            println!("{}", Value::Array(rows));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_identities(input: &Value) -> Result<Vec<Identity>> {
    input
        .as_array()
        .context("identities must be an array")?
        .iter()
        .map(|row| {
            let object = row.as_object().context("identity must be an object")?;
            ensure!(
                object.len() == 2 && object.contains_key("pid") && object.contains_key("started_at"),
                "invalid identity fields"
            );
            let pid = u32::try_from(row["pid"].as_u64().context("invalid pid")?)?;
            let started_at = row["started_at"].as_str().context("invalid creation time")?.to_owned();
            Ok(Identity { pid, started_at })
        })
        .collect()
}

fn owner(
    launch_id: &str,
    data: &Value,
    chain: &[(u32, String)],
    started_at: impl Fn(u32) -> io::Result<Option<SystemTime>>,
) -> Result<Identity> {
    let wrapper = verified_wrapper(launch_id, data, &started_at)?;
    let index = chain.iter().position(|(pid, _)| *pid == wrapper).context("wrapper is not an observable ancestor")?;
    ensure!(index > 1, "no native child above the caller");
    let native = &chain[index - 1];
    let host = data["launch"]["host"].as_str().and_then(Host::from_name).context("unknown launch host")?;
    ensure!(Host::from_ancestry(std::slice::from_ref(native)) == Some(host), "direct child is not the native host");
    let created = started_at(native.0)?.context("native child is not running")?;
    let created: chrono::DateTime<chrono::Utc> = created.into();
    Ok(Identity { pid: native.0, started_at: created.to_rfc3339() })
}

fn verified_wrapper(
    launch_id: &str,
    data: &Value,
    started_at: &impl Fn(u32) -> io::Result<Option<SystemTime>>,
) -> Result<u32> {
    let row = &data["launch"];
    ensure!(
        row["launch_id"].as_str() == Some(launch_id) && row["state"].as_str() == Some("active"),
        "launch is not active or does not match"
    );
    let wrapper = u32::try_from(row["wrapper"]["pid"].as_u64().context("missing wrapper pid")?)?;
    ensure!(wrapper > 0, "invalid wrapper pid");
    let recorded = chrono::DateTime::parse_from_rfc3339(
        row["wrapper"]["started_at"].as_str().context("missing wrapper creation time")?,
    )?;
    let actual: chrono::DateTime<chrono::Utc> = started_at(wrapper)?.context("wrapper is not running")?.into();
    // The launch writer records wrapper creation at whole-second precision.
    ensure!(
        recorded.timestamp_subsec_nanos() == 0 && recorded.timestamp() == actual.timestamp(),
        "wrapper identity does not match"
    );
    Ok(wrapper)
}

fn spawned(
    launch_id: &str,
    session: &str,
    data: &Value,
    children: &[agent::ChildIdentity],
    started_at: impl Fn(u32) -> io::Result<Option<SystemTime>>,
) -> Result<Option<Value>> {
    verified_wrapper(launch_id, data, &started_at)?;
    let launch = &data["launch"];
    let bound = &data["session"];
    let host = launch["host"].as_str().and_then(Host::from_name).context("unknown launch host")?;
    let (session_host, native_id) = session.split_once(':').context("invalid native session")?;
    ensure!(
        session_host == host.name()
            && !native_id.is_empty()
            && launch["session"].as_str() == Some(session)
            && bound["session"].as_str() == Some(session)
            && bound["host"].as_str() == Some(host.name())
            && bound["native_id"].as_str() == Some(native_id)
            && bound["launch_id"].as_str() == Some(launch_id)
            && bound["state"].as_str() == Some("active")
            && bound["wrapper"] == launch["wrapper"],
        "native session is not reciprocally bound to this launch"
    );
    let mut native =
        children.iter().filter(|child| Host::from_ancestry(&[(child.pid, child.image.clone())]) == Some(host));
    let Some(child) = native.next() else { return Ok(None) };
    ensure!(native.next().is_none(), "more than one native child belongs to the wrapper");
    ensure!(started_at(child.pid)? == Some(child.started_at), "native process identity changed");
    let created: chrono::DateTime<chrono::Utc> = child.started_at.into();
    Ok(Some(json!({
        "launch_id": launch_id, "session": session,
        "native": {"pid": child.pid, "started_at": created.to_rfc3339()}
    })))
}

fn inspect(
    input: Vec<Identity>,
    started_at: impl Fn(u32) -> io::Result<Option<SystemTime>>,
) -> Result<Vec<Inspection>> {
    input
        .into_iter()
        .map(|identity| {
            if identity.pid == 0 {
                bail!("pid must be positive");
            }
            let recorded =
                chrono::DateTime::parse_from_rfc3339(&identity.started_at).context("invalid creation time")?;
            let actual = started_at(identity.pid).context("cannot inspect process")?;
            let alive = actual.is_some_and(|time| chrono::DateTime::<chrono::Utc>::from(time) == recorded);
            Ok(Inspection { pid: identity.pid, started_at: identity.started_at, alive })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Identity, inspect, owner, parse_identities, spawned};
    use serde_json::json;
    use std::io;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn inspect_requires_valid_identity_and_preserves_exact_instants() {
        let make = |pid, time: &str| Identity { pid, started_at: time.into() };
        let read = |pid| Ok((pid == 1).then_some(UNIX_EPOCH + Duration::from_nanos(123_456_700)));
        let result = inspect(
            vec![
                make(1, "1970-01-01T00:00:00.123456700Z"),
                make(1, "1970-01-01T00:00:00.123456699Z"),
                make(2, "1970-01-01T00:00:00Z"),
            ],
            read,
        )
        .unwrap();
        assert_eq!(result.iter().map(|row| row.alive).collect::<Vec<_>>(), [true, false, false]);
        assert!(inspect(vec![make(0, "1970-01-01T00:00:00Z")], read).is_err());
        assert!(inspect(vec![make(1, "bad")], read).is_err());
        assert!(inspect(vec![make(1, "1970-01-01T00:00:00Z")], |_| Err(io::Error::other("denied"))).is_err());
    }

    #[test]
    fn input_requires_exact_fields_and_integer_pid() {
        assert!(parse_identities(&json!({})).is_err());
        assert!(parse_identities(&json!([{"pid": -1, "started_at": "x"}])).is_err());
        assert!(parse_identities(&json!([{"pid": 1.5, "started_at": "x"}])).is_err());
        assert!(parse_identities(&json!([{"pid": 1, "started_at": "x", "extra": true}])).is_err());
        assert!(parse_identities(&json!([{"pid": 1}])).is_err());
    }

    #[test]
    fn inspects_the_current_process_and_a_stopped_child() {
        let pid = std::process::id();
        let created: chrono::DateTime<chrono::Utc> = tj_cli::process::agent::started_at(pid).unwrap().unwrap().into();
        let current = Identity { pid, started_at: created.to_rfc3339() };
        assert!(inspect(vec![current], tj_cli::process::agent::started_at).unwrap()[0].alive);

        #[cfg(windows)]
        let mut child = {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", "Start-Sleep 30"])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap()
        };
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let created: chrono::DateTime<chrono::Utc> = tj_cli::process::agent::started_at(pid).unwrap().unwrap().into();
        let children = tj_cli::process::agent::direct_children(std::process::id()).unwrap();
        let observed = children.iter().find(|observed| observed.pid == pid).unwrap();
        assert_eq!(chrono::DateTime::<chrono::Utc>::from(observed.started_at), created);
        child.kill().unwrap();
        child.wait().unwrap();
        let stopped = Identity { pid, started_at: created.to_rfc3339() };
        assert!(!inspect(vec![stopped], tj_cli::process::agent::started_at).unwrap()[0].alive);
    }

    #[test]
    fn owner_matches_the_writers_wrapper_precision_but_returns_full_native_time() {
        let wrapper = UNIX_EPOCH + Duration::from_millis(1_800);
        let native = UNIX_EPOCH + Duration::from_millis(2_800);
        let timestamp =
            chrono::DateTime::<chrono::Utc>::from(wrapper).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut row = json!({"launch":{"launch_id":"right", "state":"active", "host":"codex", "wrapper":{"pid":1,"started_at":timestamp}}});
        let chain = [(3, "tj".into()), (2, "codex".into()), (1, "tj".into())];
        let read = |pid| {
            Ok(match pid {
                1 => Some(wrapper),
                2 => Some(native),
                _ => None,
            })
        };
        let identity = owner("right", &row, &chain, read).unwrap();
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&identity.started_at).unwrap(),
            chrono::DateTime::<chrono::Utc>::from(native)
        );
        for adjacent in ["1970-01-01T00:00:00Z", "1970-01-01T00:00:02Z", "1970-01-01T00:00:01.8Z"] {
            row["launch"]["wrapper"]["started_at"] = json!(adjacent);
            assert!(owner("right", &row, &chain, read).is_err());
        }
    }

    #[test]
    fn owner_requires_exact_active_wrapper_and_native_ancestry() {
        let row = json!({"launch":{"launch_id":"right", "state":"active", "host":"codex", "wrapper":{"pid":1,"started_at":"1970-01-01T00:00:01Z"}}});
        let chain = [(3, "tj".into()), (2, "codex".into()), (1, "tj".into())];
        let read = |pid| Ok(Some(UNIX_EPOCH + Duration::from_secs(u64::from(pid))));
        assert_eq!(owner("right", &row, &chain, read).unwrap().pid, 2);
        assert!(owner("wrong", &row, &chain, read).is_err());
        assert!(owner("right", &row, &chain[1..], read).is_err());
        assert!(owner("right", &row, &chain, |_| Ok(None)).is_err());
        assert!(owner("right", &row, &chain, |_| Err(io::Error::other("denied"))).is_err());
        let mut invalid = row.clone();
        invalid["launch"]["state"] = json!("ended");
        assert!(owner("right", &invalid, &chain, read).is_err());
        invalid = row.clone();
        invalid["launch"]["wrapper"]["started_at"] = json!("1970-01-01T00:00:02Z");
        assert!(owner("right", &invalid, &chain, read).is_err());
        let other = [(3, "tj".into()), (2, "shell".into()), (1, "tj".into())];
        assert!(owner("right", &row, &other, read).is_err());
    }

    fn prepared_binding() -> serde_json::Value {
        let wrapper = json!({"pid":1,"started_at":"1970-01-01T00:00:01Z"});
        json!({
            "launch":{"launch_id":"right","host":"codex","session":"codex:thread","state":"active","wrapper":wrapper},
            "session":{"session":"codex:thread","host":"codex","native_id":"thread","launch_id":"right","state":"active","wrapper":wrapper}
        })
    }

    fn native_child(pid: u32, image: &str) -> tj_cli::process::agent::ChildIdentity {
        tj_cli::process::agent::ChildIdentity {
            pid,
            image: image.to_owned(),
            started_at: UNIX_EPOCH + Duration::from_secs(u64::from(pid)),
        }
    }

    #[test]
    fn spawned_native_is_observable_without_a_hook_or_calling_ancestry() {
        let row = prepared_binding();
        let children = [native_child(2, "codex.exe"), native_child(3, "python.exe")];
        let read = |pid| Ok(Some(UNIX_EPOCH + Duration::from_secs(u64::from(pid))));
        let receipt = spawned("right", "codex:thread", &row, &children, read).unwrap().unwrap();
        assert_eq!(receipt["native"]["pid"], 2);
        assert_eq!(receipt["session"], "codex:thread");
        assert_eq!(receipt["launch_id"], "right");
        assert!(spawned("right", "codex:thread", &row, &[], read).unwrap().is_none());
        assert!(spawned("right", "codex:thread", &row, &children[1..], read).unwrap().is_none());
    }

    #[test]
    fn spawned_native_refuses_other_bindings_ambiguity_and_reused_processes() {
        let row = prepared_binding();
        let child = [native_child(2, "codex")];
        let read = |pid| Ok(Some(UNIX_EPOCH + Duration::from_secs(u64::from(pid))));
        assert!(spawned("wrong", "codex:thread", &row, &child, read).is_err());
        assert!(spawned("right", "codex:other", &row, &child, read).is_err());
        for field in ["session", "host", "native_id", "launch_id", "state", "wrapper"] {
            let mut wrong = row.clone();
            wrong["session"][field] = json!("wrong");
            assert!(spawned("right", "codex:thread", &wrong, &child, read).is_err(), "{field}");
        }
        let competing = [native_child(2, "codex"), native_child(3, "codex")];
        assert!(spawned("right", "codex:thread", &row, &competing, read).is_err());
        assert!(
            spawned("right", "codex:thread", &row, &child, |pid| {
                Ok(Some(UNIX_EPOCH + Duration::from_secs(if pid == 2 { 3 } else { 1 })))
            })
            .is_err()
        );
        assert!(
            spawned("right", "codex:thread", &row, &child, |pid| {
                if pid == 2 { Err(io::Error::other("denied")) } else { read(pid) }
            })
            .is_err()
        );
    }
}
