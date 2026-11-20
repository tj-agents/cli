use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child as ProcessChild, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ITEM: &str = "github:github.com/acme/app:1";
const BOUND: &str =
    r#"{"ok":true,"data":{"launch":{"state":"active"},"session":{"native_id":"sess-1","state":"active"}}}"#;
const PREBOUND: &str = "123e4567-e89b-12d3-a456-426614174000";
const MERGED: &str = r#"{"prs":[{"url":"u","state":"merged"}],"all_merged":true}"#;

/// Own the wrapper until its output is collected, including assertion unwinding.
struct Child {
    process: Option<ProcessChild>,
    root: PathBuf,
    #[cfg(unix)]
    log: PathBuf,
}

impl Child {
    fn new(process: ProcessChild, sandbox: &Sandbox) -> Self {
        Self {
            process: Some(process),
            root: sandbox.root.clone(),
            #[cfg(unix)]
            log: sandbox.log.clone(),
        }
    }

    fn diagnostics(&self) -> String {
        let mut snapshots = Vec::new();
        for relative in ["dummy.log", "stubs/agent-store.log", "stubs/tj-item.log"] {
            let text = std::fs::read_to_string(self.root.join(relative)).unwrap_or_default();
            let lines: Vec<_> = text.lines().collect();
            snapshots.push(format!("{relative}:\n{}", lines[lines.len().saturating_sub(20)..].join("\n")));
        }
        if let Ok(entries) = std::fs::read_dir(self.root.join("state/completion")) {
            for entry in entries.flatten().filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json")) {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    snapshots.push(format!("completion: {text}"));
                }
            }
        }
        snapshots.join("\n")
    }

    fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            // The native fixture has its own process group. Verify it still belongs to this
            // live wrapper before stopping that group, including the fixture's grandchild.
            let wrapper = self.id().to_string();
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            if let Some(pid) = log.lines().rev().find_map(|line| line.strip_prefix("pid ")?.parse::<i32>().ok()) {
                if let (Some(group), Ok(stat)) =
                    (rustix::process::Pid::from_raw(pid), std::fs::read_to_string(format!("/proc/{pid}/stat")))
                {
                    let fields: Vec<_> =
                        stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
                    if fields.get(1) == Some(&wrapper.as_str()) && fields.get(2) == Some(&pid.to_string().as_str()) {
                        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                    }
                }
            }
        }
        // Windows closes the wrapper's kill-on-close job with its process handles.
        self.process.as_mut().unwrap().kill()
    }

    fn wait_with_output(mut self) -> std::io::Result<Output> {
        self.process.take().unwrap().wait_with_output()
    }
}

impl std::ops::Deref for Child {
    type Target = ProcessChild;

    fn deref(&self) -> &Self::Target {
        self.process.as_ref().unwrap()
    }
}

impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.process.as_mut().unwrap()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.process.as_mut().is_some_and(|child| !matches!(child.try_wait(), Ok(Some(_)))) && self.kill().is_ok() {
            let _ = self.wait();
        }
    }
}

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
             if exist \"%~dp0{name}-%1.failed\" (type \"%~dp0{name}-%1.failed\" 1>&2 & exit /b 1)\r\n\
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
                 if [ -f \"$here/{name}-$1.failed\" ]; then cat \"$here/{name}-$1.failed\" >&2; exit 1; fi\n\
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
    git(&primary, &["worktree", "add", "-q", ".worktrees/Feature-1", "-b", "Feature/1"]);
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
    set_store_list(&stubs, "launches", serde_json::json!([]));
    stub(&stubs, "tj-item", r#"{"prs":[],"all_merged":false}"#);
    stub(&stubs, "tj-prepare", "prepared");
    stub(&stubs, "tj-session", "{}");
    stub(&stubs, "codex", "unexpected prompt delivery");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prepare.py"), stubs.join("prepare.py"))
        .unwrap();
    for folder in ["home", "cache", "state"] {
        std::fs::create_dir(root.join(folder)).unwrap();
    }
    let worktree = primary.join(".worktrees").join("Feature-1");
    Sandbox { log: root.join("dummy.log"), _directory: directory, root, stubs, primary, worktree }
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn search_path(sandbox: &Sandbox) -> std::ffi::OsString {
    let mut entries = vec![sandbox.stubs.clone()];
    entries.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(entries).unwrap()
}

fn environment(sandbox: &Sandbox) -> Vec<(&'static str, std::ffi::OsString)> {
    let home = sandbox.root.join("home").into_os_string();
    vec![
        ("PATH", search_path(sandbox)),
        ("TJ_CACHE_DIR", sandbox.root.join("cache").into_os_string()),
        ("TJ_STATE_DIR", sandbox.root.join("state").into_os_string()),
        ("DUMMY_LOG", sandbox.log.clone().into_os_string()),
        ("DUMMY_TJ", std::ffi::OsString::from(env!("CARGO_BIN_EXE_tj"))),
        ("CLAUDE_CONFIG_DIR", sandbox.root.join("claude").into_os_string()),
        ("CODEX_HOME", sandbox.root.join("codex").into_os_string()),
        ("FAKE_CODEX_LOG", sandbox.root.join("maintenance.jsonl").into_os_string()),
        ("CODEX_THREAD_ID", std::ffi::OsString::new()),
        ("CODEX_SESSION_ID", std::ffi::OsString::new()),
        ("CLAUDECODE", std::ffi::OsString::new()),
        ("CLAUDE_CODE_SESSION_ID", std::ffi::OsString::new()),
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
        "_session",
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
        "--native-exit-seconds",
        "2",
        "--input-idle-seconds",
        "0",
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
    static IMAGE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let image = IMAGE.get_or_init(|| {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join(name);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg(fixtures.join("native_codex.rs"))
            .arg("-o")
            .arg(&output)
            .env("TJ_FIXTURE_PYTHON", which::which(python()).unwrap())
            .env("TJ_FIXTURE_CODEX", fixtures.join("fake_codex.py"))
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        std::fs::read(output).unwrap()
    });
    std::fs::write(&executable, image).unwrap();
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
            "--remote".to_owned(),
            "unix://".to_owned(),
            "--".to_owned(),
            prompt.to_owned(),
        ],
    );
    arguments
}

fn completion_receipt(sandbox: &Sandbox, launch: &str) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["receipt", "--launch", launch])
        .envs(environment(sandbox))
        .current_dir(&sandbox.root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
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
    wait_for_log(sandbox, "pid ");
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
    Child::new(command.spawn().unwrap(), sandbox)
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
            let diagnostics = child.diagnostics();
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "TJ did not exit within {timeout:?}; stderr: {}\n{diagnostics}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.wait_with_output().unwrap()
}

fn still_running(child: &mut Child, wait: Duration) {
    std::thread::sleep(wait);
    assert!(child.try_wait().unwrap().is_none(), "TJ exited");
}

/// Runs `tj` inside the agent, as the hooks of the tab's own agent do, and returns its exit code and stderr.
fn in_agent(sandbox: &Sandbox, arguments: &[&str]) -> (i32, String) {
    in_agent_command(sandbox, serde_json::json!(arguments))
}

fn in_agent_command(sandbox: &Sandbox, command: serde_json::Value) -> (i32, String) {
    let commands = sandbox.root.join("commands");
    let number = log(sandbox, "commands").lines().count();
    let mut line = command.to_string();
    line.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(&commands).unwrap().write_all(line.as_bytes()).unwrap();
    let prefix = format!("ran {number} ");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(ran) =
            log(sandbox, "dummy.log").lines().find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
        {
            let (code, stderr) = ran.split_once(' ').unwrap_or((&ran, ""));
            return (code.parse().unwrap(), stderr.to_owned());
        }
        assert!(Instant::now() < deadline, "the agent never ran queued tj command {number}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn native_session(sandbox: &Sandbox) -> String {
    let show: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sandbox.stubs.join("agent-store-show.json")).unwrap()).unwrap();
    show["data"]["session"]["session"].as_str().unwrap().to_owned()
}

fn lifecycle(sandbox: &Sandbox, turn: &str, event: &str, report: Option<&str>, pending: Option<&str>) {
    let payload = serde_json::json!({
        "session": native_session(sandbox),
        "turn": turn,
        "event": event,
        "report": report,
        "pending": pending,
    });
    let (code, stderr) =
        in_agent_command(sandbox, serde_json::json!({"arguments": ["_lifecycle"], "input": payload.to_string()}));
    assert_eq!(code, 0, "native lifecycle {event}: {stderr}");
}

/// The agent's final native response: start the turn, request finish, then provide its nonempty report.
fn done(sandbox: &Sandbox) {
    let turn = format!("turn-{}", log(sandbox, "commands").lines().count());
    set_turn(sandbox, false);
    lifecycle(sandbox, &turn, "start", None, None);
    let (code, stderr) = in_agent(sandbox, &["finish"]);
    assert_eq!(code, 0, "tj finish: {stderr}");
    set_turn(sandbox, true);
    lifecycle(sandbox, &turn, "end", Some("Final result"), None);
}

fn refused(sandbox: &Sandbox, child: &mut Child, launch: &str) {
    done(sandbox);
    wait_until("closeout never recorded its refusal", || {
        let output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(["receipt", "--launch", launch])
            .envs(environment(sandbox))
            .current_dir(&sandbox.root)
            .output()
            .unwrap();
        let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        record["state"]["kind"] == "attention" && record["state"]["outcome"].is_object()
    });
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

/// Writes Claude's own status record for the running agent, as Claude Code does between turns.
fn set_status(sandbox: &Sandbox, status: &str) {
    set_status_at(sandbox, status, chrono::Utc::now());
}

fn set_status_at(sandbox: &Sandbox, status: &str, when: chrono::DateTime<chrono::Utc>) {
    wait_for_log(sandbox, "pid ");
    let pid = log(sandbox, "dummy.log").lines().rev().find_map(|line| line.strip_prefix("pid ")).unwrap().to_owned();
    let sessions = sandbox.root.join("claude").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let now = when.timestamp_millis();
    let record = serde_json::json!({"pid": pid.parse::<u32>().unwrap(), "status": status, "statusUpdatedAt": now});
    std::fs::write(sessions.join(format!("{pid}.json")), record.to_string()).unwrap();
}

/// Starts or ends a turn the way each host records it: Claude's status record and the Codex thread's rollout.
fn set_turn(sandbox: &Sandbox, idle: bool) {
    set_turn_at(sandbox, idle, chrono::Utc::now());
}

fn set_turn_at(sandbox: &Sandbox, idle: bool, when: chrono::DateTime<chrono::Utc>) {
    set_status_at(sandbox, if idle { "idle" } else { "busy" }, when);
    set_codex_turn(sandbox, idle, when);
}

fn set_codex_turn(sandbox: &Sandbox, idle: bool, when: chrono::DateTime<chrono::Utc>) {
    let folder = sandbox.root.join("codex").join("sessions").join("2026").join("11").join("11");
    std::fs::create_dir_all(&folder).unwrap();
    let event = serde_json::json!({
        "timestamp": when.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": "event_msg",
        "payload": {"type": if idle { "task_complete" } else { "task_started" }},
    });
    let rollout = folder.join("rollout-2026-11-11T00-00-00-sess-1.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(rollout).unwrap();
    writeln!(file, "{event}").unwrap();
}

fn publish(sandbox: &Sandbox, host: &str, revision: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["update", "--host", host, "--revision", revision])
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
            "launch": {"launch_id": launch, "host": host, "session": format!("{host}:sess-1"), "state": "active", "item": null, "parent": null},
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
    set_reply(
        sandbox,
        "tj-session-liveness.json",
        &serde_json::json!({
            "session": value["data"]["session"]["session"],
            "launch_id": launch,
            "state": "dead"
        })
        .to_string(),
    );
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
    assert!(sandbox.root.join("state").join("completion").is_dir(), "completion leaked into TJ_CACHE_DIR");
    let receipt = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["receipt", "--launch", launch])
        .envs(environment(sandbox))
        .current_dir(&sandbox.root)
        .output()
        .unwrap();
    assert!(receipt.status.success(), "{}", String::from_utf8_lossy(&receipt.stderr));
    let receipt: serde_json::Value = serde_json::from_slice(&receipt.stdout).unwrap();
    assert_eq!(receipt["state"]["kind"], "completed");
}

