use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use tj_cli::process::bounded;

use crate::host::Host;
use crate::paths;

pub(crate) const MARKETPLACE: &str = "https://api.github.com/repos/tj-agents/marketplace/commits/main";
const CHECKED_FOR: Duration = Duration::from_secs(120);
const LOCK_STALE: Duration = Duration::from_secs(30);
const RETRY_AFTER: Duration = Duration::from_secs(600);
const TIMEOUT: Duration = Duration::from_secs(1);
const UPDATE_LIFETIME: Duration = Duration::from_secs(300);

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
    #[error("updater cleanup is unverified; retries blocked: {0}")]
    Unsettled(String),
}

#[derive(Clone)]
struct State {
    folder: PathBuf,
    checked: PathBuf,
    started: PathBuf,
    running: PathBuf,
    receipt: PathBuf,
    unsettled: PathBuf,
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
            receipt: file("receipt"),
            unsettled: file("unsettled"),
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

trait PathExt {
    /// Marks now in the file's modified time. Windows leaves it unchanged when empty content replaces empty content.
    fn stamp(&self);
}

impl PathExt for Path {
    fn stamp(&self) {
        let _ = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self)
            .and_then(|file| file.set_modified(SystemTime::now()));
    }
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
    attempt: String,
    process: Option<(u32, SystemTime)>,
}

impl Running {
    fn read(path: &Path) -> Result<Option<Self>, CheckError> {
        match fs::read(path) {
            Ok(bytes) => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|error| CheckError::Record(std::io::Error::other(error)))?;
                let invalid = || CheckError::Record(std::io::Error::other("invalid updater process identity"));
                let attempt =
                    value["attempt"].as_str().filter(|value| !value.is_empty()).ok_or_else(invalid)?.to_owned();
                let process = if value["pid"].is_null() && value["started_at"].is_null() {
                    None
                } else {
                    let pid = value["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()).ok_or_else(invalid)?;
                    let started_at = value["started_at"]
                        .as_str()
                        .ok_or_else(invalid)?
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .map_err(|_| invalid())?
                        .into();
                    Some((pid, started_at))
                };
                Ok(Some(Self { attempt, process }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CheckError::Record(error)),
        }
    }

    fn live(&self) -> Result<bool, CheckError> {
        let Some((pid, started_at)) = self.process else { return Ok(false) };
        tj_cli::process::agent::started_at(pid).map(|started| started == Some(started_at)).map_err(CheckError::Record)
    }

    fn record(&self, path: &Path) -> Result<(), CheckError> {
        let value = serde_json::json!({
            "attempt": self.attempt,
            "pid": self.process.map(|(pid, _)| pid),
            "started_at": self.process.map(|(_, time)| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()),
        });
        let staged = path.with_extension("running.tmp");
        fs::write(&staged, value.to_string()).map_err(CheckError::Record)?;
        fs::rename(staged, path).map_err(CheckError::Record)
    }
}

fn unsettled(state: &State, reason: impl Into<String>) -> CheckError {
    let reason = reason.into();
    if fs::read_to_string(&state.unsettled).ok().as_deref() != Some(&reason) {
        let _ = fs::write(&state.unsettled, &reason);
        if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(&state.log) {
            let _ = writeln!(log, "updater cleanup is unverified; retries blocked: {reason}");
        }
    }
    CheckError::Unsettled(reason)
}

fn settled(state: &State, attempt: &str) -> Result<serde_json::Value, CheckError> {
    let receipt: serde_json::Value = fs::read(&state.receipt)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| unsettled(state, "missing or invalid process-tree cleanup receipt"))?;
    if receipt["attempt"] != attempt
        || receipt["settled"] != true
        || !matches!(receipt["state"].as_str(), Some("exited" | "timed_out" | "cancelled" | "failed"))
        || receipt["code"].as_u64().is_none()
    {
        return Err(unsettled(state, format!("no matching settled receipt for attempt {attempt}: {receipt}")));
    }
    let _ = fs::remove_file(&state.unsettled);
    Ok(receipt)
}

fn available(state: &State) -> Result<bool, CheckError> {
    if let Some(running) = Running::read(&state.running).map_err(|error| unsettled(state, error.to_string()))? {
        if running.live().map_err(|error| unsettled(state, error.to_string()))? {
            return Ok(false);
        }
        settled(state, &running.attempt)?;
    } else if state.unsettled.exists() {
        return Err(CheckError::Unsettled(fs::read_to_string(&state.unsettled).unwrap_or_default()));
    }
    Ok(true)
}

