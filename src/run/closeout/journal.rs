use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Launch, Plan, Removal};

pub(super) struct Journal {
    pub(super) session: String,
    pub(super) repository: PathBuf,
    pub(super) removals: Vec<Removal>,
}

fn path(launch: &Launch) -> Result<PathBuf> {
    Ok(super::super::marks(&launch.id)?.with_extension("closeout.json"))
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field].as_str().filter(|value| !value.is_empty()).with_context(|| format!("cleanup journal has no {field}"))
}

fn absolute(value: &Value, field: &str) -> Result<PathBuf> {
    let path = PathBuf::from(text(value, field)?);
    if !path.is_absolute() {
        bail!("cleanup journal {field} is not absolute");
    }
    Ok(path)
}

pub(super) fn read(launch: &Launch) -> Result<Option<Journal>> {
    let bytes = match std::fs::read(path(launch)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read cleanup journal"),
    };
    let value: Value = serde_json::from_slice(&bytes).context("invalid cleanup journal")?;
    if value["schema"] != 1 || text(&value, "launch")? != launch.id || text(&value, "host")? != launch.host.name() {
        bail!("cleanup journal does not match launch");
    }
    let removals = value["removals"]
        .as_array()
        .context("cleanup journal has no removals")?
        .iter()
        .map(|row| {
            Ok(Removal {
                primary: absolute(row, "primary")?,
                worktree: absolute(row, "worktree")?,
                registration: absolute(row, "registration")?,
                branch: text(row, "branch")?.to_owned(),
                head: text(row, "head")?.to_owned(),
                default: text(row, "default")?.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(Journal {
        session: text(&value, "session")?.to_owned(),
        repository: absolute(&value, "repository")?,
        removals,
    }))
}

pub(super) fn write(launch: &Launch, plan: &Plan) -> Result<()> {
    let path = path(launch)?;
    std::fs::create_dir_all(path.parent().context("cleanup journal has no parent")?)?;
    let removals: Vec<_> = plan
        .removals
        .iter()
        .map(|removal| {
            json!({
                "primary": removal.primary, "worktree": removal.worktree, "registration": removal.registration,
                "branch": removal.branch, "head": removal.head, "default": removal.default
            })
        })
        .collect();
    let document = json!({
        "schema": 1, "launch": launch.id, "host": launch.host.name(), "session": plan.owner.session,
        "repository": plan.repository, "removals": removals
    });
    let temporary = path.with_extension(format!("{}.tmp", super::super::generated_id()?));
    let mut file = std::fs::File::create_new(&temporary).context("cannot create cleanup journal")?;
    file.write_all(&serde_json::to_vec(&document)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, &path).context("cannot commit cleanup journal")
}

pub(super) fn clear(launch: &Launch) -> Result<()> {
    std::fs::remove_file(path(launch)?).context("cannot clear completed cleanup journal")
}
