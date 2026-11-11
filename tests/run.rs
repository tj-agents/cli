use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const ITEM: &str = "github:github.com/acme/app:1";
const BOUND: &str =
    r#"{"ok":true,"data":{"launch":{"state":"active"},"session":{"native_id":"sess-1","state":"active"}}}"#;
const CLOSING: &str =
    r#"{"ok":true,"data":{"launch":{"state":"closing"},"session":{"native_id":"sess-1","state":"active"}}}"#;
const PREBOUND: &str = "123e4567-e89b-12d3-a456-426614174000";
const MERGED: &str = r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#;

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
             if exist \"%~dp0{name}-%1.failed\" exit /b 1\r\n\
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
                 if [ -f \"$here/{name}-$1.failed\" ]; then exit 1; fi\n\
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
    #[cfg(windows)]
    std::fs::write(stubs.join("agent-store.cmd"), "@echo off\r\npython \"%~dp0run_store.py\" %*\r\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = stubs.join("agent-store");
        std::fs::write(&path, "#!/bin/sh\nexec python3 \"$(dirname \"$0\")/run_store.py\" \"$@\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_store.py"),
        stubs.join("run_store.py"),
    )
    .unwrap();
    set_store_list(&stubs, "sessions", serde_json::json!([]));
    stub(&stubs, "tj-item", r#"{"prs":[],"all_merged":false}"#);
    stub(&stubs, "tj-prepare", "prepared");
    stub(&stubs, "codex", "queued");
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
    bind(sandbox, launch, host, false);
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

fn native_codex(sandbox: &Sandbox) -> PathBuf {
    let directory = sandbox.root.join("native");
    std::fs::create_dir(&directory).unwrap();
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    let executable = directory.join(name);
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    executable
}

fn prebound_arguments(sandbox: &Sandbox, launch: &str) -> Vec<String> {
    prebound_arguments_with_prompt(sandbox, launch, "Prompt")
}

fn prebound_arguments_with_prompt(sandbox: &Sandbox, launch: &str, prompt: &str) -> Vec<String> {
    let mut arguments = run_arguments(sandbox, launch, "codex", "exit", "10");
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(
        separator..,
        [
            "--session".to_owned(),
            format!("codex:{PREBOUND}"),
            "--".to_owned(),
            native_codex(sandbox).to_string_lossy().into_owned(),
            "--cd".to_owned(),
            sandbox.worktree.to_string_lossy().into_owned(),
            "resume".to_owned(),
            PREBOUND.to_owned(),
            "--".to_owned(),
            prompt.to_owned(),
        ],
    );
    arguments
}

fn prebind(sandbox: &Sandbox, foreign_native: bool) {
    std::fs::write(
        sandbox.stubs.join("agent-store-prebind.json"),
        serde_json::json!({"session": PREBOUND, "foreign_native": foreign_native}).to_string(),
    )
    .unwrap();
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

fn refused(sandbox: &Sandbox, child: &mut Child, launch: &str) {
    done(sandbox, launch, false);
    let mark = sandbox.root.join("cache").join("run").join(launch).join("done");
    let deadline = Instant::now() + Duration::from_secs(30);
    while mark.exists() {
        assert!(Instant::now() < deadline, "closeout never checked the done mark");
        std::thread::sleep(Duration::from_millis(100));
    }
    still_running(child, Duration::from_millis(500));
    assert!(!log(sandbox, "dummy.log").contains("graceful"), "the agent was stopped");
}

fn wait_for_polls(sandbox: &Sandbox, file: &str, prefix: &str, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while log(sandbox, file).lines().filter(|line| line.starts_with(prefix)).count() < count {
        assert!(Instant::now() < deadline, "{file} never logged {count} {prefix:?} lines");
        std::thread::sleep(Duration::from_millis(100));
    }
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

fn set_store_list(stubs: &Path, command: &str, rows: serde_json::Value) {
    std::fs::write(
        stubs.join(format!("agent-store-{command}.json")),
        serde_json::json!({"ok": true, "data": {command: rows}}).to_string(),
    )
    .unwrap();
}

fn bind(sandbox: &Sandbox, launch: &str, host: &str, closing: bool) {
    let show = sandbox.stubs.join("agent-store-show.json");
    let mut value = serde_json::json!({
        "ok": true, "data": {
            "launch": {"launch_id": launch, "host": host, "session": format!("{host}:sess-1"), "state": "active"},
            "session": {"session": format!("{host}:sess-1"), "native_id": "sess-1", "host": host,
                "launch_id": launch, "worktree": sandbox.worktree, "state": "active"}
        }
    });
    if show.exists() {
        let old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&show).unwrap()).unwrap();
        for key in ["launch", "session"] {
            if let Some(fields) = old["data"][key].as_object() {
                value["data"][key].as_object_mut().unwrap().extend(fields.clone());
            } else if key == "session" {
                value["data"][key] = serde_json::Value::Null;
            }
        }
    }
    if closing {
        value["data"]["launch"]["state"] = serde_json::json!("closing");
    }
    std::fs::write(&show, value.to_string()).unwrap();
    if !sandbox.stubs.join("agent-store-worktrees.json").exists() {
        if sandbox.worktree.exists() {
            claim(sandbox, launch, host, std::slice::from_ref(&sandbox.worktree));
        } else {
            set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([]));
        }
    }
}

fn claim(sandbox: &Sandbox, launch: &str, host: &str, paths: &[PathBuf]) {
    let rows: Vec<_> = paths
        .iter()
        .map(|path| {
            serde_json::json!({
                "path": path.canonicalize().unwrap(), "session": format!("{host}:sess-1"), "launch_id": launch,
                "claimed_at": "2026-11-11T00:00:00Z"
            })
        })
        .collect();
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!(rows));
}

