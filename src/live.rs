use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::{FileExt, TryLockError};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use walkdir::WalkDir;

use crate::paths;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn command(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    pub cwd: PathBuf,
    pub name: String,
    pub model: String,
    pub effort: String,
}

#[derive(Debug, Error)]
pub enum LiveError {
    #[error("cannot find an agent home directory")]
    Home {
        #[source]
        source: anyhow::Error,
    },
    #[error("cannot inspect process {pid}")]
    Process {
        pid: u32,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Deserialize)]
struct ClaudeRegistry {
    pid: Option<u32>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    name: Option<String>,
    #[serde(rename = "procStart")]
    proc_start: Option<String>,
    #[serde(rename = "pidDomain")]
    pid_domain: Option<String>,
    kind: Option<String>,
}

pub fn sessions() -> Result<Vec<Session>, LiveError> {
    let mut found = claude_sessions()?;
    found.extend(codex_sessions()?);
    Ok(found)
}

pub fn dead_claude_sessions() -> Result<Vec<Session>, LiveError> {
    claude_sessions_matching(false)
}

fn claude_sessions() -> Result<Vec<Session>, LiveError> {
    claude_sessions_matching(true)
}

fn claude_sessions_matching(want_live: bool) -> Result<Vec<Session>, LiveError> {
    let dir = paths::claude_home().map_err(|source| LiveError::Home { source })?.join("sessions");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(LiveError::Read { path: dir, source }),
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let Ok(registry) = serde_json::from_str::<ClaudeRegistry>(&text) else { continue };
        let (Some(pid), Some(id), Some(cwd)) = (registry.pid, registry.session_id, registry.cwd) else { continue };
        if registry.kind.as_deref() != Some("interactive") || !is_local_domain(registry.pid_domain.as_deref()) {
            continue;
        }
        if pid_is_alive(pid, registry.proc_start.as_deref())? != want_live {
            continue;
        }
        sessions.push(Session {
            agent: Agent::Claude,
            id,
            cwd,
            name: registry.name.unwrap_or_default(),
            model: String::new(),
            effort: String::new(),
        });
    }
    Ok(sessions)
}

fn is_local_domain(domain: Option<&str>) -> bool {
    let Some(domain) = domain else { return false };
    let platform = if cfg!(windows) { "win32" } else { "linux" };
    let host = std::env::var(if cfg!(windows) { "COMPUTERNAME" } else { "HOSTNAME" }).unwrap_or_default();
    domain.eq_ignore_ascii_case(&format!("{platform}:{host}"))
}

fn pid_is_alive(pid: u32, expected_start: Option<&str>) -> Result<bool, LiveError> {
    let start = process_start(pid)?;
    let Some(start) = start else { return Ok(false) };
    Ok(expected_start.and_then(|value| value.parse::<u64>().ok()).is_none_or(|expected| expected == start))
}

#[cfg(windows)]
fn process_start(pid: u32) -> Result<Option<u64>, LiveError> {
    use winsafe::{FILETIME, HPROCESS, co, prelude::*};

    let process = match HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, pid) {
        Ok(process) => process,
        Err(error) if error == co::ERROR::INVALID_PARAMETER => return Ok(None),
        Err(error) => return Err(LiveError::Process { pid, source: std::io::Error::other(error.to_string()) }),
    };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    process
        .GetProcessTimes(&mut created, &mut exited, &mut kernel, &mut user)
        .map_err(|error| LiveError::Process { pid, source: std::io::Error::other(error.to_string()) })?;
    Ok(Some(u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32)))
}

#[cfg(not(windows))]
fn process_start(pid: u32) -> Result<Option<u64>, LiveError> {
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LiveError::Read { path, source }),
    };
    let Some(close) = text.rfind(')') else { return Ok(None) };
    Ok(text[close + 1..].split_whitespace().nth(19).and_then(|field| field.parse().ok()))
}

fn codex_sessions() -> Result<Vec<Session>, LiveError> {
    let home = paths::codex_home().map_err(|source| LiveError::Home { source })?;
    let locks = home.join("thread-writer-locks");
    let entries = match fs::read_dir(&locks) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(LiveError::Read { path: locks, source }),
    };
    let metadata = codex_metadata(&home);
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = lock_id(&path) else { continue };
        if !is_locked(&path) {
            continue;
        }
        let Some(session) = metadata.get(&id) else { continue };
        sessions.push(session.clone());
    }
    Ok(sessions)
}

fn lock_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_suffix(".lock").filter(|id| !id.is_empty() && !id.starts_with('.')).map(str::to_string)
}

fn is_locked(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else { return false };
    matches!(FileExt::try_lock(&file), Err(TryLockError::WouldBlock))
}

fn codex_metadata(home: &Path) -> std::collections::HashMap<String, Session> {
    WalkDir::new(home.join("sessions"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|entry| codex_meta(entry.path()))
        .map(|session| (session.id.clone(), session))
        .collect()
}

fn codex_meta(path: &Path) -> Option<Session> {
    let line = fs::read_to_string(path).ok()?.lines().next()?.to_string();
    let value = serde_json::from_str::<Value>(&line).ok()?;
    codex_meta_value(&value)
}

fn codex_meta_value(value: &Value) -> Option<Session> {
    let payload = value.get("payload")?;
    let id = payload.get("session_id")?.as_str()?.to_string();
    let cwd = payload.get("cwd")?.as_str().map(PathBuf::from)?;
    if payload.pointer("/source/subagent").is_some() {
        return None;
    }
    Some(Session {
        agent: Agent::Codex,
        id,
        cwd,
        name: payload.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
        model: payload.get("model").and_then(Value::as_str).unwrap_or_default().to_string(),
        effort: payload.get("model_reasoning_effort").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_the_coordination_lock() {
        assert_eq!(lock_id(Path::new(".coordination.lock")), None);
        assert_eq!(lock_id(Path::new("thread-1.lock")), Some("thread-1".into()));
    }

    #[test]
    fn parses_a_claude_registry_fixture() {
        let registry = serde_json::from_str::<ClaudeRegistry>(include_str!("../tests/fixtures/claude-registry.json"))
            .expect("Claude registry fixture");
        assert_eq!(registry.session_id.as_deref(), Some("claude-session-id"));
        assert_eq!(registry.name.as_deref(), Some("Recover this work"));
    }

    #[test]
    fn parses_a_codex_rollout_fixture() {
        let value = serde_json::from_str::<Value>(include_str!("../tests/fixtures/codex-rollout.jsonl"))
            .expect("Codex rollout fixture");
        let session = codex_meta_value(&value).expect("Codex session metadata");
        assert_eq!(session.id, "codex-thread-id");
        assert_eq!(session.model, "gpt-5.3-codex");
        assert_eq!(session.effort, "xhigh");
    }
}
