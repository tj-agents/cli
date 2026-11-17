use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::hidden;

/// Only the project's tracker can resolve an unqualified item; complete references are portable.
pub(super) fn reference(value: &str, cwd: &Path) -> Result<String> {
    if value.contains(':') {
        return Ok(value.to_owned());
    }
    let output = hidden("tj-item")
        .arg("--cwd")
        .arg(cwd)
        .args(["reference", value])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .context("could not run tj item reference; pass a full work item reference or URL")?;
    if !output.status.success() {
        bail!("tj item reference {value}: {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    let reply: Value = serde_json::from_slice(&output.stdout).context("tj item reference printed no JSON")?;
    let reference = reply["ref"]
        .as_str()
        .filter(|reference| reference.contains(':') && !reference.chars().any(char::is_whitespace))
        .context("tj item reference did not return a full work item reference or URL")?;
    Ok(reference.to_owned())
}