fn spawn(host: Host, cwd: &Path, state: &State) -> Result<OwnedUpdate, CheckError> {
    let log = OpenOptions::new().create(true).append(true).open(&state.log).map_err(CheckError::Spawn)?;
    let errors = log.try_clone().map_err(CheckError::Spawn)?;
    let attempt = crate::run::generated_id().map_err(|error| CheckError::Record(std::io::Error::other(error)))?;
    // Persist the attempt before creating any writer. A crash in spawn must never authorize a retry.
    let mut running = Running { attempt: attempt.clone(), process: None };
    running.record(&state.running)?;
    let helper = std::env::current_exe()
        .map_err(CheckError::Spawn)?
        .with_file_name(format!("tj_hook{}", std::env::consts::EXE_SUFFIX));
    let mut command = Command::new(helper);
    command
        .arg("--bounded")
        .arg(UPDATE_LIFETIME.as_millis().to_string())
        .arg(&state.receipt)
        .arg(&attempt)
        .args(["--", "tj-plugins", "update", "--host", host.name()])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|error| {
        let outcome = bounded::Outcome::Failed(std::io::Error::new(error.kind(), error.to_string()));
        let _ = outcome.record(&state.receipt, &attempt);
        CheckError::Spawn(error)
    })?;
    let owned = OwnedUpdate { child, state: state.clone(), attempt, finished: false };
    running.process = tj_cli::process::agent::started_at(owned.child.id())
        .map_err(CheckError::Record)?
        .map(|started| (owned.child.id(), started));
    running.record(&state.running)?;
    Ok(owned)
}

fn record_exit(state: &State, attempt: &str) {
    if let Ok(receipt) = settled(state, attempt)
        && receipt["code"] != 0
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&state.log)
    {
        let _ = writeln!(
            file,
            "native updater failed ({}; code {}); process tree settled; ten-minute retry window: {}",
            receipt["state"], receipt["code"], receipt["error"]
        );
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

fn check(
    state: &State,
    host: Host,
    cwd: &Path,
    url: &str,
) -> Result<Option<(Option<String>, OwnedUpdate)>, CheckError> {
    let now = SystemTime::now();
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    let _ = fs::create_dir_all(&state.folder);
    let Some(_lock) = Lock::take(&state.lock, now) else { return Ok(None) };
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    if !available(state)? {
        return Ok(None);
    }
    state.checked.stamp();
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
            state.started.stamp();
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
                std::thread::spawn(move || {
                    while !child.finished() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                });
                message
            })
        })
    }))
}

/// Records a failed mid-session check in the updater log. The agent owns the console then, so nothing is printed.
/// An unsettled updater is already logged once, when it is first found.
pub(crate) fn record_failure(host: Host, error: CheckError) {
    if let Ok(cache) = paths::cache_dir() {
        append_failure(&State::new(&cache, host).log, error);
    }
}

fn append_failure(log: &Path, error: CheckError) {
    if matches!(error, CheckError::Unsettled(_)) {
        return;
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(
            file,
            "{} could not check for tj-agents updates: {:#}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            anyhow::Error::from(error)
        );
    }
}

/// An updater supervised by the bounded helper; dropping it requests cleanup of its whole process tree.
pub(crate) struct OwnedUpdate {
    child: Child,
    state: State,
    attempt: String,
    finished: bool,
}

impl OwnedUpdate {
    pub(crate) fn finished(&mut self) -> bool {
        if self.finished {
            return true;
        }
        let exited = match self.child.try_wait() {
            Ok(status) => status.is_some(),
            // The session wrapper is also a subreaper. Its native wait may already have reaped this helper.
            Err(_) => tj_cli::process::agent::started_at(self.child.id()).is_ok_and(|started| started.is_none()),
        };
        if exited {
            record_exit(&self.state, &self.attempt);
            self.finished = true;
        }
        exited
    }
}

impl Drop for OwnedUpdate {
    fn drop(&mut self) {
        if self.finished() {
            return;
        }
        let _ = fs::write(bounded::cancel_path(&self.state.receipt), &self.attempt);
        let deadline = Instant::now() + bounded::CLEANUP_LIMIT + Duration::from_secs(2);
        while !self.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !self.finished {
            let _ = self.child.kill();
            let reap_deadline = Instant::now() + Duration::from_secs(2);
            while self.child.try_wait().is_ok_and(|status| status.is_none()) && Instant::now() < reap_deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = unsettled(&self.state, "bounded updater helper did not confirm cleanup after cancellation");
        }
    }
}

