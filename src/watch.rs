use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use clap::Args as ClapArgs;
use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{live, paths};

const POLL_INTERVAL: Duration = Duration::from_secs(10);

#[derive(ClapArgs)]
pub struct WatchArgs {}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub boot: u64,
    pub tick: u64,
    pub abrupt_stop: Option<AbruptStop>,
    pub records: Vec<Record>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AbruptStop {
    pub boot: u64,
    pub final_tick: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Record {
    pub session: live::Session,
    pub first_seen_boot: u64,
    pub first_seen_tick: u64,
    pub last_seen_boot: u64,
    pub last_seen_tick: u64,
    pub ended_boot: Option<u64>,
    pub ended_tick: Option<u64>,
    #[serde(default)]
    pub missing_polls: u8,
}

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("cannot determine the tj state directory: {source}")]
    StateDirectory { source: anyhow::Error },
    #[error("cannot create the tj state directory at {path}: {source}")]
    CreateStateDirectory { path: PathBuf, source: io::Error },
    #[error("cannot open watcher lock at {path}: {source}")]
    OpenLock { path: PathBuf, source: io::Error },
    #[error("tj watch is already running")]
    AlreadyRunning,
    #[error("cannot lock watcher state: {source}")]
    Lock { source: io::Error },
    #[error("cannot read watcher state at {path}: {source}")]
    ReadState { path: PathBuf, source: io::Error },
    #[error("cannot parse watcher state at {path}: {source}")]
    ParseState {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot write watcher state at {path}: {source}")]
    WriteState { path: PathBuf, source: io::Error },
    #[error("cannot serialize watcher state: {source}")]
    SerializeState { source: serde_json::Error },
    #[error(transparent)]
    Live(#[from] live::LiveError),
    #[error("cannot determine the system boot marker: {source}")]
    BootMarker { source: io::Error },
}

pub fn run(_args: WatchArgs) -> anyhow::Result<std::process::ExitCode> {
    run_forever()?;
    Ok(std::process::ExitCode::SUCCESS)
}

pub fn run_forever() -> Result<(), WatchError> {
    let state_directory = paths::state_dir().map_err(|source| WatchError::StateDirectory { source })?;
    fs::create_dir_all(&state_directory)
        .map_err(|source| WatchError::CreateStateDirectory { path: state_directory.clone(), source })?;

    let _lock = lock(&state_directory.join("watch.lock"))?;
    let state_path = state_directory.join("watch.json");

    loop {
        let boot = boot_marker()?;
        let sessions = live::sessions()?;
        let mut state = load(&state_path)?;
        capture(&mut state, boot, sessions);
        save(&state_path, &state)?;
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn load_current() -> Result<Option<State>, WatchError> {
    let state_directory = paths::state_dir().map_err(|source| WatchError::StateDirectory { source })?;
    let path = state_directory.join("watch.json");
    match path.try_exists().map_err(|source| WatchError::ReadState { path: path.clone(), source })? {
        true => load(&path).map(Some),
        false => Ok(None),
    }
}

fn lock(path: &Path) -> Result<File, WatchError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|source| WatchError::OpenLock { path: path.to_path_buf(), source })?;

    match FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(WatchError::AlreadyRunning),
        Err(TryLockError::Error(source)) => Err(WatchError::Lock { source }),
    }
}

fn load(path: &Path) -> Result<State, WatchError> {
    match fs::read(path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|source| WatchError::ParseState { path: path.to_path_buf(), source }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(source) => Err(WatchError::ReadState { path: path.to_path_buf(), source }),
    }
}

fn save(path: &Path, state: &State) -> Result<(), WatchError> {
    let contents = serde_json::to_vec(state).map_err(|source| WatchError::SerializeState { source })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, contents).map_err(|source| WatchError::WriteState { path: temporary.clone(), source })?;
    fs::rename(&temporary, path).map_err(|source| WatchError::WriteState { path: path.to_path_buf(), source })
}

pub fn capture(state: &mut State, boot: u64, sessions: Vec<live::Session>) {
    if state.boot != 0 && state.boot != boot {
        state.abrupt_stop = Some(AbruptStop { boot: state.boot, final_tick: state.tick });
        state.tick = 0;
    }

    state.boot = boot;
    state.tick += 1;

    for session in &sessions {
        if let Some(record) = state
            .records
            .iter_mut()
            .find(|record| record.session.agent == session.agent && record.session.id == session.id)
        {
            record.session = session.clone();
            record.last_seen_boot = boot;
            record.last_seen_tick = state.tick;
            record.ended_boot = None;
            record.ended_tick = None;
            record.missing_polls = 0;
        } else {
            state.records.push(Record {
                session: session.clone(),
                first_seen_boot: boot,
                first_seen_tick: state.tick,
                last_seen_boot: boot,
                last_seen_tick: state.tick,
                ended_boot: None,
                ended_tick: None,
                missing_polls: 0,
            });
        }
    }

    for record in &mut state.records {
        if record.last_seen_boot == boot && record.last_seen_tick != state.tick {
            record.missing_polls = record.missing_polls.saturating_add(1);
            if record.missing_polls >= 2 && record.ended_tick.is_none() {
                record.ended_boot = Some(boot);
                record.ended_tick = Some(state.tick);
            }
        }
    }
}

fn boot_marker() -> Result<u64, WatchError> {
    #[cfg(target_os = "linux")]
    {
        let contents = fs::read_to_string("/proc/stat").map_err(|source| WatchError::BootMarker { source })?;
        let marker = contents
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| WatchError::BootMarker {
                source: io::Error::new(io::ErrorKind::InvalidData, "missing btime in /proc/stat"),
            })?;
        return Ok(marker);
    }

    #[cfg(windows)]
    {
        return Ok(winsafe::GetTickCount64() / 1_000);
    }

    #[allow(unreachable_code)]
    Err(WatchError::BootMarker { source: io::Error::new(io::ErrorKind::Unsupported, "unsupported operating system") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> live::Session {
        live::Session {
            agent: live::Agent::Claude,
            id: id.to_owned(),
            cwd: PathBuf::from("C:/work"),
            name: String::new(),
            model: String::new(),
            effort: String::new(),
        }
    }

    #[test]
    fn records_a_clean_exit_after_two_absent_polls() {
        let mut state = State::default();
        capture(&mut state, 1, vec![session("one")]);
        capture(&mut state, 1, Vec::new());
        assert_eq!(state.records[0].ended_tick, None);
        capture(&mut state, 1, Vec::new());
        assert_eq!(state.records[0].ended_tick, Some(3));
    }

    #[test]
    fn marks_an_abrupt_stop_when_the_boot_marker_changes() {
        let mut state = State::default();
        capture(&mut state, 1, vec![session("one")]);
        capture(&mut state, 2, Vec::new());
        assert_eq!(state.abrupt_stop.as_ref().map(|stop| stop.boot), Some(1));
    }
}
