use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn tj(path: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tj")).args(arguments).env("PATH", path).output().unwrap()
}

fn write_tool(directory: &Path, name: &str) {
    #[cfg(windows)]
    std::fs::write(directory.join(format!("tj-{name}.cmd")), "@echo off\r\necho %*\r\nexit /b 7\r\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tool = directory.join(format!("tj-{name}"));
        std::fs::write(&tool, "#!/bin/sh\necho \"$@\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn unknown_subcommand_runs_the_matching_tool_on_path() {
    let directory = tempfile::tempdir().unwrap();
    write_tool(directory.path(), "echo");
    let output = tj(directory.path(), &["echo", "progress", "461"]);
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "progress 461");
}

#[test]
fn unknown_subcommand_without_a_tool_names_the_missing_program() {
    let directory = tempfile::tempdir().unwrap();
    let output = tj(directory.path(), &["nothing-here"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no `tj-nothing-here` is on PATH"));
}

#[test]
fn public_hosts_forward_native_arguments_to_the_installed_agent_commands() {
    let directory = tempfile::tempdir().unwrap();
    for host in ["claude", "codex"] {
        write_tool(directory.path(), host);
        for arguments in [vec!["--help"], vec!["resume", "thread-id", "--", "--literal-prompt"]] {
            let input = [vec![host], arguments.clone()].concat();
            let output = tj(directory.path(), &input);
            assert_eq!(output.status.code(), Some(7));
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), arguments.join(" "));
        }
    }
}

struct WriterTree {
    directory: tempfile::TempDir,
    helper: std::process::Child,
    unrelated: std::process::Child,
}

impl WriterTree {
    fn start(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/updater_writer.py");
        let helper = Command::new(env!("CARGO_BIN_EXE_tj_hook"))
            .arg("--bounded")
            .arg("5000")
            .arg(directory.path().join("update.receipt"))
            .args(["writer-test", "--", python])
            .arg(&fixture)
            .arg(directory.path())
            .args(["root", mode])
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let unrelated = Command::new(python)
            .arg(fixture)
            .arg(directory.path())
            .args(["unrelated", "hang"])
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        Self { directory, helper, unrelated }
    }

    fn cancel(&self) {
        std::fs::write(self.directory.path().join("update.cancel"), "writer-test").unwrap();
    }

    fn ready(&self) -> bool {
        ["root", "child", "grandchild", "unrelated"].iter().all(|role| {
            self.directory.path().join(format!("{role}.writer")).metadata().is_ok_and(|meta| meta.len() > 0)
        })
    }

    fn prove_settled(&mut self, state: &str, code: i32) {
        let deadline = Instant::now() + Duration::from_secs(18);
        let status = loop {
            if let Some(status) = self.helper.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "bounded helper exceeded lifetime plus cleanup allowance");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(code));
        let ready_deadline = Instant::now() + Duration::from_secs(4);
        while !self.ready() && Instant::now() < ready_deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(self.ready(), "all three owned generations and the unrelated writer must have started");
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.directory.path().join("update.receipt")).unwrap()).unwrap();
        assert_eq!(receipt["attempt"], "writer-test");
        assert_eq!(receipt["state"], state);
        assert_eq!(receipt["settled"], true);
        assert_eq!(receipt["code"], code);
        let writers: Vec<_> = ["root", "child", "grandchild"]
            .iter()
            .map(|role| {
                let pid: u32 = std::fs::read_to_string(self.directory.path().join(format!("{role}.pid")))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(tj_cli::process::agent::started_at(pid).unwrap().is_none(), "{role} is still alive");
                #[cfg(unix)]
                assert!(!Path::new(&format!("/proc/{pid}")).exists(), "{role} was not reaped");
                let path = self.directory.path().join(format!("{role}.writer"));
                let size = path.metadata().unwrap().len();
                (path, size)
            })
            .collect();
        let unrelated = self.directory.path().join("unrelated.writer");
        let previous = unrelated.metadata().unwrap().len();
        std::thread::sleep(Duration::from_millis(200));
        for (path, size) in writers {
            assert_eq!(path.metadata().unwrap().len(), size, "an owned descendant kept writing");
        }
        assert!(self.unrelated.try_wait().unwrap().is_none(), "cleanup killed an unrelated child");
        assert!(unrelated.metadata().unwrap().len() > previous, "unrelated writer stopped progressing");
    }
}

impl Drop for WriterTree {
    fn drop(&mut self) {
        // Also stop every fixture on assertion failure, even if containment regresses.
        let _ = std::fs::write(self.directory.path().join("stop"), "");
        let _ = std::fs::write(self.directory.path().join("update.cancel"), "writer-test");
        let deadline = Instant::now() + Duration::from_secs(12);
        while self.helper.try_wait().is_ok_and(|status| status.is_none()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.helper.kill();
        let _ = self.helper.wait();
        let _ = self.unrelated.kill();
        let _ = self.unrelated.wait();
    }
}

#[test]
fn bounded_updater_timeout_reaps_child_and_grandchild_writers() {
    WriterTree::start("hang").prove_settled("timed_out", 124);
}

#[test]
fn bounded_updater_normal_exit_still_reaps_descendant_writers() {
    WriterTree::start("root-exit").prove_settled("exited", 0);
}

#[test]
fn bounded_updater_cancellation_reaps_child_and_grandchild_writers() {
    let mut tree = WriterTree::start("hang");
    let deadline = Instant::now() + Duration::from_secs(4);
    while !tree.ready() {
        assert!(Instant::now() < deadline, "dummy writer generations did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    tree.cancel();
    tree.prove_settled("cancelled", 125);
}