fn feed_messages(sandbox: &Sandbox, host: &str) -> Vec<serde_json::Value> {
    let text = log(sandbox, &format!("cache/feed/{host}-sess-1.jsonl"));
    let Some((complete, _)) = text.rsplit_once('\n') else { return Vec::new() };
    complete.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn read_feed(sandbox: &Sandbox, host: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .arg("feed")
        .envs(environment(sandbox))
        .env("CLAUDECODE", if host == "claude" { "1" } else { "" })
        .env("CLAUDE_CODE_SESSION_ID", if host == "claude" { "sess-1" } else { "" })
        .env("CODEX_THREAD_ID", if host == "codex" { "sess-1" } else { "" })
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_no_prompt_delivery(sandbox: &Sandbox) {
    assert!(log(sandbox, "stubs/codex.log").is_empty(), "the native Codex CLI was invoked");
    assert!(!sandbox.root.join("cache").join("bridge").exists(), "a Claude relay was created");
    assert!(!log(sandbox, "dummy.log").contains("typed "), "text was typed into the agent");
}

fn assert_unfinished(sandbox: &Sandbox, launch: &str) {
    assert!(sandbox.worktree.exists(), "unfinished worktree was removed");
    assert!(!git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
    assert!(!log(sandbox, "stubs/tj-item.log").contains("--state Done"));
    let store = log(sandbox, "stubs/agent-store.log");
    assert!(!store.contains(&format!("end --launch-id {launch} --reason closed")), "{store}");
    assert!(!store.contains("release-worktree"), "{store}");
}

#[test]
fn codex_final_result_reaches_its_parent_once_and_has_a_durable_receipt() {
    let sandbox = sandbox();
    let launch = "codex-parent-result";
    let keys = cfg!(windows);
    let mut arguments = run_arguments(&sandbox, launch, "codex", if keys { "keys" } else { "signal" }, "10");
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(separator..separator, ["--parent", "codex:parent-1"].map(str::to_owned));
    let show = sandbox.stubs.join("agent-store-show.json");
    let mut binding: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&show).unwrap()).unwrap();
    binding["data"]["launch"]["parent"] = serde_json::json!("codex:parent-1");
    std::fs::write(&show, binding.to_string()).unwrap();
    set_reply(
        &sandbox,
        "agent-store-show-codex-parent-1.json",
        &serde_json::json!({
            "ok": true, "data": {"session": {
                "session": "codex:parent-1", "host": "codex", "native_id": "parent-1", "state": "active"
            }}
        })
        .to_string(),
    );
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, if keys { "raw" } else { "pid " });
    assert_eq!(in_agent(&sandbox, &["hold"]).0, 0);
    done(&sandbox);
    let messages = || {
        log(&sandbox, "cache/feed/codex-parent-1.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    wait_until("Codex parent never received the final result", || messages().len() == 1);
    let message = &messages()[0];
    assert_eq!(message["kind"], "result");
    assert_eq!(message["from"], "codex:sess-1");
    assert_eq!(message["text"], "Final result");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(messages().len(), 1, "the final result was delivered more than once");
    assert_eq!(in_agent(&sandbox, &["hold", "--release"]).0, 0);
    assert_closed_out(&sandbox, &finish(child, Duration::from_secs(60)), launch);
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
    bind(&sandbox, "merged-before", "claude", true);
    still_running(&mut child, Duration::from_secs(2));
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "merged-before");
}

#[test]
fn merge_notices_reach_both_hosts_once_per_pr_set_despite_reads_polls_and_later_turns() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("merged-during-{host}");
        let mut child = start_host(&sandbox, &launch, host);
        set_turn(&sandbox, false);
        wait_for_polls(&sandbox, "stubs/tj-item.log", "prs ", 1);
        set_reply(
            &sandbox,
            "tj-item-prs.json",
            r#"{"all_merged":true,"prs":[{"url":"https://example.com/pr/2"},{"url":"https://example.com/pr/1"},{"url":"https://example.com/pr/2"}]}"#,
        );
        wait_until("merge notice never reached the feed", || feed_messages(&sandbox, host).len() == 1);
        let notice = &feed_messages(&sandbox, host)[0];
        assert_eq!(notice["name"], "TJ");
        assert!(notice["from"].is_null());
        assert_eq!(notice["kind"], "merged");
        assert!(notice["text"].as_str().unwrap().contains(ITEM));
        assert_eq!(notice["event"]["item"], ITEM);
        assert_eq!(notice["event"]["prs"], serde_json::json!(["https://example.com/pr/1", "https://example.com/pr/2"]));
        assert!(feed_messages(&sandbox, if host == "claude" { "codex" } else { "claude" }).is_empty());
        assert!(read_feed(&sandbox, host).contains("TJ: Every PR linked"));
        for _ in 0..2 {
            set_turn(&sandbox, false);
            still_running(&mut child, Duration::from_secs(2));
            set_turn(&sandbox, true);
            still_running(&mut child, Duration::from_secs(2));
        }
        set_reply(
            &sandbox,
            "tj-item-prs.json",
            r#"{"all_merged":true,"prs":[{"url":"https://example.com/pr/1","title":"renamed"},{"url":"https://example.com/pr/2"}]}"#,
        );
        let polls = log(&sandbox, "stubs/tj-item.log").lines().filter(|line| line.starts_with("prs ")).count();
        wait_for_polls(&sandbox, "stubs/tj-item.log", "prs ", polls + 3);
        assert_eq!(feed_messages(&sandbox, host).len(), 1);
        assert_eq!(read_feed(&sandbox, host).trim(), "no unread messages");
        assert_unfinished(&sandbox, &launch);
        assert_no_prompt_delivery(&sandbox);
        set_reply(
            &sandbox,
            "tj-item-prs.json",
            r#"{"all_merged":true,"prs":[{"url":"https://example.com/pr/1"},{"url":"https://example.com/pr/2"},{"url":"https://example.com/pr/3"}]}"#,
        );
        wait_until("a new PR set never produced its own notice", || feed_messages(&sandbox, host).len() == 2);
        let notices = feed_messages(&sandbox, host);
        assert_ne!(notices[0]["event"], notices[1]["event"]);
        assert!(read_feed(&sandbox, host).contains("Every PR linked"));
        assert_no_prompt_delivery(&sandbox);
        done(&sandbox);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, &launch);
    }
}

#[test]
fn idle_sessions_never_generate_notices_or_prompts() {
    for host in ["claude", "codex"] {
        for has_item in [false, true] {
            let sandbox = sandbox();
            let launch = format!("idle-silent-{host}-{has_item}");
            let keys = cfg!(windows) && host == "codex";
            let mut arguments = run_arguments(&sandbox, &launch, host, if keys { "keys" } else { "signal" }, "10");
            if !has_item {
                let item = arguments.iter().position(|argument| argument == "--item").unwrap();
                arguments.drain(item..item + 2);
            }
            set_codex_turn(&sandbox, false, chrono::Utc::now() - chrono::Duration::minutes(32));
            let mut child = spawn(&sandbox, &arguments, &sandbox.root);
            wait_for_log(&sandbox, if keys { "raw" } else { "agent " });
            // Observe a busy turn before simulating a long idle interval.
            set_turn_at(&sandbox, false, chrono::Utc::now() - chrono::Duration::minutes(32));
            still_running(&mut child, Duration::from_secs(2));
            set_turn_at(&sandbox, true, chrono::Utc::now() - chrono::Duration::minutes(31));
            still_running(&mut child, Duration::from_secs(3));
            assert!(feed_messages(&sandbox, host).is_empty());
            assert_no_prompt_delivery(&sandbox);
            assert_unfinished(&sandbox, &launch);
            bind(&sandbox, &launch, host, true);
            still_running(&mut child, Duration::from_secs(2));
            done(&sandbox);
            let output = finish(child, Duration::from_secs(60));
            assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
            assert!(!sandbox.worktree.exists());
            if !has_item {
                assert!(log(&sandbox, "stubs/tj-item.log").is_empty(), "a launch without an item queried the tracker");
            }
            assert_no_prompt_delivery(&sandbox);
        }
    }
}

#[test]
fn fresh_launches_reject_the_retired_idle_ask_option() {
    let sandbox = sandbox();
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["_session", "--ask-idle-seconds", "2", "--", "unused"])
        .envs(environment(&sandbox))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument '--ask-idle-seconds'"), "{stderr}");
    assert!(log(&sandbox, "stubs/agent-store.log").is_empty());
}

#[test]
fn item_reference_launch_uses_the_effective_project_and_keeps_agent_arguments() {
    let sandbox = sandbox();
    git(&sandbox.primary, &["remote", "set-url", "origin", "https://github.com/acme/app.git"]);
    let reference = "azure-devops:dev.azure.com/contoso:461";
    stub(&sandbox.stubs, "tj-item", &serde_json::json!({"ref": reference}).to_string());
    let mut arguments = run_arguments(&sandbox, "azure-item", "claude", "exit", "10");
    let item = arguments.iter().position(|arg| arg == "--item").unwrap() + 1;
    arguments[item] = "#461".into();
    arguments.extend(["--remote", "unix://", "prompt with spaces"].map(str::to_owned));
    let output = finish(spawn(&sandbox, &arguments, &sandbox.root), Duration::from_secs(30));
    assert_eq!(output.status.code(), Some(3), "{}", String::from_utf8_lossy(&output.stderr));
    let resolution = log(&sandbox, "stubs/tj-item.log");
    assert_item_resolution(&resolution, &sandbox.worktree, "#461");
    let stored = log(&sandbox, "stubs/agent-store.log");
    assert!(stored.contains(&format!("--item {reference}")), "{stored}");
    assert!(!stored.contains("--item #461"));
    assert!(log(&sandbox, "dummy.log").contains("args --remote unix:// prompt with spaces"));
}

#[test]
fn item_reference_closeout_resolves_once_before_querying_without_authorizing_completion() {
    let sandbox = sandbox();
    let reference = "azure-devops:dev.azure.com/contoso:461";
    stub(&sandbox.stubs, "tj-item", &serde_json::json!({"ref": reference}).to_string());
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["closeout", "461"])
        .envs(environment(&sandbox))
        .current_dir(&sandbox.worktree)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let resolution = log(&sandbox, "stubs/tj-item.log");
    assert_eq!(resolution.lines().count(), 1);
    assert_item_resolution(&resolution, &sandbox.worktree, "461");
    let stored = log(&sandbox, "stubs/agent-store.log");
    assert!(!stored.contains("mark-closing"), "{stored}");
    assert!(stored.contains(&format!("launches --item {reference}")), "{stored}");
}

fn assert_item_resolution(log: &str, cwd: &Path, item: &str) {
    let suffix = format!(" reference {item}");
    let directory = log
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("--cwd "))
        .and_then(|line| line.strip_suffix(suffix.as_str()))
        .unwrap_or_else(|| panic!("unexpected item reference command: {log}"));
    // Windows batch %* retains the path's surrounding quotes; Unix $* does not.
    let directory = directory.strip_prefix('"').and_then(|text| text.strip_suffix('"')).unwrap_or(directory);
    assert_eq!(Path::new(directory), cwd, "{log}");
}