fn set_reply(sandbox: &Sandbox, file: &str, json: &str) {
    let path = sandbox.stubs.join(file);
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    if file == "agent-store-show.json" && path.exists() {
        let old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for key in ["launch", "session"] {
            if let Some(fields) = value["data"][key].as_object_mut() {
                if let Some(previous) = old["data"][key].as_object() {
                    for (name, field) in previous {
                        fields.entry(name).or_insert_with(|| field.clone());
                    }
                }
            }
        }
    }
    std::fs::write(path, value.to_string()).unwrap();
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
fn an_item_merged_before_the_session_started_never_stops_it() {
    let sandbox = sandbox();
    set_reply(&sandbox, "tj-item-prs.json", MERGED);
    let mut child = start(&sandbox, "merged-before", "claude", "signal", "10");
    wait_for_polls(&sandbox, "stubs/agent-store.log", "show --launch-id merged-before", 4);
    still_running(&mut child, Duration::from_millis(500));
    assert!(sandbox.worktree.exists());
    assert!(!log(&sandbox, "dummy.log").contains("graceful"), "the agent was stopped");
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "merged-before");
}

#[test]
fn a_merge_mid_session_asks_the_agent_once_and_only_its_done_closes_out() {
    let sandbox = sandbox();
    let keys = cfg!(windows);
    let mut child = start(&sandbox, "merged-during", "codex", if keys { "keys" } else { "signal" }, "10");
    wait_for_polls(&sandbox, "stubs/tj-item.log", "prs ", 1);
    set_reply(&sandbox, "tj-item-prs.json", MERGED);
    wait_for_polls(&sandbox, "stubs/codex.log", "queue --thread sess-1 --message", 1);
    wait_for_polls(&sandbox, "stubs/agent-store.log", "show --launch-id merged-during", 8);
    still_running(&mut child, Duration::from_millis(500));
    assert!(sandbox.worktree.exists());
    assert!(!log(&sandbox, "dummy.log").contains("graceful"), "the agent was stopped");
    let asked = log(&sandbox, "stubs/codex.log");
    assert_eq!(asked.lines().count(), 1, "{asked}");
    assert!(asked.contains("tj done"), "{asked}");
    done(&sandbox, "merged-during", false);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "merged-during");
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
        claim(&sandbox, &launch, host, std::slice::from_ref(&sandbox.worktree));
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
        done(&sandbox, &launch, false);
        let output = finish(child, Duration::from_secs(60));
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
        let mut child = start(&sandbox, launch, "claude", "signal", "2");
        refused(&sandbox, &mut child, launch);
        assert!(sandbox.worktree.exists());
        assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
        binding["data"]["session"]["launch_id"] = serde_json::json!(launch);
        binding["data"]["session"]["host"] = serde_json::json!("claude");
        binding["data"]["session"]["worktree"] = serde_json::json!(sandbox.worktree);
        set_reply(&sandbox, "agent-store-show.json", &binding.to_string());
        done(&sandbox, launch, false);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, launch);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("not bound") || stderr.contains("not absolute"), "{stderr}");
    }
}

