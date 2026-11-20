//! CLI boundary for native evidence, explicit disposition, and durable result receipts.

use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::completion::{self, EventId, Identity, Record, Report, State, Store, Turn, WaitReason};
use crate::message;
use crate::paths;

use super::{owner, store};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum WaitKind {
    User,
    Child,
    Ci,
    Approval,
}

impl From<WaitKind> for completion::WaitKind {
    fn from(value: WaitKind) -> Self {
        match value {
            WaitKind::User => Self::User,
            WaitKind::Child => Self::Child,
            WaitKind::Ci => Self::Ci,
            WaitKind::Approval => Self::Approval,
        }
    }
}

#[derive(clap::Args)]
pub struct WaitArgs {
    #[arg(value_enum)]
    kind: WaitKind,
    /// What is pending and what will let this session continue.
    reason: String,
}

#[derive(clap::Args)]
pub struct HoldArgs {
    /// Permit automatic closure again; this does not request completion.
    #[arg(long)]
    release: bool,
}

#[derive(clap::Args)]
pub struct ReceiptArgs {
    /// Inspect this recorded launch, including after its terminal has closed.
    #[arg(long)]
    launch: Option<String>,
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().filter(|text| !text.trim().is_empty()).with_context(|| format!("missing {key}"))
}

fn binding(launch: &str, data: &Value, current: bool) -> Result<Identity> {
    let session = &data["session"];
    let row = &data["launch"];
    let handle = text(session, "session")?;
    let host = text(session, "host")?;
    let native = text(session, "native_id")?;
    if text(row, "launch_id")? != launch
        || (current && text(session, "launch_id")? != launch)
        || text(row, "session")? != handle
        || text(row, "host")? != host
        || handle != format!("{host}:{native}")
        || !matches!(host, "claude" | "codex")
    {
        bail!("completion requires an exact reciprocal native session binding");
    }
    Ok(Identity::new(launch, handle)?)
}

pub fn for_launch(launch: &str) -> Result<Store> {
    let data = store::call(&["show", "--launch-id", launch])?;
    Ok(Store::open(paths::state_dir()?.join("completion"), binding(launch, &data, true)?))
}

fn startup_binding(
    launch: &super::Launch,
    data: &Value,
    wrapper_pid: u32,
    started_at: &str,
) -> Result<Option<Identity>> {
    if data.get("session") == Some(&Value::Null) && data["launch"].get("session") == Some(&Value::Null) {
        store::exact_launch(data, launch, wrapper_pid, started_at)?;
        return Ok(None);
    }
    binding(&launch.id, data, true).map(Some)
}

/// Only an explicitly unbound launch owned by this wrapper is still awaiting SessionStart.
pub(super) fn for_startup(launch: &super::Launch, started_at: &str) -> Result<Option<Store>> {
    let data = store::call(&["show", "--launch-id", &launch.id])?;
    let Some(identity) = startup_binding(launch, &data, std::process::id(), started_at)? else { return Ok(None) };
    Ok(Some(Store::open(paths::state_dir()?.join("completion"), identity)))
}

pub fn for_recovery(launch: &str) -> Result<Option<Store>> {
    let data = store::call(&["show", "--launch-id", launch])?;
    let identity = binding(launch, &data, false)?;
    if text(&data["session"], "launch_id")? != launch {
        // Recovery of the current launch owns eligible predecessor claims and journals.
        // An older launch cannot close a conversation that has since been resumed.
        return Ok(None);
    }
    Ok(Some(Store::open(paths::state_dir()?.join("completion"), identity)))
}

fn historical(launch: &str) -> Result<Store> {
    let data = store::call(&["show", "--launch-id", launch])?;
    Ok(Store::open(paths::state_dir()?.join("completion"), binding(launch, &data, false)?))
}

/// Resuming a native conversation creates a new launch, but cannot withdraw the user's keep-open request.
fn inherit_hold(current_store: &Store) -> Result<()> {
    if current_store.load()?.is_some() {
        return Ok(());
    }
    let data = store::call(&["launches"])?;
    let rows = data["launches"].as_array().context("agent-store launches returned no list")?;
    inherit_recorded_hold(current_store, rows, &paths::state_dir()?.join("completion"))
}

