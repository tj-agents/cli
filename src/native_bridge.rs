//! Durable native requests consumed inside their owning host.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{completion::Identity, host::Host, paths};

#[derive(clap::Args)]
pub(crate) struct HostArgs {
    #[arg(long)]
    session: String,
}

#[derive(clap::Args)]
pub(crate) struct ReceiptArgs {
    #[arg(long)]
    session: String,
    #[arg(long)]
    request: String,
    #[arg(long)]
    lease: String,
    #[arg(long)]
    result: String,
}

struct Queue {
    root: PathBuf,
    session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Message { target: String, text: String },
    ReloadPlugins { revision: String },
}

impl Action {
    fn decode(value: &Value) -> Result<Self> {
        match value["operation"].as_str() {
            Some("message") => {
                Ok(Self::Message { target: field(value, "target")?.to_owned(), text: field(value, "text")?.to_owned() })
            }
            Some("reload_plugins") => Ok(Self::ReloadPlugins { revision: field(value, "revision")?.to_owned() }),
            _ => bail!("unknown native operation"),
        }
    }

    fn encode(&self) -> Value {
        match self {
            Self::Message { target, text } => json!({"operation":"message", "target":target, "text":text}),
            Self::ReloadPlugins { revision } => json!({"operation":"reload_plugins", "revision":revision}),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReloadError {
    #[error("could not access the native reload request")]
    Queue(#[source] anyhow::Error),
    #[error("native reload failed: {0}")]
    Failed(String),
    #[error("native reload has no completion receipt; activation remains unverified")]
    Unverified,
    #[error("native reload returned an invalid completion receipt")]
    InvalidReceipt,
}

impl Queue {
    fn open(root: &Path, session: &str) -> Result<Self> {
        Identity::new("native", session)?;
        let root = root.join(session.replace(':', "-"));
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        directory.create(&root)?;
        Ok(Self { root, session: session.to_owned() })
    }

    fn file(&self, name: &str) -> Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        Ok(options.open(self.root.join(name))?)
    }

    fn live(&self) -> Result<bool> {
        let lease = self.file("host.lock")?;
        match fs4::FileExt::try_lock(&lease) {
            Ok(()) => Ok(false),
            Err(fs4::TryLockError::WouldBlock) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    fn edit<T>(&self, update: impl FnOnce(&mut File, &[Value]) -> Result<T>) -> Result<T> {
        let mut file = self.file("requests.jsonl")?;
        fs4::FileExt::lock(&file)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        // A torn receipt could follow a successful native send. Never discard it and resend.
        if !text.is_empty() && !text.ends_with('\n') {
            bail!("native request log is incomplete; execution is unknown and must not be retried automatically");
        }
        let records = text.lines().map(serde_json::from_str).collect::<Result<Vec<Value>, _>>()?;
        update(&mut file, &records)
    }

    fn enqueue(&self, key: &Value, action: &Action) -> Result<String> {
        self.edit(|file, records| {
            if let Some(request) = records.iter().find(|row| row["kind"] == "request" && row["key"] == *key) {
                if Action::decode(request)? != *action {
                    bail!("native retry changes the original operation or content");
                }
                return Ok(field(request, "id")?.to_owned());
            }
            if !self.live()? {
                bail!("native adapter is unavailable in {}; reload Agent in that session", self.session);
            }
            let id = crate::run::generated_id()?;
            let mut request = action.encode();
            request["kind"] = json!("request");
            request["id"] = json!(id);
            request["key"] = key.clone();
            request["session"] = json!(self.session);
            append(file, &request)?;
            Ok(id)
        })
    }

    fn take(&self, lease: &str) -> Result<Option<Value>> {
        self.edit(|file, records| {
            let claimed: HashMap<_, _> =
                records.iter().filter(|row| row["kind"] == "claimed").map(|row| (row["id"].clone(), row)).collect();
            let request = records.iter().find(|row| {
                if row["kind"] != "request"
                    || records.iter().any(|receipt| receipt["kind"] == "receipt" && receipt["id"] == row["id"])
                {
                    return false;
                }
                match claimed.get(&row["id"]) {
                    None => true,
                    Some(previous) => {
                        row["operation"] == "reload_plugins"
                            && previous["lease"] != lease
                            && records
                                .iter()
                                .filter(|claim| claim["kind"] == "claimed" && claim["id"] == row["id"])
                                .count()
                                < 2
                    }
                }
            });
            let Some(request) = request else { return Ok(None) };
            append(file, &json!({"kind":"claimed", "id":request["id"], "lease":lease}))?;
            let mut request = request.clone();
            request["lease"] = json!(lease);
            Ok(Some(request))
        })
    }

    fn result(&self, id: &str) -> Result<Option<Value>> {
        self.edit(|_, records| {
            Ok(records.iter().find(|row| row["kind"] == "receipt" && row["id"] == id).map(|row| row["result"].clone()))
        })
    }

    fn receipt(&self, id: &str, lease: &str, result: &Value) -> Result<()> {
        self.edit(|file, records| {
            let request = records
                .iter()
                .find(|row| row["kind"] == "request" && row["id"] == id)
                .context("native receipt has no request")?;
            match Action::decode(request)? {
                Action::Message { .. }
                    if result["isDelivered"].as_bool().is_some() || result["error"].as_str().is_some() => {}
                Action::ReloadPlugins { .. } if result["completed"] == true || result["error"].as_str().is_some() => {}
                _ => bail!("native receipt does not match its requested operation"),
            }
            let claimed = records
                .iter()
                .rev()
                .find(|row| row["kind"] == "claimed" && row["id"] == id)
                .context("native receipt has no claimed request")?;
            if claimed["lease"] != lease {
                bail!("native receipt belongs to a superseded adapter lease");
            }
            if let Some(previous) = records.iter().find(|row| row["kind"] == "receipt" && row["id"] == id) {
                if previous["result"] != *result {
                    bail!("native receipt conflicts with its recorded result");
                }
                return Ok(());
            }
            append(file, &json!({"kind":"receipt", "id":id, "result":result}))
        })
    }

    fn exhausted_reload(&self, id: &str) -> Result<bool> {
        self.edit(|_, records| {
            let claims: Vec<_> = records.iter().filter(|row| row["kind"] == "claimed" && row["id"] == id).collect();
            let host = records.iter().rev().find(|row| row["kind"] == "host");
            Ok(claims.len() >= 2
                && host.is_some_and(|host| claims.last().is_some_and(|claim| claim["lease"] != host["lease"])))
        })
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().filter(|value| !value.is_empty()).with_context(|| format!("native request is missing {name}"))
}

fn append(file: &mut File, value: &Value) -> Result<()> {
    file.seek(SeekFrom::End(0))?;
    writeln!(file, "{value}")?;
    file.sync_data()?;
    Ok(())
}

fn queue(session: &str) -> Result<Queue> {
    Queue::open(&paths::state_dir()?.join("native"), session)
}

fn source(session: &str) -> Result<()> {
    if Host::current().and_then(Host::handle).as_deref() != Some(session) {
        bail!("native adapter must run in its own source session");
    }
    Ok(())
}

pub(crate) fn host(args: HostArgs) -> Result<ExitCode> {
    source(&args.session)?;
    let queue = queue(&args.session)?;
    let lease = queue.file("host.lock")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs4::FileExt::try_lock(&lease) {
            Ok(()) => break,
            Err(fs4::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(error) => return Err(error).context("another native adapter still owns this session"),
        }
    }
    let identity = crate::run::generated_id()?;
    queue.edit(|file, _| append(file, &json!({"kind":"host", "lease":identity})))?;
    let mut stdout = std::io::stdout().lock();
    loop {
        if let Some(request) = queue.take(&identity)? {
            writeln!(stdout, "{request}")?;
            stdout.flush()?;
        } else {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub(crate) fn receipt(args: ReceiptArgs) -> Result<ExitCode> {
    source(&args.session)?;
    let result: Value = serde_json::from_str(&args.result)?;
    queue(&args.session)?.receipt(&args.request, &args.lease, &result)?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn send(session: &str, target: &str, text: &str, key: &Value) -> Result<Value> {
    let queue = queue(session)?;
    let id = queue.enqueue(key, &Action::Message { target: target.to_owned(), text: text.to_owned() })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = queue.result(&id)? {
            return Ok(json!({
                "transport":"native", "request":id,
                "state":if result["isDelivered"] == true { "submitted" }
                    else if result["isDelivered"] == false { "not-delivered" } else { "unknown" },
                "native":result,
            }));
        }
        if Instant::now() >= deadline || !queue.live()? {
            bail!("native message {id} has no receipt; delivery is unknown, do not resend or switch transports");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn reload(session: &str, revision: &str) -> Result<bool, ReloadError> {
    let queue = queue(session).map_err(ReloadError::Queue)?;
    let id = queue
        .enqueue(
            &json!({"kind":"reload_plugins", "revision":revision}),
            &Action::ReloadPlugins { revision: revision.to_owned() },
        )
        .map_err(ReloadError::Queue)?;
    match queue.result(&id).map_err(ReloadError::Queue)? {
        Some(result) if result["error"].as_str().is_some() => {
            Err(ReloadError::Failed(field(&result, "error").map_err(ReloadError::Queue)?.to_owned()))
        }
        Some(result) if result["completed"] == true => Ok(true),
        Some(_) => Err(ReloadError::InvalidReceipt),
        None if queue.live().map_err(ReloadError::Queue)?
            && !queue.exhausted_reload(&id).map_err(ReloadError::Queue)? =>
        {
            Ok(false)
        }
        None => Err(ReloadError::Unverified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(target: &str, text: &str) -> Action {
        Action::Message { target: target.to_owned(), text: text.to_owned() }
    }

    #[test]
    fn a_claimed_message_is_never_resent_after_the_adapter_restarts() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let id = queue.enqueue(&json!("stable"), &message("claude:target", "result")).unwrap();
        assert_eq!(queue.take("first").unwrap().unwrap()["id"], id);
        assert!(queue.take("after-crash").unwrap().is_none());
        assert_eq!(queue.enqueue(&json!("stable"), &message("claude:target", "result")).unwrap(), id);
        assert!(queue.enqueue(&json!("stable"), &message("claude:other", "result")).is_err());
    }

    #[test]
    fn receipts_replay_exactly_and_refusals_do_not_requeue() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let id = queue.enqueue(&json!("once"), &message("claude:target", "text")).unwrap();
        let refusal = json!({"isDelivered":false,"reason":"native permission refusal"});
        assert!(queue.receipt(&id, "bridge", &refusal).is_err());
        queue.take("bridge").unwrap();
        queue.receipt(&id, "bridge", &refusal).unwrap();
        queue.receipt(&id, "bridge", &refusal).unwrap();
        assert_eq!(queue.result(&id).unwrap(), Some(refusal));
        assert!(queue.receipt(&id, "bridge", &json!({"isDelivered":true})).is_err());
        assert!(queue.take("bridge").unwrap().is_none());
    }

    #[test]
    fn missing_adapter_and_torn_receipts_never_authorize_a_send() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        assert!(queue.enqueue(&json!("new"), &message("claude:target", "text")).is_err());
        std::fs::write(queue.root.join("requests.jsonl"), "{\"kind\":\"receipt\"").unwrap();
        assert!(queue.take("new-adapter").is_err());
    }

    #[test]
    fn reload_receipts_cannot_be_confused_with_message_delivery() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let action = Action::ReloadPlugins { revision: "build\npublication".to_owned() };
        let id = queue.enqueue(&json!("reload"), &action).unwrap();
        let request = queue.take("owner").unwrap().unwrap();
        assert_eq!(request["operation"], "reload_plugins");
        assert!(queue.receipt(&id, "owner", &json!({"isDelivered":true})).is_err());
        assert!(queue.receipt(&id, "owner", &json!({"completed":false})).is_err());
        queue.receipt(&id, "owner", &json!({"completed":true, "output":{"text":"native result"}})).unwrap();
        assert_eq!(queue.result(&id).unwrap().unwrap()["completed"], true);
        assert_eq!(queue.enqueue(&json!("reload"), &action).unwrap(), id);
        assert!(queue.take("owner").unwrap().is_none());
        assert!(queue.enqueue(&json!("reload"), &message("claude:peer", "text")).is_err());
    }

    #[test]
    fn replacing_an_adapter_can_confirm_one_reload_without_resending_messages() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let sent = queue.enqueue(&json!("message"), &message("claude:target", "once")).unwrap();
        let reload = queue.enqueue(&json!("reload"), &Action::ReloadPlugins { revision: "build".to_owned() }).unwrap();
        assert_eq!(queue.take("old").unwrap().unwrap()["id"], sent);
        assert_eq!(queue.take("old").unwrap().unwrap()["id"], reload);
        assert!(queue.take("old").unwrap().is_none());
        let confirmation = queue.take("new").unwrap().unwrap();
        assert_eq!(confirmation["id"], reload);
        assert_eq!(confirmation["lease"], "new");
        queue.edit(|file, _| append(file, &json!({"kind":"host", "lease":"third"}))).unwrap();
        assert!(queue.exhausted_reload(&reload).unwrap());
        assert!(queue.take("third").unwrap().is_none(), "reload reconciliation must be bounded");
        assert!(queue.receipt(&reload, "old", &json!({"completed":true})).is_err());
        queue.receipt(&reload, "new", &json!({"completed":true})).unwrap();
        queue.receipt(&sent, "old", &json!({"isDelivered":true})).unwrap();
        assert!(queue.take("third").unwrap().is_none());
    }
}