#[test]
fn an_orphaned_launch_cleans_its_bound_worktree_when_the_launch_directory_is_gone() {
    for (host, state) in [("claude", "closing"), ("codex", "closing"), ("claude", "ended"), ("codex", "ended")] {
        let sandbox = sandbox();
        let launch = format!("orphan-{host}");
        let row = serde_json::json!({
            "launch_id": launch, "host": host, "title": "Orphan",
            "worktree": sandbox.root.join("gone"), "state": state, "end_reason": "exited",
            "item": {"ref": ITEM},
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
        bind(&sandbox, &launch, host, true);
        set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
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
    assert!(log(&sandbox, "dummy.log").contains("graceful"), "the agent was not stopped gracefully");
    assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty(), "the merged branch is still there");
}

#[test]
fn new_input_cancels_done_and_mark_closing_still_closes() {
    let sandbox = sandbox();
    let mut child = start(&sandbox, "cancel", "claude", "signal", "10");
    done(&sandbox, "cancel", false);
    done(&sandbox, "cancel", true);
    still_running(&mut child, Duration::from_secs(5));
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
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
    refused(&sandbox, &mut child, "unmerged");
    assert!(sandbox.worktree.exists());
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    git(&sandbox.primary, &["merge", "--squash", "Feature/1"]);
    git(&sandbox.primary, &["commit", "-q", "-m", "squash work"]);
    git(&sandbox.primary, &["push", "-q", "origin", "main"]);
    done(&sandbox, "unmerged", false);
    let output = finish(child, Duration::from_secs(60));
    assert!(String::from_utf8_lossy(&output.stderr).contains("worktree has unmerged commits"));
    assert_eq!(output.status.code(), Some(0));
    assert!(!sandbox.worktree.exists());
}

#[test]
fn an_agent_ignoring_the_stop_is_killed_with_its_tree() {
    let sandbox = sandbox();
    let child = start(&sandbox, "stubborn", "claude", "stubborn", "2");
    done(&sandbox, "stubborn", false);
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
fn a_prebound_native_codex_session_registers_and_verifies_before_spawning() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    let output =
        finish(spawn(&sandbox, &prebound_arguments(&sandbox, "prebound"), &sandbox.root), Duration::from_secs(30));
    assert!(!output.status.success());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(store.contains(&format!(
        "register --session codex:{PREBOUND} --cwd {} --launch-id prebound",
        sandbox.worktree.display()
    )));
    assert!(store.matches("show ").count() >= 4, "{store}");
    assert!(store.contains("end --launch-id prebound --reason exited"), "{store}");
    assert!(!store.contains("--reason not_started"), "{store}");
}

#[test]
fn a_delimited_option_shaped_prompt_is_registered_before_spawning() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    let output = finish(
        spawn(&sandbox, &prebound_arguments_with_prompt(&sandbox, "option-prompt", "--model=other"), &sandbox.root),
        Duration::from_secs(30),
    );
    assert!(!output.status.success());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(store.contains(&format!("register --session codex:{PREBOUND}")), "{store}");
    assert!(store.contains("end --launch-id option-prompt --reason exited"), "{store}");
    assert!(!store.contains("--reason not_started"), "{store}");
}

#[test]
fn a_prebind_registration_failure_ends_only_the_verified_launch() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    set_reply(&sandbox, "agent-store-register.json", r#"{"ok":false,"error":{"message":"storage failed"}}"#);
    let output =
        finish(spawn(&sandbox, &prebound_arguments(&sandbox, "not-started"), &sandbox.root), Duration::from_secs(30));
    assert!(!output.status.success());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(store.contains("launch --launch-id not-started --host codex"), "{store}");
    assert!(store.contains("register --session codex:"), "{store}");
    assert_eq!(store.matches("end --launch-id not-started --reason not_started").count(), 1, "{store}");
    assert!(!store.contains("--reason exited"), "{store}");
}

#[test]
fn a_malformed_reciprocal_binding_never_ends_or_spawns_the_native_session() {
    let sandbox = sandbox();
    prebind(&sandbox, true);
    let output =
        finish(spawn(&sandbox, &prebound_arguments(&sandbox, "foreign"), &sandbox.root), Duration::from_secs(30));
    assert!(!output.status.success());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(store.contains("register --session codex:"), "{store}");
    assert!(!store.contains("end --launch-id foreign"), "{store}");
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

#[test]
fn an_agent_that_exits_after_delivery_cleans_its_owned_worktrees() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        set_reply(&sandbox, "tj-item-prs.json", r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#);
        let launch = format!("delivered-exit-{host}");
        let mut arguments = run_arguments(&sandbox, &launch, host, "exit", "10");
        let poll = arguments.iter().position(|argument| argument == "--poll-seconds").unwrap() + 1;
        arguments[poll] = "60".into();
        let child = spawn(&sandbox, &arguments, &sandbox.root);
        let output = finish(child, Duration::from_secs(60));
        assert_eq!(output.status.code(), Some(3), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(!sandbox.worktree.exists());
        assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        let store = log(&sandbox, "stubs/agent-store.log");
        assert!(store.contains(&format!("end --launch-id {launch} --reason closed")), "{store}");
        assert!(!store.contains("--reason exited"), "{store}");
        assert!(log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    }
}

#[cfg(windows)]
#[test]
fn codex_is_stopped_with_a_ctrl_c_keypress() {
    let sandbox = sandbox();
    let child = start(&sandbox, "codex", "codex", "keys", "10");
    wait_for_log(&sandbox, "raw");
    done(&sandbox, "codex", false);
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
    done(&sandbox, "pane", false);
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
    set_reply(&sandbox, "agent-store-show.json", CLOSING);
    std::fs::remove_file(sandbox.stubs.join("agent-store-show.json")).unwrap();
    bind(&sandbox, "lonely", "claude", true);
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

#[test]
fn closes_out_all_owned_worktrees_across_repositories() {
    for host in ["claude", "codex"] {
        let other = sandbox();
        let sandbox = sandbox();
        let extra = sandbox.root.join("extra worktree");
        git(&sandbox.primary, &["worktree", "add", "-q", extra.to_str().unwrap(), "-b", "Feature/2"]);
        let launch = format!("collection-{host}");
        let keys = cfg!(windows) && host == "codex";
        let mut arguments = run_arguments(&sandbox, &launch, host, if keys { "keys" } else { "signal" }, "2");
        let cwd = arguments.iter().position(|arg| arg == "--cwd").unwrap() + 1;
        arguments[cwd] = sandbox.primary.to_string_lossy().into_owned();
        claim(&sandbox, &launch, host, &[sandbox.worktree.clone(), extra.clone(), other.worktree.clone()]);
        let child = spawn(&sandbox, &arguments, &sandbox.root);
        wait_for_log(&sandbox, if keys { "raw" } else { "agent " });
        done(&sandbox, &launch, false);
        let output = finish(child, Duration::from_secs(120));
        assert_closed_out(&sandbox, &output, &launch);
        assert!(!extra.exists());
        assert!(!other.worktree.exists());
        assert!(sandbox.primary.exists() && other.primary.exists());
        assert!(git(&sandbox.primary, &["branch", "--list", "Feature/2"]).is_empty());
        assert!(git(&other.primary, &["branch", "--list", "Feature/1"]).is_empty());
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("release-worktree ").count(), 3);
    }
}

#[test]
fn one_dirty_owned_worktree_keeps_every_worktree_and_the_agent_open() {
    let sandbox = sandbox();
    let extra = sandbox.root.join("dirty worktree");
    git(&sandbox.primary, &["worktree", "add", "-q", extra.to_str().unwrap(), "-b", "Feature/2"]);
    let mut child = start(&sandbox, "dirty-collection", "claude", "signal", "2");
    claim(&sandbox, "dirty-collection", "claude", &[sandbox.worktree.clone(), extra.clone()]);
    std::fs::write(extra.join("unfinished.txt"), "only copy").unwrap();
    refused(&sandbox, &mut child, "dirty-collection");
    assert!(sandbox.worktree.exists() && extra.exists());
    assert!(!log(&sandbox, "stubs/agent-store.log").contains("release-worktree"));
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    std::fs::remove_file(extra.join("unfinished.txt")).unwrap();
    done(&sandbox, "dirty-collection", false);
    let output = finish(child, Duration::from_secs(120));
    assert_closed_out(&sandbox, &output, "dirty-collection");
    assert!(!extra.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("worktree has uncommitted changes").count(), 1, "{stderr}");
}

#[test]
fn another_live_session_blocks_closeout_even_when_its_metadata_says_ended() {
    let sandbox = sandbox();
    let pid = std::process::id();
    let started = tj_cli::process::agent::started_at(pid).unwrap().unwrap();
    let started: chrono::DateTime<chrono::Utc> = started.into();
    set_store_list(
        &sandbox.stubs,
        "sessions",
        serde_json::json!([{
            "session": "codex:other", "state": "ended", "worktree": sandbox.worktree,
            "wrapper": {"pid": pid, "started_at": started.to_rfc3339()}
        }]),
    );
    let mut child = start(&sandbox, "live-owner", "claude", "signal", "2");
    refused(&sandbox, &mut child, "live-owner");
    assert!(sandbox.worktree.exists());
    set_store_list(&sandbox.stubs, "sessions", serde_json::json!([]));
    done(&sandbox, "live-owner", false);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "live-owner");
    assert!(String::from_utf8_lossy(&output.stderr).contains("another session may still be using"));
}

#[test]
fn release_failure_keeps_the_claim_and_never_marks_done() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-release-worktree.json", r#"{"ok":false,"error":{"message":"storage failed"}}"#);
    let child = start(&sandbox, "failed-release", "claude", "signal", "2");
    done(&sandbox, "failed-release", false);
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(1));
    assert!(!sandbox.worktree.exists());
    assert!(!sandbox.stubs.join("released.json").exists());
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    assert!(!log(&sandbox, "stubs/agent-store.log").contains("--reason closed"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("storage failed"));
    assert!(sandbox.root.join("cache/run/failed-release.closeout.json").is_file());
    std::fs::remove_file(sandbox.stubs.join("agent-store-release-worktree.json")).unwrap();
    let output = retry_closeout(&sandbox, "failed-release");
    assert_closed_out(&sandbox, &output, "failed-release");
    assert!(!sandbox.root.join("cache/run/failed-release.closeout.json").exists());
}

fn retry_closeout(sandbox: &Sandbox, launch: &str) -> Output {
    set_store_list(
        &sandbox.stubs,
        "launches",
        serde_json::json!([{
            "launch_id": launch, "host": "claude", "title": "Test tab",
            "worktree": sandbox.worktree, "state": "ended", "end_reason": "exited",
            "item": {"ref": ITEM},
            "wrapper": {"pid": 0, "started_at": "2026-01-01T00:00:00Z"}
        }]),
    );
    Command::new(env!("CARGO_BIN_EXE_tj"))
        .arg("closeout")
        .envs(environment(sandbox))
        .current_dir(&sandbox.root)
        .output()
        .unwrap()
}

#[test]
fn tracker_and_launch_finalization_failures_can_be_retried_after_all_releases() {
    for stage in ["tracker", "end"] {
        let sandbox = sandbox();
        let failed = if stage == "tracker" { "tj-item-progress.failed" } else { "agent-store-end.json" };
        set_reply(&sandbox, failed, r#"{"ok":false,"error":{"message":"finalization failed"}}"#);
        let launch = format!("finalize-{stage}");
        let child = start(&sandbox, &launch, "claude", "signal", "2");
        done(&sandbox, &launch, false);
        let output = finish(child, Duration::from_secs(60));
        assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(!sandbox.worktree.exists());
        assert!(sandbox.stubs.join("released.json").is_file());
        let journal = sandbox.root.join("cache/run").join(format!("{launch}.closeout.json"));
        assert!(journal.is_file());
        std::fs::remove_file(sandbox.stubs.join(failed)).unwrap();
        git(&sandbox.primary, &["branch", "Feature/1"]);
        let ended = log(&sandbox, "stubs/agent-store.log").matches("--reason closed").count();
        let refused = retry_closeout(&sandbox, &launch);
        assert_eq!(refused.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&refused.stdout).contains("branch was retained"));
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("--reason closed").count(), ended);
        assert!(journal.is_file());
        assert!(!git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        git(&sandbox.primary, &["branch", "-d", "Feature/1"]);
        let output = retry_closeout(&sandbox, &launch);
        assert_closed_out(&sandbox, &output, &launch);
        assert!(!journal.exists());
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("release-worktree ").count(), 1);
    }
}

#[test]
fn changes_written_during_shutdown_are_kept_and_never_marked_done() {
    let sandbox = sandbox();
    std::fs::write(
        sandbox.root.join("dirty-on-stop"),
        sandbox.worktree.join("unfinished.txt").to_string_lossy().as_bytes(),
    )
    .unwrap();
    let child = start(&sandbox, "shutdown-change", "claude", "signal", "2");
    done(&sandbox, "shutdown-change", false);
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(sandbox.worktree.join("unfinished.txt")).unwrap(), "unfinished");
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    assert!(!log(&sandbox, "stubs/agent-store.log").contains("release-worktree"));
}

#[test]
fn an_unclaimed_linked_worktree_requires_recovery_before_closing() {
    let sandbox = sandbox();
    let mut child = start(&sandbox, "unclaimed", "claude", "signal", "2");
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([]));
    refused(&sandbox, &mut child, "unclaimed");
    assert!(sandbox.worktree.exists());
    claim(&sandbox, "unclaimed", "claude", std::slice::from_ref(&sandbox.worktree));
    done(&sandbox, "unclaimed", false);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "unclaimed");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no recorded ownership; recovery required"));
}