#[test]
fn item_reference_failures_never_start_or_close_a_session() {
    for (input, reply) in [
        ("461", None),
        ("461", Some("not JSON")),
        ("461", Some(r#"{"ref":"461"}"#)),
        ("461", Some(r#"{"ref":42}"#)),
        ("461", Some(r#"{"ref":"https://[invalid"}"#)),
        ("461", Some("{}")),
        ("https://[invalid", Some("{}")),
        ("url:https://[invalid", Some("{}")),
    ] {
        let sandbox = sandbox();
        if let Some(reply) = reply {
            stub(&sandbox.stubs, "tj-item", reply);
        } else {
            std::fs::write(sandbox.stubs.join("tj-item---cwd.failed"), "unknown tracker profile").unwrap();
        }
        let mut launch = run_arguments(&sandbox, "refused-item", "claude", "exit", "10");
        let item = launch.iter().position(|arg| arg == "--item").unwrap() + 1;
        launch[item] = input.into();
        let title = launch.iter().position(|arg| arg == "--title").unwrap();
        launch.drain(title..title + 2);
        for arguments in [launch, vec!["closeout".into(), input.into()]] {
            let output = finish(spawn(&sandbox, &arguments, &sandbox.root), Duration::from_secs(30));
            assert_eq!(output.status.code(), Some(2));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("tj item reference"), "{error}");
            if reply.is_none() {
                assert!(error.contains("unknown tracker profile"), "{error}");
            }
            assert!(log(&sandbox, "stubs/agent-store.log").is_empty());
            assert!(log(&sandbox, "stubs/tj-prepare.log").is_empty());
            assert!(log(&sandbox, "dummy.log").is_empty());
        }
    }
}

#[test]
fn item_reference_qualified_values_need_no_project_profile() {
    for reference in [
        "azure-devops:dev.azure.com/contoso:461",
        "https://dev.azure.com/Contoso/_workitems/edit/461",
        "https://tracker.example/items/APP-12",
        "url:https://tracker.example/items/APP-12",
    ] {
        let sandbox = sandbox();
        std::fs::write(sandbox.stubs.join("tj-item---cwd.failed"), "unknown tracker profile").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(["closeout", reference])
            .envs(environment(&sandbox))
            .current_dir(&sandbox.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(log(&sandbox, "stubs/tj-item.log").is_empty());
        let stored = log(&sandbox, "stubs/agent-store.log");
        assert!(stored.contains(&format!("launches --item {reference}")), "{stored}");
        assert!(!stored.contains("mark-closing"), "{stored}");
    }
}

fn kill_dummy(sandbox: &Sandbox) {
    let pid = log(sandbox, "dummy.log").lines().rev().find_map(|line| line.strip_prefix("pid ")).unwrap().to_owned();
    let mut kill = if cfg!(windows) { Command::new("powershell.exe") } else { Command::new("kill") };
    if cfg!(windows) {
        kill.args(["-NoProfile", "-NonInteractive", "-Command", "Stop-Process", "-Id", &pid, "-Force"]);
    } else {
        kill.args(["-KILL", &pid]);
    }
    assert!(kill.output().unwrap().status.success());
}

#[test]
fn merge_notice_identity_survives_new_launches_of_the_same_session() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        set_reply(&sandbox, "tj-item-prs.json", MERGED);
        for (number, launch) in [(0, "original"), (2, "new-launch"), (3, "another-item")] {
            // A real new launch replaces the binding, while the native session and its feed persist.
            if number >= 2 {
                std::fs::remove_file(sandbox.stubs.join("agent-store-show.json")).unwrap();
                std::fs::remove_file(sandbox.stubs.join("agent-store-worktrees.json")).unwrap();
            }
            let mut arguments = run_arguments(&sandbox, launch, host, "signal", "10");
            if number == 2 {
                let item = arguments.iter().position(|argument| argument == "--item").unwrap() + 1;
                arguments[item] = "https://github.com/acme/app/issues/1".into();
                set_reply(
                    &sandbox,
                    "tj-item-prs.json",
                    &serde_json::json!({"ref": ITEM, "all_merged": true, "prs": [{"url": "u"}]}).to_string(),
                );
            }
            if number == 3 {
                let item = arguments.iter().position(|argument| argument == "--item").unwrap() + 1;
                arguments[item] = "github:github.com/acme/app:2".into();
                set_reply(
                    &sandbox,
                    "tj-item-prs.json",
                    r#"{"ref":"github:github.com/acme/app:2","all_merged":true,"prs":[{"url":"u"}]}"#,
                );
            }
            let starts = log(&sandbox, "dummy.log").lines().filter(|line| line.starts_with("pid ")).count();
            let child = spawn(&sandbox, &arguments, &sandbox.root);
            wait_for_polls(&sandbox, "dummy.log", "pid ", starts + 1);
            let polls = log(&sandbox, "stubs/tj-item.log").lines().filter(|line| line.starts_with("prs ")).count();
            wait_for_polls(&sandbox, "stubs/tj-item.log", "prs ", polls + 3);
            assert_eq!(feed_messages(&sandbox, host).len(), if number == 3 { 2 } else { 1 });
            let read = read_feed(&sandbox, host);
            if number == 0 || number == 3 {
                assert!(read.contains("TJ: Every PR linked"), "{read}");
            } else {
                assert_eq!(read.trim(), "no unread messages");
            }
            assert_unfinished(&sandbox, launch);
            assert_no_prompt_delivery(&sandbox);
            kill_dummy(&sandbox);
            let output = finish(child, Duration::from_secs(60));
            assert!(!output.status.success(), "the killed dummy unexpectedly succeeded");
            assert_unfinished(&sandbox, launch);
        }
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("launch --launch-id original --host").count(), 1);
    }
}

#[test]
fn incomplete_or_empty_pr_sets_never_produce_a_merge_notice() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("invalid-prs-{host}");
        let child = start_host(&sandbox, &launch, host);
        for reply in [
            r#"{"all_merged":false,"prs":[{"url":"https://example.com/pr/1"}]}"#,
            r#"{"all_merged":true,"prs":[]}"#,
            r#"{"all_merged":true,"prs":[{}]}"#,
            r#"{"all_merged":true,"prs":[{"url":"https://example.com/pr/1"},{"url":""}]}"#,
            r#"{"all_merged":true,"prs":[{"url":"   "}]}"#,
        ] {
            set_reply(&sandbox, "tj-item-prs.json", reply);
            let polls = log(&sandbox, "stubs/tj-item.log").lines().filter(|line| line.starts_with("prs ")).count();
            wait_for_polls(&sandbox, "stubs/tj-item.log", "prs ", polls + 2);
            assert!(feed_messages(&sandbox, host).is_empty(), "notice for {reply}");
        }
        set_reply(&sandbox, "tj-item-prs.json", MERGED);
        wait_until("a valid merged PR set never produced a notice", || feed_messages(&sandbox, host).len() == 1);
        assert_no_prompt_delivery(&sandbox);
        done(&sandbox);
        assert_closed_out(&sandbox, &finish(child, Duration::from_secs(60)), &launch);
    }
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
        done(&sandbox);
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
        let mut child = start(&sandbox, launch, "claude", "signal", "2");
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
        if field == "worktree" {
            refused(&sandbox, &mut child, launch);
        } else {
            let (code, stderr) = in_agent(&sandbox, &["finish"]);
            assert_ne!(code, 0, "finish accepted a foreign binding");
            assert!(stderr.contains("exact reciprocal native session binding"), "{stderr}");
            still_running(&mut child, Duration::from_millis(500));
            assert!(!log(&sandbox, "dummy.log").contains("graceful"));
        }
        assert!(sandbox.worktree.exists());
        assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
        binding["data"]["session"]["launch_id"] = serde_json::json!(launch);
        binding["data"]["session"]["host"] = serde_json::json!("claude");
        binding["data"]["session"]["worktree"] = serde_json::json!(sandbox.worktree);
        set_reply(&sandbox, "agent-store-show.json", &binding.to_string());
        done(&sandbox);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, launch);
    }
}

#[test]
fn a_closing_orphan_without_a_final_receipt_keeps_its_bound_worktree() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("orphan-{host}");
        let row = serde_json::json!({
            "launch_id": launch, "host": host, "title": "Orphan",
            "worktree": sandbox.root.join("gone"), "state": "closing", "end_reason": "exited",
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
        let output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .arg("closeout")
            .envs(environment(&sandbox))
            .current_dir(&sandbox.root)
            .output()
            .unwrap();
        assert_unfinished(&sandbox, &launch);
        assert!(!git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("closed     "));
    }
}

#[test]
fn sweeping_merged_only_dead_launches_preserves_their_worktrees() {
    for host in ["claude", "codex"] {
        for state in ["active", "ended"] {
            let sandbox = sandbox();
            let launch = format!("merged-orphan-{host}-{state}");
            bind(&sandbox, &launch, host, false);
            set_store_list(
                &sandbox.stubs,
                "launches",
                serde_json::json!([{
                    "launch_id": launch, "host": host, "title": "Unfinished orphan",
                    "worktree": sandbox.worktree, "state": state, "end_reason": "exited",
                    "item": {"ref": ITEM},
                    "wrapper": {"pid": 0, "started_at": "2026-01-01T00:00:00Z"}
                }]),
            );
            set_reply(&sandbox, "tj-item-prs.json", MERGED);
            let marks = sandbox.root.join("cache").join("run").join(&launch);
            std::fs::create_dir_all(&marks).unwrap();
            std::fs::write(marks.join("done"), "an unverifiable old completion mark").unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_tj"))
                .arg("closeout")
                .envs(environment(&sandbox))
                .current_dir(&sandbox.root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_unfinished(&sandbox, &launch);
            assert!(!String::from_utf8_lossy(&output.stdout).contains("closed     "));
        }
    }
}

#[test]
fn closes_out_after_done_and_idle() {
    let sandbox = sandbox();
    let child = start(&sandbox, "idle", "claude", "signal", "10");
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "idle");
    assert!(log(&sandbox, "dummy.log").contains("graceful"), "the agent was not stopped gracefully");
    assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty(), "the merged branch is still there");
}

fn start_host(sandbox: &Sandbox, launch: &str, host: &str) -> Child {
    let keys = cfg!(windows) && host == "codex";
    let child = start(sandbox, launch, host, if keys { "keys" } else { "signal" }, "10");
    wait_for_log(sandbox, if keys { "raw" } else { "agent " });
    child
}

#[test]
fn dropping_a_test_wrapper_stops_its_fixture_tree() {
    let sandbox = sandbox();
    let child = start(&sandbox, "fixture-owner", "claude", "ignore", "10");
    wait_for_log(&sandbox, "grandchild ");
    let mut pids = vec![child.id()];
    pids.extend(log(&sandbox, "dummy.log").lines().filter_map(|line| {
        line.strip_prefix("pid ").or_else(|| line.strip_prefix("grandchild "))?.parse::<u32>().ok()
    }));
    assert_eq!(pids.len(), 3);
    drop(child);
    wait_until("the abandoned fixture retained a process", || {
        pids.iter().all(|pid| tj_cli::process::agent::started_at(*pid).unwrap().is_none())
    });
}

#[test]
fn codex_closes_out_after_done_and_idle() {
    let sandbox = sandbox();
    let child = start_host(&sandbox, "idle", "codex");
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "idle");
}

#[test]
fn new_input_cancels_finish_and_explicit_closing_does_not_override_it() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("withdrawn-{host}");
        let mut child = start_host(&sandbox, &launch, host);
        lifecycle(&sandbox, "finish-request", "start", None, None);
        assert_eq!(in_agent(&sandbox, &["finish"]).0, 0);
        lifecycle(&sandbox, "new-input", "start", None, None);
        set_turn(&sandbox, true);
        still_running(&mut child, Duration::from_secs(5));
        assert!(!log(&sandbox, "dummy.log").contains("graceful"), "the agent was stopped");
        bind(&sandbox, &launch, host, true);
        still_running(&mut child, Duration::from_secs(2));
        assert!(sandbox.worktree.exists(), "closing overrode missing final report");
        done(&sandbox);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, &launch);
    }
}

#[test]
fn explicit_closing_does_not_override_a_keep_open_hold() {
    let sandbox = sandbox();
    let launch = "held-closing";
    let mut child = start_host(&sandbox, launch, "codex");
    assert_eq!(in_agent(&sandbox, &["hold"]).0, 0);
    done(&sandbox);
    bind(&sandbox, launch, "codex", true);
    still_running(&mut child, Duration::from_secs(3));
    assert!(sandbox.worktree.exists(), "closing overrode the keep-open hold");
    assert_eq!(in_agent(&sandbox, &["hold", "--release"]).0, 0);
    assert_closed_out(&sandbox, &finish(child, Duration::from_secs(60)), launch);
}

#[test]
fn finish_waits_for_the_native_turn_end_however_long_it_takes() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("running-{host}");
        let mut child = start_host(&sandbox, &launch, host);
        lifecycle(&sandbox, "still-running", "start", None, None);
        assert_eq!(in_agent(&sandbox, &["finish"]).0, 0);
        still_running(&mut child, Duration::from_secs(5));
        assert!(sandbox.worktree.exists(), "finish closed before the native turn ended");
        lifecycle(&sandbox, "still-running", "end", Some("Final result"), None);
        set_turn(&sandbox, true);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, &launch);
    }
}

#[test]
fn a_shell_left_running_under_the_agent_keeps_final_result_waiting() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("shell-{host}");
        let mut child = start_host(&sandbox, &launch, host);
        assert_eq!(in_agent(&sandbox, &["shell", "start"]).0, 0);
        done(&sandbox);
        still_running(&mut child, Duration::from_secs(6));
        assert!(sandbox.worktree.exists(), "background shell did not keep the final result open");
        assert_eq!(in_agent(&sandbox, &["shell", "stop"]).0, 0);
        let output = finish(child, Duration::from_secs(60));
        assert_closed_out(&sandbox, &output, &launch);
    }
}

#[test]
fn a_forced_nonzero_exit_preserves_worktrees_after_an_unfinished_finish_request() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        set_reply(&sandbox, "tj-item-prs.json", MERGED);
        let launch = format!("done-failed-exit-{host}");
        let child = start_host(&sandbox, &launch, host);
        lifecycle(&sandbox, "unfinished", "start", None, None);
        let (code, stderr) = in_agent(&sandbox, &["finish"]);
        assert_eq!(code, 0, "tj finish: {stderr}");
        kill_dummy(&sandbox);
        let output = finish(child, Duration::from_secs(60));
        assert!(!output.status.success());
        assert_unfinished(&sandbox, &launch);
        assert!(log(&sandbox, "stubs/agent-store.log").contains(&format!("end --launch-id {launch} --reason exited")));
    }
}

/// Keep the existing dummy's process tree and command environment, controlling only its natural exit.
fn natural_exit(sandbox: &Sandbox, launch: &str, host: &str, mark_done: bool) -> Child {
    const EXIT: &str = r#"
import json, os, runpy, subprocess, sys, time
from pathlib import Path
fixture, mark_done, host = sys.argv[1], sys.argv[2] == "done", sys.argv[3]
original_exit = sys.exit
def exit_fixture(code=0):
    if code != 3:
        original_exit(code)
    if mark_done:
        turn = "natural-exit"
        for event, report in [("start", None), ("end", "Final result")]:
            if event == "end":
                result = subprocess.run([os.environ["DUMMY_TJ"], "finish"], capture_output=True, text=True)
                if result.returncode:
                    raise RuntimeError(result.stderr)
            result = subprocess.run(
                [os.environ["DUMMY_TJ"], "_lifecycle"],
                input=json.dumps({"session": f"{host}:sess-1", "turn": turn, "event": event, "report": report, "pending": None}),
                capture_output=True,
                text=True,
            )
            if result.returncode:
                raise RuntimeError(result.stderr)
    ready = Path(os.environ["DUMMY_LOG"]).with_name("exit-ready")
    ready.touch()
    while not ready.with_name("exit-now").exists():
        time.sleep(0.05)
    original_exit(0)
sys.exit = exit_fixture
sys.argv = [fixture, "exit"]
runpy.run_path(fixture, run_name="__main__")
"#;
    set_codex_turn(sandbox, false, chrono::Utc::now());
    let mut arguments = run_arguments(sandbox, launch, host, "exit", "10");
    let idle = arguments.iter().position(|argument| argument == "--idle-seconds").unwrap() + 1;
    arguments[idle] = "60".into();
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(
        separator + 1..,
        [
            python().to_owned(),
            "-c".into(),
            EXIT.into(),
            fixture().to_string_lossy().into_owned(),
            if mark_done { "done".into() } else { "unfinished".into() },
            host.into(),
        ],
    );
    let child = spawn(sandbox, &arguments, &sandbox.root);
    wait_for_log(sandbox, "pid ");
    set_turn(sandbox, false);
    wait_until("the dummy never prepared its natural exit", || sandbox.root.join("exit-ready").is_file());
    // Host records have millisecond timestamps; a later state must be newer than the done mark.
    std::thread::sleep(Duration::from_millis(50));
    child
}