pub(crate) fn start_owned(
    host: Host,
    cwd: &Path,
    url: String,
) -> Option<JoinHandle<Result<Option<OwnedUpdate>, CheckError>>> {
    let state = State::new(&paths::cache_dir().ok()?, host);
    let cwd = cwd.to_path_buf();
    Some(std::thread::spawn(move || check(&state, host, &cwd, &url).map(|update| update.map(|(_, child)| child))))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        Answer, CheckError, OwnedUpdate, Running, State, TIMEOUT, append_failure, check, converged, install, recent,
        record_probe_failure, request,
    };

    const SHA: &str = "e555013e5e7bab455193e79be12a15e588bdd221";

    #[test]
    fn observer_shutdown_asks_the_owned_helper_to_settle_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path(), crate::host::Host::Claude);
        std::fs::create_dir_all(&state.folder).unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        let child = std::process::Command::new(python)
            .args(["-c", "import pathlib,sys,time; p=pathlib.Path(sys.argv[1]);\nwhile not p.exists(): time.sleep(.01)\npathlib.Path(sys.argv[2]).write_text(sys.argv[3])"])
            .arg(tj_cli::process::bounded::cancel_path(&state.receipt)).arg(&state.receipt)
            .arg(r#"{"attempt":"cancel-test","settled":true,"state":"cancelled","code":125}"#)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        drop(OwnedUpdate { child, state: state.clone(), attempt: "cancel-test".into(), finished: false });
        assert!(tj_cli::process::agent::started_at(pid).unwrap().is_none());
        assert!(!state.unsettled.exists());
        assert!(super::settled(&state, "cancel-test").is_ok());
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
        let mut owned = OwnedUpdate { child, state: state.clone(), attempt: "live-test".into(), finished: false };
        let started_at = tj_cli::process::agent::started_at(owned.child.id()).unwrap().unwrap();
        Running { attempt: "live-test".into(), process: Some((owned.child.id(), started_at)) }
            .record(&state.running)
            .unwrap();
        let running = Running::read(&state.running).unwrap().unwrap();
        assert!(running.live().unwrap());
        assert!(check(&state, crate::host::Host::Claude, root.path(), "invalid://no-request").unwrap().is_none());
        assert!(!state.checked.exists(), "an active updater must not consume the next check");
        let reused = Running {
            attempt: "reused".into(),
            process: Some((owned.child.id(), started_at + Duration::from_secs(1))),
        };
        assert!(!reused.live().unwrap(), "a reused PID is not the recorded updater");
        owned.child.kill().unwrap();
        owned.child.wait().unwrap();
        assert!(owned.finished());
        assert!(!running.live().unwrap());
        assert!(state.unsettled.exists());
        assert!(super::available(&state).is_err(), "a dead root alone cannot authorize another writer");
        tj_cli::process::bounded::Outcome::TimedOut.record(&state.receipt, "live-test").unwrap();
        assert!(super::available(&state).unwrap());
        assert!(!state.unsettled.exists());
    }

    #[test]
    fn absent_mismatched_or_unsettled_receipts_block_retry_across_watchers() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path(), crate::host::Host::Claude);
        std::fs::create_dir_all(&state.folder).unwrap();
        Running { attempt: "attempt".into(), process: None }.record(&state.running).unwrap();
        for receipt in [
            None,
            Some(serde_json::json!({"attempt":"older","settled":true,"state":"exited","code":0})),
            Some(serde_json::json!({"attempt":"attempt","settled":false,"state":"unsettled","code":127})),
        ] {
            if let Some(receipt) = receipt {
                std::fs::write(&state.receipt, receipt.to_string()).unwrap();
            }
            let restarted = State::new(root.path(), crate::host::Host::Claude);
            assert!(super::available(&restarted).is_err());
            assert!(restarted.unsettled.exists());
            let diagnostic = std::fs::read(&restarted.log).unwrap();
            assert!(super::available(&restarted).is_err());
            assert_eq!(std::fs::read(&restarted.log).unwrap(), diagnostic, "unchanged failures must not spam the log");
        }
    }

    #[test]
    fn a_check_advances_the_throttle_stamp_even_when_the_file_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path(), crate::host::Host::Claude);
        std::fs::create_dir_all(&state.folder).unwrap();
        // A recent attempt makes the unreachable probe return before any real native updater can start.
        std::fs::write(&state.started, "").unwrap();
        std::fs::write(&state.checked, "").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new().write(true).open(&state.checked).unwrap().set_modified(old).unwrap();
        assert!(check(&state, crate::host::Host::Claude, root.path(), "invalid://no-request").unwrap().is_none());
        assert!(recent(super::modified(&state.checked).unwrap(), SystemTime::now(), super::CHECKED_FOR));
    }

    #[test]
    fn a_failed_mid_session_check_is_logged_but_a_repeated_unsettled_state_is_not() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("claude.log");
        append_failure(&log, CheckError::Status(403));
        append_failure(&log, CheckError::Unsettled("reason".to_owned()));
        let logged = std::fs::read_to_string(&log).unwrap();
        assert_eq!(logged.lines().count(), 1, "{logged}");
        assert!(logged.contains("could not check for tj-agents updates: GitHub answered 403"), "{logged}");
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
