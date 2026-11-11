use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const ITEM: &str = "github:github.com/acme/app:1";
const BOUND: &str =
    r#"{"ok":true,"data":{"launch":{"state":"active"},"session":{"native_id":"sess-1","state":"active"}}}"#;
const CLOSING: &str =
    r#"{"ok":true,"data":{"launch":{"state":"closing"},"session":{"native_id":"sess-1","state":"active"}}}"#;

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
             if exist \"%~dp0{name}-%1.json\" (type \"%~dp0{name}-%1.json\") else (echo {fallback})\r\n\
             if \"{name}\"==\"tj-prepare\" python \"%~dp0prepare.py\"\r\n"
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
                 if [ -f \"$here/{name}-$1.json\" ]; then cat \"$here/{name}-$1.json\"; else echo '{fallback}'; fi\n\
                 if [ '{name}' = 'tj-prepare' ]; then exec python3 \"$here/prepare.py\"; fi\n"
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
    stub(&stubs, "tj-prepare", "prepared");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prepare.py"), stubs.join("prepare.py"))
        .unwrap();
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

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("dummy_agent.py")
}

fn run_arguments(sandbox: &Sandbox, launch: &str, host: &str, mode: &str, grace: &str) -> Vec<String> {
    let fixture = fixture();
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
        "--restart-grace-seconds",
        "2",
        "--",
        python(),
        &fixture.to_string_lossy(),
        mode,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn start(sandbox: &Sandbox, launch: &str, host: &str, mode: &str, grace: &str) -> Child {
    let child = spawn(sandbox, &run_arguments(sandbox, launch, host, mode, grace), &sandbox.root);
    wait_for_log(sandbox, "agent ");
    child
}

fn spawn(sandbox: &Sandbox, arguments: &[String], cwd: &Path) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
    command
        .args(arguments)
        .current_dir(cwd)
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
    command.spawn().unwrap()
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

fn request_restart(sandbox: &Sandbox, launch: &str, reason: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["restart", "--reason", reason])
        .env("TJ_RUN_LAUNCH_ID", launch)
        .env("TJ_CACHE_DIR", sandbox.root.join("cache"))
        .output()
        .unwrap()
}

fn mark_idle(sandbox: &Sandbox, launch: &str, background: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["idle", "--background", background])
        .env("TJ_RUN_LAUNCH_ID", launch)
        .env("TJ_CACHE_DIR", sandbox.root.join("cache"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
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
fn closes_out_the_worktree_bound_after_launch() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        git(&sandbox.primary, &["worktree", "remove", sandbox.worktree.to_str().unwrap()]);
        let launch = format!("later-worktree-{host}");
        let keys = cfg!(windows) && host == "codex";
        let mode = if keys { "keys" } else { "signal" };
        let mut arguments = run_arguments(&sandbox, &launch, host, mode, "2");
        let cwd = arguments.iter().position(|argument| argument == "--cwd").unwrap() + 1;
        arguments[cwd] = sandbox.primary.to_string_lossy().into_owned();
        let child = spawn(&sandbox, &arguments, &sandbox.root);
        wait_for_log(&sandbox, if keys { "raw" } else { "agent " });
        git(&sandbox.primary, &["worktree", "add", "-q", sandbox.worktree.to_str().unwrap(), "Feature/1"]);
        let binding = serde_json::json!({
            "ok": true,
            "data": {
                "launch": {"launch_id": launch, "state": "active", "cwd": sandbox.primary},
                "session": {
                    "native_id": "sess-1", "state": "active", "host": host,
                    "launch_id": launch, "worktree": sandbox.worktree
                }
            }
        });
        set_reply(&sandbox, "agent-store-show.json", &binding.to_string());
        set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
        let output = finish(child, Duration::from_secs(30));
        assert_closed_out(&sandbox, &output, &launch);
        let store = log(&sandbox, "stubs/agent-store.log");
        let recorded = store.lines().find(|line| line.starts_with("launch ")).unwrap();
        assert!(recorded.contains(sandbox.primary.to_str().unwrap()), "{recorded}");
        assert!(!recorded.contains(sandbox.worktree.to_str().unwrap()), "{recorded}");
        assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
    }
}

