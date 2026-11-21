use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fs4::FileExt;
use serde_json::{Value, json};
use tj_cli::process::{self, agent};

use super::publication::Publication;
use super::{Watcher, refresh, upstream};
use crate::app_server::Server;
use crate::host::Host;

const POLL: Duration = Duration::from_secs(5);
const UPSTREAM: Duration = Duration::from_secs(120);
const START_TIMEOUT: Duration = Duration::from_secs(12);
static WITHDRAWAL: AtomicU64 = AtomicU64::new(0);

#[derive(clap::Args)]
pub struct Args {
    /// Absolute native Codex executable selected by the adapter.
    #[arg(long)]
    codex: PathBuf,
    /// Existing daemon control socket; this command never starts a daemon.
    #[arg(long)]
    socket: PathBuf,
    /// Existing directory used to run the plugin updater.
    #[arg(long)]
    cwd: PathBuf,
    /// Start a hidden observer or verify an existing one.
    #[arg(long, conflicts_with_all = ["status", "foreground"])]
    start: bool,
    /// Exit 0 if ready, 1 if missing, 3 if not applicable, or 2 on error.
    #[arg(long, conflicts_with_all = ["start", "foreground"])]
    status: bool,
    #[arg(long, hide = true, conflicts_with_all = ["start", "status"])]
    foreground: bool,
}

pub(super) fn sibling(socket: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = socket.file_name().context("socket has no file name")?.to_os_string();
    name.push(suffix);
    Ok(socket.with_file_name(name))
}

fn lock_path(socket: &Path) -> Result<PathBuf> {
    sibling(socket, ".tj-observer.lock")
}
fn ready_path(socket: &Path) -> Result<PathBuf> {
    sibling(socket, ".tj-observer.ready")
}

#[cfg(windows)]
pub(super) fn socket_identity(socket: &Path) -> Result<(u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::metadata(socket).context("could not inspect daemon socket")?;
    Ok((metadata.creation_time(), metadata.last_write_time()))
}

#[cfg(target_os = "linux")]
pub(super) struct SocketGuard(File);

#[cfg(target_os = "linux")]
impl SocketGuard {
    pub(super) fn open(socket: &Path) -> Result<Self> {
        use rustix::fs::{Mode, OFlags, open};
        let fd = open(socket, OFlags::PATH | OFlags::CLOEXEC, Mode::empty()).context("could not pin daemon socket")?;
        Ok(Self(File::from(fd)))
    }

    pub(super) fn identity(&self) -> Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = self.0.metadata().context("could not inspect pinned daemon socket")?;
        Ok((metadata.dev(), metadata.ino()))
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) struct SocketGuard((u64, u64));

#[cfg(not(target_os = "linux"))]
impl SocketGuard {
    pub(super) fn open(socket: &Path) -> Result<Self> {
        socket_identity(socket).map(Self)
    }
    pub(super) fn identity(&self) -> Result<(u64, u64)> {
        Ok(self.0)
    }
}

#[cfg(unix)]
pub(super) fn socket_identity(socket: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(socket).context("could not inspect daemon socket")?;
    Ok((metadata.dev(), metadata.ino()))
}

fn lock(socket: &Path, create: bool) -> Result<Option<File>> {
    let path = lock_path(socket)?;
    let file = match OpenOptions::new().read(true).write(true).create(create).truncate(false).open(&path) {
        Ok(file) => file,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("could not open observer lock"),
    };
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error).context("could not lock observer"),
    }
}

fn started(pid: u32) -> Result<Option<u64>> {
    let Some(time) = agent::started_at(pid).context("could not inspect observer process")? else { return Ok(None) };
    Ok(Some(u64::try_from(time.duration_since(UNIX_EPOCH)?.as_nanos())?))
}

fn owner_ready(socket: &Path) -> Result<bool> {
    // Acquiring the lock proves no observer owns it. Never create it during status.
    if lock(socket, false)?.is_some() {
        return Ok(false);
    }
    if !lock_path(socket)?.exists() {
        return Ok(false);
    }
    let document: Value = match fs::read(ready_path(socket)?) {
        Ok(data) => match serde_json::from_slice(&data) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("could not read observer readiness"),
    };
    let Some(pid) = document["pid"].as_u64().and_then(|value| u32::try_from(value).ok()) else { return Ok(false) };
    let Some(identity) = document["started"].as_u64() else { return Ok(false) };
    let socket_identity = socket_identity(socket)?;
    Ok(started(pid)? == Some(identity)
        && document["socket"][0].as_u64() == Some(socket_identity.0)
        && document["socket"][1].as_u64() == Some(socket_identity.1))
}

fn loaded(server: &mut Server) -> Result<bool> {
    Ok(!refresh::threads(server)?.is_empty())
}