#[test]
fn a_successful_natural_exit_with_a_valid_final_receipt_closes_out_without_the_idle_grace() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("successful-done-exit-{host}");
        let child = natural_exit(&sandbox, &launch, host, true);
        set_turn(&sandbox, true);
        std::fs::write(sandbox.root.join("exit-now"), "").unwrap();
        assert_closed_out(&sandbox, &finish(child, Duration::from_secs(45)), &launch);
    }
}

#[test]
fn successful_exit_preserves_merged_worktrees_without_a_valid_unwithdrawn_done() {
    for host in ["claude", "codex"] {
        for completion in ["absent", "withdrawn", "unreadable"] {
            let sandbox = sandbox();
            set_reply(&sandbox, "tj-item-prs.json", MERGED);
            let launch = format!("unfinished-exit-{host}-{completion}");
            let child = natural_exit(&sandbox, &launch, host, completion != "absent");
            match completion {
                "absent" => set_turn(&sandbox, true),
                "withdrawn" => set_turn(&sandbox, false),
                "unreadable" => {
                    if host == "claude" {
                        let pid = log(&sandbox, "dummy.log")
                            .lines()
                            .find_map(|line| line.strip_prefix("pid "))
                            .unwrap()
                            .to_owned();
                        std::fs::write(sandbox.root.join("claude").join("sessions").join(format!("{pid}.json")), "{")
                            .unwrap();
                    } else {
                        let folder = sandbox.root.join("codex").join("sessions").join("2026").join("11").join("11");
                        std::fs::write(folder.join("rollout-2026-11-11T00-00-00-sess-1.jsonl"), "{").unwrap();
                    }
                }
                _ => unreachable!("only the completion states above are tested"),
            }
            std::fs::write(sandbox.root.join("exit-now"), "").unwrap();
            let output = finish(child, Duration::from_secs(60));
            assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
            assert_unfinished(&sandbox, &launch);
            assert!(
                log(&sandbox, "stubs/agent-store.log").contains(&format!("end --launch-id {launch} --reason exited"))
            );
        }
    }
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
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert!(String::from_utf8_lossy(&output.stderr).contains("worktree has unmerged commits"));
    assert_eq!(output.status.code(), Some(0));
    assert!(!sandbox.worktree.exists());
}

#[test]
fn an_agent_ignoring_the_stop_is_killed_with_its_tree() {
    let sandbox = sandbox();
    let child = start(&sandbox, "stubborn", "claude", "stubborn", "2");
    done(&sandbox);
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
    finish(Child::new(child, &sandbox), Duration::from_secs(60));
    let seen_by_agent = log(&sandbox, "dummy.log");
    assert!(seen_by_agent.contains("launch --launch-id first --host"), "{seen_by_agent}");
}

fn awaiting_registration(sandbox: &Sandbox, launch: &str, host: &str, mode: &str) -> Child {
    let arguments = run_arguments(sandbox, launch, host, mode, "10");
    std::fs::write(
        sandbox.stubs.join("agent-store-prebind.json"),
        serde_json::json!({"session":"sess-1", "host":host}).to_string(),
    )
    .unwrap();
    let child = spawn(sandbox, &arguments, &sandbox.root);
    wait_for_log(sandbox, "pid ");
    child
}

#[test]
fn registration_delayed_across_watchdog_polls_is_quiet_and_then_observed() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("registration-delayed-{host}");
        let child = awaiting_registration(&sandbox, &launch, host, "signal");
        wait_for_polls(&sandbox, "stubs/agent-store.log", &format!("show --launch-id {launch}"), 4);
        let mut row: serde_json::Value =
            serde_json::from_str(&log(&sandbox, "stubs/agent-store-prebind-launch.json")).unwrap();
        row["session"] = serde_json::json!(format!("{host}:sess-1"));
        row["created_at"] = row["wrapper"]["started_at"].clone();
        set_store_list(&sandbox.stubs, "launches", serde_json::json!([row]));
        std::fs::write(sandbox.stubs.join("agent-store-prebind-bound"), "").unwrap();
        lifecycle(&sandbox, "registered", "start", None, None);
        lifecycle(&sandbox, "registered", "end", Some("Registered final response"), None);
        wait_until("registered completion was not observed", || {
            completion_receipt(&sandbox, &launch)["state"]["kind"] == "attention"
        });
        assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
        let output = finish(child, Duration::from_secs(30));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(!stderr.contains("missing session"), "{stderr}");
        assert!(!stderr.contains("still awaiting native session registration"), "{stderr}");
        assert!(sandbox.worktree.exists());
    }
}

#[test]
fn registration_missing_at_native_exit_is_reported_and_preserves_the_exit_code() {
    for host in ["claude", "codex"] {
        for code in [0, 3] {
            let sandbox = sandbox();
            let launch = format!("registration-exit-{host}-{code}");
            let child = awaiting_registration(&sandbox, &launch, host, if code == 0 { "signal" } else { "exit" });
            if code == 0 {
                wait_for_polls(&sandbox, "stubs/agent-store.log", &format!("show --launch-id {launch}"), 3);
                assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
            }
            let output = finish(child, Duration::from_secs(30));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(code), "{stderr}");
            assert_eq!(stderr.matches("native exited without a verified session binding").count(), 1, "{stderr}");
            assert_eq!(stderr.matches("missing session").count(), 1, "premature or absent warning: {stderr}");
            assert!(sandbox.worktree.exists());
            assert!(!log(&sandbox, "stubs/agent-store.log").contains("release-worktree"));
            assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
        }
    }
}

#[test]
fn registration_malformed_binding_and_store_errors_are_reported_while_native_is_live() {
    for host in ["claude", "codex"] {
        for failure in ["missing", "mismatched", "store"] {
            let sandbox = sandbox();
            let launch = format!("registration-error-{host}-{failure}");
            let child = awaiting_registration(&sandbox, &launch, host, "signal");
            let prefix = format!("show --launch-id {launch}");
            wait_for_polls(&sandbox, "stubs/agent-store.log", &prefix, 3);
            let mut reply: serde_json::Value =
                serde_json::from_str(&log(&sandbox, "stubs/agent-store-show.json")).unwrap();
            let diagnostic = match failure {
                "missing" => {
                    reply["data"]["session"] = serde_json::json!({});
                    "missing session"
                }
                "mismatched" => {
                    reply["data"]["session"]["launch_id"] = serde_json::json!("foreign");
                    "completion requires an exact reciprocal native session binding"
                }
                "store" => {
                    reply = serde_json::json!({"ok":false,"error":{"message":"store unavailable"}});
                    "agent-store show: store unavailable"
                }
                _ => unreachable!(),
            };
            std::fs::write(sandbox.stubs.join("agent-store-show.json"), reply.to_string()).unwrap();
            std::fs::remove_file(sandbox.stubs.join("agent-store-prebind.json")).unwrap();
            let observed =
                log(&sandbox, "stubs/agent-store.log").lines().filter(|line| line.starts_with(&prefix)).count();
            wait_for_polls(&sandbox, "stubs/agent-store.log", &prefix, observed + 3);
            assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
            let output = finish(child, Duration::from_secs(30));
            let stderr = String::from_utf8_lossy(&output.stderr);
            let live_report = format!("TJ: completion remains unverified: {diagnostic}");
            assert_eq!(stderr.lines().filter(|line| line.trim() == live_report).count(), 1, "{stderr}");
            assert!(!stderr.contains("still awaiting native session registration"), "{stderr}");
            assert!(sandbox.worktree.exists());
        }
    }
}

#[test]
fn repository_preparation_finishes_before_the_agent_starts() {
    let sandbox = sandbox();
    select_tj_agents_checked(&sandbox);
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
fn repository_preparation_failure_ends_the_launch_before_starting_either_host() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        select_tj_agents_checked(&sandbox);
        std::fs::write(sandbox.stubs.join("prepare-failure"), "failed").unwrap();
        let launch = "prepare-failed";
        // Before SessionStart the ordinary launch has a wrapper but no session binding.
        let setup = serde_json::json!({"host":host, "session":PREBOUND});
        std::fs::write(sandbox.stubs.join("agent-store-prebind.json"), setup.to_string()).unwrap();
        let args = if host == "codex" {
            prebound_arguments_with_prompt(&sandbox, launch, "hold receipt fixture")
        } else {
            run_arguments(&sandbox, launch, host, "exit", "10")
        };
        let output = finish(spawn(&sandbox, &args, &sandbox.root), Duration::from_secs(30));
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("repository standards preparation failed")
                && error.contains("simulated preparation failure"),
            "{error}"
        );
        assert!(log(&sandbox, "dummy.log").is_empty());
        assert!(log(&sandbox, "maintenance.jsonl").is_empty());
        assert!(log(&sandbox, "stubs/agent-store.log").contains("end --launch-id prepare-failed --reason not_started"));
    }
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
fn prebound_receipt_identifies_only_the_verified_frontend() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    let arguments = prebound_arguments_with_prompt(&sandbox, "frontend", "hold receipt fixture");
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_until("native frontend did not start", || log(&sandbox, "maintenance.jsonl").contains("frontend"));
    let receipt = || {
        Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(["_process", "spawned", "--launch-id", "frontend", "--session", &format!("codex:{PREBOUND}")])
            .envs(environment(&sandbox))
            .output()
            .unwrap()
    };
    let output = receipt();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let observed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let transport = sandbox.root.join("cache/run/frontend/transport.json");
    let mut recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(&transport).unwrap()).unwrap();
    assert_eq!(observed["native"], recorded["native"]);
    assert!(observed["native"]["pid"].as_u64().unwrap() > 0);
    assert_eq!(observed["launch_id"], "frontend");
    assert_eq!(observed["session"], format!("codex:{PREBOUND}"));
    assert_eq!(observed["codex_transport"], "local-daemon");
    assert_eq!(
        Path::new(observed["endpoint"].as_str().unwrap()),
        sandbox.root.join("codex/app-server-control/app-server-control.sock")
    );
    recorded["native"]["started_at"] = serde_json::json!("2000-01-01T00:00:00Z");
    std::fs::write(&transport, serde_json::to_vec(&recorded).unwrap()).unwrap();
    assert!(!receipt().status.success());
    recorded["native"] = serde_json::Value::Null;
    std::fs::write(&transport, serde_json::to_vec(&recorded).unwrap()).unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&receipt().stdout).unwrap(), serde_json::Value::Null);
    std::fs::write(sandbox.root.join("maintenance.exit"), "").unwrap();
    assert!(finish(child, Duration::from_secs(30)).status.success());
}

#[test]
fn prebound_daemon_start_failure_closes_the_unstarted_launch_and_reports_stderr() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    std::fs::write(sandbox.root.join("maintenance.fail"), "").unwrap();
    let args = prebound_arguments_with_prompt(&sandbox, "daemon-failure", "hold receipt fixture");
    let output = finish(spawn(&sandbox, &args, &sandbox.root), Duration::from_secs(30));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture daemon start refused"));
    assert!(!log(&sandbox, "maintenance.jsonl").contains("frontend"));
    assert!(log(&sandbox, "stubs/agent-store.log").contains("end --launch-id daemon-failure --reason not_started"));
}

