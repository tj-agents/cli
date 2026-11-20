use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crate::host::Host;
use crate::paths;

pub(crate) const MARKETPLACE: &str = "https://api.github.com/repos/tj-agents/marketplace/commits/main";
const CHECKED_FOR: Duration = Duration::from_secs(120);
const LOCK_STALE: Duration = Duration::from_secs(30);
const RETRY_AFTER: Duration = Duration::from_secs(600);
const TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Unchanged,
    Changed(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckError {
    #[error("could not reach GitHub")]
    Request(#[source] Box<ureq::Error>),
    #[error("GitHub answered {0}")]
    Status(u16),
    #[error("GitHub sent no commit")]
    Commit,
    #[error("could not start tj-plugins")]
    Spawn(#[source] std::io::Error),
    #[error("could not record the background update check")]
    Record(#[source] std::io::Error),
}

struct State {
    folder: PathBuf,
    checked: PathBuf,
    started: PathBuf,
    running: PathBuf,
    lock: PathBuf,
    log: PathBuf,
    published: PathBuf,
}

impl State {
    fn new(cache: &Path, host: Host) -> Self {
        let folder = cache.join("update");
        let file = |suffix: &str| folder.join(format!("{}.{suffix}", host.name()));
        Self {
            checked: file("checked"),
            started: file("started"),
            running: file("running"),
            lock: file("lock"),
            log: file("log"),
            published: cache.join("published").join(host.name()),
            folder,
        }
    }
}

struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path, now: SystemTime) -> Option<Self> {
        if modified(path).is_some_and(|at| !recent(at, now, LOCK_STALE)) {
            let _ = fs::remove_file(path);
        }
        OpenOptions::new().write(true).create_new(true).open(path).ok().map(|_| Self(path.to_path_buf()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The marketplace commit this host's plugins last converged to, as published by `tj update`.
fn converged(published: &str) -> Option<&str> {
    published.lines().next().map(str::trim).filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|meta| meta.modified()).ok()
}

fn recent(at: SystemTime, now: SystemTime, window: Duration) -> bool {
    now.duration_since(at).is_ok_and(|age| age < window) || at > now
}

/// The commit to install, unless an install of it already started within the retry window.
fn install(answer: Answer, started: Option<(&str, SystemTime)>, now: SystemTime) -> Option<String> {
    let Answer::Changed(sha) = answer else { return None };
    let retrying = started.is_some_and(|(previous, at)| previous == sha && recent(at, now, RETRY_AFTER));
    (!retrying).then_some(sha)
}

fn request(url: &str, etag: Option<&str>) -> Result<Answer, CheckError> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).http_status_as_error(false).build().into();
    let mut request = agent.get(url).header("Accept", "application/vnd.github.sha").header("User-Agent", "tj");
    if let Some(sha) = etag {
        request = request.header("If-None-Match", format!("\"{sha}\""));
    }
    let mut response = request.call().map_err(|error| CheckError::Request(Box::new(error)))?;
    match response.status().as_u16() {
        304 => Ok(Answer::Unchanged),
        200 => {
            let body = response.body_mut().read_to_string().map_err(|error| CheckError::Request(Box::new(error)))?;
            converged(&body).map(|sha| Answer::Changed(sha.to_owned())).ok_or(CheckError::Commit)
        }
        status => Err(CheckError::Status(status)),
    }
}

struct Running {
    pid: u32,
    started_at: SystemTime,
}

impl Running {
    fn read(path: &Path) -> Result<Option<Self>, CheckError> {
        match fs::read(path) {
            Ok(bytes) => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|error| CheckError::Record(std::io::Error::other(error)))?;
                let invalid = || CheckError::Record(std::io::Error::other("invalid updater process identity"));
                let pid = value["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()).ok_or_else(invalid)?;
                let started_at = value["started_at"]
                    .as_str()
                    .ok_or_else(invalid)?
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .map_err(|_| invalid())?
                    .into();
                Ok(Some(Self { pid, started_at }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CheckError::Record(error)),
        }
    }

    fn live(&self) -> Result<bool, CheckError> {
        tj_cli::process::agent::started_at(self.pid)
            .map(|started| started == Some(self.started_at))
            .map_err(CheckError::Record)
    }

    fn record(child: &Child, path: &Path) -> Result<(), CheckError> {
        if let Some(started_at) = tj_cli::process::agent::started_at(child.id()).map_err(CheckError::Record)? {
            let value = serde_json::json!({
                "pid": child.id(),
                "started_at": chrono::DateTime::<chrono::Utc>::from(started_at).to_rfc3339(),
            });
            fs::write(path, value.to_string()).map_err(CheckError::Record)?;
        }
        Ok(())
    }
}

fn spawn(host: Host, cwd: &Path, state: &State) -> Result<Child, CheckError> {
    let log = OpenOptions::new().create(true).append(true).open(&state.log).map_err(CheckError::Spawn)?;
    let errors = log.try_clone().map_err(CheckError::Spawn)?;
    let mut child = crate::run::hidden("tj-plugins")
        .args(["update", "--host", host.name()])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors)
        .spawn()
        .map_err(CheckError::Spawn)?;
    if let Err(error) = Running::record(&child, &state.running) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

fn record_exit(log: &Path, status: &std::io::Result<std::process::ExitStatus>) {
    let failure = match status {
        Ok(status) if status.success() => return,
        Ok(status) => status.to_string(),
        Err(error) => error.to_string(),
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(file, "native updater failed ({failure}); retry after ten minutes if still needed");
    }
}

/// Exceptions can contain credentials or proxy URLs; record only a safe transport category.
fn record_probe_failure(log: &Path, error: &ureq::Error) -> Result<(), CheckError> {
    let category = match error {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::HostNotFound => "DNS",
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "TLS",
        ureq::Error::Io(_) | ureq::Error::ConnectionFailed => "connection",
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => "proxy",
        _ => "request",
    };
    let mut file = OpenOptions::new().create(true).append(true).open(log).map_err(CheckError::Record)?;
    writeln!(
        file,
        "{} GitHub probe unavailable ({category}); delegating verification to the native updater",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    )
    .map_err(CheckError::Record)
}

fn check(state: &State, host: Host, cwd: &Path, url: &str) -> Result<Option<(Option<String>, Child)>, CheckError> {
    let now = SystemTime::now();
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    let _ = fs::create_dir_all(&state.folder);
    let Some(_lock) = Lock::take(&state.lock, now) else { return Ok(None) };
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    if Running::read(&state.running)?.map(|running| running.live()).transpose()?.unwrap_or(false) {
        return Ok(None);
    }
    let _ = fs::write(&state.checked, "");
    let published = fs::read_to_string(&state.published).unwrap_or_default();
    let answer = match request(url, converged(&published)) {
        Ok(answer) => answer,
        Err(CheckError::Request(error)) => {
            // The native updater already handles unknown freshness through its configured credentials
            // and native marketplace refresh. A failed fast probe must not prevent that recovery.
            if modified(&state.started).is_some_and(|at| recent(at, now, RETRY_AFTER)) {
                return Ok(None);
            }
            record_probe_failure(&state.log, &error)?;
            let child = spawn(host, cwd, state)?;
            // Empty means an unverified attempt, not a requested or successfully installed SHA.
            let _ = fs::write(&state.started, "");
            return Ok(Some((None, child)));
        }
        Err(error) => return Err(error),
    };
    let previous = fs::read_to_string(&state.started).unwrap_or_default();
    let started = modified(&state.started).map(|at| (previous.trim(), at));
    let Some(sha) = install(answer, started, now) else { return Ok(None) };
    let child = spawn(host, cwd, state)?;
    let _ = fs::write(&state.started, &sha);
    Ok(Some((Some(format!("tj-agents changed upstream; updating {} plugins in the background", host.name())), child)))
}

/// Checks once, off the launch's critical path, whether tj-agents moved past what this host last installed, and
/// starts the install in the background when it did. Joining yields the one line to show, if any.
pub(crate) fn start(host: Host, cwd: &Path, url: String) -> Option<JoinHandle<Result<Option<String>, CheckError>>> {
    let state = State::new(&paths::cache_dir().ok()?, host);
    let cwd = cwd.to_path_buf();
    Some(std::thread::spawn(move || {
        check(&state, host, &cwd, &url).map(|update| {
            update.and_then(|(message, mut child)| {
                std::thread::spawn(move || record_exit(&state.log, &child.wait()));
                message
            })
        })
    }))
}

/// An updater owned by the daemon observer; dropping it ends its whole process tree.
pub(crate) struct OwnedUpdate(Child);

impl OwnedUpdate {
    pub(crate) fn finished(&mut self) -> bool {
        self.0.try_wait().is_ok_and(|status| status.is_some())
    }
}

impl Drop for OwnedUpdate {
    fn drop(&mut self) {
        if self.finished() {
            return;
        }
        #[cfg(unix)]
        if let Ok(raw) = i32::try_from(self.0.id())
            && let Some(group) = rustix::process::Pid::from_raw(raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn start_owned(
    host: Host,
    cwd: &Path,
    url: String,
) -> Option<JoinHandle<Result<Option<OwnedUpdate>, CheckError>>> {
    let state = State::new(&paths::cache_dir().ok()?, host);
    let cwd = cwd.to_path_buf();
    Some(std::thread::spawn(move || {
        check(&state, host, &cwd, &url).map(|update| update.map(|(_, child)| OwnedUpdate(child)))
    }))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        Answer, CheckError, OwnedUpdate, Running, State, TIMEOUT, check, converged, install, recent,
        record_probe_failure, request,
    };

    const SHA: &str = "e555013e5e7bab455193e79be12a15e588bdd221";

    #[test]
    fn owned_updater_is_terminated_when_the_observer_exits() {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let child = std::process::Command::new(python)
            .args(["-c", "import time; time.sleep(60)"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        drop(OwnedUpdate(child));
        assert!(tj_cli::process::agent::started_at(pid).unwrap().is_none());
    }

    #[test]
    fn a_live_updater_blocks_checks_until_its_exact_process_exits() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path(), crate::host::Host::Claude);
        std::fs::create_dir_all(&state.folder).unwrap();
        // A regression must never fall through to a real native updater in this test.
        std::fs::write(&state.started, "").unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        let child = std::process::Command::new(python)
            .args(["-c", "import time; time.sleep(60)"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let owned = OwnedUpdate(child);
        Running::record(&owned.0, &state.running).unwrap();
        let running = Running::read(&state.running).unwrap().unwrap();
        assert!(running.live().unwrap());
        assert!(check(&state, crate::host::Host::Claude, root.path(), "invalid://no-request").unwrap().is_none());
        assert!(!state.checked.exists(), "an active updater must not consume the next check");
        let reused = Running { pid: running.pid, started_at: running.started_at + Duration::from_secs(1) };
        assert!(!reused.live().unwrap(), "a reused PID is not the recorded updater");
        drop(owned);
        assert!(!running.live().unwrap());
    }

    #[test]
    fn the_converged_commit_is_the_published_revision_when_it_is_a_sha() {
        assert_eq!(converged(&format!("{SHA}\n123.4")), Some(SHA));
        assert_eq!(converged(r#"{"agent":"abc"}"#), None);
        assert_eq!(converged(""), None);
    }

    #[test]
    fn a_change_installs_once_per_retry_window_per_commit() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let changed = || Answer::Changed(SHA.to_owned());
        assert_eq!(install(Answer::Unchanged, None, now), None);
        assert_eq!(install(changed(), None, now).as_deref(), Some(SHA));
        assert_eq!(install(changed(), Some((SHA, now - Duration::from_secs(599))), now), None);
        assert_eq!(install(changed(), Some((SHA, now - Duration::from_secs(600))), now).as_deref(), Some(SHA));
        assert_eq!(install(changed(), Some(("other", now)), now).as_deref(), Some(SHA));
    }

    #[test]
    fn a_stamp_from_the_future_still_counts_as_recent() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        assert!(recent(now - Duration::from_secs(59), now, Duration::from_secs(60)));
        assert!(!recent(now - Duration::from_secs(60), now, Duration::from_secs(60)));
        assert!(recent(now + Duration::from_secs(5), now, Duration::from_secs(60)));
    }

    #[test]
    fn marketplace_checks_become_due_at_two_minutes() {
        let checked = UNIX_EPOCH + Duration::from_secs(100);
        assert!(recent(checked, checked + Duration::from_secs(119), super::CHECKED_FOR));
        assert!(!recent(checked, checked + Duration::from_secs(120), super::CHECKED_FOR));
    }

    #[test]
    fn an_unanswered_request_fails_within_its_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let started = std::time::Instant::now();
        let result = request(&url, Some(SHA));
        assert!(matches!(result, Err(CheckError::Request(_))), "{result:?}");
        assert!(started.elapsed() < TIMEOUT + Duration::from_millis(500), "{:?}", started.elapsed());
        drop(listener);
    }

    #[test]
    fn probe_diagnostics_do_not_record_credentials_from_transport_errors() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("update.log");
        let error = ureq::Error::ConnectProxyFailed("Authorization: secret-token".to_owned());
        record_probe_failure(&log, &error).unwrap();
        let diagnostic = std::fs::read_to_string(log).unwrap();
        assert!(diagnostic.contains("proxy"));
        assert!(!diagnostic.contains("secret-token"));
        assert!(!diagnostic.contains("Authorization"));
    }
}