fn status(codex: &Path, socket: &Path) -> Result<u8> {
    if !socket.exists() {
        return Ok(3);
    }
    let mut server = Server::connect_socket(codex, socket).context("could not connect to existing Codex daemon")?;
    if !loaded(&mut server)? {
        return Ok(3);
    }
    Ok(if owner_ready(socket)? { 0 } else { 1 })
}

struct Starting(Option<process::Background>);

impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
        }
    }
}

fn start(args: &Args) -> Result<ExitCode> {
    match status(&args.codex, &args.socket)? {
        0 => return Ok(ExitCode::SUCCESS),
        3 => return Ok(ExitCode::from(3)),
        _ => {}
    }
    let exe = std::env::current_exe()?;
    let hook = exe.with_file_name(if cfg!(windows) { "tj_hook.exe" } else { "tj_hook" });
    let command = vec![
        hook.into_os_string(),
        OsString::from("--"),
        exe.into_os_string(),
        OsString::from("observe-codex-updates"),
        OsString::from("--codex"),
        args.codex.clone().into_os_string(),
        OsString::from("--socket"),
        args.socket.clone().into_os_string(),
        OsString::from("--cwd"),
        args.cwd.clone().into_os_string(),
        OsString::from("--foreground"),
    ];
    let spawn = || process::background(&command, &args.cwd).context("could not start observer");
    let mut child = Starting(Some(spawn()?));
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if owner_ready(&args.socket)? {
            match status(&args.codex, &args.socket)? {
                0 => {
                    child.0.take();
                    return Ok(ExitCode::SUCCESS);
                }
                3 => return Ok(ExitCode::from(3)),
                _ => {}
            }
        }
        if !child.0.as_mut().context("observer process is missing")?.running()?
            && (!lock_path(&args.socket)?.exists() || lock(&args.socket, false)?.is_some())
        {
            child.0 = Some(spawn()?);
        }
        if Instant::now() >= deadline {
            bail!("observer did not become ready");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop_check(check: Option<JoinHandle<Result<Option<upstream::OwnedUpdate>, upstream::CheckError>>>) {
    if let Some(check) = check {
        let _ = check.join();
    }
}

struct Location<'a> {
    cwd: &'a Path,
    socket: &'a Path,
    identity: (u64, u64),
}

fn observe(
    server: &mut Server,
    location: Location<'_>,
    mut watcher: Watcher,
    settings: (Duration, bool),
    current_socket: impl Fn() -> Result<bool>,
    withdraw: impl FnOnce() -> Result<()>,
    mut check: Option<JoinHandle<Result<Option<upstream::OwnedUpdate>, upstream::CheckError>>>,
) -> Result<()> {
    let (poll, check_updates) = settings;
    let mut updater = None;
    let result = (|| {
        let mut next_poll = Instant::now();
        let mut next_upstream = Instant::now() + START_TIMEOUT;
        loop {
            let now = Instant::now();
            if now >= next_poll {
                if !current_socket()? {
                    return Ok(());
                }
                let loaded = refresh::threads(server)?;
                if loaded.is_empty() {
                    return Ok(());
                }
                if let Some(revision) = watcher.revision()
                    && refresh::apply(server, location.socket, location.identity, Some(&revision), &loaded)?
                {
                    watcher.applied(Some(revision));
                }
                next_poll = now + poll;
            }
            if check_updates && now >= next_upstream && check.is_none() && updater.is_none() {
                check = upstream::start_owned(Host::Codex, location.cwd, upstream::MARKETPLACE.to_owned());
                next_upstream = now + UPSTREAM;
            }
            if check.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(Ok(Ok(child))) = check.take().map(JoinHandle::join) {
                    updater = child;
                }
            }
            if updater.as_mut().is_some_and(upstream::OwnedUpdate::finished) {
                updater = None;
            }
            let _ = server.next(next_poll).context("Codex daemon disconnected")?;
        }
    })();
    let withdrawal = withdraw();
    stop_check(check);
    drop(updater);
    withdrawal?;
    result
}

fn cycles<T>(
    state: &mut T,
    mut cycle: impl FnMut(&mut T) -> Result<()>,
    mut active: impl FnMut(&mut T) -> Result<bool>,
) -> Result<()> {
    loop {
        cycle(state)?;
        if !active(state)? {
            return Ok(());
        }
    }
}

