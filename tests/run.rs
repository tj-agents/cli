use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const ITEM: &str = "github:github.com/acme/app:1";

struct Sandbox {
    _directory: tempfile::TempDir,
    root: PathBuf,
    stubs: PathBuf,
    primary: PathBuf,
    worktree: PathBuf,
    log: PathBuf,
}

fn git(cwd: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn stub(directory: &Path, name: &str, fallback: &str) {
    #[cfg(windows)]
    std::fs::write(
        directory.join(format!("{name}.cmd")),
        format!(
            "@echo off\r\necho %*>>\"%~dp0{name}.log\"\r\n\
             if exist \"%~dp0{name}-%1.json\" (type \"%~dp0{name}-%1.json\") else (echo {fallback})\r\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nhere=\"$(dirname \"$0\")\"\necho \"$*\" >> \"$here/{name}.log\"\n\
                 if [ -f \"$here/{name}-$1.json\" ]; then cat \"$here/{name}-$1.json\"; else echo '{fallback}'; fi\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn sandbox() -> Sandbox {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let seed = root.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q"]);
    std::fs::write(seed.join("README.md"), "seed\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "seed"]);
    git(&root, &["clone", "-q", "--bare", "seed", "origin.git"]);
    git(&root, &["clone", "-q", "origin.git", "repo"]);
    let primary = root.join("repo");
    git(&primary, &["worktree", "add", "-q", "../repo.worktrees/Feature-1", "-b", "Feature/1"]);
    let stubs = root.join("stubs");
    std::fs::create_dir(&stubs).unwrap();
    stub(&stubs, "agent-store", r#"{"ok":true,"data":{}}"#);
    stub(&stubs, "tj-item", r#"{"prs":[],"all_merged":false}"#);
    for folder in ["home", "cache"] {
        std::fs::create_dir(root.join(folder)).unwrap();
    }
    let worktree = root.join("repo.worktrees").join("Feature-1");
    Sandbox { log: root.join("dummy.log"), _directory: directory, root, stubs, primary, worktree }
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn search_path(sandbox: &Sandbox) -> std::ffi::OsString {
    let worktrunk = which::which("git-wt").expect("git-wt is not on PATH: python -m tj_cli.worktrunk_install");
    let mut entries = vec![sandbox.stubs.clone(), worktrunk.parent().unwrap().to_path_buf()];
    entries.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(entries).unwrap()
}

fn environment(sandbox: &Sandbox) -> Vec<(&'static str, std::ffi::OsString)> {
    let home = sandbox.root.join("home").into_os_string();
    vec![
        ("PATH", search_path(sandbox)),
        ("TJ_CACHE_DIR", sandbox.root.join("cache").into_os_string()),
        ("DUMMY_LOG", sandbox.log.clone().into_os_string()),
        ("HOME", home.clone()),
        ("APPDATA", home.clone()),
        ("XDG_CONFIG_HOME", home),
    ]
}

fn run_arguments(sandbox: &Sandbox, launch: &str, host: &str, mode: &str, grace: &str) -> Vec<String> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("dummy_agent.py");
    [
        "run",
        "--host",
        host,
        "--title",
        "Test tab",
        "--cwd",
        &sandbox.worktree.to_string_lossy(),
        "--item",
        ITEM,
        "--launch-id",
        launch,
        "--poll-seconds",
        "1",
        "--idle-seconds",
        "2",
        "--grace-seconds",
        grace,
        "--",
        python(),
        &fixture.to_string_lossy(),
        mode,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn start(sandbox: &Sandbox, launch: &str, host: &str, mode: &str, grace: &str) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
    command
        .args(run_arguments(sandbox, launch, host, mode, grace))
        .envs(environment(sandbox))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command.spawn().unwrap();
    wait_for_log(sandbox, "agent ");
    child
}

fn log(sandbox: &Sandbox, name: &str) -> String {
    std::fs::read_to_string(sandbox.root.join(name)).unwrap_or_default()
}

fn wait_for_log(sandbox: &Sandbox, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log(sandbox, "dummy.log").contains(text) {
        assert!(Instant::now() < deadline, "the dummy agent never logged {text:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn finish(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("tj run did not exit; stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.wait_with_output().unwrap()
}

fn still_running(child: &mut Child, wait: Duration) {
    std::thread::sleep(wait);
    assert!(child.try_wait().unwrap().is_none(), "tj run exited");
}

fn done(sandbox: &Sandbox, launch: &str, cancel: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
    command.arg("done").env("TJ_RUN_LAUNCH_ID", launch).env("TJ_CACHE_DIR", sandbox.root.join("cache"));
    if cancel {
        command.arg("--cancel");
    }
    assert!(command.output().unwrap().status.success());
}

fn set_reply(sandbox: &Sandbox, file: &str, json: &str) {
    std::fs::write(sandbox.stubs.join(file), json).unwrap();
}

fn assert_closed_out(sandbox: &Sandbox, output: &Output, launch: &str) {
    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(!sandbox.worktree.exists(), "the worktree is still there");
    assert!(!git(&sandbox.primary, &["worktree", "list"]).contains("Feature-1"));
    let store = log(sandbox, "stubs/agent-store.log");
    assert!(store.contains(&format!("launch --launch-id {launch} --host")), "{store}");
    assert!(store.contains(&format!("end --launch-id {launch} --reason closed")), "{store}");
    assert!(log(sandbox, "stubs/tj-item.log").contains(&format!("progress {ITEM} --state Done")));
}

#[test]
fn closes_out_when_every_pr_has_merged() {
    let sandbox = sandbox();
    set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
    let child = start(&sandbox, "merged", "claude", "signal", "10");
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "merged");
    assert!(log(&sandbox, "dummy.log").contains("graceful"), "the agent was not stopped gracefully");
    assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty(), "the merged branch is still there");
}

#[test]
fn closes_out_after_done_and_idle() {
    let sandbox = sandbox();
    let child = start(&sandbox, "idle", "claude", "signal", "10");
    done(&sandbox, "idle", false);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "idle");
}

#[test]
fn new_input_cancels_done_and_mark_closing_still_closes() {
    let sandbox = sandbox();
    let mut child = start(&sandbox, "cancel", "claude", "signal", "10");
    done(&sandbox, "cancel", false);
    done(&sandbox, "cancel", true);
    still_running(&mut child, Duration::from_secs(5));
    set_reply(&sandbox, "agent-store-show.json", r#"{"ok":true,"data":{"launch":{"state":"closing"},"session":null}}"#);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "cancel");
}

#[test]
fn unmerged_commits_keep_the_session_open_until_delivered() {
    let sandbox = sandbox();
    std::fs::write(sandbox.worktree.join("work.txt"), "work\n").unwrap();
    git(&sandbox.worktree, &["add", "."]);
    git(&sandbox.worktree, &["commit", "-q", "-m", "work"]);
    let mut child = start(&sandbox, "unmerged", "claude", "signal", "10");
    done(&sandbox, "unmerged", false);
    still_running(&mut child, Duration::from_secs(5));
    assert!(sandbox.worktree.exists());
    set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
    let output = finish(child, Duration::from_secs(60));
    assert!(String::from_utf8_lossy(&output.stderr).contains("worktree has unmerged commits"));
    assert_eq!(output.status.code(), Some(0));
    assert!(!sandbox.worktree.exists());
}

#[test]
fn an_agent_ignoring_the_stop_is_killed_with_its_tree() {
    let sandbox = sandbox();
    set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
    let child = start(&sandbox, "stubborn", "claude", "stubborn", "2");
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "stubborn");
    assert!(!log(&sandbox, "dummy.log").contains("graceful"));
}

#[test]
fn an_agent_that_exits_ends_the_launch_and_keeps_the_worktree() {
    let sandbox = sandbox();
    let child = start(&sandbox, "exit", "claude", "exit", "10");
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(3));
    assert!(sandbox.worktree.exists());
    assert!(log(&sandbox, "stubs/agent-store.log").contains("end --launch-id exit --reason exited"));
}

#[cfg(windows)]
#[test]
fn codex_is_stopped_with_a_ctrl_c_keypress() {
    let sandbox = sandbox();
    set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
    let child = start(&sandbox, "codex", "codex", "keys", "10");
    wait_for_log(&sandbox, "raw");
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "codex");
    assert!(log(&sandbox, "dummy.log").contains("graceful"), "the keypress did not reach the agent");
}

#[cfg(unix)]
#[test]
fn tmux_pane_closes_after_codex_is_stopped_with_a_keypress() {
    if which::which("tmux").is_err() {
        panic!("tmux is required for the pane test");
    }
    let sandbox = sandbox();
    set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
    let socket = sandbox.root.join("tmux.sock");
    let quote = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
    let script = std::iter::once(env!("CARGO_BIN_EXE_tj").to_owned())
        .chain(run_arguments(&sandbox, "pane", "codex", "keys", "10"))
        .map(|argument| quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    let mut tmux = Command::new("tmux");
    tmux.arg("-S").arg(&socket).args(["-f", "/dev/null", "new-session", "-d", "-s", "run", "-x", "120", "-y", "40"]);
    for (name, value) in environment(&sandbox) {
        tmux.arg("-e").arg(format!("{name}={}", value.to_string_lossy()));
    }
    assert!(tmux.arg(script).status().unwrap().success());
    wait_for_log(&sandbox, "raw");
    let deadline = Instant::now() + Duration::from_secs(60);
    while Command::new("tmux").arg("-S").arg(&socket).args(["has-session", "-t", "run"]).status().unwrap().success() {
        assert!(Instant::now() < deadline, "the tmux pane did not close");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(log(&sandbox, "dummy.log").contains("graceful"), "the keypress did not reach the agent");
    assert!(!sandbox.worktree.exists());
    assert!(log(&sandbox, "stubs/agent-store.log").contains("end --launch-id pane --reason closed"));
}

#[test]
fn done_outside_tj_run_fails_but_cancel_is_a_no_op() {
    let run = |arguments: &[&str]| {
        let mut output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(arguments)
            .env_remove("TJ_RUN_LAUNCH_ID")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = String::new();
        output.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
        (output.wait().unwrap().success(), stderr)
    };
    let (succeeded, stderr) = run(&["done"]);
    assert!(!succeeded && stderr.contains("only works inside a tj run session"));
    assert!(run(&["done", "--cancel"]).0);
}