#[test]
fn an_unrelated_worktree_binding_does_not_stop_or_clean_the_agent() {
    for (field, wrong) in [("launch_id", "another-launch"), ("host", "codex"), ("worktree", "relative/path")] {
        let sandbox = sandbox();
        let launch = "wrong-binding";
        let mut binding = serde_json::json!({
            "ok": true,
            "data": {
                "launch": {"launch_id": launch, "state": "active"},
                "session": {
                    "native_id": "sess-1", "state": "active", "host": "claude",
                    "launch_id": launch, "worktree": sandbox.worktree
                }
            }
        });
        binding["data"]["session"][field] = serde_json::Value::String(wrong.to_owned());
        set_reply(&sandbox, "agent-store-show.json", &binding.to_string());
        set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
        let mut child = start(&sandbox, launch, "claude", "signal", "2");
        still_running(&mut child, Duration::from_secs(2));
        assert!(sandbox.worktree.exists());
        assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
        binding["data"]["session"]["launch_id"] = serde_json::json!(launch);
        binding["data"]["session"]["host"] = serde_json::json!("claude");
        binding["data"]["session"]["worktree"] = serde_json::json!(sandbox.worktree);
        set_reply(&sandbox, "agent-store-show.json", &binding.to_string());
        let output = finish(child, Duration::from_secs(30));
        assert_closed_out(&sandbox, &output, launch);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("not bound") || stderr.contains("not absolute"), "{stderr}");
    }
}

#[test]
fn an_orphaned_launch_cleans_its_bound_worktree_when_the_launch_directory_is_gone() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("orphan-{host}");
        let row = serde_json::json!({
            "launch_id": launch, "host": host, "title": "Orphan",
            "worktree": sandbox.root.join("gone"), "state": "closing",
            "wrapper": {"pid": 0, "started_at": "2026-01-01T00:00:00Z"}
        });
        set_reply(
            &sandbox,
            "agent-store-launches.json",
            &serde_json::json!({
                "ok": true, "data": {"launches": [row]}
            })
            .to_string(),
        );
        set_reply(
            &sandbox,
            "agent-store-show.json",
            &serde_json::json!({
                "ok": true,
                "data": {
                    "launch": {"launch_id": launch},
                    "session": {
                        "native_id": "sess-1", "host": host,
                        "launch_id": launch, "worktree": sandbox.worktree
                    }
                }
            })
            .to_string(),
        );
        let output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .arg("closeout")
            .envs(environment(&sandbox))
            .current_dir(&sandbox.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(!sandbox.worktree.exists(), "the bound worktree is still there");
        assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        assert!(log(&sandbox, "stubs/agent-store.log").contains(&format!("end --launch-id {launch} --reason closed")));
    }
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
fn the_launch_is_recorded_before_the_agent_starts() {
    let sandbox = sandbox();
    let mut arguments = run_arguments(&sandbox, "first", "claude", "exit", "10");
    arguments.truncate(arguments.iter().position(|argument| argument == "--").unwrap() + 1);
    let store_log = sandbox.stubs.join("agent-store.log");
    arguments.extend([
        python().to_owned(),
        "-c".to_owned(),
        format!("import os, shutil; shutil.copyfile(r'{}', os.environ['DUMMY_LOG'])", store_log.display()),
    ]);
    let child = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(arguments)
        .envs(environment(&sandbox))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    finish(child, Duration::from_secs(60));
    let seen_by_agent = log(&sandbox, "dummy.log");
    assert!(seen_by_agent.contains("launch --launch-id first --host"), "{seen_by_agent}");
}

#[test]
fn repository_preparation_finishes_before_the_agent_starts() {
    let sandbox = sandbox();
    let mut arguments = run_arguments(&sandbox, "prepared", "codex", "exit", "10");
    arguments.truncate(arguments.iter().position(|argument| argument == "--").unwrap() + 1);
    let preparation_log = sandbox.stubs.join("tj-prepare.log");
    arguments.extend([
        python().to_owned(),
        "-c".to_owned(),
        format!("import os, shutil; shutil.copyfile(r'{}', os.environ['DUMMY_LOG'])", preparation_log.display()),
        "multiline\nprompt & | < > ^ %".to_owned(),
    ]);
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    let output = finish(child, Duration::from_secs(30));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let prepared = log(&sandbox, "dummy.log");
    assert!(prepared.contains("--host codex --cwd"), "{prepared}");
    assert!(prepared.contains(sandbox.worktree.to_str().unwrap()), "{prepared}");
    let input: Vec<String> = serde_json::from_str(&log(&sandbox, "stubs/prepare-input.json")).unwrap();
    assert_eq!(input.last().unwrap(), "multiline\nprompt & | < > ^ %");
}