fn withdraw(marker: &Path) -> Result<()> {
    // Retire the name before unlinking. On Windows a handle that outlives unlinking
    // can leave the old name inaccessible until the last reader closes it.
    let serial = WITHDRAWAL.fetch_add(1, Ordering::Relaxed);
    let withdrawn = marker.with_extension(format!("withdrawn-{}-{serial}", std::process::id()));
    match fs::rename(marker, &withdrawn) {
        Ok(()) => fs::remove_file(&withdrawn).context("could not remove withdrawn observer readiness"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("could not withdraw observer readiness"),
    }
}

fn foreground(args: &Args) -> Result<ExitCode> {
    let Some(_lock) = lock(&args.socket, true)? else { return Ok(ExitCode::from(1)) };
    let marker = ready_path(&args.socket)?;
    withdraw(&marker)?;
    let socket_guard = SocketGuard::open(&args.socket)?;
    let socket = socket_guard.identity()?;
    let mut server =
        Server::connect_socket(&args.codex, &args.socket).context("could not connect to existing Codex daemon")?;
    if socket_identity(&args.socket)? != socket {
        bail!("daemon socket changed during observer startup");
    }
    if !loaded(&mut server)? {
        return Ok(ExitCode::from(3));
    }
    let pid = std::process::id();
    let identity = started(pid)?.context("observer process identity unavailable")?;
    cycles(
        &mut server,
        |server| {
            let watcher = Watcher::new(std::env::current_exe()?, Publication::of(Host::Codex)?);
            loop {
                let loaded = refresh::threads(server)?;
                if loaded.is_empty() {
                    return Ok(());
                }
                if refresh::apply(server, &args.socket, socket, watcher.applied_revision(), &loaded)? {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            fs::write(
                &marker,
                serde_json::to_vec(&json!({"pid": pid, "started": identity, "socket": [socket.0, socket.1]}))?,
            )?;
            observe(
                server,
                Location { cwd: &args.cwd, socket: &args.socket, identity: socket },
                watcher,
                (POLL, true),
                || Ok(socket_identity(&args.socket)? == socket),
                || withdraw(&marker),
                None,
            )
        },
        |server| Ok(socket_identity(&args.socket)? == socket && loaded(server)?),
    )?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run(args: Args) -> Result<ExitCode> {
    if !args.codex.is_absolute() {
        bail!("observer requires an absolute Codex executable");
    }
    if !args.socket.is_absolute() || !args.cwd.is_absolute() || !args.cwd.is_dir() {
        bail!("observer requires an absolute socket and existing absolute working directory");
    }
    if args.status {
        return Ok(ExitCode::from(status(&args.codex, &args.socket)?));
    }
    if args.start {
        return start(&args);
    }
    if args.foreground {
        return foreground(&args);
    }
    bail!("choose --start or --status")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codex's publication record under `directory`, already announcing an old revision.
    fn announced(directory: &Path) -> (Publication, PathBuf) {
        let record = directory.join("published").join("codex");
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        fs::write(&record, "old-revision").unwrap();
        (Publication::new(directory, Host::Codex), record)
    }

    fn fake(log: &Path, publication: &Path, scenario: &str) -> Server {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let mut command = crate::app_server::host_command(python).unwrap();
        command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.py"))
            .args(["app-server", "proxy"])
            .env("FAKE_CODEX_LOG", log)
            .env("FAKE_CODEX_SCENARIO", scenario)
            .env("FAKE_OBSERVER_PUBLICATION", publication);
        Server::connect_command(command, Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn lock_has_one_owner_and_readiness_requires_live_matching_owner() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        fs::write(&socket, "socket").unwrap();
        let owner = lock(&socket, true).unwrap().unwrap();
        assert!(lock(&socket, true).unwrap().is_none());
        assert!(!owner_ready(&socket).unwrap());
        let pid = std::process::id();
        let identity = socket_identity(&socket).unwrap();
        fs::write(
            ready_path(&socket).unwrap(),
            serde_json::to_vec(
                &json!({"pid":pid,"started":started(pid).unwrap().unwrap(),"socket":[identity.0,identity.1]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(owner_ready(&socket).unwrap());
        drop(owner);
        assert!(!owner_ready(&socket).unwrap());
        assert!(lock(&socket, true).unwrap().is_some());
    }

    #[test]
    fn readiness_can_be_replaced_while_a_reader_holds_the_withdrawn_record() {
        use std::io::Read;

        let directory = tempfile::tempdir().unwrap();
        let marker = ready_path(&directory.path().join("daemon.sock")).unwrap();
        fs::write(&marker, "old").unwrap();
        let mut reader = File::open(&marker).unwrap();
        withdraw(&marker).unwrap();
        assert_eq!(fs::read(&marker).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        fs::write(&marker, "new").unwrap();
        let mut second = File::open(&marker).unwrap();
        withdraw(&marker).unwrap();
        fs::write(&marker, "current").unwrap();
        let mut prior = String::new();
        reader.read_to_string(&mut prior).unwrap();
        assert_eq!(prior, "old");
        let mut next = String::new();
        second.read_to_string(&mut next).unwrap();
        assert_eq!(next, "new");
        assert_eq!(fs::read_to_string(marker).unwrap(), "current");
    }

    #[test]
    fn observer_reloads_changed_publication_and_exits_with_last_thread() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("daemon.jsonl");
        let (watched, publication) = announced(directory.path());
        let socket = directory.path().join("daemon.sock");
        fs::write(&socket, "socket").unwrap();
        let mut server = fake(&log, &publication, "observer");
        let watcher = Watcher::new(std::env::current_exe().unwrap(), watched);
        observe(
            &mut server,
            Location { cwd: directory.path(), socket: &socket, identity: socket_identity(&socket).unwrap() },
            watcher,
            (Duration::from_millis(20), false),
            || Ok(true),
            || Ok(()),
            None,
        )
        .unwrap();
        drop(server);
        let events = fs::read_to_string(log).unwrap();
        assert_eq!(events.matches("config/batchWrite").count(), 1);
        assert_eq!(events.matches("thread/loaded/list").count(), 2);
    }

    #[test]
    fn observer_exits_when_daemon_disconnects() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("daemon.jsonl");
        let (watched, publication) = announced(directory.path());
        let socket = directory.path().join("daemon.sock");
        fs::write(&socket, "socket").unwrap();
        let mut server = fake(&log, &publication, "observer-disconnect");
        let watcher = Watcher::new(std::env::current_exe().unwrap(), watched);
        assert!(
            observe(
                &mut server,
                Location { cwd: directory.path(), socket: &socket, identity: socket_identity(&socket).unwrap() },
                watcher,
                (Duration::from_millis(20), false),
                || Ok(true),
                || Ok(()),
                None
            )
            .is_err()
        );
        drop(server);
        let events = fs::read_to_string(log).unwrap();
        assert!(events.contains("thread/loaded/list"));
    }

    #[test]
    fn absent_socket_is_not_applicable_without_starting_a_daemon() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(status(&directory.path().join("unused"), &directory.path().join("missing.sock")).unwrap(), 3);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_socket_identifies_a_replacement_listener() {
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.sock");
        let old = UnixListener::bind(&path).unwrap();
        let guard = SocketGuard::open(&path).unwrap();
        let identity = guard.identity().unwrap();
        fs::remove_file(&path).unwrap();
        drop(old);
        let _replacement = UnixListener::bind(&path).unwrap();
        assert_ne!(identity, socket_identity(&path).unwrap());
    }

    #[test]
    fn new_thread_during_shutdown_restarts_observation_after_readiness_withdrawal() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        let marker = ready_path(&socket).unwrap();
        let log = directory.path().join("daemon.jsonl");
        let (watched, publication) = announced(directory.path());
        fs::write(&socket, "socket").unwrap();
        let owner = lock(&socket, true).unwrap().unwrap();
        let pid = std::process::id();
        let identity = socket_identity(&socket).unwrap();
        let marker_contents = serde_json::to_vec(
            &json!({"pid":pid,"started":started(pid).unwrap().unwrap(),"socket":[identity.0,identity.1]}),
        )
        .unwrap();
        let (withdrawn_tx, withdrawn_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let new_thread = Arc::new(AtomicBool::new(false));
        let active = Arc::clone(&new_thread);
        let cwd = directory.path().to_path_buf();
        let marker_for_worker = marker.clone();
        let worker = std::thread::spawn(move || {
            let mut server = fake(&log, &publication, "observer");
            let mut runs = 0;
            let mut resume_rx = Some(resume_rx);
            cycles(
                &mut server,
                |server| {
                    runs += 1;
                    fs::write(&marker_for_worker, &marker_contents)?;
                    if runs == 2 {
                        return Ok(());
                    }
                    let resume_rx = resume_rx.take().unwrap();
                    let check = std::thread::spawn(move || {
                        resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(None)
                    });
                    let watcher = Watcher::new(std::env::current_exe()?, watched.clone());
                    observe(
                        server,
                        Location {
                            cwd: &cwd,
                            socket: &cwd.join("daemon.sock"),
                            identity: socket_identity(&cwd.join("daemon.sock")).unwrap(),
                        },
                        watcher,
                        (Duration::from_millis(20), false),
                        || Ok(true),
                        || {
                            withdraw(&marker_for_worker)?;
                            withdrawn_tx.send(()).unwrap();
                            Ok(())
                        },
                        Some(check),
                    )
                },
                |_| Ok(active.swap(false, Ordering::SeqCst)),
            )
            .unwrap();
            runs
        });
        withdrawn_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!owner_ready(&socket).unwrap());
        assert!(lock(&socket, true).unwrap().is_none());
        new_thread.store(true, Ordering::SeqCst);
        resume_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), 2);
        assert!(owner_ready(&socket).unwrap());
        drop(owner);
    }
}
