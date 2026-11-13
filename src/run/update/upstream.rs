use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crate::host::Host;
use crate::paths;

pub(crate) const MARKETPLACE: &str = "https://api.github.com/repos/tj-agents/marketplace/commits/main";
const CHECKED_FOR: Duration = Duration::from_secs(60);
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
    #[error("could not start tj-install")]
    Spawn(#[source] std::io::Error),
}

struct State {
    folder: PathBuf,
    checked: PathBuf,
    started: PathBuf,
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

fn spawn(cwd: &Path, log: &Path) -> Result<(), CheckError> {
    let log = OpenOptions::new().create(true).append(true).open(log).map_err(CheckError::Spawn)?;
    let errors = log.try_clone().map_err(CheckError::Spawn)?;
    let mut child = crate::run::hidden("tj-install")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors)
        .spawn()
        .map_err(CheckError::Spawn)?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn check(state: &State, cwd: &Path, url: &str) -> Result<Option<String>, CheckError> {
    let now = SystemTime::now();
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    let _ = fs::create_dir_all(&state.folder);
    let Some(_lock) = Lock::take(&state.lock, now) else { return Ok(None) };
    if modified(&state.checked).is_some_and(|at| recent(at, now, CHECKED_FOR)) {
        return Ok(None);
    }
    let _ = fs::write(&state.checked, "");
    let published = fs::read_to_string(&state.published).unwrap_or_default();
    let answer = request(url, converged(&published))?;
    let previous = fs::read_to_string(&state.started).unwrap_or_default();
    let started = modified(&state.started).map(|at| (previous.trim(), at));
    let Some(sha) = install(answer, started, now) else { return Ok(None) };
    spawn(cwd, &state.log)?;
    let _ = fs::write(&state.started, &sha);
    Ok(Some("tj-agents changed upstream; running tj install in the background".to_owned()))
}

/// Checks once, off the launch's critical path, whether tj-agents moved past what this host last installed, and
/// starts the install in the background when it did. Joining yields the one line to show, if any.
pub(crate) fn start(host: Host, cwd: &Path, url: String) -> Option<JoinHandle<Result<Option<String>, CheckError>>> {
    let state = State::new(&paths::cache_dir().ok()?, host);
    let cwd = cwd.to_path_buf();
    Some(std::thread::spawn(move || check(&state, &cwd, &url)))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Answer, CheckError, TIMEOUT, converged, install, recent, request};

    const SHA: &str = "3f0578aa4000485aa25477e36592978fa5fa750f";

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
    fn an_unanswered_request_fails_within_its_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let started = std::time::Instant::now();
        let result = request(&url, Some(SHA));
        assert!(matches!(result, Err(CheckError::Request(_))), "{result:?}");
        assert!(started.elapsed() < TIMEOUT + Duration::from_millis(500), "{:?}", started.elapsed());
        drop(listener);
    }
}
