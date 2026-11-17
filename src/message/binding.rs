//! Bind a request to one recipient and transport before either transport can dispatch it.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub(super) enum BindError {
    #[error("could not create the message binding folder")]
    Folder(#[source] io::Error),
    #[error("could not open the message binding journal")]
    Open(#[source] io::Error),
    #[error("could not lock the message binding journal")]
    Lock(#[source] io::Error),
    #[error("could not read the message binding journal")]
    Read(#[source] io::Error),
    #[error("message binding journal is incomplete; the original request cannot be verified")]
    Incomplete,
    #[error("message binding journal is not valid JSON")]
    Json(#[source] serde_json::Error),
    #[error("message binding journal has an unsupported or malformed record")]
    Malformed,
    #[error("message retry changes the recorded recipient, transport, content or kind")]
    Conflict,
    #[error("could not append to the message binding journal")]
    Append(#[source] io::Error),
    #[error("could not synchronize the message binding journal")]
    Sync(#[source] io::Error),
}

fn validate(record: &Value) -> Result<(), BindError> {
    let fields = ["schema", "source", "event", "target", "transport", "text", "kind"];
    let Some(object) = record.as_object() else { return Err(BindError::Malformed) };
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || record["schema"] != 1
        || !(record["source"].is_null() || record["source"].is_string())
        || record["target"].as_str().is_none_or(str::is_empty)
        || !matches!(record["transport"].as_str(), Some("feed" | "native"))
        || !record["text"].is_string()
        || record["kind"].as_str().is_none_or(str::is_empty)
    {
        return Err(BindError::Malformed);
    }
    Ok(())
}

/// The lock covers the binding only; transport submission retains its own durable deduplication.
pub(super) fn bind(root: &Path, request: &Value) -> Result<(), BindError> {
    validate(request)?;
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
    directory.create(root).map_err(BindError::Folder)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(root.join("bindings.jsonl")).map_err(BindError::Open)?;
    fs4::FileExt::lock(&file).map_err(BindError::Lock)?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(BindError::Read)?;
    // Never discard a partial binding: its original recipient or transport could be different.
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(BindError::Incomplete);
    }
    let mut found = false;
    for line in text.lines() {
        let previous: Value = serde_json::from_str(line).map_err(BindError::Json)?;
        validate(&previous)?;
        if previous["source"] == request["source"] && previous["event"] == request["event"] {
            if previous != *request {
                return Err(BindError::Conflict);
            }
            found = true;
        }
    }
    if !found {
        file.seek(SeekFrom::End(0)).map_err(BindError::Append)?;
        writeln!(file, "{request}").map_err(BindError::Append)?;
    }
    // Also sync a replay: a prior process could have exited after writing the complete line but
    // before synchronizing it. No caller may dispatch until this binding is durable.
    file.sync_all().map_err(BindError::Sync)?;
    #[cfg(unix)]
    std::fs::File::open(root).and_then(|directory| directory.sync_all()).map_err(BindError::Sync)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request() -> Value {
        json!({
            "schema":1, "source":"codex:source", "event":{"kind":"result","turn":"one"},
            "target":"codex:target", "transport":"feed", "text":"private result", "kind":"result",
        })
    }

    #[test]
    fn a_binding_survives_reopen_before_dispatch_and_binds_content_kind_and_transport() {
        let root = tempfile::tempdir().unwrap();
        let original = request();
        bind(root.path(), &original).unwrap();
        bind(root.path(), &original).unwrap();
        for (field, value) in
            [("target", "claude:other"), ("transport", "native"), ("text", "changed"), ("kind", "message")]
        {
            let mut changed = original.clone();
            changed[field] = json!(value);
            assert!(matches!(bind(root.path(), &changed), Err(BindError::Conflict)), "{field}");
        }
        assert_eq!(std::fs::read_to_string(root.path().join("bindings.jsonl")).unwrap().lines().count(), 1);
    }

    #[test]
    fn sources_and_complete_event_identities_have_independent_bindings() {
        let root = tempfile::tempdir().unwrap();
        let original = request();
        bind(root.path(), &original).unwrap();
        let mut other_source = original.clone();
        other_source["source"] = json!("codex:another");
        other_source["target"] = json!("claude:other");
        bind(root.path(), &other_source).unwrap();
        let mut other_event = original;
        other_event["event"]["turn"] = json!("two");
        other_event["text"] = json!("next result");
        bind(root.path(), &other_event).unwrap();
        assert_eq!(std::fs::read_to_string(root.path().join("bindings.jsonl")).unwrap().lines().count(), 3);
    }

    #[test]
    fn concurrent_conflicting_requests_bind_only_one_recipient() {
        let root = tempfile::tempdir().unwrap();
        let start = std::sync::Barrier::new(2);
        let results = std::thread::scope(|threads| {
            let workers = ["codex:first", "claude:second"].map(|target| {
                let start = &start;
                let root = root.path();
                threads.spawn(move || {
                    let mut request = request();
                    request["target"] = json!(target);
                    start.wait();
                    bind(root, &request)
                })
            });
            workers.map(|worker| worker.join().unwrap())
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| matches!(result, Err(BindError::Conflict))).count(), 1);
        assert_eq!(std::fs::read_to_string(root.path().join("bindings.jsonl")).unwrap().lines().count(), 1);
    }
}