fn inherit_recorded_hold(current_store: &Store, rows: &[Value], root: &Path) -> Result<()> {
    let current = rows
        .iter()
        .find(|row| row["launch_id"].as_str() == Some(current_store.identity().launch_id()))
        .context("current launch is absent from launch history")?;
    let created = text(current, "created_at")?;
    let mut previous: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["session"].as_str() == Some(current_store.identity().session_handle())
                && row["launch_id"].as_str() != Some(current_store.identity().launch_id())
                && row["created_at"].as_str().is_some_and(|at| at < created)
        })
        .collect();
    previous.sort_by_key(|row| std::cmp::Reverse(row["created_at"].as_str()));
    for previous in previous {
        let identity = Identity::new(text(previous, "launch_id")?, current_store.identity().session_handle())?;
        // A resume can bind and crash before its first native turn. It has no recorded hold decision.
        let Some(prior) = Store::open(root, identity).load()? else { continue };
        if prior.held_open() {
            let event = EventId::new("inherited-hold")?;
            current_store.transition(|record| if record.revision() == 0 { record.set_hold(event) } else { Ok(()) })?;
        }
        // A recorded release is authoritative; do not resurrect an older hold.
        break;
    }
    Ok(())
}

/// Deliver the captured response even when the user has asked to keep this session open.
pub fn deliver(store: &Store, record: Record) -> Result<Record> {
    let Some(outcome) = record.state().outcome() else { return Ok(record) };
    if outcome.outbox().receipt(completion::Step::Delivery).is_some() {
        return Ok(record);
    }
    let revision = record.state().closing_revision().unwrap_or(record.revision());
    let delivered = (|| -> Result<completion::Receipt> {
        let data = store::call(&["show", "--launch-id", store.identity().launch_id()])?;
        if binding(store.identity().launch_id(), &data, true)? != *store.identity() {
            bail!("result delivery binding changed");
        }
        let event = json!({
            "kind":"result", "launch":store.identity().launch_id(),
            "session":store.identity().session_handle(), "turn":outcome.turn().as_str()
        });
        match &data["launch"]["parent"] {
            Value::Null => Ok(completion::Receipt::new(format!("native-result:{event}"))?),
            Value::String(parent) => {
                let receipt = message::send(
                    parent,
                    Some(store.identity().session_handle()),
                    Some("TJ final result"),
                    outcome.report().as_str(),
                    &event,
                    "result",
                )?;
                if !matches!(receipt["state"].as_str(), Some("stored" | "submitted")) {
                    bail!("final result has no delivery receipt: {receipt}");
                }
                Ok(completion::Receipt::new(receipt.to_string())?)
            }
            _ => bail!("recorded parent is neither a session handle nor null"),
        }
    })();
    let receipt = match delivered {
        Ok(receipt) => receipt,
        Err(error) => {
            let event = fresh_event()?;
            let failure = completion::Failure::new(completion::Step::Delivery, format!("{error:#}"))?;
            if let Err(saved) = store.transition(|record| record.fail(event, revision, failure)) {
                return Err(error.context(format!("delivery failure could not be recorded: {saved:#}")));
            }
            return Err(error);
        }
    };
    let event = fresh_event()?;
    Ok(store.transition(|record| record.record_delivery(event, revision, receipt))?)
}

pub struct Closing {
    store: Store,
    revision: u64,
}

impl Closing {
    pub fn begin(store: Store, record: &Record) -> Result<Self> {
        if record.state().outcome().and_then(|outcome| outcome.outbox().receipt(completion::Step::Delivery)).is_none() {
            bail!("final result delivery has no receipt");
        }
        if let Some(revision) = record.state().closing_revision() {
            return Ok(Self { store, revision });
        }
        let event = fresh_event()?;
        let saved = store.transition(|saved| saved.begin_close(event, record.revision()))?;
        let revision = saved.state().closing_revision().context("closing transition has no revision")?;
        Ok(Self { store, revision })
    }

    pub fn check(&self) -> Result<Record> {
        let record = current(&self.store)?;
        if record.held_open() || record.state().closing_revision() != Some(self.revision) {
            bail!("new input or a keep-open hold withdrew closeout");
        }
        Ok(record)
    }

