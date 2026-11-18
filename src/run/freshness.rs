use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const REMOTE: &str = "origin";
const SELECTION: &str = ".agents/plugins.json";
const FETCH_SECONDS: Duration = Duration::from_secs(5);
const LOOKUP_SECONDS: Duration = Duration::from_secs(4);
const GIT_SECONDS: Duration = Duration::from_secs(3);
const TOTAL_SECONDS: Duration = Duration::from_secs(12);
const SYNC: &str = "engineering:sync-checkout";
const SHIPPED: [&str; 2] = ["MERGED", "CLOSED"];

struct Output {
    code: i32,
    out: String,
    err: String,
}

fn kill(id: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &id.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &id.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn bounded(cwd: &Path, program: &str, arguments: &[String], limit: Duration) -> Option<Output> {
    if limit.is_zero() {
        return None;
    }
    let child = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let id = child.id();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(limit) {
        Ok(Ok(output)) => Some(Output {
            code: output.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        _ => {
            kill(id);
            None
        }
    }
}

struct Checkout {
    top: PathBuf,
    deadline: Instant,
}

impl Checkout {
    fn exec(&self, program: &str, arguments: Vec<String>, limit: Duration) -> Option<Output> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        bounded(&self.top, program, &arguments, limit.min(remaining))
    }

    fn git_cmd(&self, arguments: &[&str], limit: Duration) -> Option<Output> {
        let mut full = vec!["-c".to_owned(), "credential.interactive=never".to_owned()];
        full.extend(arguments.iter().map(|argument| (*argument).to_owned()));
        self.exec("git", full, limit)
    }

    fn git(&self, arguments: &[&str], limit: Duration) -> Option<String> {
        let result = self.git_cmd(arguments, limit)?;
        (result.code == 0).then(|| result.out.trim().to_owned())
    }

    fn succeeds(&self, arguments: &[&str]) -> bool {
        self.git_cmd(arguments, GIT_SECONDS).is_some_and(|result| result.code == 0)
    }

    fn gh(&self, arguments: &[&str], limit: Duration) -> Option<Output> {
        self.exec("gh", arguments.iter().map(|argument| (*argument).to_owned()).collect(), limit)
    }
}

struct Fetched {
    checkout: Checkout,
    branch: String,
    had_selection: bool,
    failure: String,
    head: Option<String>,
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("")
}

fn commits(count: u64) -> String {
    format!("{count} commit{}", if count == 1 { "" } else { "s" })
}

fn fetched(directory: &Path, deadline: Instant) -> Option<Fetched> {
    let probe = Checkout { top: directory.to_path_buf(), deadline };
    let output = probe.git(&["rev-parse", "--show-toplevel", "--abbrev-ref", "HEAD"], GIT_SECONDS)?;
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() != 2 || lines[1] == "HEAD" {
        return None;
    }
    let checkout = Checkout { top: PathBuf::from(lines[0]), deadline };
    let had_selection = checkout.top.join(SELECTION).is_file();
    let (remote, fetch, head) = std::thread::scope(|scope| {
        let remote = scope.spawn(|| checkout.git(&["config", "--get", &format!("remote.{REMOTE}.url")], GIT_SECONDS));
        let fetch = scope.spawn(|| checkout.git_cmd(&["fetch", "--quiet", "--prune", "--no-tags", REMOTE], FETCH_SECONDS));
        let head = scope.spawn(|| {
            checkout.git(&["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{REMOTE}/HEAD")], GIT_SECONDS)
        });
        (remote.join().unwrap_or(None), fetch.join().unwrap_or(None), head.join().unwrap_or(None))
    });
    remote?;
    let failure = match &fetch {
        None => format!("could not fetch {REMOTE} (timed out or could not start)"),
        Some(result) if result.code != 0 => {
            let reason = first_line(&result.err);
            format!("could not fetch {REMOTE} ({})", if reason.is_empty() { "failed" } else { reason })
        }
        Some(_) => String::new(),
    };
    Some(Fetched { checkout, branch: lines[1].to_owned(), had_selection, failure, head })
}

fn default_branch(checkout: &Checkout, head: Option<&str>) -> Option<String> {
    if let Some(head) = head
        && let Some(name) = head.strip_prefix(&format!("{REMOTE}/"))
    {
        return Some(name.to_owned());
    }
    for candidate in ["main", "master"] {
        if checkout.succeeds(&["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{REMOTE}/{candidate}")]) {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn counts(checkout: &Checkout, upstream: &str) -> Option<(u64, u64)> {
    let output = checkout.git(&["rev-list", "--left-right", "--count", &format!("{upstream}...HEAD")], GIT_SECONDS)?;
    let values: Vec<&str> = output.split_whitespace().collect();
    if values.len() != 2 {
        return None;
    }
    Some((values[0].parse().ok()?, values[1].parse().ok()?))
}

fn pull_request(checkout: &Checkout, branch: &str) -> (bool, Option<Value>) {
    let Some(result) = checkout.gh(
        &["pr", "list", "--head", branch, "--state", "all", "--limit", "1", "--json", "number,state,url"],
        LOOKUP_SECONDS,
    ) else {
        return (false, None);
    };
    if result.code != 0 {
        return (false, None);
    }
    let Ok(found) = serde_json::from_str::<Value>(result.out.trim()) else { return (false, None) };
    let Some(array) = found.as_array() else { return (false, None) };
    (true, array.first().filter(|candidate| candidate.is_object()).cloned())
}

fn branch_state(checkout: &Checkout, branch: &str, upstream: &str) -> (String, bool) {
    if checkout.succeeds(&["merge-base", "--is-ancestor", "HEAD", upstream]) {
        return (format!("It is fully contained in {upstream}."), true);
    }
    let track = checkout
        .git(
            &["for-each-ref", "--format=%(upstream:short)%00%(upstream:track)", &format!("refs/heads/{branch}")],
            GIT_SECONDS,
        )
        .unwrap_or_default();
    let mut parts = track.splitn(2, '\0');
    let tracked = parts.next().unwrap_or("").to_owned();
    let status = parts.next().unwrap_or("");
    let gone = !tracked.is_empty() && status.contains("gone");
    let (known, request) = pull_request(checkout, branch);
    if let Some(request) = request {
        let state = request.get("state").and_then(Value::as_str).unwrap_or("").to_uppercase();
        let url = request.get("url").and_then(Value::as_str);
        let number = request.get("number").map_or_else(String::new, |value| value.to_string());
        let mut described = format!("Its PR #{number} is {state}");
        described += &url.map_or_else(|| ".".to_owned(), |url| format!(" ({url})."));
        let shipped = SHIPPED.contains(&state.as_str()) || gone;
        return (described, shipped);
    }
    if gone {
        return (format!("Its remote branch {tracked} is gone."), true);
    }
    if known {
        return ("No pull request exists for it.".to_owned(), false);
    }
    ("Its pull request state is unknown.".to_owned(), false)
}

fn feature_notice(checkout: &Checkout, branch: &str, default: &str, behind: u64) -> String {
    let upstream = format!("{REMOTE}/{default}");
    let (state, shipped) = branch_state(checkout, branch, &upstream);
    let fresh = format!("start new work in a fresh worktree from {upstream}");
    let action = if shipped {
        let switch = format!("`git switch {default}`, then `git merge --ff-only {upstream}`");
        let retire = format!("or run `{SYNC}` to retire this branch");
        format!("switch to {default} ({switch}) {retire}; {fresh}")
    } else {
        let merge = format!("run `{SYNC}` to merge {upstream} into this branch");
        format!("{merge} before continuing; {fresh}")
    };
    format!(
        "STALE CHECKOUT: branch {branch} is {} behind {upstream}. {state} Next: {action}. This branch was not changed.",
        commits(behind)
    )
}

fn default_notice(checkout: &Checkout, default: &str, behind: u64, ahead: u64) -> String {
    let upstream = format!("{REMOTE}/{default}");
    let stale = format!("STALE CHECKOUT: {default} is {} behind", commits(behind));
    if ahead > 0 {
        return format!(
            "{stale} and {} ahead of {upstream}, so it cannot fast-forward. Next: run `{SYNC}` and move the local commits to a feature branch.",
            commits(ahead)
        );
    }
    let status = checkout.git(&["--no-optional-locks", "status", "--porcelain", "--untracked-files=no"], GIT_SECONDS);
    if status.as_deref() != Some("") {
        return format!(
            "{stale} {upstream} and has uncommitted changes, so it was not fast-forwarded. Next: commit or set aside those changes, then run `{SYNC}`."
        );
    }
    let merged = checkout.git_cmd(&["merge", "--ff-only", "--quiet", &upstream], GIT_SECONDS);
    let failed = merged.as_ref().is_none_or(|result| result.code != 0);
    if failed {
        let reason = merged.as_ref().map_or_else(
            || "timed out".to_owned(),
            |result| {
                let line = first_line(&result.err);
                if line.is_empty() { "failed".to_owned() } else { line.to_owned() }
            },
        );
        return format!("{stale} {upstream} and the fast-forward failed ({reason}). Next: run `{SYNC}`.");
    }
    let revision = checkout.git(&["rev-parse", "--short", "HEAD"], GIT_SECONDS).unwrap_or_else(|| upstream.clone());
    format!("Checkout freshness: fast-forwarded {default} by {} to {upstream} ({revision}).", commits(behind))
}

fn selection_notice(checkout: &Checkout, default: &str, had_selection: bool) -> String {
    let upstream = format!("{REMOTE}/{default}");
    if checkout.top.join(SELECTION).is_file() {
        if had_selection {
            return String::new();
        }
        return format!("The update added {SELECTION}; restart the session so its plugins load.");
    }
    if !checkout.succeeds(&["cat-file", "-e", &format!("{upstream}:{SELECTION}")]) {
        return String::new();
    }
    format!(
        "PLUGINS WILL NOT LOAD: this checkout has no {SELECTION}, but {upstream} selects plugins, so no repository \
         plugin or standard is active in this session. Next: bring this checkout up to date with {upstream}, then \
         restart the session."
    )
}

fn report(state: Option<Fetched>) -> String {
    let Some(state) = state else { return String::new() };
    let checkout = &state.checkout;
    let default = default_branch(checkout, state.head.as_deref());
    let mut lines = Vec::new();
    if !state.failure.is_empty() {
        let compared = default.as_ref().map_or_else(
            || "freshness unknown".to_owned(),
            |default| format!("compared with the last fetched {REMOTE}/{default}"),
        );
        lines.push(format!("Checkout freshness: {}; {compared}.", state.failure));
    }
    let Some(default) = default else { return lines.join("\n") };
    if let Some((behind, ahead)) = counts(checkout, &format!("{REMOTE}/{default}"))
        && behind > 0
    {
        if state.branch == default {
            lines.push(default_notice(checkout, &default, behind, ahead));
        } else {
            lines.push(feature_notice(checkout, &state.branch, &default, behind));
        }
    }
    let selection = selection_notice(checkout, &default, state.had_selection);
    if !selection.is_empty() {
        lines.push(selection);
    }
    lines.join("\n")
}

pub(crate) fn notice(directory: &Path) -> String {
    let deadline = Instant::now() + TOTAL_SECONDS;
    report(fetched(directory, deadline))
}

pub(crate) struct Background(Option<std::thread::JoinHandle<String>>);

impl Background {
    pub(crate) fn start(cwd: &Path, successor: bool) -> Self {
        if successor {
            return Self(None);
        }
        let cwd = cwd.to_path_buf();
        Self(Some(std::thread::spawn(move || notice(&cwd))))
    }

    pub(crate) fn finish(mut self) {
        if let Some(handle) = self.0.take()
            && let Ok(report) = handle.join()
            && !report.is_empty()
        {
            eprintln!("{report}");
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.join();
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    cwd: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<std::process::ExitCode> {
    let cwd = std::path::absolute(args.cwd.map_or_else(std::env::current_dir, Ok)?)?;
    let report = notice(&cwd);
    if !report.is_empty() {
        eprintln!("{report}");
    }
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::{TOTAL_SECONDS, notice};

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

    struct Fixture {
        _directory: TempDir,
        origin: std::path::PathBuf,
        repo: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let seed = root.join("seed");
        std::fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        std::fs::write(seed.join("README.md"), "seed\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "seed"]);
        let origin = root.join("origin.git");
        git(&root, &["clone", "-q", "--bare", "seed", "origin.git"]);
        let repo = root.join("repo");
        git(&root, &["clone", "-q", "origin.git", "repo"]);
        Fixture { _directory: directory, origin, repo }
    }

    fn advance_origin(fixture: &Fixture) {
        let work = fixture.origin.with_extension("work");
        git(
            fixture.origin.parent().unwrap(),
            &["clone", "-q", fixture.origin.to_str().unwrap(), work.to_str().unwrap()],
        );
        std::fs::write(work.join("more.txt"), "more\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "advance"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
    }

    #[test]
    fn a_clean_behind_default_branch_is_fast_forwarded() {
        let fixture = fixture();
        advance_origin(&fixture);
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        let result = notice(&fixture.repo);
        assert!(result.contains("fast-forwarded main"), "{result}");
        let after = git(&fixture.repo, &["rev-parse", "HEAD"]);
        assert_ne!(before, after, "the default branch was not moved");
    }

    #[test]
    fn a_dirty_behind_default_branch_is_reported_but_not_touched() {
        let fixture = fixture();
        advance_origin(&fixture);
        std::fs::write(fixture.repo.join("README.md"), "dirty\n").unwrap();
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        let result = notice(&fixture.repo);
        assert!(result.contains("STALE CHECKOUT: main"), "{result}");
        assert!(result.contains("uncommitted changes"), "{result}");
        let after = git(&fixture.repo, &["rev-parse", "HEAD"]);
        assert_eq!(before, after, "a dirty default branch was rewritten");
    }

    #[test]
    fn a_behind_feature_branch_is_reported_and_never_rewritten() {
        let fixture = fixture();
        git(&fixture.repo, &["switch", "-q", "-c", "Feature/1"]);
        git(&fixture.repo, &["push", "-q", "-u", "origin", "Feature/1"]);
        advance_origin(&fixture);
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        let result = notice(&fixture.repo);
        assert!(result.contains("STALE CHECKOUT: branch Feature/1 is 1 commit behind origin/main"), "{result}");
        assert!(result.contains("This branch was not changed."), "{result}");
        let after = git(&fixture.repo, &["rev-parse", "HEAD"]);
        assert_eq!(before, after, "a feature branch was rewritten");
    }

    #[test]
    fn a_feature_branch_fully_contained_in_the_default_branch_is_reported_as_shipped() {
        let fixture = fixture();
        git(&fixture.repo, &["switch", "-q", "-c", "Feature/1"]);
        std::fs::write(fixture.repo.join("feature.txt"), "feature\n").unwrap();
        git(&fixture.repo, &["add", "."]);
        git(&fixture.repo, &["commit", "-q", "-m", "feature"]);
        git(&fixture.repo, &["push", "-q", "-u", "origin", "Feature/1"]);
        git(&fixture.repo, &["switch", "-q", "main"]);
        git(&fixture.repo, &["merge", "-q", "--no-ff", "-m", "merge", "Feature/1"]);
        git(&fixture.repo, &["push", "-q", "origin", "main"]);
        git(&fixture.repo, &["switch", "-q", "Feature/1"]);
        let result = notice(&fixture.repo);
        assert!(result.contains("fully contained in origin/main"), "{result}");
    }

    #[test]
    fn a_checkout_missing_the_selection_present_on_the_default_branch_is_reported() {
        let fixture = fixture();
        let work = fixture.origin.with_extension("select");
        git(
            fixture.origin.parent().unwrap(),
            &["clone", "-q", fixture.origin.to_str().unwrap(), work.to_str().unwrap()],
        );
        std::fs::create_dir(work.join(".agents")).unwrap();
        std::fs::write(work.join(".agents").join("plugins.json"), r#"{"plugins":["agent@tj-agents"]}"#).unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "select"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        std::fs::write(fixture.repo.join("README.md"), "dirty\n").unwrap();
        let result = notice(&fixture.repo);
        assert!(result.contains("PLUGINS WILL NOT LOAD"), "{result}");
        assert!(!fixture.repo.join(".agents").join("plugins.json").exists(), "{result}");
    }

    #[test]
    fn an_unreachable_remote_reports_the_fetch_failure_and_still_returns() {
        let fixture = fixture();
        git(&fixture.repo, &["remote", "set-url", "origin", "file:///nonexistent/origin.git"]);
        let result = notice(&fixture.repo);
        assert!(result.contains("could not fetch origin"), "{result}");
    }

    #[test]
    fn a_checkout_already_current_produces_no_notice() {
        let fixture = fixture();
        let result = notice(&fixture.repo);
        assert_eq!(result, "", "{result}");
    }

    #[test]
    fn a_linked_worktree_checks_and_fetches_as_fast_as_a_plain_clone() {
        let fixture = fixture();
        git(&fixture.repo, &["worktree", "add", "-q", ".worktrees/Feature-1", "-b", "Feature/1"]);
        let worktree = fixture.repo.join(".worktrees").join("Feature-1");
        let started = Instant::now();
        let result = notice(&worktree);
        let elapsed = started.elapsed();
        assert_eq!(result, "", "{result}");
        assert!(elapsed < TOTAL_SECONDS, "took {elapsed:?}");
    }
}