#[test]
fn a_prebound_codex_accepts_model_and_effort_before_the_literal_prompt() {
    let sandbox = sandbox();
    prebind(&sandbox, false);
    let mut arguments = prebound_arguments_with_prompt(&sandbox, "lane-model", "--model=literal-prompt");
    let delimiter = arguments.iter().position(|argument| argument == "--remote").unwrap();
    arguments.splice(
        delimiter..delimiter,
        ["--model", "chosen-model", "-c", "model_reasoning_effort=xhigh"].map(str::to_owned),
    );
    let output = finish(spawn(&sandbox, &arguments, &sandbox.root), Duration::from_secs(30));
    assert!(!output.status.success());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(store.contains(&format!("register --session codex:{PREBOUND}")), "{store}");
    assert!(store.contains("end --launch-id lane-model --reason exited"), "{store}");
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
fn a_repository_without_tj_agents_starts_the_agent_without_preparation() {
    let sandbox = sandbox();
    let child = start(&sandbox, "unprepared", "claude", "exit", "10");
    let output = finish(child, Duration::from_secs(30));
    assert_eq!(output.status.code(), Some(3), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(log(&sandbox, "stubs/tj-prepare.log"), "", "prepared a repository that selects no tj-agents plugins");
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
fn a_launch_is_open_while_its_agent_runs_and_closed_once_it_exits() {
    let sandbox = sandbox();
    let child = start(&sandbox, "tab", "claude", "exit", "10");
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(3));
    let store = log(&sandbox, "stubs/agent-store.log");
    let opened = store.find("set-open --launch-id tab --open true").expect("the launch was never marked open");
    let closed = store.find("set-open --launch-id tab --open false").expect("the launch was never marked closed");
    assert!(opened < closed, "{store}");
}

#[test]
fn a_failed_exit_preserves_merged_worktrees_without_an_explicit_completion_signal() {
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
        assert_unfinished(&sandbox, &launch);
        let store = log(&sandbox, "stubs/agent-store.log");
        assert!(store.contains(&format!("end --launch-id {launch} --reason exited")), "{store}");
    }
}

#[cfg(windows)]
#[test]
fn codex_is_stopped_with_a_ctrl_c_keypress() {
    let sandbox = sandbox();
    let child = start(&sandbox, "codex", "codex", "keys", "10");
    wait_for_log(&sandbox, "raw");
    done(&sandbox);
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
    done(&sandbox);
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
fn finish_outside_a_managed_session_fails() {
    let run = |arguments: &[&str]| {
        let mut output = Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(arguments)
            .env_remove("TJ_RUN_LAUNCH_ID")
            .env_remove("CODEX_THREAD_ID")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = String::new();
        output.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
        (output.wait().unwrap().success(), stderr)
    };
    let (succeeded, stderr) = run(&["finish"]);
    assert!(!succeeded && stderr.contains("managed session"));
}

fn with_prompt(sandbox: &Sandbox, launch: &str, host: &str, mode: &str) -> Vec<String> {
    let mut arguments = run_arguments(sandbox, launch, host, mode, "10");
    arguments.extend(["--model", "m", "the prompt"].map(str::to_owned));
    arguments
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn fake_codex(sandbox: &Sandbox) -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.py");
    #[cfg(windows)]
    std::fs::write(sandbox.stubs.join("codex.cmd"), format!("@echo off\r\npython \"{}\" %*\r\n", script.display()))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = sandbox.stubs.join("codex");
        std::fs::write(&path, format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", script.display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    sandbox.root.join("codex.log")
}

#[test]
fn only_the_tabs_own_agent_can_request_finish() {
    let sandbox = sandbox();
    let mut child = start(&sandbox, "owned", "claude", "signal", "10");
    wait_for_log(&sandbox, "wrapper ");
    let wrapper =
        log(&sandbox, "dummy.log").lines().find_map(|line| line.strip_prefix("wrapper ").map(str::to_owned)).unwrap();
    let (pid, started_at) = wrapper.split_once(' ').unwrap();
    lifecycle(&sandbox, "owned-turn", "start", None, None);
    let receipt = || {
        Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(["receipt", "--launch", "owned"])
            .envs(environment(&sandbox))
            .current_dir(&sandbox.root)
            .output()
            .unwrap()
    };
    let before = receipt();
    assert!(before.status.success(), "{}", String::from_utf8_lossy(&before.stderr));
    let foreign = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tj"))
            .args(arguments)
            .envs(environment(&sandbox))
            .envs([("TJ_RUN_LAUNCH_ID", "owned"), ("TJ_RUN_PID", pid), ("TJ_RUN_STARTED_AT", started_at)])
            .env_remove("CODEX_THREAD_ID")
            .env("TJ_CACHE_DIR", sandbox.root.join("cache"))
            .output()
            .unwrap()
    };
    let refused = foreign(&["finish"]);
    assert!(!refused.status.success(), "an unrelated process was accepted as the tab's agent: {refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("requires the managed session's own agent process"));
    let after = receipt();
    assert!(after.status.success(), "{}", String::from_utf8_lossy(&after.stderr));
    assert_eq!(after.stdout, before.stdout, "a process outside the tab changed the completion record");
    assert!(foreign(&["launch"]).stdout.is_empty());
    assert_eq!(in_agent(&sandbox, &["launch"]).0, 0);
    done(&sandbox);
    still_running(&mut child, Duration::from_secs(1));
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "owned");
}

/// Run the real CLI adapter against the dummy Claude binding; never start a native provider.
struct NativeAdapter {
    child: ProcessChild,
    session: String,
    requests: mpsc::Receiver<Result<serde_json::Value, String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl NativeAdapter {
    fn command(sandbox: &Sandbox, session: &str, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
        command
            .args(arguments)
            .envs(environment(sandbox))
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_SESSION_ID", session.strip_prefix("claude:").unwrap())
            .env_remove("CODEX_THREAD_ID")
            .current_dir(&sandbox.root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
    }

    fn start(sandbox: &Sandbox) -> Self {
        let session = native_session(sandbox);
        let mut child = Self::command(sandbox, &session, &["_native-host", "--session", &session])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(sandbox.root.join("native-adapter-stderr.log")).unwrap())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, requests) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let parsed = line
                    .map_err(|error| error.to_string())
                    .and_then(|line| serde_json::from_str(&line).map_err(|error| error.to_string()));
                let failed = parsed.is_err();
                if sender.send(parsed).is_err() || failed {
                    break;
                }
            }
        });
        // Own the process before readiness assertions so a failure also cleans up its reader.
        let mut adapter = Self { child, session, requests, reader: Some(reader) };
        let lease = sandbox.root.join("state/native").join(adapter.session.replace(':', "-")).join("host.lock");
        wait_until("native adapter never acquired its lease", || {
            assert!(
                adapter.child.try_wait().unwrap().is_none(),
                "native adapter exited: {}",
                log(sandbox, "native-adapter-stderr.log")
            );
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lease)
                .is_ok_and(|file| matches!(fs4::FileExt::try_lock(&file), Err(fs4::TryLockError::WouldBlock)))
        });
        adapter
    }

    fn request(&self) -> serde_json::Value {
        self.requests.recv_timeout(Duration::from_secs(30)).expect("native reload never reached the adapter").unwrap()
    }

    fn assert_quiet(&self) {
        assert!(
            matches!(self.requests.recv_timeout(Duration::from_secs(4)), Err(mpsc::RecvTimeoutError::Timeout)),
            "the native adapter emitted another request or stopped"
        );
    }

    fn receipt(&self, sandbox: &Sandbox, request: &serde_json::Value) {
        assert_eq!(request["session"], self.session);
        let result = serde_json::json!({"completed":true, "output":{"text":"Reloaded fixture plugins"}});
        let output = Self::command(
            sandbox,
            &self.session,
            &[
                "_native-receipt",
                "--session",
                &self.session,
                "--request",
                request["id"].as_str().unwrap(),
                "--lease",
                request["lease"].as_str().unwrap(),
                "--result",
                &result.to_string(),
            ],
        )
        .output()
        .unwrap();
        assert!(output.status.success(), "native receipt: {}", String::from_utf8_lossy(&output.stderr));
    }
}

impl Drop for NativeAdapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn native_records(sandbox: &Sandbox) -> Vec<serde_json::Value> {
    let path = sandbox.root.join("state/native").join(native_session(sandbox).replace(':', "-")).join("requests.jsonl");
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("cannot open native request journal: {error}"),
    };
    fs4::FileExt::lock_shared(&file).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    let Some((complete, _)) = text.rsplit_once('\n') else { return Vec::new() };
    complete.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn native_record_snapshots_wait_for_the_journal_writer() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-show.json", r#"{"data":{"session":{"session":"claude:locked"}}}"#);
    let path = sandbox.root.join("state/native/claude-locked/requests.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = std::fs::File::create(&path).unwrap();
    writeln!(writer, "{{\"kind\":\"request\"}}").unwrap();
    fs4::FileExt::lock(&writer).unwrap();
    let (ready, started) = mpsc::channel();
    let (send, received) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            ready.send(()).unwrap();
            send.send(native_records(&sandbox)).unwrap();
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let early = received.recv_timeout(Duration::from_millis(100));
        drop(writer);
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)), "snapshot ignored the writer lock: {early:?}");
        assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), [serde_json::json!({"kind":"request"})]);
    });
}

#[test]
fn each_claude_update_uses_one_native_request_and_waits_for_its_completion_receipt() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-show.json", BOUND);
    let mode = if cfg!(windows) { "keys" } else { "signal" };
    let child = spawn(&sandbox, &with_prompt(&sandbox, "plugins", "claude", mode), &sandbox.root);
    wait_for_log(&sandbox, if cfg!(windows) { "raw" } else { "agent " });
    let adapter = NativeAdapter::start(&sandbox);
    set_status(&sandbox, "busy");
    let commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    publish(&sandbox, "claude", commit);
    let first_publication = log(&sandbox, "cache/published/claude");
    adapter.assert_quiet();
    assert!(
        !native_records(&sandbox).iter().any(|record| record["kind"] == "request"),
        "reload was queued during a busy turn"
    );
    assert_no_prompt_delivery(&sandbox);
    set_status(&sandbox, "idle");
    let first = adapter.request();
    assert_eq!(first["operation"], "reload_plugins");
    assert_eq!(first["session"], native_session(&sandbox));
    assert_eq!(first["revision"], first_publication);
    assert!(!first["lease"].as_str().unwrap().is_empty());
    adapter.assert_quiet();
    let pending = native_records(&sandbox);
    assert_eq!(pending.iter().filter(|record| record["kind"] == "request").count(), 1);
    assert!(!pending.iter().any(|record| record["kind"] == "receipt"), "completion was invented");
    adapter.receipt(&sandbox, &first);
    adapter.assert_quiet();
    publish(&sandbox, "codex", "plugins c");
    adapter.assert_quiet();
    assert_eq!(native_records(&sandbox).iter().filter(|record| record["kind"] == "request").count(), 1);
    assert_eq!(log(&sandbox, "cache/published/claude"), first_publication, "Codex changed Claude's publication");
    publish(&sandbox, "claude", commit);
    let second_publication = log(&sandbox, "cache/published/claude");
    assert_eq!(first_publication.lines().next(), second_publication.lines().next());
    assert_ne!(first_publication, second_publication, "a second publish reused its stamp");
    let second = adapter.request();
    assert_eq!(second["operation"], "reload_plugins");
    assert_eq!(second["session"], first["session"]);
    assert_eq!(second["revision"], second_publication);
    assert_ne!(second["id"], first["id"], "a distinct publication reused the completed request");
    adapter.assert_quiet();
    assert_eq!(native_records(&sandbox).iter().filter(|record| record["kind"] == "receipt").count(), 1);
    adapter.receipt(&sandbox, &second);
    adapter.assert_quiet();
    let records = native_records(&sandbox);
    assert_eq!(records.iter().filter(|record| record["kind"] == "request").count(), 2);
    assert_eq!(records.iter().filter(|record| record["kind"] == "receipt").count(), 2);
    assert!(!sandbox.root.join("state/messages").exists(), "reload polluted the message binding journal");
    assert_no_prompt_delivery(&sandbox);
    let agent = log(&sandbox, "dummy.log");
    assert_eq!(agent.matches("agent plugins Test tab").count(), 1, "the agent restarted: {agent}");
    assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
    let output = finish(child, Duration::from_secs(60));
    assert!(output.status.success());
    assert_unfinished(&sandbox, "plugins");
}