    pub fn record(&self, step: completion::Step, proof: &str) -> Result<()> {
        let record = self.check()?;
        if record.state().outcome().and_then(|outcome| outcome.outbox().receipt(step)).is_some() {
            return Ok(());
        }
        let event = fresh_event()?;
        let receipt = completion::Receipt::new(proof)?;
        match step {
            completion::Step::Delivery => {
                self.store.transition(|record| record.record_delivery(event, self.revision, receipt))?
            }
            completion::Step::Tracker => {
                self.store.transition(|record| record.record_tracker(event, self.revision, receipt))?
            }
            completion::Step::Cleanup => {
                self.store.transition(|record| record.record_cleanup(event, self.revision, receipt))?
            }
        };
        Ok(())
    }

    pub fn fail(&self, step: completion::Step, error: &str) -> Result<()> {
        let event = fresh_event()?;
        let failure = completion::Failure::new(step, error)?;
        self.store.transition(|record| record.fail(event, self.revision, failure))?;
        Ok(())
    }

    pub fn recorded(&self, step: completion::Step) -> Result<bool> {
        Ok(self.check()?.state().outcome().is_some_and(|outcome| outcome.outbox().receipt(step).is_some()))
    }

    pub fn attempt<T>(&self, step: completion::Step, action: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        match action() {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Err(saved) = self.fail(step, &format!("{error:#}")) {
                    return Err(error.context(format!("closeout failure could not be recorded: {saved:#}")));
                }
                Err(error)
            }
        }
    }

    pub fn complete(&self) -> Result<()> {
        self.check()?;
        let event = fresh_event()?;
        self.store.transition(|record| record.complete(event, self.revision))?;
        Ok(())
    }

    pub fn withdraw(&self, reason: &str) -> Result<()> {
        let record = self.check()?;
        let turn = record.state().outcome().context("closing result is missing")?.turn().clone();
        let event = fresh_event()?;
        self.store.transition(|record| record.interrupt(event, &turn, reason))?;
        Ok(())
    }
}

fn owned() -> Result<Store> {
    let launch = owner::launch().context("this command requires the managed session's own agent process")?;
    for_launch(&launch)
}

fn event_id(kind: &str, turn: &str, pending: Option<&str>) -> Result<EventId> {
    // Native hooks identify a turn, not a separate delivery attempt. Encoding its opaque bytes keeps
    // retries stable without depending on a platform-specific or randomly seeded hash.
    let key = serde_json::to_vec(&(kind, turn, pending))?;
    Ok(EventId::new(key.iter().map(|byte| format!("{byte:02x}")).collect::<String>())?)
}

fn fresh_event() -> Result<EventId> {
    Ok(EventId::new(super::generated_id()?)?)
}

fn current(store: &Store) -> Result<Record> {
    store.load()?.context("no native user turn has been recorded; completion remains unverified")
}

pub fn finish() -> Result<ExitCode> {
    let store = owned()?;
    let record = current(&store)?;
    let turn = record.state().turn().context("finish requires the current native turn")?.clone();
    let event = fresh_event()?;
    store.transition(|record| record.request_finish(event, &turn))?;
    println!(
        "Finish requested. Deliver the final response; TJ will receipt that turn and verify safe closeout. New input cancels."
    );
    Ok(ExitCode::SUCCESS)
}

pub fn wait(args: WaitArgs) -> Result<ExitCode> {
    let store = owned()?;
    let record = current(&store)?;
    let turn = record.state().turn().context("wait requires the current native turn")?.clone();
    let event = fresh_event()?;
    let reason = WaitReason::new(args.kind.into(), args.reason)?;
    store.transition(|record| record.wait(event, &turn, reason))?;
    println!("Waiting recorded. This session will remain open until its work resumes and finishes.");
    Ok(ExitCode::SUCCESS)
}

pub fn hold(args: HoldArgs) -> Result<ExitCode> {
    let store = owned()?;
    let event = fresh_event()?;
    store.transition(|record| if args.release { record.release_hold(event) } else { record.set_hold(event) })?;
    println!(
        "{}",
        if args.release { "Keep-open hold released." } else { "Keep-open hold recorded; new input preserves it." }
    );
    Ok(ExitCode::SUCCESS)
}

pub fn receipt(args: ReceiptArgs) -> Result<ExitCode> {
    let store = match args.launch {
        Some(launch) => historical(&launch)?,
        None => owned()?,
    };
    let record = current(&store)?;
    println!("{}", String::from_utf8(record.encode()?)?);
    Ok(ExitCode::SUCCESS)
}

