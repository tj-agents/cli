use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Shutdown(PathBuf);

impl Drop for Shutdown {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("stop"), "");
        let ready = self.0.join("daemon.sock.tj-observer.ready");
        let deadline = Instant::now() + Duration::from_secs(15);
        while ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn call(root: &Path, socket: &Path, mode: &str, path: &std::ffi::OsStr) -> Output {
    let mut command = if mode == "--start" {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tj_hook"));
        command.args(["--preserve-descendants", "--", env!("CARGO_BIN_EXE_tj")]);
        command
    } else {
        Command::new(env!("CARGO_BIN_EXE_tj"))
    };
    command
        .args(["observe-codex-updates", "--codex"])
        .arg(root.join(if cfg!(windows) { "codex.cmd" } else { "codex" }))
        .arg("--socket")
        .arg(socket)
        .arg("--cwd")
        .arg(root)
        .arg(mode)
        .env("PATH", path)
        .env("TJ_CACHE_DIR", root.join("cache"))
        .env("FAKE_CODEX_LOG", root.join("daemon.jsonl"))
        .env("FAKE_CODEX_SCENARIO", "observer-live")
        .env("FAKE_OBSERVER_SHUTDOWN", root.join("stop"));
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(command.output());
    });
    match receiver.recv_timeout(Duration::from_secs(25)) {
        Ok(result) => result.unwrap(),
        Err(error) => {
            let _ = fs::write(root.join("stop"), "");
            let _ = receiver.recv_timeout(Duration::from_secs(15));
            panic!("observer command did not return promptly: {error}");
        }
    }
}

#[test]
fn starts_one_observer_reloads_publications_and_exits_after_last_thread() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let _shutdown = Shutdown(root.to_path_buf());
    let socket = root.join("daemon.sock");
    fs::write(&socket, "dummy socket").unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.py");
    let python = which::which(if cfg!(windows) { "python" } else { "python3" }).unwrap();
    #[cfg(windows)]
    {
        fs::write(
            root.join("codex.cmd"),
            format!("@echo off\r\n\"{}\" \"{}\" %*\r\n", python.display(), script.display()),
        )
        .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let codex = root.join("codex");
        fs::write(&codex, format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", python.display(), script.display())).unwrap();
        fs::set_permissions(codex, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bin = Path::new(env!("CARGO_BIN_EXE_tj_hook")).parent().unwrap();
    let mut paths = vec![root.to_path_buf(), bin.to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(paths).unwrap();
    let update = root.join("cache/update");
    let published = root.join("cache/published");
    fs::create_dir_all(&update).unwrap();
    fs::create_dir_all(&published).unwrap();
    fs::write(update.join("codex.checked"), "").unwrap();
    let publication = published.join("codex");
    fs::write(&publication, "old-revision").unwrap();

    assert_eq!(call(root, &socket, "--status", &path).status.code(), Some(1));
    let first = call(root, &socket, "--start", &path);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(call(root, &socket, "--status", &path).status.success());
    let ready = root.join("daemon.sock.tj-observer.ready");
    let owner = fs::read(&ready).unwrap();
    assert!(call(root, &socket, "--start", &path).status.success());
    assert_eq!(fs::read(&ready).unwrap(), owner, "duplicate start replaced the owner");

    fs::write(&publication, "new-revision").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let log = fs::read_to_string(root.join("daemon.jsonl")).unwrap_or_default();
        if log.matches("config/batchWrite").count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "publication was not reloaded: {log}");
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::remove_file(&socket).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&socket, "replacement socket").unwrap();
    assert_eq!(call(root, &socket, "--status", &path).status.code(), Some(1));
    let restarted = call(root, &socket, "--start", &path);
    assert!(restarted.status.success(), "{}", String::from_utf8_lossy(&restarted.stderr));
    assert_ne!(fs::read(&ready).unwrap(), owner, "replacement socket kept the old observer");
    fs::write(root.join("stop"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while ready.exists() {
        assert!(Instant::now() < deadline, "observer remained after its last thread exited");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(call(root, &socket, "--status", &path).status.code(), Some(3));
}