#[test]
fn missing_claude_adapter_or_completion_receipt_never_applies_or_types_an_update() {
    for claimed in [false, true] {
        let sandbox = sandbox();
        set_reply(&sandbox, "agent-store-show.json", BOUND);
        let launch = if claimed { "reload-unverified" } else { "reload-unavailable" };
        let mode = if cfg!(windows) { "keys" } else { "signal" };
        let stderr_path = sandbox.root.join("wrapper-stderr.log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
        command
            .args(with_prompt(&sandbox, launch, "claude", mode))
            .envs(environment(&sandbox))
            .current_dir(&sandbox.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr_path).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = Child::new(command.spawn().unwrap(), &sandbox);
        wait_for_log(&sandbox, if cfg!(windows) { "raw" } else { "agent " });
        let adapter = claimed.then(|| NativeAdapter::start(&sandbox));
        set_status(&sandbox, "idle");
        publish(&sandbox, "claude", "plugins without a completion receipt");
        if let Some(adapter) = adapter {
            let request = adapter.request();
            assert_eq!(request["operation"], "reload_plugins");
            adapter.assert_quiet();
            // A wrapper that marked the publication applied before completion will stop checking
            // and fail to report the missing receipt after the adapter disappears.
            drop(adapter);
        }
        let diagnostic = if claimed { "activation remains unverified" } else { "native adapter is unavailable" };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            if stderr.contains(diagnostic) {
                break;
            }
            assert!(Instant::now() < deadline, "missing {diagnostic:?}: {stderr}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let records = native_records(&sandbox);
        assert_eq!(records.iter().filter(|record| record["kind"] == "request").count(), usize::from(claimed));
        assert!(!records.iter().any(|record| record["kind"] == "receipt"));
        assert_no_prompt_delivery(&sandbox);
        let agent = log(&sandbox, "dummy.log");
        assert_eq!(agent.matches(&format!("agent {launch} Test tab")).count(), 1, "the agent restarted: {agent}");
        assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
        let output = finish(child, Duration::from_secs(60));
        let stderr = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(stderr.contains(diagnostic), "missing {diagnostic:?}: {stderr}");
        assert!(output.status.success());
        assert_unfinished(&sandbox, launch);
    }
}

#[test]
fn a_managed_codex_update_stays_unverified_without_reloading_an_unrelated_daemon() {
    let sandbox = sandbox();
    let codex = fake_codex(&sandbox);
    let diagnostic = "this command has no verified default-daemon attachment";
    let stderr_path = sandbox.root.join("wrapper-stderr.log");
    set_reply(&sandbox, "agent-store-show.json", BOUND);
    let keys = cfg!(windows);
    let arguments = with_prompt(&sandbox, "codex-reload", "codex", if keys { "keys" } else { "signal" });
    let mut command = Command::new(env!("CARGO_BIN_EXE_tj"));
    command
        .args(&arguments)
        .current_dir(&sandbox.root)
        .envs(environment(&sandbox))
        .env("FAKE_CODEX_LOG", &codex)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = Child::new(command.spawn().unwrap(), &sandbox);
    wait_for_log(&sandbox, if keys { "raw" } else { "agent " });
    publish(&sandbox, "codex", "plugins b");
    wait_until("the wrapper never reported the unavailable native reload", || {
        std::fs::read_to_string(&stderr_path).unwrap_or_default().contains(diagnostic)
    });
    still_running(&mut child, Duration::from_millis(500));
    assert!(!codex.exists(), "the wrapper contacted a shared daemon that does not own its native engine");
    let agent = log(&sandbox, "dummy.log");
    assert_eq!(agent.matches("agent codex-reload Test tab").count(), 1, "the agent restarted: {agent}");
    assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
    let output = finish(child, Duration::from_secs(60));
    assert!(output.status.success());
    assert_unfinished(&sandbox, "codex-reload");
    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    assert_eq!(stderr.matches(diagnostic).count(), 1, "{stderr}");
}

#[test]
fn a_binary_and_plugin_publication_keep_the_same_frontend_and_announce_the_next_launch_once() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let bin = sandbox.root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let name = |stem: &str| format!("{stem}{}", std::env::consts::EXE_SUFFIX);
        let install = |extra: &[u8]| {
            let target = bin.join(name("tj"));
            if target.exists() {
                std::fs::rename(&target, bin.join(format!("{}.old", name("tj")))).unwrap();
            }
            let mut binary = std::fs::read(env!("CARGO_BIN_EXE_tj")).unwrap();
            binary.extend_from_slice(extra);
            std::fs::write(&target, binary).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        install(b"");
        #[cfg(windows)]
        std::fs::copy(env!("CARGO_BIN_EXE_tj_hook"), bin.join(name("tj_hook"))).unwrap();
        set_reply(&sandbox, "agent-store-show.json", BOUND);
        let arguments = with_prompt(&sandbox, "upgrade", host, "signal");
        let stderr = sandbox.root.join("upgrade-stderr.log");
        let mut command = Command::new(bin.join(name("tj")));
        command
            .args(arguments)
            .current_dir(&sandbox.root)
            .envs(environment(&sandbox))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = Child::new(command.spawn().unwrap(), &sandbox);
        wait_for_log(&sandbox, "pid ");
        let adapter = (host == "claude").then(|| NativeAdapter::start(&sandbox));
        set_status(&sandbox, "idle");
        install(b"\0");
        publish(&sandbox, host, "plugins with new binary");
        if let Some(adapter) = adapter {
            let request = adapter.request();
            adapter.receipt(&sandbox, &request);
        } else {
            // The arbitrary dummy command lacks a daemon attachment; reload must still be attempted.
            wait_until("missing plugin refresh diagnostic", || {
                log(&sandbox, "upgrade-stderr.log").contains("no verified default-daemon attachment")
            });
        }
        let notice = "A new tj binary is installed. Your next launch will use it.";
        wait_until("missing pending binary notice", || log(&sandbox, "upgrade-stderr.log").contains(notice));
        still_running(&mut child, Duration::from_secs(2));
        let native = log(&sandbox, "dummy.log");
        assert_eq!(native.lines().filter(|line| line.starts_with("pid ")).count(), 1, "{native}");
        assert!(!native.contains("graceful"), "{native}");
        assert_no_prompt_delivery(&sandbox);
        assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
        assert!(finish(child, Duration::from_secs(60)).status.success());
        assert_eq!(log(&sandbox, "upgrade-stderr.log").matches(notice).count(), 1);
    }
}

#[test]
fn launch_prints_nothing_outside_tj_run() {
    let output = Command::new(env!("CARGO_BIN_EXE_tj"))
        .arg("launch")
        .env_remove("TJ_RUN_LAUNCH_ID")
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();
    assert!(output.status.success() && output.stdout.is_empty());
}

fn shell_started_session_needs_only_the_agent_command(host: &str) {
    let sandbox = sandbox();
    set_reply(
        &sandbox,
        "agent-store-launches.json",
        &serde_json::json!({"ok": true, "data": {"launches": [{"launch_id": "x", "host": host, "title": "Feature-1", "worktree": "w", "state": "active", "wrapper": {"pid": 1, "started_at": "2026-01-01T00:00:00Z"}}]}}).to_string(),
    );
    let fixture = fixture().to_string_lossy().into_owned();
    let arguments = ["_session", "--host", host, "--", python(), &fixture, "exit"].map(str::to_owned);
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
            .contains(&format!("launch --launch-id {id} --host {host} --title Feature-1-2"))
    );

    let unknown = ["_session", "--", python(), &fixture, "exit"].map(str::to_owned);
    let output = finish(spawn(&sandbox, &unknown, &sandbox.worktree), Duration::from_secs(60));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pass --host"));
}

#[test]
fn a_shell_started_session_needs_only_the_agent_command() {
    shell_started_session_needs_only_the_agent_command("claude");
}

#[test]
fn a_shell_started_codex_session_needs_only_the_agent_command() {
    shell_started_session_needs_only_the_agent_command("codex");
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
        done(&sandbox);
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

fn predecessor(sandbox: &Sandbox, launch: &str, host: &str) -> serde_json::Value {
    serde_json::json!({
        "launch_id": launch, "host": host, "title": "Previous tab", "cwd": sandbox.primary,
        "worktree": sandbox.primary, "session": format!("{host}:sess-1"),
        "item": {"ref": ITEM}, "state": "active", "open": false,
        "wrapper": {"pid": 0, "started_at": "2026-01-01T00:00:00Z"}
    })
}

fn recovered_arguments(sandbox: &Sandbox, launch: &str, host: &str) -> Vec<String> {
    let keys = cfg!(windows) && host == "codex";
    let mut arguments = run_arguments(sandbox, launch, host, if keys { "keys" } else { "signal" }, "2");
    let cwd = arguments.iter().position(|arg| arg == "--cwd").unwrap() + 1;
    arguments[cwd] = sandbox.primary.to_string_lossy().into_owned();
    set_reply(
        sandbox,
        "agent-store-show.json",
        &serde_json::json!({
            "ok": true, "data": {
                "launch": {"item": {"ref": ITEM}}, "session": {"worktree": sandbox.primary}
            }
        })
        .to_string(),
    );
    arguments
}

fn crash_after_final_response(sandbox: &Sandbox, arguments: &[String]) {
    let prior = log(sandbox, "dummy.log").lines().filter(|line| line.starts_with("pid ")).count();
    let mut child = spawn(sandbox, arguments, &sandbox.root);
    wait_for_polls(sandbox, "dummy.log", "pid ", prior + 1);
    set_turn(sandbox, false);
    let turn = format!("crash-{prior}");
    lifecycle(sandbox, &turn, "start", None, None);
    let (code, stderr) = in_agent(sandbox, &["finish"]);
    assert_eq!(code, 0, "{stderr}");
    lifecycle(sandbox, &turn, "end", Some("Final result before wrapper failure"), None);
    child.kill().unwrap();
    assert!(!finish(child, Duration::from_secs(60)).status.success());
}

#[test]
fn recovered_done_from_primary_removes_predecessor_claims_with_original_identity() {
    for host in ["claude", "codex"] {
        let sandbox = sandbox();
        let launch = format!("recovered-{host}");
        let old = format!("previous-{host}");
        let arguments = recovered_arguments(&sandbox, &launch, host);
        let mut rows = vec![predecessor(&sandbox, &old, host)];
        // Store's canonical goal controls history rather than the CLI item spelling.
        let item = serde_json::json!({"ref": "github:github.com/acme/app:42"});
        rows[0]["item"] = item.clone();
        rows[0]["state"] = serde_json::json!("ended");
        rows[0]["end_reason"] = serde_json::json!("closed");
        let mut claims = vec![serde_json::json!({
            "path": sandbox.worktree.canonicalize().unwrap(), "session": format!("{host}:sess-1"), "launch_id": old
        })];
        let mut retained = Vec::new();
        for (id, session, item) in [("foreign", "codex:foreign", ITEM), ("other-item", "claude:sess-1", ITEM)] {
            let path = sandbox.root.join(id);
            git(&sandbox.primary, &["worktree", "add", "-q", path.to_str().unwrap(), "-b", id]);
            let mut row = predecessor(&sandbox, id, host);
            // The other-item row shares this native session in both host modes.
            let session = if id == "other-item" { format!("{host}:sess-1") } else { session.to_owned() };
            row["session"] = serde_json::json!(session);
            row["item"]["ref"] = serde_json::json!(item);
            rows.push(row);
            claims.push(serde_json::json!({"path": path.canonicalize().unwrap(), "session": session, "launch_id": id}));
            retained.push(path);
        }
        let current = sandbox.root.join("current");
        if host == "codex" {
            git(&sandbox.primary, &["worktree", "add", "-q", current.to_str().unwrap(), "-b", "current"]);
            claims.push(serde_json::json!({"path": current.canonicalize().unwrap(), "session": format!("{host}:sess-1"), "launch_id": launch}));
        }
        set_store_list(&sandbox.stubs, "launches", serde_json::json!(rows));
        set_store_list(&sandbox.stubs, "worktrees", serde_json::json!(claims));
        let child = spawn(&sandbox, &arguments, &sandbox.root);
        wait_for_log(&sandbox, if cfg!(windows) && host == "codex" { "raw" } else { "agent " });
        // The launch fixture records the supplied spelling first; native registration then
        // resolves it to the canonical tracker identity before completion is requested.
        set_reply(
            &sandbox,
            "agent-store-show.json",
            &serde_json::json!({"ok": true, "data": {"launch": {"item": item}, "session": {}}}).to_string(),
        );
        done(&sandbox);
        let output = finish(child, Duration::from_secs(120));
        assert_closed_out(&sandbox, &output, &launch);
        assert!(git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        assert!(retained.iter().all(|path| path.exists()));
        assert!(!current.exists());
        let store = log(&sandbox, "stubs/agent-store.log");
        assert!(
            store.contains(&format!("release-worktree --session {host}:sess-1 --launch-id {old} --path ")),
            "{store}"
        );
        assert!(!store.contains("--launch-id foreign --path"), "{store}");
        assert!(!store.contains("--launch-id other-item --path"), "{store}");
        assert_eq!(store.matches("release-worktree ").count(), if host == "codex" { 2 } else { 1 });
    }
}

#[test]
fn unlinked_recovery_keeps_historical_claims_and_cleans_only_the_current_launch() {
    let sandbox = sandbox();
    let launch = "unlinked-current";
    let mut arguments = recovered_arguments(&sandbox, launch, "claude");
    let item = arguments.iter().position(|value| value == "--item").unwrap();
    arguments.drain(item..=item + 1);
    set_reply(
        &sandbox,
        "agent-store-show.json",
        &serde_json::json!({"ok": true, "data": {"launch": {"item": null}, "session": {}}}).to_string(),
    );
    let mut old = predecessor(&sandbox, "unlinked-previous", "claude");
    old["item"] = serde_json::Value::Null;
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old]));
    let current = sandbox.root.join("current");
    git(&sandbox.primary, &["worktree", "add", "-q", current.to_str().unwrap(), "-b", "current"]);
    set_store_list(
        &sandbox.stubs,
        "worktrees",
        serde_json::json!([
            {"path": sandbox.worktree.canonicalize().unwrap(), "session": "claude:sess-1", "launch_id": "unlinked-previous"},
            {"path": current.canonicalize().unwrap(), "session": "claude:sess-1", "launch_id": launch}
        ]),
    );
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    done(&sandbox);
    let output = finish(child, Duration::from_secs(120));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(sandbox.worktree.exists(), "unlinked historical work belongs to an unidentified task");
    assert!(!current.exists());
    assert!(!git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert!(!store.contains("--launch-id unlinked-previous --path"), "{store}");
    assert_eq!(store.matches("release-worktree ").count(), 1, "{store}");
    assert!(store.contains("end --launch-id unlinked-current --reason closed"));
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
}

