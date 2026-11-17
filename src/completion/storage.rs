use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};

use super::{
    Applied, Archived, EventId, Failure, IdError, Identity, Intent, Outbox, Outcome, Receipt, Record, ResultReceipt,
    SCHEMA, State, Step, StepState, Turn, WaitKind, WaitReason, Withdrawal,
};

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("completion record is not valid JSON")]
    Json(#[source] serde_json::Error),
    #[error("completion record is malformed: {0}")]
    Malformed(&'static str),
    #[error("completion record schema {0} is unsupported")]
    Schema(u64),
    #[error("completion record has an invalid identifier")]
    Identity(#[source] IdError),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("could not create completion storage root")]
    Root(#[source] io::Error),
    #[error("could not open completion lock {}", .0.display())]
    Lock(PathBuf, #[source] io::Error),
    #[error("could not read completion record {}", .0.display())]
    Read(PathBuf, #[source] io::Error),
    #[error("could not decode completion record {}", .0.display())]
    Decode(PathBuf, #[source] WireError),
    #[error("stored completion identity does not match this store")]
    IdentityMismatch,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError<E: std::error::Error + Send + Sync + 'static> {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("completion transition failed")]
    Transition(#[source] E),
    #[error("could not encode completion record")]
    Encode(#[source] WireError),
    #[error("could not stage completion record {}", .0.display())]
    Stage(PathBuf, #[source] io::Error),
    #[error("could not publish completion record {}", .0.display())]
    Publish(PathBuf, #[source] io::Error),
}

/// A durable record store.  Each transition loads, changes, synchronizes, and publishes under one sibling lock.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    identity: Identity,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>, identity: Identity) -> Self {
        Self { root: root.into(), identity }
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn load(&self) -> Result<Option<Record>, LoadError> {
        let lock = self.lock()?;
        let record = self.read()?;
        drop(lock);
        Ok(record)
    }

    pub fn transition<E>(&self, change: impl FnOnce(&mut Record) -> Result<(), E>) -> Result<Record, StoreError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let _lock = self.lock().map_err(StoreError::Load)?;
        let previous = self.read().map_err(StoreError::Load)?;
        let mut record = previous.clone().unwrap_or_else(|| Record::new(self.identity.clone()));
        change(&mut record).map_err(StoreError::Transition)?;
        if previous.as_ref() != Some(&record) {
            self.write(&record)?;
        }
        Ok(record)
    }

    fn name(&self) -> String {
        let (host, id) = self.identity.session_handle().split_once(':').expect("Identity validates session handle");
        format!(
            "completion-{}-{}--{}-{}-{}.json",
            self.identity.launch_id().len(),
            self.identity.launch_id(),
            host.len(),
            host,
            id
        )
    }
    fn path(&self) -> PathBuf {
        self.root.join(self.name())
    }
    fn lock_path(&self) -> PathBuf {
        self.root.join(format!("{}.lock", self.name()))
    }
    fn lock(&self) -> Result<File, LoadError> {
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        directory.create(&self.root).map_err(LoadError::Root)?;
        let path = self.lock_path();
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&path).map_err(|error| LoadError::Lock(path.clone(), error))?;
        fs4::FileExt::lock(&file).map_err(|error| LoadError::Lock(path, error))?;
        Ok(file)
    }
    fn read(&self) -> Result<Option<Record>, LoadError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(LoadError::Read(path, error)),
        };
        let record = decode(&bytes).map_err(|error| LoadError::Decode(self.path(), error))?;
        if record.identity() != &self.identity {
            return Err(LoadError::IdentityMismatch);
        }
        Ok(Some(record))
    }
    fn write<E>(&self, record: &Record) -> Result<(), StoreError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        if record.identity() != &self.identity {
            return Err(StoreError::Load(LoadError::IdentityMismatch));
        }
        let path = self.path();
        let bytes = encode(record).map_err(StoreError::Encode)?;
        let (temporary, mut file) = loop {
            let temporary = self.root.join(format!(
                "{}.{}.{}.tmp",
                self.name(),
                std::process::id(),
                TEMPORARY.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            match options.open(&temporary) {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(StoreError::Stage(temporary, error)),
            }
        };
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| StoreError::Stage(temporary.clone(), error))?;
        drop(file);
        std::fs::rename(&temporary, &path).map_err(|error| StoreError::Publish(path.clone(), error))?;
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| StoreError::Publish(path, error))?;
        Ok(())
    }
}

pub(super) fn encode(record: &Record) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(&json!({
        "schema": SCHEMA,
        "identity": identity(&record.identity),
        "revision": record.revision,
        "hold": record.hold,
        "state": state(&record.state),
        "withdrawals": record.withdrawals.iter().map(withdrawal).collect::<Vec<_>>(),
        "archive": record.archive.iter().map(archived).collect::<Vec<_>>(),
        "applied": record.applied.iter().map(applied).collect::<Vec<_>>(),
    }))
    .map_err(WireError::Json)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Record, WireError> {
    let value = serde_json::from_slice(bytes).map_err(WireError::Json)?;
    let root =
        as_object(&value, &["schema", "identity", "revision", "hold", "state", "withdrawals", "archive", "applied"])?;
    let schema = number(root, "schema")?;
    if schema != SCHEMA {
        return Err(WireError::Schema(schema));
    }
    let record = Record {
        identity: decode_identity(field(root, "identity")?)?,
        revision: number(root, "revision")?,
        hold: boolean(root, "hold")?,
        state: decode_state(field(root, "state")?)?,
        withdrawals: array(root, "withdrawals")?.iter().map(decode_withdrawal).collect::<Result<_, _>>()?,
        archive: array(root, "archive")?.iter().map(decode_archived).collect::<Result<_, _>>()?,
        applied: array(root, "applied")?.iter().map(decode_applied).collect::<Result<_, _>>()?,
    };
    if record.revision != record.applied.len() as u64
        || record
            .applied
            .iter()
            .enumerate()
            .any(|(index, event)| record.applied[..index].iter().any(|previous| previous.id == event.id))
    {
        return Err(WireError::Malformed("revision or event identity"));
    }
    match &record.state {
        State::Active { turn, intent: Some(intent) } if intent.turn != *turn => {
            return Err(WireError::Malformed("intent turn"));
        }
        State::Closing { revision, .. } if record.hold || *revision == 0 || *revision > record.revision => {
            return Err(WireError::Malformed("closing revision or hold"));
        }
        State::Completed { outcome } if record.hold || !outcome.outbox.complete() => {
            return Err(WireError::Malformed("completed without all receipts"));
        }
        _ => {}
    }
    Ok(record)
}

fn identity(identity: &Identity) -> Value {
    json!({"launch_id": identity.launch_id(), "session_handle": identity.session_handle()})
}
fn encode_receipt(receipt: &Receipt) -> Value {
    json!({"id": receipt.id()})
}
fn encode_failure(failure: &Failure) -> Value {
    json!({"step": step(failure.step()), "description": failure.description()})
}
fn outbox(outbox: &Outbox) -> Value {
    json!({"delivery": step_state(outbox.step(Step::Delivery)), "tracker": step_state(outbox.step(Step::Tracker)), "cleanup": step_state(outbox.step(Step::Cleanup))})
}
fn step_state(state: &StepState) -> Value {
    match state {
        StepState::Pending => json!({"kind":"pending"}),
        StepState::Recorded(receipt) => json!({"kind":"recorded", "receipt":encode_receipt(receipt)}),
        StepState::Failed(failure) => json!({"kind":"failed", "failure":encode_failure(failure)}),
    }
}
fn encode_outcome(outcome: &Outcome) -> Value {
    json!({"turn": outcome.turn.as_str(), "report": outcome.report.as_str(), "receipt": {"event": outcome.receipt.event.as_str()}, "outbox": outbox(&outcome.outbox)})
}
fn state(state: &State) -> Value {
    match state {
        State::Inactive => json!({"kind":"inactive"}),
        State::Active { turn, intent } => {
            json!({"kind":"active", "turn":turn.as_str(), "intent":intent.as_ref().map(|intent| json!({"turn":intent.turn.as_str()}))})
        }
        State::Waiting { turn, reason, report, finish_requested } => {
            json!({"kind":"waiting", "turn":turn.as_str(), "reason":{"kind":wait_kind(reason.kind), "description":reason.description}, "report": report.as_ref().map(super::Report::as_str), "finish_requested":finish_requested})
        }
        State::Attention { reason, report, outcome } => {
            json!({"kind":"attention", "reason":reason, "report":report.as_ref().map(super::Report::as_str), "outcome":outcome.as_ref().map(encode_outcome)})
        }
        State::Ready { outcome } => json!({"kind":"ready", "outcome":encode_outcome(outcome)}),
        State::Closing { revision, outcome } => {
            json!({"kind":"closing", "revision":revision, "outcome":encode_outcome(outcome)})
        }
        State::Completed { outcome } => json!({"kind":"completed", "outcome":encode_outcome(outcome)}),
    }
}
fn withdrawal(withdrawal: &Withdrawal) -> Value {
    json!({"turn":withdrawal.turn.as_str(), "reason":withdrawal.reason})
}
fn archived(archived: &Archived) -> Value {
    json!({"outcome":encode_outcome(&archived.outcome), "reason":archived.reason})
}
fn applied(applied: &Applied) -> Value {
    json!({"id":applied.id.as_str(), "operation":applied.operation, "facts":applied.facts})
}
fn step(step: Step) -> &'static str {
    match step {
        Step::Delivery => "delivery",
        Step::Tracker => "tracker",
        Step::Cleanup => "cleanup",
    }
}
fn wait_kind(kind: WaitKind) -> &'static str {
    match kind {
        WaitKind::User => "user",
        WaitKind::Child => "child",
        WaitKind::Ci => "ci",
        WaitKind::Approval => "approval",
    }
}

fn as_object<'a>(value: &'a Value, expected: &[&str]) -> Result<&'a Map<String, Value>, WireError> {
    let object = value.as_object().ok_or(WireError::Malformed("expected object"))?;
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(WireError::Malformed("unexpected or missing object field"));
    }
    Ok(object)
}
fn field<'a>(object: &'a Map<String, Value>, key: &'static str) -> Result<&'a Value, WireError> {
    object.get(key).ok_or(WireError::Malformed(key))
}
fn text(object: &Map<String, Value>, key: &'static str) -> Result<String, WireError> {
    field(object, key)?.as_str().map(str::to_owned).ok_or(WireError::Malformed(key))
}
fn number(object: &Map<String, Value>, key: &'static str) -> Result<u64, WireError> {
    field(object, key)?.as_u64().ok_or(WireError::Malformed(key))
}
fn boolean(object: &Map<String, Value>, key: &'static str) -> Result<bool, WireError> {
    field(object, key)?.as_bool().ok_or(WireError::Malformed(key))
}
fn array<'a>(object: &'a Map<String, Value>, key: &'static str) -> Result<&'a Vec<Value>, WireError> {
    field(object, key)?.as_array().ok_or(WireError::Malformed(key))
}
fn optional(value: &Value) -> Result<Option<&Value>, WireError> {
    if value.is_null() { Ok(None) } else { Ok(Some(value)) }
}
fn decode_identity(value: &Value) -> Result<Identity, WireError> {
    let object = as_object(value, &["launch_id", "session_handle"])?;
    Identity::new(text(object, "launch_id")?, text(object, "session_handle")?).map_err(WireError::Identity)
}
fn decode_turn(value: &Value) -> Result<Turn, WireError> {
    Turn::new(value.as_str().ok_or(WireError::Malformed("turn"))?).map_err(WireError::Identity)
}
fn decode_event(value: &Value) -> Result<EventId, WireError> {
    EventId::new(value.as_str().ok_or(WireError::Malformed("event"))?).map_err(WireError::Identity)
}
fn decode_receipt(value: &Value) -> Result<Receipt, WireError> {
    let object = as_object(value, &["id"])?;
    Receipt::new(text(object, "id")?).map_err(WireError::Identity)
}
fn decode_failure(value: &Value) -> Result<Failure, WireError> {
    let object = as_object(value, &["step", "description"])?;
    Failure::new(decode_step(&text(object, "step")?)?, text(object, "description")?).map_err(WireError::Identity)
}
fn decode_step(value: &str) -> Result<Step, WireError> {
    match value {
        "delivery" => Ok(Step::Delivery),
        "tracker" => Ok(Step::Tracker),
        "cleanup" => Ok(Step::Cleanup),
        _ => Err(WireError::Malformed("step")),
    }
}
fn decode_wait_kind(value: &str) -> Result<WaitKind, WireError> {
    match value {
        "user" => Ok(WaitKind::User),
        "child" => Ok(WaitKind::Child),
        "ci" => Ok(WaitKind::Ci),
        "approval" => Ok(WaitKind::Approval),
        _ => Err(WireError::Malformed("wait kind")),
    }
}
fn decode_outbox(value: &Value) -> Result<Outbox, WireError> {
    let object = as_object(value, &["delivery", "tracker", "cleanup"])?;
    Ok(Outbox {
        delivery: decode_step_state(field(object, "delivery")?)?,
        tracker: decode_step_state(field(object, "tracker")?)?,
        cleanup: decode_step_state(field(object, "cleanup")?)?,
    })
}
fn decode_step_state(value: &Value) -> Result<StepState, WireError> {
    let object = value.as_object().ok_or(WireError::Malformed("step state"))?;
    match text(object, "kind")?.as_str() {
        "pending" => {
            strict(object, &["kind"])?;
            Ok(StepState::Pending)
        }
        "recorded" => {
            strict(object, &["kind", "receipt"])?;
            Ok(StepState::Recorded(decode_receipt(field(object, "receipt")?)?))
        }
        "failed" => {
            strict(object, &["kind", "failure"])?;
            Ok(StepState::Failed(decode_failure(field(object, "failure")?)?))
        }
        _ => Err(WireError::Malformed("step state kind")),
    }
}
fn decode_outcome(value: &Value) -> Result<Outcome, WireError> {
    let object = as_object(value, &["turn", "report", "receipt", "outbox"])?;
    let receipt_object = as_object(field(object, "receipt")?, &["event"])?;
    Ok(Outcome {
        turn: decode_turn(field(object, "turn")?)?,
        report: super::Report::new(text(object, "report")?).map_err(WireError::Identity)?,
        receipt: ResultReceipt { event: decode_event(field(receipt_object, "event")?)? },
        outbox: decode_outbox(field(object, "outbox")?)?,
    })
}
fn decode_state(value: &Value) -> Result<State, WireError> {
    let object = value.as_object().ok_or(WireError::Malformed("state"))?;
    match text(object, "kind")?.as_str() {
        "inactive" => {
            strict(object, &["kind"])?;
            Ok(State::Inactive)
        }
        "active" => {
            strict(object, &["kind", "turn", "intent"])?;
            let intent = match optional(field(object, "intent")?)? {
                None => None,
                Some(value) => {
                    let intent = as_object(value, &["turn"])?;
                    Some(Intent { turn: decode_turn(field(intent, "turn")?)? })
                }
            };
            Ok(State::Active { turn: decode_turn(field(object, "turn")?)?, intent })
        }
        "waiting" => {
            strict(object, &["kind", "turn", "reason", "report", "finish_requested"])?;
            let reason = as_object(field(object, "reason")?, &["kind", "description"])?;
            Ok(State::Waiting {
                turn: decode_turn(field(object, "turn")?)?,
                reason: WaitReason::new(decode_wait_kind(&text(reason, "kind")?)?, text(reason, "description")?)
                    .map_err(WireError::Identity)?,
                report: optional(field(object, "report")?)?
                    .map(|value| {
                        super::Report::new(value.as_str().ok_or(WireError::Malformed("report"))?)
                            .map_err(WireError::Identity)
                    })
                    .transpose()?,
                finish_requested: boolean(object, "finish_requested")?,
            })
        }
        "attention" => {
            strict(object, &["kind", "reason", "report", "outcome"])?;
            Ok(State::Attention {
                reason: text(object, "reason")?,
                report: optional(field(object, "report")?)?
                    .map(|value| {
                        super::Report::new(value.as_str().ok_or(WireError::Malformed("report"))?)
                            .map_err(WireError::Identity)
                    })
                    .transpose()?,
                outcome: optional(field(object, "outcome")?)?.map(decode_outcome).transpose()?,
            })
        }
        "ready" => {
            strict(object, &["kind", "outcome"])?;
            Ok(State::Ready { outcome: decode_outcome(field(object, "outcome")?)? })
        }
        "closing" => {
            strict(object, &["kind", "revision", "outcome"])?;
            Ok(State::Closing {
                revision: number(object, "revision")?,
                outcome: decode_outcome(field(object, "outcome")?)?,
            })
        }
        "completed" => {
            strict(object, &["kind", "outcome"])?;
            Ok(State::Completed { outcome: decode_outcome(field(object, "outcome")?)? })
        }
        _ => Err(WireError::Malformed("state kind")),
    }
}
fn strict(object: &Map<String, Value>, fields: &[&str]) -> Result<(), WireError> {
    if object.len() != fields.len() || fields.iter().any(|key| !object.contains_key(*key)) {
        Err(WireError::Malformed("unexpected or missing object field"))
    } else {
        Ok(())
    }
}
fn decode_withdrawal(value: &Value) -> Result<Withdrawal, WireError> {
    let object = as_object(value, &["turn", "reason"])?;
    Ok(Withdrawal { turn: decode_turn(field(object, "turn")?)?, reason: text(object, "reason")? })
}
fn decode_archived(value: &Value) -> Result<Archived, WireError> {
    let object = as_object(value, &["outcome", "reason"])?;
    Ok(Archived { outcome: decode_outcome(field(object, "outcome")?)?, reason: text(object, "reason")? })
}
fn decode_applied(value: &Value) -> Result<Applied, WireError> {
    let object = as_object(value, &["id", "operation", "facts"])?;
    let operation = text(object, "operation")?;
    let operation = match operation.as_str() {
        "new_input" => "new_input",
        "request_finish" => "request_finish",
        "wait" => "wait",
        "set_hold" => "set_hold",
        "release_hold" => "release_hold",
        "end_turn" => "end_turn",
        "interrupt" => "interrupt",
        "begin_close" => "begin_close",
        "record_delivery" => "record_delivery",
        "record_tracker" => "record_tracker",
        "record_cleanup" => "record_cleanup",
        "fail" => "fail",
        "complete" => "complete",
        _ => return Err(WireError::Malformed("operation")),
    };
    Ok(Applied { id: decode_event(field(object, "id")?)?, operation, facts: text(object, "facts")? })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::completion::{EventId, Report};

    fn identity() -> Identity {
        Identity::new("launch", "codex:thread").unwrap()
    }
    fn ready() -> Record {
        let mut record = Record::new(identity());
        let turn = Turn::new("turn").unwrap();
        record.new_input(EventId::new("input").unwrap(), turn.clone()).unwrap();
        record.request_finish(EventId::new("finish").unwrap(), &turn).unwrap();
        record.end_turn(EventId::new("end").unwrap(), &turn, Some(Report::new("report").unwrap()), None).unwrap();
        record
    }

    #[test]
    fn malformed_and_unsupported_documents_fail_closed() {
        assert!(matches!(decode(b"{"), Err(WireError::Json(_))));
        let mut document: Value = serde_json::from_slice(&encode(&ready()).unwrap()).unwrap();
        document["schema"] = json!(99);
        assert!(matches!(decode(&serde_json::to_vec(&document).unwrap()), Err(WireError::Schema(99))));
        document["schema"] = json!(1);
        document["extra"] = json!(true);
        assert!(matches!(decode(&serde_json::to_vec(&document).unwrap()), Err(WireError::Malformed(_))));
    }

    #[test]
    fn interrupted_staging_file_never_replaces_a_synchronized_record() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), identity());
        let saved = store
            .transition(|record| {
                *record = ready();
                Ok::<_, std::convert::Infallible>(())
            })
            .unwrap();
        std::fs::write(directory.path().join("completion-launch--codex-thread.json.1.0.tmp"), b"{").unwrap();
        assert_eq!(store.load().unwrap(), Some(saved));
    }

    #[test]
    fn malformed_published_data_and_identity_mismatch_are_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), identity());
        let path = store.path();
        std::fs::write(&path, b"{").unwrap();
        assert!(matches!(
            store.transition(|_| Ok::<_, std::convert::Infallible>(())),
            Err(StoreError::Load(LoadError::Decode(_, _)))
        ));
        let other = Record::new(Identity::new("other", "codex:thread").unwrap());
        std::fs::write(&path, encode(&other).unwrap()).unwrap();
        assert!(matches!(
            store.transition(|_| Ok::<_, std::convert::Infallible>(())),
            Err(StoreError::Load(LoadError::IdentityMismatch))
        ));
        assert_eq!(decode(&std::fs::read(&path).unwrap()).unwrap().identity(), other.identity());
    }
}
