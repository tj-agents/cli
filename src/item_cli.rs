use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use serde_json::Value;
use thiserror::Error;

use crate::domain::item;

#[derive(Debug, Error)]
pub(crate) enum ResolveError {
    #[error("could not run tj item reference; pass a full work item reference or URL")]
    Run(#[source] io::Error),
    #[error("tj item reference failed ({status}): {stderr}")]
    Failed { status: ExitStatus, stderr: String },
    #[error("tj item reference printed no JSON")]
    Decode(#[source] serde_json::Error),
    #[error("tj item reference returned no string ref")]
    MissingReference,
    #[error("invalid tj item reference")]
    Reference(#[source] item::ParseError),
}

/// The caller supplies the process adapter; only a project's tracker can resolve a bare id.
pub(crate) fn reference(value: &str, cwd: &Path, mut command: Command) -> Result<item::Reference, ResolveError> {
    if value.contains(':') {
        return item::Reference::parse(value).map_err(ResolveError::Reference);
    }
    let output = command
        .arg("--cwd")
        .arg(cwd)
        .args(["reference", value])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(ResolveError::Run)?;
    if !output.status.success() {
        return Err(ResolveError::Failed {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(ResolveError::Decode)?;
    let reference = reply["ref"].as_str().ok_or(ResolveError::MissingReference)?;
    item::Reference::parse(reference).map_err(ResolveError::Reference)
}