#[test]
fn recovered_preflight_refuses_live_unknown_unmanaged_and_unsafe_historical_claims() {
    let sandbox = sandbox();
    let launch = "recovered-refusals";
    let arguments = recovered_arguments(&sandbox, launch, "claude");
    let old = predecessor(&sandbox, "previous", "claude");
    let claim = serde_json::json!({"path": sandbox.worktree.canonicalize().unwrap(), "session": "claude:sess-1", "launch_id": "previous"});
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([claim]));
    let mut live = old.clone();
    let pid = std::process::id();
    let started: chrono::DateTime<chrono::Utc> = tj_cli::process::agent::started_at(pid).unwrap().unwrap().into();
    live["wrapper"] = serde_json::json!({"pid": pid, "started_at": started.to_rfc3339()});
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([live]));
    let mut child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    refused(&sandbox, &mut child, launch);

    let mut unknown = old.clone();
    unknown["wrapper"]["started_at"] = serde_json::json!("invalid");
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([unknown]));
    refused(&sandbox, &mut child, launch);
    let mut incomplete = old.clone();
    incomplete["wrapper"] = serde_json::Value::Null;
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([incomplete]));
    refused(&sandbox, &mut child, launch);
    let mut missing_item = old.clone();
    missing_item.as_object_mut().unwrap().remove("item");
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([missing_item]));
    refused(&sandbox, &mut child, launch);
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old]));

    let mut unmanaged = claim.clone();
    unmanaged["launch_id"] = serde_json::Value::Null;
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([claim, unmanaged]));
    refused(&sandbox, &mut child, launch);
    let mut duplicate = claim.clone();
    duplicate["launch_id"] = serde_json::json!(launch);
    let path = sandbox.worktree.canonicalize().unwrap().to_string_lossy().into_owned();
    duplicate["path"] = serde_json::json!(if cfg!(windows) { path.to_lowercase() } else { format!("{path}/") });
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([claim, duplicate]));
    refused(&sandbox, &mut child, launch);

    let mut primary = claim.clone();
    primary["path"] = serde_json::json!(sandbox.primary.canonicalize().unwrap());
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([primary]));
    refused(&sandbox, &mut child, launch);
    set_store_list(&sandbox.stubs, "worktrees", serde_json::json!([claim]));
    std::fs::write(sandbox.worktree.join("unfinished.txt"), "only copy").unwrap();
    refused(&sandbox, &mut child, launch);
    assert_eq!(std::fs::read_to_string(sandbox.worktree.join("unfinished.txt")).unwrap(), "only copy");
    git(&sandbox.worktree, &["add", "."]);
    git(&sandbox.worktree, &["commit", "-q", "-m", "unfinished"]);
    refused(&sandbox, &mut child, launch);
    assert_unfinished(&sandbox, launch);
    git(&sandbox.primary, &["merge", "--squash", "Feature/1"]);
    git(&sandbox.primary, &["commit", "-q", "-m", "delivered"]);
    git(&sandbox.primary, &["push", "-q", "origin", "main"]);
    done(&sandbox);
    let output = finish(child, Duration::from_secs(120));
    assert_closed_out(&sandbox, &output, launch);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for reason in [
        "historical launch wrapper is still alive",
        "invalid wrapper creation time",
        "historical launch identity is incomplete",
        "agent-store launch is missing its item field",
        "unmanaged worktree claims; recovery/adoption required",
        "duplicate worktree ownership",
        "primary checkout",
        "uncommitted changes",
        "unmerged commits",
    ] {
        assert!(stderr.contains(reason), "missing {reason}: {stderr}");
    }
}

#[test]
fn recovered_journal_retries_partial_multi_launch_releases_under_original_owners() {
    let sandbox = sandbox();
    let launch = "recovered-journal";
    let arguments = recovered_arguments(&sandbox, launch, "claude");
    let mut old = predecessor(&sandbox, "previous", "claude");
    old["state"] = serde_json::json!("ended");
    old["end_reason"] = serde_json::json!("closed");
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old]));
    let current = sandbox.root.join("a-current");
    git(&sandbox.primary, &["worktree", "add", "-q", current.to_str().unwrap(), "-b", "current"]);
    set_store_list(
        &sandbox.stubs,
        "worktrees",
        serde_json::json!([
            {"path": current.canonicalize().unwrap(), "session": "claude:sess-1", "launch_id": launch},
            {"path": sandbox.worktree.canonicalize().unwrap(), "session": "claude:sess-1", "launch_id": "previous"}
        ]),
    );
    let failure = "agent-store-release-worktree-previous.json";
    set_reply(&sandbox, failure, r#"{"ok":false,"error":{"message":"historical release failed"}}"#);
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, "agent ");
    done(&sandbox);
    let output = finish(child, Duration::from_secs(120));
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!sandbox.worktree.exists() && !current.exists());
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    assert!(!log(&sandbox, "stubs/agent-store.log").contains("--reason closed"));
    let journal = sandbox.root.join("cache/run/recovered-journal.closeout.json");
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
    assert_eq!(saved["schema"], 1);
    assert_eq!(saved["removals"].as_array().unwrap().len(), 2);
    std::fs::remove_file(sandbox.stubs.join(failure)).unwrap();
    let mut current_row = predecessor(&sandbox, launch, "claude");
    current_row["state"] = serde_json::json!("closing");
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old, current_row]));
    let output = sweep_closeout(&sandbox);
    assert_closed_out(&sandbox, &output, launch);
    assert!(!journal.exists());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert_eq!(
        store.matches(&format!("release-worktree --session claude:sess-1 --launch-id {launch} --path ")).count(),
        1
    );
    assert_eq!(store.matches("release-worktree --session claude:sess-1 --launch-id previous --path ").count(), 2);
}

#[test]
fn recovered_predecessor_journal_survives_another_recovery_and_retires_consumed_proof() {
    let sandbox = sandbox();
    let arguments = recovered_arguments(&sandbox, "recovered-proof", "claude");
    crash_after_final_response(&sandbox, &arguments);
    claim(&sandbox, "previous", "claude", std::slice::from_ref(&sandbox.worktree));
    let journal_dir = sandbox.root.join("cache/run");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let previous = journal_dir.join("previous.closeout.json");
    let proof = serde_json::json!({
        "schema": 1, "launch": "previous", "host": "claude", "session": "claude:sess-1",
        "repository": sandbox.primary,
        "removals": [{"primary": sandbox.primary,
            "worktree": sandbox.worktree.canonicalize().unwrap(), "registration": sandbox.worktree.canonicalize().unwrap(),
            "branch": "Feature/1", "head": git(&sandbox.worktree, &["rev-parse", "HEAD"]), "default": "main"}]
    });
    std::fs::write(&previous, proof.to_string()).unwrap();
    git(&sandbox.primary, &["worktree", "remove", sandbox.worktree.to_str().unwrap()]);
    git(&sandbox.primary, &["branch", "-d", "Feature/1"]);
    let mut old = predecessor(&sandbox, "previous", "claude");
    old["state"] = serde_json::json!("ended");
    old["end_reason"] = serde_json::json!("closed");
    let mut current = predecessor(&sandbox, "recovered-proof", "claude");
    current["state"] = serde_json::json!("closing");
    let mut previous_binding: serde_json::Value =
        serde_json::from_str(&log(&sandbox, "stubs/agent-store-show.json")).unwrap();
    previous_binding["data"]["launch"] = old.clone();
    set_reply(&sandbox, "agent-store-show-previous.json", &previous_binding.to_string());
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old, current]));
    for (field, value) in [
        ("session", serde_json::json!("claude:foreign")),
        ("launch", serde_json::json!("foreign")),
        ("host", serde_json::json!("codex")),
    ] {
        let mut foreign = proof.clone();
        foreign[field] = value;
        std::fs::write(&previous, foreign.to_string()).unwrap();
        let refused = sweep_closeout(&sandbox);
        assert_eq!(refused.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&refused.stdout).contains("cleanup journal"));
        assert!(previous.exists());
        assert!(!log(&sandbox, "stubs/agent-store.log").contains("release-worktree"));
        assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    }
    std::fs::write(&previous, proof.to_string()).unwrap();
    let failure = "agent-store-release-worktree-previous.json";
    set_reply(&sandbox, failure, r#"{"ok":false,"error":{"message":"interrupted release"}}"#);
    let interrupted = sweep_closeout(&sandbox);
    assert_eq!(interrupted.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&interrupted.stdout).contains("interrupted release"));
    assert!(!previous.exists());
    let recovered = journal_dir.join("recovered-proof.closeout.json");
    assert!(recovered.exists());
    std::fs::remove_file(sandbox.stubs.join(failure)).unwrap();
    set_reply(
        &sandbox,
        "agent-store-show.json",
        &serde_json::json!({"ok":true,"data": {
            "launch": {"launch_id": "recovered-again"}, "session": {"launch_id": "recovered-again"}
        }})
        .to_string(),
    );
    current["state"] = serde_json::json!("ended");
    current["end_reason"] = serde_json::json!("closed");
    let mut next = predecessor(&sandbox, "recovered-again", "claude");
    next["state"] = serde_json::json!("closing");
    set_store_list(&sandbox.stubs, "launches", serde_json::json!([old, current, next]));
    let arguments = recovered_arguments(&sandbox, "recovered-again", "claude");
    crash_after_final_response(&sandbox, &arguments);
    let mut current_binding: serde_json::Value =
        serde_json::from_str(&log(&sandbox, "stubs/agent-store-show.json")).unwrap();
    current_binding["data"]["launch"] = current.clone();
    set_reply(&sandbox, "agent-store-show-recovered-proof.json", &current_binding.to_string());
    let finished = Command::new(env!("CARGO_BIN_EXE_tj"))
        .args(["closeout", ITEM])
        .envs(environment(&sandbox))
        .current_dir(&sandbox.root)
        .output()
        .unwrap();
    assert!(finished.status.success(), "{}", String::from_utf8_lossy(&finished.stdout));
    assert!(!previous.exists() && !recovered.exists() && !journal_dir.join("recovered-again.closeout.json").exists());
    let store = log(&sandbox, "stubs/agent-store.log");
    assert_eq!(store.matches("release-worktree --session claude:sess-1 --launch-id previous --path ").count(), 2);
    assert!(store.contains("end --launch-id recovered-again --reason closed"));
    git(&sandbox.primary, &["worktree", "add", "-q", sandbox.worktree.to_str().unwrap(), "-b", "Feature/1"]);
    let reused = sweep_closeout(&sandbox);
    assert!(reused.status.success(), "{}", String::from_utf8_lossy(&reused.stdout));
    assert!(sandbox.worktree.exists());
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
    done(&sandbox);
    let output = finish(child, Duration::from_secs(120));
    assert_closed_out(&sandbox, &output, "dirty-collection");
    assert!(!extra.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("worktree has uncommitted changes"), "{stderr}");
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
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "live-owner");
    assert!(String::from_utf8_lossy(&output.stderr).contains("another session may still be using"));
}

#[test]
fn release_failure_keeps_the_claim_and_never_marks_done() {
    let sandbox = sandbox();
    set_reply(&sandbox, "agent-store-release-worktree.json", r#"{"ok":false,"error":{"message":"storage failed"}}"#);
    let child = start(&sandbox, "failed-release", "claude", "signal", "2");
    done(&sandbox);
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
    retry_closeout_with_reason(sandbox, launch, "exited")
}

fn retry_closeout_with_reason(sandbox: &Sandbox, launch: &str, reason: &str) -> Output {
    set_store_list(
        &sandbox.stubs,
        "launches",
        serde_json::json!([{
            "launch_id": launch, "host": "claude", "title": "Test tab",
            "worktree": sandbox.worktree, "state": "ended", "end_reason": reason,
            "item": {"ref": ITEM},
            "wrapper": {"pid": 0, "started_at": "2026-01-01T00:00:00Z"}
        }]),
    );
    sweep_closeout(sandbox)
}

fn sweep_closeout(sandbox: &Sandbox) -> Output {
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
        done(&sandbox);
        // This exercises both real Git/worktrunk preflights before injecting the finalization failure.
        // Windows CI exceeded the old 60-second fixture budget on the identical tree that passed the PR.
        let started = Instant::now();
        let output = finish(child, Duration::from_secs(120));
        eprintln!("{stage} finalization reached its injected failure in {:?}", started.elapsed());
        assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(!sandbox.worktree.exists());
        assert!(sandbox.stubs.join("released.json").is_file());
        let journal = sandbox.root.join("cache/run").join(format!("{launch}.closeout.json"));
        assert!(journal.is_file());
        let saved_journal = std::fs::read(&journal).unwrap();
        std::fs::remove_file(sandbox.stubs.join(failed)).unwrap();
        if stage == "end" {
            std::fs::write(sandbox.stubs.join("tj-item-progress.failed"), "tracker must not be called twice").unwrap();
        }
        git(&sandbox.primary, &["branch", "Feature/1"]);
        let ended = log(&sandbox, "stubs/agent-store.log").matches("--reason closed").count();
        let refused = retry_closeout(&sandbox, &launch);
        assert_eq!(refused.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&refused.stdout).contains("branch was retained"));
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("--reason closed").count(), ended);
        assert!(journal.is_file());
        assert!(!git(&sandbox.primary, &["branch", "--list", "Feature/1"]).is_empty());
        git(&sandbox.primary, &["branch", "-d", "Feature/1"]);
        let output = retry_closeout_with_reason(&sandbox, &launch, if stage == "end" { "closed" } else { "exited" });
        assert_closed_out(&sandbox, &output, &launch);
        assert!(!journal.exists());
        assert_eq!(log(&sandbox, "stubs/agent-store.log").matches("release-worktree ").count(), 1);
        assert_eq!(
            log(&sandbox, "stubs/tj-item.log").matches("--state Done").count(),
            if stage == "tracker" { 2 } else { 1 }
        );
        if stage == "end" {
            std::fs::write(&journal, saved_journal).unwrap();
            let recovered = retry_closeout_with_reason(&sandbox, &launch, "closed");
            assert!(recovered.status.success(), "{}", String::from_utf8_lossy(&recovered.stdout));
            assert!(!journal.exists());
            assert_eq!(log(&sandbox, "stubs/tj-item.log").matches("--state Done").count(), 1);
        }
    }
}