fn apply(store: &Store, payload: &Value) -> Result<Record> {
    if text(payload, "session")? != store.identity().session_handle() {
        bail!("native hook belongs to another session");
    }
    let kind = text(payload, "event")?;
    let turn = Turn::new(text(payload, "turn")?)?;
    let pending = match &payload["pending"] {
        Value::Null => None,
        Value::String(reason) if !reason.trim().is_empty() => Some(reason.as_str()),
        _ => bail!("native pending reason must be nonempty text or null"),
    };
    let event = event_id(kind, turn.as_str(), pending)?;
    match kind {
        "start" => Ok(store.transition(|record| record.new_input(event, turn))?),
        "end" => {
            let report = Report::new(text(payload, "report")?)?;
            let pending = pending.map(|reason| WaitReason::new(completion::WaitKind::Child, reason)).transpose()?;
            Ok(store.transition(|record| record.end_turn(event, &turn, Some(report), pending))?)
        }
        "interrupt" => {
            let reason = pending.unwrap_or("The native turn was interrupted");
            Ok(store.transition(|record| record.interrupt(event, &turn, reason))?)
        }
        _ => bail!("unsupported native lifecycle event {kind:?}"),
    }
}

pub fn message(record: &Record) -> String {
    let state = match record.state() {
        State::Inactive => "Waiting for native user input".to_owned(),
        State::Active { intent: None, .. } => "Active".to_owned(),
        State::Active { intent: Some(_), .. } => "Finish requested; waiting for the final response".to_owned(),
        State::Waiting { reason, .. } => format!("Waiting: {}", reason.description()),
        State::Attention { reason, .. } => format!("Incomplete: {reason}"),
        State::Ready { .. } => "Final result recorded; safe closeout pending".to_owned(),
        State::Closing { .. } => "Final result recorded; closeout in progress".to_owned(),
        State::Completed { .. } => "Completed with a durable closeout receipt".to_owned(),
    };
    if record.held_open() { format!("TJ: {state}. Keep-open hold is active.") } else { format!("TJ: {state}.") }
}

