//! A durable queue consumed inside the sending host. No provider protocol or credentials live here.

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
    result: String,
}

struct Queue {
    root: PathBuf,
    session: String,
}

impl Queue {
    fn open(root: &Path, session: &str) -> Result<Self> {
        Identity::new("message", session)?;
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
            bail!("native message log is incomplete; delivery is unknown and must not be retried automatically");
        }
        let records = text.lines().map(serde_json::from_str).collect::<Result<Vec<Value>, _>>()?;
        update(&mut file, &records)
    }

    fn enqueue(&self, key: &Value, target: &str, text: &str) -> Result<String> {
        self.edit(|file, records| {
            if let Some(request) = records.iter().find(|row| row["kind"] == "request" && row["key"] == *key) {
                if request["target"] != target || request["text"] != text {
                    bail!("message retry changes the original recipient or content");
                }
                return Ok(field(request, "id")?.to_owned());
            }
            if !self.live()? {
                bail!("native message adapter is unavailable in {}; reload Agent in that session", self.session);
            }
            let id = crate::run::generated_id()?;
            append(
                file,
                &json!({
                    "kind":"request", "id":id, "key":key, "session":self.session,
                    "target":target, "text":text,
                }),
            )?;
            Ok(id)
        })
    }

    fn take(&self, lease: &str) -> Result<Option<Value>> {
        self.edit(|file, records| {
            let claimed: HashMap<_, _> =
                records.iter().filter(|row| row["kind"] == "claimed").map(|row| (row["id"].clone(), row)).collect();
            let request = records.iter().find(|row| row["kind"] == "request" && !claimed.contains_key(&row["id"]));
            let Some(request) = request else { return Ok(None) };
            append(file, &json!({"kind":"claimed", "id":request["id"], "lease":lease}))?;
            Ok(Some(request.clone()))
        })
    }

    fn result(&self, id: &str) -> Result<Option<Value>> {
        self.edit(|_, records| {
            Ok(records.iter().find(|row| row["kind"] == "receipt" && row["id"] == id).map(|row| row["result"].clone()))
        })
    }

    fn receipt(&self, id: &str, result: &Value) -> Result<()> {
        self.edit(|file, records| {
            if !records.iter().any(|row| row["kind"] == "claimed" && row["id"] == id) {
                bail!("message receipt has no claimed request");
            }
            if let Some(previous) = records.iter().find(|row| row["kind"] == "receipt" && row["id"] == id) {
                if previous["result"] != *result {
                    bail!("message receipt conflicts with its recorded result");
                }
                return Ok(());
            }
            append(file, &json!({"kind":"receipt", "id":id, "result":result}))
        })
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().filter(|value| !value.is_empty()).with_context(|| format!("message is missing {name}"))
}

fn append(file: &mut File, value: &Value) -> Result<()> {
    file.seek(SeekFrom::End(0))?;
    writeln!(file, "{value}")?;
    file.sync_data()?;
    Ok(())
}

fn queue(session: &str) -> Result<Queue> {
    Queue::open(&paths::state_dir()?.join("messages"), session)
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
    fs4::FileExt::try_lock(&lease).context("a native message adapter already owns this session")?;
    let identity = crate::run::generated_id()?;
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
    if result["isDelivered"].as_bool().is_none() && result["error"].as_str().is_none() {
        bail!("native message result has no submission outcome");
    }
    queue(&args.session)?.receipt(&args.request, &result)?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn send(session: &str, target: &str, text: &str, key: &Value) -> Result<Value> {
    let queue = queue(session)?;
    let id = queue.enqueue(key, target, text)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claimed_message_is_never_resent_after_the_adapter_restarts() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let id = queue.enqueue(&json!("stable"), "claude:target", "result").unwrap();
        assert_eq!(queue.take("first").unwrap().unwrap()["id"], id);
        assert!(queue.take("after-crash").unwrap().is_none());
        assert_eq!(queue.enqueue(&json!("stable"), "claude:target", "result").unwrap(), id);
        assert!(queue.enqueue(&json!("stable"), "claude:other", "result").is_err());
    }

    #[test]
    fn receipts_replay_exactly_and_refusals_do_not_requeue() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        let lease = queue.file("host.lock").unwrap();
        fs4::FileExt::lock(&lease).unwrap();
        let id = queue.enqueue(&json!("once"), "claude:target", "text").unwrap();
        let refusal = json!({"isDelivered":false,"reason":"native permission refusal"});
        assert!(queue.receipt(&id, &refusal).is_err());
        queue.take("bridge").unwrap();
        queue.receipt(&id, &refusal).unwrap();
        queue.receipt(&id, &refusal).unwrap();
        assert_eq!(queue.result(&id).unwrap(), Some(refusal));
        assert!(queue.receipt(&id, &json!({"isDelivered":true})).is_err());
        assert!(queue.take("bridge").unwrap().is_none());
    }

    #[test]
    fn missing_adapter_and_torn_receipts_never_authorize_a_send() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(root.path(), "claude:source").unwrap();
        assert!(queue.enqueue(&json!("new"), "claude:target", "text").is_err());
        std::fs::write(queue.root.join("requests.jsonl"), "{\"kind\":\"receipt\"").unwrap();
        assert!(queue.take("new-adapter").is_err());
    }
}