#[test]
fn recovery_without_a_journal_keeps_live_or_unverified_native_sessions() {
    let sandbox = sandbox();
    let launch = "native-still-live";
    let child = start(&sandbox, launch, "claude", "signal", "2");
    set_reply(
        &sandbox,
        "tj-session-liveness.json",
        &serde_json::json!({"session":"claude:sess-1", "launch_id":launch, "state":"live"}).to_string(),
    );
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_eq!(output.status.code(), Some(1));
    assert!(sandbox.worktree.exists());
    assert!(!sandbox.root.join("cache/run").join(format!("{launch}.closeout.json")).exists());
    let kept = retry_closeout(&sandbox, launch);
    assert_eq!(kept.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&kept.stdout).contains("native session may still be live"));
    std::fs::write(sandbox.stubs.join("tj-session-liveness.failed"), "provider query unavailable").unwrap();
    let unknown = retry_closeout(&sandbox, launch);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stdout).contains("native session liveness is unverified"));
    assert!(sandbox.worktree.exists());
    assert!(!log(&sandbox, "stubs/tj-item.log").contains("--state Done"));
    std::fs::remove_file(sandbox.stubs.join("tj-session-liveness.failed")).unwrap();
    set_reply(
        &sandbox,
        "tj-session-liveness.json",
        &serde_json::json!({"session":"claude:sess-1", "launch_id":launch, "state":"dead"}).to_string(),
    );
    let recovered = retry_closeout(&sandbox, launch);
    assert_closed_out(&sandbox, &recovered, launch);
}

#[test]
fn native_daemon_idle_grace_finishes_when_the_provider_unloads() {
    let sandbox = sandbox();
    let launch = "native-exit-grace";
    let child = start(&sandbox, launch, "codex", if cfg!(windows) { "keys" } else { "signal" }, "3");
    let state = |state| serde_json::json!({"session":"codex:sess-1", "launch_id":launch, "state":state}).to_string();
    set_reply(&sandbox, "tj-session-liveness.json", &state("live"));
    done(&sandbox);
    wait_until("wrapper never checked native teardown", || !log(&sandbox, "stubs/tj-session.log").is_empty());
    let next = sandbox.stubs.join("native-unloaded.json");
    std::fs::write(&next, state("dead")).unwrap();
    std::fs::rename(next, sandbox.stubs.join("tj-session-liveness.json")).unwrap();
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, launch);
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
    done(&sandbox);
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
    done(&sandbox);
    let output = finish(child, Duration::from_secs(60));
    assert_closed_out(&sandbox, &output, "unclaimed");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no recorded ownership; recovery required"));
}

const MARKETPLACE_SHA: &str = "e555013e5e7bab455193e79be12a15e588bdd221";
const UPSTREAM_SHA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";

struct Upstream {
    url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Upstream {
    /// Answers every request with `reply`, or holds the connection open without answering when it is empty.
    fn serve(reply: String) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/repos/tj-agents/marketplace/commits/main", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().map_while(Result::ok) {
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|read| read == 1) {
                    request.push(byte[0]);
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&request).to_lowercase());
                if reply.is_empty() {
                    std::thread::sleep(Duration::from_secs(120));
                } else {
                    let _ = stream.write_all(reply.as_bytes());
                }
            }
        });
        Self { url, requests }
    }

    fn unchanged() -> Self {
        Self::serve("HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned())
    }

    fn changed() -> Self {
        Self::serve(format!("HTTP/1.1 200 OK\r\nContent-Length: 40\r\nConnection: close\r\n\r\n{UPSTREAM_SHA}"))
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn select_tj_agents(sandbox: &Sandbox) {
    let folder = sandbox.worktree.join(".agents");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("plugins.json"), r#"{"plugins":["agent@tj-agents"]}"#).unwrap();
}

/// Selects tj-agents with a recent update check, so the launch makes no request.
fn select_tj_agents_checked(sandbox: &Sandbox) {
    select_tj_agents(sandbox);
    std::fs::create_dir_all(update_state(sandbox, "")).unwrap();
    for host in ["claude", "codex"] {
        std::fs::write(update_state(sandbox, &format!("{host}.checked")), "").unwrap();
    }
}

fn launch_checking(sandbox: &Sandbox, upstream: &Upstream, launch: &str) -> (Output, Duration) {
    let mut arguments = run_arguments(sandbox, launch, "claude", "exit", "10");
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(separator..separator, ["--update-url".to_owned(), upstream.url.clone()]);
    let started = Instant::now();
    let child = spawn(sandbox, &arguments, &sandbox.root);
    wait_for_log(sandbox, &format!("agent {launch}"));
    let reached = started.elapsed();
    (finish(child, Duration::from_secs(60)), reached)
}

fn update_state(sandbox: &Sandbox, file: &str) -> PathBuf {
    sandbox.root.join("cache").join("update").join(file)
}

#[test]
fn an_unchanged_marketplace_makes_one_conditional_request_and_installs_nothing() {
    let sandbox = sandbox();
    select_tj_agents(&sandbox);
    publish(&sandbox, "claude", MARKETPLACE_SHA);
    let upstream = Upstream::unchanged();
    let (output, _) = launch_checking(&sandbox, &upstream, "unchanged");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("tj-agents"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = upstream.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains(&format!("if-none-match: \"{MARKETPLACE_SHA}\"")), "{}", requests[0]);
    assert!(requests[0].contains("accept: application/vnd.github.sha"), "{}", requests[0]);
    launch_checking(&sandbox, &upstream, "again");
    assert_eq!(upstream.requests().len(), 1, "checked again within the shared interval");
    assert_eq!(log(&sandbox, "stubs/tj-plugins.log"), "");
}

#[test]
fn a_changed_marketplace_installs_in_the_background_once_per_commit() {
    let sandbox = sandbox();
    select_tj_agents(&sandbox);
    stub(&sandbox.stubs, "tj-plugins", "updated");
    let upstream = Upstream::changed();
    let (output, _) = launch_checking(&sandbox, &upstream, "changed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("updating claude plugins in the background"), "{stderr}");
    wait_until("tj-plugins never ran", || log(&sandbox, "stubs/tj-plugins.log").contains("update --host claude"));
    assert!(!upstream.requests()[0].contains("if-none-match"), "{}", upstream.requests()[0]);
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    launch_checking(&sandbox, &upstream, "retry");
    assert_eq!(upstream.requests().len(), 2);
    assert_eq!(log(&sandbox, "stubs/tj-plugins.log").lines().count(), 1, "installed the same commit twice");
}

#[test]
fn a_running_tab_checks_again_and_starts_an_update() {
    let sandbox = sandbox();
    select_tj_agents_checked(&sandbox);
    stub(&sandbox.stubs, "tj-plugins", "updated");
    let upstream = Upstream::changed();
    let mut arguments = run_arguments(&sandbox, "periodic", "claude", "signal", "10");
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(separator..separator, ["--update-url".to_owned(), upstream.url.clone()]);
    let mut child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, "agent periodic");
    assert!(upstream.requests().is_empty(), "launch ignored the recent shared check");
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    wait_until("running tab never started the update", || {
        log(&sandbox, "stubs/tj-plugins.log").contains("update --host claude")
    });
    assert_eq!(upstream.requests().len(), 1);
    // The supervisor collects the check on its next polls. Its result must not reach the agent's console.
    still_running(&mut child, Duration::from_secs(3));
    child.kill().unwrap();
    let stderr = String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).into_owned();
    assert!(!stderr.contains("tj-agents"), "update text written over the running agent: {stderr}");
}

#[test]
fn pending_codex_activation_does_not_suppress_periodic_checks_or_repeat_the_warning() {
    let sandbox = sandbox();
    select_tj_agents_checked(&sandbox);
    stub(&sandbox.stubs, "tj-plugins", "updated");
    let upstream = Upstream::changed();
    let mut arguments = run_arguments(&sandbox, "pending-activation", "codex", "signal", "10");
    let separator = arguments.iter().position(|argument| argument == "--").unwrap();
    arguments.splice(separator..separator, ["--update-url".to_owned(), upstream.url.clone()]);
    let child = spawn(&sandbox, &arguments, &sandbox.root);
    wait_for_log(&sandbox, "agent pending-activation");
    publish(&sandbox, "codex", "pending revision");
    std::fs::remove_file(update_state(&sandbox, "codex.checked")).unwrap();
    wait_until("pending activation suppressed the updater", || {
        log(&sandbox, "stubs/tj-plugins.log").contains("update --host codex")
    });
    std::fs::remove_file(update_state(&sandbox, "codex.checked")).unwrap();
    wait_until("pending activation suppressed a later marketplace check", || upstream.requests().len() == 2);
    assert_eq!(log(&sandbox, "stubs/tj-plugins.log").lines().count(), 1, "duplicate updater");
    assert_no_prompt_delivery(&sandbox);
    assert_eq!(in_agent(&sandbox, &["exit-native"]).0, 0);
    let output = finish(child, Duration::from_secs(30));
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("this command has no verified default-daemon attachment").count(), 1, "{stderr}");
}

#[test]
fn a_failed_updater_reports_failure_and_retries_without_publishing_success() {
    let sandbox = sandbox();
    select_tj_agents(&sandbox);
    stub(&sandbox.stubs, "tj-plugins", "updated");
    std::fs::write(sandbox.stubs.join("tj-plugins-update.failed"), "installation failed").unwrap();
    let upstream = Upstream::changed();
    launch_checking(&sandbox, &upstream, "failed-update");
    wait_until("updater failure was not reported", || {
        log(&sandbox, "cache/update/claude.log").contains("native updater failed")
    });
    assert!(!sandbox.root.join("cache/published/claude").exists());
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    launch_checking(&sandbox, &upstream, "debounced-failure");
    assert_eq!(log(&sandbox, "stubs/tj-plugins.log").lines().count(), 1);
    // Forget the attempt to exercise retry without waiting ten minutes. Unit tests cover the boundary.
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    std::fs::remove_file(update_state(&sandbox, "claude.started")).unwrap();
    std::fs::remove_file(sandbox.stubs.join("tj-plugins-update.failed")).unwrap();
    launch_checking(&sandbox, &upstream, "retry-update");
    wait_until("failed update never retried", || log(&sandbox, "stubs/tj-plugins.log").lines().count() == 2);
    assert!(!sandbox.root.join("cache/published/claude").exists(), "only the native updater may publish convergence");
}

#[test]
fn an_unanswered_probe_uses_the_native_updater_and_recovers_without_a_false_success() {
    let sandbox = sandbox();
    select_tj_agents(&sandbox);
    stub(&sandbox.stubs, "tj-plugins", "updated");
    publish(&sandbox, "claude", MARKETPLACE_SHA);
    let publication = std::fs::read_to_string(sandbox.root.join("cache/published/claude")).unwrap();
    let upstream = Upstream::serve(String::new());
    let (output, reached) = launch_checking(&sandbox, &upstream, "offline");
    assert!(
        reached < Duration::from_secs(60),
        "the agent waited {reached:?} for GitHub: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("could not check for tj-agents updates"), "{stderr}");
    assert!(!stderr.contains("changed upstream"), "a failed probe claimed a change: {stderr}");
    wait_until("the unavailable probe never handed over to the native updater", || {
        log(&sandbox, "stubs/tj-plugins.log").contains("update --host claude")
    });
    assert_eq!(std::fs::read_to_string(sandbox.root.join("cache/published/claude")).unwrap(), publication);
    assert_eq!(std::fs::read_to_string(update_state(&sandbox, "claude.started")).unwrap(), "");
    assert!(update_state(&sandbox, "claude.checked").exists(), "a failed check is retried at once");

    // Another wrapper after the check interval must not start a second fallback in the retry window.
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    launch_checking(&sandbox, &upstream, "offline-again");
    assert_eq!(log(&sandbox, "stubs/tj-plugins.log").lines().count(), 1);

    // A genuinely fresh head is actionable immediately, even after that unverified refresh attempt.
    std::fs::remove_file(update_state(&sandbox, "claude.checked")).unwrap();
    let recovered = Upstream::changed();
    launch_checking(&sandbox, &recovered, "recovered");
    wait_until("the recovered probe never started the newly verified update", || {
        log(&sandbox, "stubs/tj-plugins.log").lines().count() == 2
    });
    assert_eq!(std::fs::read_to_string(update_state(&sandbox, "claude.started")).unwrap(), UPSTREAM_SHA);
}

#[test]
fn a_held_lock_or_a_project_without_tj_agents_makes_no_request() {
    let sandbox = sandbox();
    select_tj_agents(&sandbox);
    let upstream = Upstream::unchanged();
    std::fs::create_dir_all(update_state(&sandbox, "")).unwrap();
    std::fs::write(update_state(&sandbox, "claude.lock"), "").unwrap();
    launch_checking(&sandbox, &upstream, "locked");
    assert_eq!(upstream.requests().len(), 0, "checked while another launch held the lock");
    std::fs::remove_file(update_state(&sandbox, "claude.lock")).unwrap();
    std::fs::write(sandbox.worktree.join(".agents").join("plugins.json"), r#"{"plugins":["agent@agents"]}"#).unwrap();
    launch_checking(&sandbox, &upstream, "unselected");
    assert_eq!(upstream.requests().len(), 0, "checked a project that selects no tj-agents plugins");
}