pub fn native_event() -> Result<ExitCode> {
    let Some(launch) = owner::launch() else {
        println!("{{}}");
        return Ok(ExitCode::SUCCESS);
    };
    let store = for_launch(&launch)?;
    let mut input = String::new();
    io::stdin().take(16 * 1024 * 1024).read_to_string(&mut input)?;
    let payload: Value = serde_json::from_str(&input).context("invalid native lifecycle payload")?;
    if payload["event"] == "start" {
        inherit_hold(&store)?;
    }
    let record = apply(&store, &payload)?;
    // UserPromptSubmit emits no context or new prompt. Stops may display a status only.
    let message = (payload["event"] != "start").then(|| message(&record));
    println!("{}", json!({"message":message}));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_pending_requires_explicit_nulls_and_the_exact_active_wrapper() {
        for host in [crate::host::Host::Claude, crate::host::Host::Codex] {
            let root = tempfile::tempdir().unwrap();
            let launch = super::super::Launch {
                id: "launch".into(),
                host,
                title: "Tab".into(),
                cwd: root.path().to_path_buf(),
                item: None,
            };
            let started = "2026-11-18T10:00:00Z";
            let pending = json!({"session":null, "launch":{
                "launch_id":"launch", "session":null, "host":host.name(), "state":"active",
                "title":"Tab", "cwd":root.path(), "wrapper":{"pid":42, "started_at":started}
            }});
            assert!(startup_binding(&launch, &pending, 42, started).unwrap().is_none());
            assert!(binding("launch", &pending, true).is_err(), "strict completion accepted an unbound launch");
            for (pointer, value) in [
                ("/session", json!({})),
                ("/launch/session", json!(format!("{}:native", host.name()))),
                ("/launch/launch_id", json!("another-launch")),
                ("/launch/host", json!("other-host")),
                ("/launch/state", json!("ended")),
                ("/launch/title", json!("another-title")),
                ("/launch/cwd", json!("another-directory")),
                ("/launch/wrapper/pid", json!(43)),
                ("/launch/wrapper/started_at", json!("another-start")),
            ] {
                let mut invalid = pending.clone();
                *invalid.pointer_mut(pointer).unwrap() = value;
                assert!(startup_binding(&launch, &invalid, 42, started).is_err(), "accepted {pointer}: {invalid}");
            }
            for pointer in ["", "/launch"] {
                let mut invalid = pending.clone();
                invalid.pointer_mut(pointer).unwrap().as_object_mut().unwrap().remove("session");
                assert!(startup_binding(&launch, &invalid, 42, started).is_err(), "accepted absent {pointer}/session");
            }
            let mut bound = pending;
            let handle = format!("{}:native", host.name());
            bound["launch"]["session"] = json!(handle);
            bound["session"] = json!({"session":handle,"native_id":"native","host":host.name(),"launch_id":"launch"});
            assert_eq!(startup_binding(&launch, &bound, 42, started).unwrap().unwrap().session_handle(), handle);
            bound["session"]["launch_id"] = json!("another-launch");
            assert!(startup_binding(&launch, &bound, 42, started).is_err());
        }
    }

    #[test]
    fn interrupted_resumes_preserve_the_last_recorded_hold_decision() {
        for released in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let identity = |launch| Identity::new(launch, "codex:session").unwrap();
            let held = Store::open(root.path(), identity("held"));
            held.transition(|record| record.set_hold(EventId::new("hold").unwrap())).unwrap();
            if released {
                let released = Store::open(root.path(), identity("released"));
                released.transition(|record| record.release_hold(EventId::new("release").unwrap())).unwrap();
            }
            let rows = vec![
                json!({"launch_id":"interrupted", "session":"codex:session", "created_at":"2026-11-17T03:00:00Z"}),
                json!({"launch_id":"current", "session":"codex:session", "created_at":"2026-11-17T04:00:00Z"}),
                json!({"launch_id":"held", "session":"codex:session", "created_at":"2026-11-17T01:00:00Z"}),
                json!({"launch_id":"released", "session":"codex:session", "created_at":"2026-11-17T02:00:00Z"}),
            ];
            let current = Store::open(root.path(), identity("current"));
            inherit_recorded_hold(&current, &rows, root.path()).unwrap();
            assert_eq!(current.load().unwrap().is_some_and(|record| record.held_open()), !released);
            // The next native turn must retain the inherited disposition.
            let event = json!({"session":"codex:session", "turn":"turn", "event":"start", "pending":null});
            assert_eq!(apply(&current, &event).unwrap().held_open(), !released);
        }
    }

    #[test]
    fn malformed_previous_completion_cannot_silently_release_a_hold() {
        let root = tempfile::tempdir().unwrap();
        let held = Store::open(root.path(), Identity::new("held", "codex:session").unwrap());
        held.transition(|record| record.set_hold(EventId::new("hold").unwrap())).unwrap();
        let path = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|extension| extension == "json"))
            .unwrap();
        std::fs::write(path, "{torn").unwrap();
        let current = Store::open(root.path(), Identity::new("current", "codex:session").unwrap());
        let rows = vec![
            json!({"launch_id":"held", "session":"codex:session", "created_at":"2026-11-17T01:00:00Z"}),
            json!({"launch_id":"current", "session":"codex:session", "created_at":"2026-11-17T02:00:00Z"}),
        ];
        assert!(inherit_recorded_hold(&current, &rows, root.path()).is_err());
        assert!(current.load().unwrap().is_none());
    }

    #[test]
    fn native_events_keep_waits_and_reject_foreign_or_conflicting_results() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path(), Identity::new("launch", "codex:session").unwrap());
        let mut event = json!({"session":"codex:session", "turn":"turn", "event":"start", "pending":null});
        apply(&store, &event).unwrap();
        store
            .transition(|record| {
                record.wait(
                    EventId::new("wait").unwrap(),
                    &Turn::new("turn").unwrap(),
                    WaitReason::new(completion::WaitKind::User, "answer needed").unwrap(),
                )
            })
            .unwrap();
        event["event"] = json!("end");
        event["report"] = json!("What should the name be?");
        assert!(matches!(apply(&store, &event).unwrap().state(), State::Waiting { .. }));
        let saved = store.load().unwrap();
        event["session"] = json!("codex:other");
        assert!(apply(&store, &event).is_err());
        event["session"] = json!("codex:session");
        event["report"] = json!("A conflicting retry");
        assert!(apply(&store, &event).is_err());
        assert_eq!(store.load().unwrap(), saved);
    }
}