#[test]
fn repository_preparation_failure_warns_and_still_starts_the_agent() {
    let sandbox = sandbox();
    std::fs::write(sandbox.stubs.join("prepare-failure"), "failed").unwrap();
    let child = start(&sandbox, "prepare-failed", "claude", "exit", "10");
    let output = finish(child, Duration::from_secs(30));
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("standards preparation failed"));
    assert!(log(&sandbox, "dummy.log").contains("agent prepare-failed"));
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
    struct Server(PathBuf);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux").arg("-S").arg(&self.0).arg("kill-server").output();
        }
    }
    let _server = Server(socket.clone());
    let quote = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
    let script = std::iter::once(env!("CARGO_BIN_EXE_tj").to_owned())
        .chain(run_arguments(&sandbox, "pane", "codex", "keys", "10"))
        .map(|argument| quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    let mut tmux = Command::new("tmux");
    tmux.env("PATH", search_path(&sandbox)).arg("-S").arg(&socket).args([
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "run",
        "-x",
        "120",
        "-y",
        "40",
    ]);
    for (name, value) in environment(&sandbox) {
        tmux.arg("-e").arg(format!("{name}={}", value.to_string_lossy()));
    }
    assert!(tmux.arg(script).status().unwrap().success());
    wait_for_log(&sandbox, "raw");
    let deadline = Instant::now() + Duration::from_secs(60);
    while Command::new("tmux").arg("-S").arg(&socket).args(["has-session", "-t", "run"]).status().unwrap().success() {
        if Instant::now() > deadline {
            let pane = Command::new("tmux").arg("-S").arg(&socket).args(["capture-pane", "-p", "-t", "run"]).output();
            let processes = Command::new("ps").args(["-eo", "pid,ppid,pgid,tpgid,stat,args"]).output();
            panic!(
                "the tmux pane did not close
pane:
{}
agent: {}
store: {}
item: {}
processes:
{}",
                pane.map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default(),
                log(&sandbox, "dummy.log"),
                log(&sandbox, "stubs/agent-store.log"),
                log(&sandbox, "stubs/tj-item.log"),
                processes.map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default(),
            );
        }
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

fn with_prompt(sandbox: &Sandbox, launch: &str, host: &str, mode: &str) -> Vec<String> {
    let mut arguments = run_arguments(sandbox, launch, host, mode, "10");
    arguments.extend(["--model", "m", "the prompt"].map(str::to_owned));
    arguments
}

fn not_restarted(sandbox: &Sandbox, child: &mut Child) {
    still_running(child, Duration::from_secs(5));
    let agent = log(sandbox, "dummy.log");
    assert!(!agent.contains("sess-1") && !agent.contains("graceful"), "restarted: {agent}");
}

fn restarts_in_place_once_idle(host: &str) {
    let sandbox = sandbox();
    let launch = format!("again-{host}");
    let keys = cfg!(windows) && host == "codex";
    let mode = if keys { "keys" } else { "signal" };
    set_reply(&sandbox, "agent-store-show.json", BOUND);
    let mut child = spawn(&sandbox, &with_prompt(&sandbox, &launch, host, mode), &sandbox.root);
    wait_for_log(&sandbox, if keys { "raw" } else { "agent " });
    assert!(request_restart(&sandbox, &launch, "plugins b").status.success());
    mark_idle(&sandbox, &launch, "0");
    let resumed = if host == "claude" { "args --model m --resume sess-1" } else { "args --model m resume sess-1" };
    wait_for_log(&sandbox, resumed);
    still_running(&mut child, Duration::from_secs(1));
    let agent = log(&sandbox, "dummy.log");
    if host == "claude" || keys {
        assert!(agent.contains("graceful"), "the agent was not stopped gracefully: {agent}");
    }
    assert_eq!(agent.matches(&format!("agent {launch} Test tab")).count(), 2, "{agent}");
    assert!(agent.lines().any(|line| line == "args --model m the prompt"), "{agent}");
    assert!(agent.lines().any(|line| line == resumed), "{agent}");
    let store = log(&sandbox, "stubs/agent-store.log");
    assert_eq!(store.matches(&format!("launch --launch-id {launch} ")).count(), 1, "{store}");
    assert!(!store.contains(&format!("end --launch-id {launch}")), "{store}");
    let repeated = request_restart(&sandbox, &launch, "plugins b");
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already restarted"));
    if keys {
        let deadline = Instant::now() + Duration::from_secs(30);
        while log(&sandbox, "dummy.log").matches("raw").count() < 2 {
            assert!(Instant::now() < deadline, "the resumed agent never read keys");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, &launch);
}

#[test]
fn a_requested_restart_resumes_an_idle_claude_in_the_same_launch() {
    restarts_in_place_once_idle("claude");
}

#[test]
fn a_requested_restart_resumes_an_idle_codex_in_the_same_launch() {
    restarts_in_place_once_idle("codex");
}

#[test]
fn a_restart_waits_for_an_idle_mark_with_no_background_work() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-show.json", BOUND);
    let mut child = spawn(&sandbox, &with_prompt(&sandbox, "busy", "claude", "signal"), &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    assert!(request_restart(&sandbox, "busy", "plugins b").status.success());
    not_restarted(&sandbox, &mut child);
    mark_idle(&sandbox, "busy", "unknown");
    not_restarted(&sandbox, &mut child);
    mark_idle(&sandbox, "busy", "2");
    not_restarted(&sandbox, &mut child);
    mark_idle(&sandbox, "busy", "0");
    wait_for_log(&sandbox, "--resume sess-1");
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "busy");
}

#[test]
fn a_new_prompt_clears_the_idle_mark_and_holds_the_restart() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-show.json", BOUND);
    let mut child = spawn(&sandbox, &with_prompt(&sandbox, "typed", "claude", "signal"), &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    assert!(request_restart(&sandbox, "typed", "plugins b").status.success());
    mark_idle(&sandbox, "typed", "0");
    done(&sandbox, "typed", true);
    assert!(!sandbox.root.join("cache").join("run").join("typed").join("idle").exists());
    not_restarted(&sandbox, &mut child);
    mark_idle(&sandbox, "typed", "0");
    wait_for_log(&sandbox, "--resume sess-1");
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "typed");
}

#[test]
fn a_restart_without_a_bound_session_keeps_the_agent_and_says_so_once() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-show.json", r#"{"ok":true,"data":{"launch":{"state":"active"},"session":null}}"#);
    let mut child = spawn(&sandbox, &with_prompt(&sandbox, "lonely", "claude", "signal"), &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    for _ in 0..2 {
        assert!(request_restart(&sandbox, "lonely", "plugins b").status.success());
        mark_idle(&sandbox, "lonely", "0");
        still_running(&mut child, Duration::from_secs(5));
    }
    set_reply(&sandbox, "agent-store-show.json", r#"{"ok":true,"data":{"launch":{"state":"closing"},"session":null}}"#);
    let output = finish(child, Duration::from_secs(60));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("not restarting: no claude session is bound to launch lonely").count(), 1, "{stderr}");
    assert_eq!(log(&sandbox, "dummy.log").matches("agent lonely").count(), 1);
    assert_closed_out(&sandbox, &output, "lonely");
}

#[test]
fn restart_outside_tj_run_fails_but_idle_is_a_no_op() {
    let tj = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tj")).args(arguments).env_remove("TJ_RUN_LAUNCH_ID").output().unwrap()
    };
    let output = tj(&["restart"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only works inside a tj run session"));
    assert!(tj(&["idle", "--background", "0"]).status.success());
    assert!(!tj(&["idle", "--background", "some"]).status.success());
}

#[test]
fn a_shell_started_session_needs_only_the_agent_command() {
    let sandbox = sandbox();
    set_reply(
        &sandbox,
        "agent-store-launches.json",
        r#"{"ok":true,"data":{"launches":[{"launch_id":"x","host":"claude","title":"Feature-1","worktree":"w","state":"active","wrapper":{"pid":1,"started_at":"2026-01-01T00:00:00Z"}}]}}"#,
    );
    let fixture = fixture().to_string_lossy().into_owned();
    let arguments = ["run", "--host", "claude", "--", python(), &fixture, "exit"].map(str::to_owned);
    let output = finish(spawn(&sandbox, &arguments, &sandbox.worktree), Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(3), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let agent = log(&sandbox, "dummy.log");
    let line = agent.lines().find(|line| line.starts_with("agent ")).unwrap();
    let mut fields = line.splitn(4, ' ').skip(1);
    let (id, title, cwd) = (fields.next().unwrap(), fields.next().unwrap(), fields.next().unwrap());
    assert!(id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    assert_eq!(title, "Feature-1-2");
    assert_eq!(std::fs::canonicalize(cwd).unwrap(), std::fs::canonicalize(&sandbox.worktree).unwrap());
    assert!(
        log(&sandbox, "stubs/agent-store.log")
            .contains(&format!("launch --launch-id {id} --host claude --title Feature-1-2"))
    );

    let unknown = ["run", "--", python(), &fixture, "exit"].map(str::to_owned);
    let output = finish(spawn(&sandbox, &unknown, &sandbox.worktree), Duration::from_secs(60));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pass --host"));
}
