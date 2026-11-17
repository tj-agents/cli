use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Launch, hidden, lifecycle, store};
use crate::completion::{State, Step};
use crate::{feed, paths};

mod journal;

fn repository(launch: &Launch) -> Result<PathBuf> {
    if launch.cwd.is_dir() {
        return Ok(launch.cwd.clone());
    }
    let owner = store::ownership(launch)?;
    for claim in owner.claims {
        let path = claim.path;
        if path.is_dir() {
            let common = git(&path, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            return Ok(Path::new(&common).parent().context("git common dir has no parent")?.to_path_buf());
        }
    }
    if let Some(saved) = journal::read(launch)? {
        if saved.session != owner.session {
            bail!("cleanup journal belongs to another session; recovery required");
        }
        return Ok(saved.repository);
    }
    bail!("launch directory is gone and no owned repository is available")
}

pub fn notify(launch: &Launch) -> Result<()> {
    let Some(item) = &launch.item else { return Ok(()) };
    let output = hidden("tj-item")
        .args(["prs", item])
        .current_dir(repository(launch)?)
        .stdin(Stdio::null())
        .output()
        .context("could not run tj item prs")?;
    if !output.status.success() {
        bail!("tj item prs {item}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let reply: Value = serde_json::from_slice(&output.stdout).context("tj item prs printed no JSON")?;
    let item = reply["ref"].as_str().unwrap_or(item);
    let Some(event) = merged_event(item, &reply) else { return Ok(()) };
    let session = store::bound_session(&launch.id)?
        .with_context(|| format!("no {} session is bound to launch {} yet", launch.host.name(), launch.id))?;
    feed::Feed::new(&feed::folder()?, &format!("{}:{session}", launch.host.name()))?.post_once(
        &event,
        &json!({
            "at": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            "from": null,
            "name": "TJ",
            "kind": "merged",
            "text": format!("Every PR linked to {item} has merged. Completion still requires the session's final result after all remaining work and waits finish.")
        }),
    )?;
    Ok(())
}

/// Identity is the item and PR set, not launch, turn, title or query ordering.
fn merged_event(item: &str, reply: &Value) -> Option<Value> {
    if reply["all_merged"] != Value::Bool(true) {
        return None;
    }
    let mut urls = reply["prs"].as_array()?.iter().map(|pr| pr["url"].as_str()).collect::<Option<Vec<_>>>()?;
    if urls.is_empty() || urls.iter().any(|url| url.trim().is_empty()) {
        return None;
    }
    urls.sort_unstable();
    urls.dedup();
    Some(json!({"kind": "merged", "item": item, "prs": urls}))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Removal {
    primary: PathBuf,
    worktree: PathBuf,
    registration: PathBuf,
    branch: String,
    head: String,
    default: String,
}

pub struct Plan {
    owner: store::Ownership,
    removals: Vec<Removal>,
    repository: PathBuf,
    journals: Vec<String>,
}

fn identity(path: &Path) -> Result<String> {
    Ok(paths::norm(
        &path.canonicalize().with_context(|| format!("cannot resolve {}", path.display()))?.to_string_lossy(),
    ))
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let output =
        hidden("git").args(arguments).current_dir(cwd).stdin(Stdio::null()).output().context("could not run git")?;
    if !output.status.success() {
        bail!("git {}: {}", arguments.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn linked(cwd: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let toplevel = PathBuf::from(git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"])?);
    let common = PathBuf::from(git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    if identity(&common)? == identity(&toplevel.join(".git"))? {
        return Ok(None);
    }
    Ok(Some((toplevel, common.parent().context("git common dir has no parent")?.to_path_buf())))
}

fn other_writers(owner: &store::Ownership) -> Result<()> {
    for session in store::sessions()? {
        if session["session"].as_str() == Some(owner.session.as_str()) {
            continue;
        }
        for location in ["cwd", "worktree"] {
            let Some(path) = session[location].as_str().map(Path::new).filter(|path| path.exists()) else { continue };
            let location = identity(path)?;
            if !owner
                .claims
                .iter()
                .any(|claim| identity(&claim.path).is_ok_and(|owned| paths::within(&location, &owned)))
            {
                continue;
            }
            let wrapper = &session["wrapper"];
            let pid = wrapper["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok());
            let recorded =
                wrapper["started_at"].as_str().and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
            let proven_dead = match (pid, recorded) {
                (Some(pid), Some(recorded)) => tj_cli::process::agent::started_at(pid)
                    .context("cannot inspect another session wrapper")?
                    .is_none_or(|started| {
                        let started: chrono::DateTime<chrono::Utc> = started.into();
                        (started - recorded.to_utc()).num_seconds().abs() > 2
                    }),
                _ => false,
            };
            if !proven_dead {
                bail!("another session may still be using {}: {}", path.display(), session["session"]);
            }
            native_stopped(
                session["launch_id"].as_str().context("another session has no managed launch to verify")?,
                session["session"].as_str().context("another session has no native identity to verify")?,
            )?;
        }
    }
    Ok(())
}

fn integrated(cwd: &Path, root: &Path) -> Result<String> {
    let remote = git(cwd, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])?;
    let default = remote.strip_prefix("origin/").context("default remote is not origin")?;
    git(cwd, &["fetch", "--quiet", "origin", &format!("+refs/heads/{default}:refs/remotes/origin/{default}")])?;
    git(cwd, &["merge-base", "--is-ancestor", &format!("refs/heads/{default}"), &remote])
        .context("local default branch contains unpublished commits; cannot prove integration")?;
    if git(cwd, &["rev-parse", "--abbrev-ref", &format!("{default}@{{upstream}}")])? != remote {
        bail!("default branch does not track {remote}");
    }
    let listing = git(
        cwd,
        &[
            "-c",
            &format!("worktrunk.default-branch={default}"),
            "wt",
            "--config-set",
            "list.full=false",
            "--config-set",
            "list.json-schema=2",
            "list",
            "--format=json",
        ],
    )?;
    let listing: Value = serde_json::from_str(&listing).context("git-wt list printed no JSON")?;
    if listing["schema"] != 2 || listing["repo"]["default_branch"].as_str() != Some(default) {
        bail!("git-wt list returned an unexpected integration schema or default branch");
    }
    let target = identity(root)?;
    let rows = listing["items"].as_array().context("git-wt list returned no items")?;
    let row = rows
        .iter()
        .find(|row| {
            row["worktree"]["path"]
                .as_str()
                .is_some_and(|path| identity(Path::new(path)).is_ok_and(|path| path == target))
        })
        .context("owned worktree is not registered with Git")?;
    if row["display"]["state"].as_str() != Some("empty") && !row["default_branch"]["integration"].is_object() {
        bail!("worktree has unmerged commits: {}", root.display());
    }
    Ok(default.to_owned())
}

fn preflight(claim: &store::Claim) -> Result<Removal> {
    let path = &claim.path;
    let (root, primary) = linked(path)?.context("ownership points to a primary checkout; refusing removal")?;
    if identity(path)? != identity(&root)? {
        bail!("ownership is not an exact linked root: {}", path.display());
    }
    if !git(&root, &["status", "--porcelain"])?.is_empty() {
        bail!("worktree has uncommitted changes: {}", root.display());
    }
    let branch = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let head = git(&root, &["rev-parse", "HEAD"])?;
    let default = integrated(&root, &root)?;
    let hook = hidden("git-wt")
        .args(["hook", "pre-remove"])
        .current_dir(&root)
        .stdin(Stdio::null())
        .output()
        .context("could not run git-wt pre-remove")?;
    if !hook.status.success() {
        bail!("pre-remove refused: {}", String::from_utf8_lossy(&hook.stderr).trim());
    }
    Ok(Removal { primary, worktree: path.to_path_buf(), registration: root, branch, head, default })
}

fn completion_owner(launch: &Launch) -> Result<(store::Ownership, Vec<journal::Journal>)> {
    let mut owner = store::ownership(launch)?;
    store::require_managed_claims(&owner.session)?;
    let predecessors = store::predecessors(launch, &owner)?;
    let mut saved = journal::read(launch)?.into_iter().collect::<Vec<_>>();
    for predecessor in &predecessors {
        let claims = store::claims(&owner.session, &predecessor.launch.id)?;
        let previous = journal::read(&predecessor.launch)?;
        if claims.is_empty() && previous.is_none() {
            continue;
        }
        if wrapper_alive(predecessor)? {
            bail!("historical launch wrapper is still alive: {}", predecessor.launch.id);
        }
        owner.claims.extend(claims);
        saved.extend(previous);
    }
    for journal in &saved {
        if journal.session != owner.session {
            bail!("cleanup journal belongs to another session; recovery required");
        }
    }
    let mut roots = std::collections::HashSet::new();
    for claim in &owner.claims {
        let root =
            if claim.path.try_exists()? { identity(&claim.path)? } else { paths::norm(&claim.path.to_string_lossy()) };
        if !roots.insert(root) {
            bail!("duplicate worktree ownership; recovery required: {}", claim.path.display());
        }
    }
    owner.claims.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((owner, saved))
}

fn removal_claim<'a>(owner: &'a store::Ownership, removal: &Removal) -> Option<&'a store::Claim> {
    owner
        .claims
        .iter()
        .find(|claim| paths::norm(&claim.path.to_string_lossy()) == paths::norm(&removal.worktree.to_string_lossy()))
}

pub fn owned_preflight(launch: &Launch) -> Result<Plan, String> {
    let prepare = || -> Result<Plan> {
        let (owner, saved) = completion_owner(launch)?;
        let mut journals = Vec::new();
        let mut proof: Vec<Removal> = Vec::new();
        for saved in saved {
            if saved.launch_id != launch.id {
                journals.push(saved.launch_id);
            }
            for removal in saved.removals {
                if let Some(previous) = proof.iter().find(|previous| {
                    paths::norm(&previous.worktree.to_string_lossy())
                        == paths::norm(&removal.worktree.to_string_lossy())
                }) {
                    if previous != &removal {
                        bail!("conflicting cleanup journal proofs; recovery required");
                    }
                } else {
                    proof.push(removal);
                }
            }
        }
        journals.sort();
        journals.dedup();
        for path in [Some(&launch.cwd), owner.location.as_ref()].into_iter().flatten().filter(|path| path.exists()) {
            if let Some((root, _)) = linked(path)? {
                let root = identity(&root)?;
                if !owner.claims.iter().any(|claim| identity(&claim.path).is_ok_and(|owned| owned == root)) {
                    bail!("linked worktree has no recorded ownership; recovery required: {}", path.display());
                }
            }
        }
        other_writers(&owner)?;
        let mut removals = owner
            .claims
            .iter()
            .map(|claim| {
                let path = &claim.path;
                if path.try_exists()? {
                    return preflight(claim);
                }
                let removal = proof
                    .iter()
                    .find(|removal| {
                        paths::norm(&removal.worktree.to_string_lossy()) == paths::norm(&path.to_string_lossy())
                    })
                    .context("missing owned root has no cleanup journal; recovery required")?;
                removed(removal)?;
                Ok(removal.clone())
            })
            .collect::<Result<Vec<_>>>()?;
        for removal in proof {
            if removal_claim(&owner, &removal).is_none() {
                removed(&removal)?;
                removals.push(removal);
            }
        }
        removals.sort_by(|left, right| left.worktree.cmp(&right.worktree));
        let repository = removals.first().map_or_else(|| repository(launch), |removal| Ok(removal.primary.clone()))?;
        Ok(Plan { owner, removals, repository, journals })
    };
    prepare().map_err(|error| format!("{error:#}"))
}

fn remove(removal: &Removal) -> Result<()> {
    let worktree = removal.worktree.to_string_lossy();
    let mut last = String::new();
    for _ in 0..2 {
        let output = hidden("git")
            .args([
                "-c",
                &format!("worktrunk.default-branch={}", removal.default),
                "wt",
                "remove",
                "--foreground",
                &worktree,
            ])
            .current_dir(&removal.primary)
            .stdin(Stdio::null())
            .output()
            .context("could not run git-wt remove")?;
        if output.status.success() {
            break;
        }
        last = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        std::thread::sleep(Duration::from_secs(1));
    }
    if removal.worktree.exists() {
        std::fs::remove_dir(&removal.worktree)
            .with_context(|| format!("{} is still there: {last}", removal.worktree.display()))?;
    }
    removed(removal)
}

fn removed(removal: &Removal) -> Result<()> {
    if removal.worktree.try_exists()? {
        bail!("worktree is still present: {}", removal.worktree.display());
    }
    let listed = git(&removal.primary, &["worktree", "list", "--porcelain", "-z"])?;
    if listed
        .split('\0')
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| paths::norm(path) == paths::norm(&removal.registration.to_string_lossy()))
    {
        bail!("worktree is still registered: {}", removal.worktree.display());
    }
    if git(&removal.primary, &["branch", "--format=%(refname)"])?
        .lines()
        .any(|branch| branch == format!("refs/heads/{}", removal.branch))
    {
        bail!("worktree branch was retained: {}", removal.branch);
    }
    Ok(())
}

fn native_dead(launch: &str, session: &str) -> Result<bool> {
    let output = hidden("tj-session")
        .args(["liveness", "--session", session, "--launch-id", launch])
        .stdin(Stdio::null())
        .output()
        .context("could not verify native session liveness")?;
    if !output.status.success() {
        bail!("native session liveness is unverified: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let proof: Value = serde_json::from_slice(&output.stdout).context("native liveness returned no JSON")?;
    if proof["session"].as_str() != Some(session) || proof["launch_id"].as_str() != Some(launch) {
        bail!("native session liveness returned a different identity");
    }
    match proof["state"].as_str() {
        Some("dead") => Ok(true),
        Some("live") => Ok(false),
        _ => bail!("native session liveness returned an unknown state"),
    }
}

fn native_stopped(launch: &str, session: &str) -> Result<()> {
    if !native_dead(launch, session)? {
        bail!("native session may still be live; cleanup retained: {session}");
    }
    Ok(())
}

pub fn await_native_exit(launch: &Launch, closing: &lifecycle::Closing, timeout: Duration) -> Result<()> {
    closing.attempt(Step::Cleanup, || {
        let deadline = Instant::now() + timeout;
        let mut announced = false;
        loop {
            let record = closing.check()?;
            if native_dead(&launch.id, record.identity().session_handle())? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("native session is still loaded after host exit; cleanup retained for tj closeout");
            }
            if !announced {
                eprintln!("TJ: waiting for the native conversation to unload before cleanup.");
                announced = true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    })
}

fn cleanup(launch: &Launch, plan: &Plan, closing: &lifecycle::Closing) -> Result<()> {
    closing.check()?;
    native_stopped(&launch.id, &plan.owner.session)?;
    let rechecked = owned_preflight(launch).map_err(anyhow::Error::msg)?;
    if rechecked.owner != plan.owner || rechecked.removals != plan.removals || rechecked.journals != plan.journals {
        bail!("worktree ownership or content changed while stopping the agent; recovery required");
    }
    journal::write(launch, plan)?;
    // The aggregate is durable before any predecessor proof is retired.
    for source in &plan.journals {
        journal::clear(source)?;
    }
    for removal in &plan.removals {
        closing.check()?;
        let Some(claim) = removal_claim(&plan.owner, removal) else {
            removed(removal)?;
            continue;
        };
        if removal.worktree.try_exists()? {
            remove(removal)?;
        } else {
            removed(removal)?;
        }
        store::release(&plan.owner, claim)?;
    }
    if !completion_owner(launch)?.0.claims.is_empty() {
        bail!("owned worktrees remain after cleanup; recovery required");
    }
    closing.record(Step::Cleanup, &format!("owned-worktrees-released:{}", launch.id))?;
    Ok(())
}

pub fn finish(launch: &Launch, plan: Plan, closing: &lifecycle::Closing) -> Result<()> {
    closing.attempt(Step::Cleanup, || cleanup(launch, &plan, closing))?;
    if !closing.recorded(Step::Tracker)? {
        closing.attempt(Step::Tracker, || {
            if let Some(item) = &launch.item {
                let output = hidden("tj-item")
                    .args(["progress", item, "--state", "Done"])
                    .current_dir(&plan.repository)
                    .stdin(Stdio::null())
                    .output()
                    .context("could not run tj item progress")?;
                if !output.status.success() {
                    bail!("tj item progress {item}: {}", String::from_utf8_lossy(&output.stderr).trim());
                }
            }
            closing.record(Step::Tracker, &format!("tracker-done:{}", launch.item.as_deref().unwrap_or("no-work-item")))
        })?;
    }
    closing.check()?;
    store::end(&launch.id, "closed")?;
    closing.complete()?;
    journal::clear(&launch.id)
}

fn wrapper_alive(row: &store::Row) -> Result<bool> {
    let recorded =
        chrono::DateTime::parse_from_rfc3339(&row.wrapper_started_at).context("invalid wrapper creation time")?;
    let Some(started) = tj_cli::process::agent::started_at(row.wrapper_pid).context("cannot inspect launch wrapper")?
    else {
        return Ok(false);
    };
    let started: chrono::DateTime<chrono::Utc> = started.into();
    Ok((started - recorded.to_utc()).num_seconds().abs() <= 2)
}

#[derive(clap::Args)]
pub struct Args {
    /// Retry verified completion for this item's dead sessions, or sweep interrupted closeout receipts.
    item: Option<String>,
}

pub fn run(args: Args) -> Result<ExitCode> {
    let mut failed = false;
    for row in store::launches(args.item.as_deref())? {
        match wrapper_alive(&row) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                failed = true;
                println!("kept       {}: {error:#}", row.launch.title);
                continue;
            }
        }
        let launch = &row.launch;
        let started = !matches!(journal::read(launch), Ok(None));
        let selected = args.item.is_some() || row.closing || started;
        let result = (|| -> Result<bool> {
            let store = match lifecycle::for_recovery(&launch.id) {
                Ok(Some(store)) => store,
                Ok(None) if args.item.is_none() => return Ok(false),
                Ok(None) => bail!("native session belongs to a newer launch; resume that session to finish this work"),
                Err(_) if !selected => return Ok(false),
                Err(error) => return Err(error),
            };
            let record = match store.load()? {
                Some(record) => record,
                None if !selected => return Ok(false),
                None => bail!("no final result receipt; resume this session to finish its work"),
            };
            if matches!(record.state(), State::Completed { .. }) {
                if !started {
                    return Ok(false);
                }
                let plan = owned_preflight(launch).map_err(anyhow::Error::msg)?;
                native_stopped(&launch.id, &plan.owner.session)?;
                if !plan.owner.claims.is_empty() {
                    bail!("completed session acquired new worktree claims; recovery required");
                }
                if !row.closed {
                    store::end(&launch.id, "closed")?;
                }
                journal::clear(&launch.id)?;
                return Ok(true);
            }
            if !selected && record.state().outcome().is_none() {
                return Ok(false);
            }
            let record = lifecycle::deliver(&store, record)?;
            let closing = lifecycle::Closing::begin(store, &record)?;
            let plan = closing.attempt(Step::Cleanup, || owned_preflight(launch).map_err(anyhow::Error::msg))?;
            finish(launch, plan, &closing)?;
            Ok(true)
        })();
        match result {
            Ok(true) => println!("closed     {} ({})", launch.title, launch.cwd.display()),
            Ok(false) => {}
            Err(error) => {
                failed = true;
                println!("kept       {} ({}): {error:#}", launch.title, launch.cwd.display());
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

#[cfg(test)]
mod tests {
    use super::{merged_event, wrapper_alive};
    use crate::host::Host;
    use crate::run::{Launch, store};

    #[test]
    fn merge_identity_ignores_order_and_duplicates_but_preserves_new_prs() {
        let event = |prs| merged_event("item", &serde_json::json!({"all_merged": true, "prs": prs}));
        let first = event(serde_json::json!([{"url": "b"}, {"url": "a"}, {"url": "b"}]));
        assert_eq!(first, event(serde_json::json!([{"url": "a"}, {"url": "b"}])));
        assert_ne!(first, event(serde_json::json!([{"url": "a"}, {"url": "b"}, {"url": "c"}])));
        for prs in [serde_json::json!([]), serde_json::json!([{}]), serde_json::json!([{"url": ""}])] {
            assert_eq!(event(prs), None);
        }
        assert_eq!(merged_event("item", &serde_json::json!({"all_merged": false, "prs": [{"url": "a"}]})), None);
    }

    #[test]
    fn wrapper_checks_require_a_matching_process_identity() {
        let pid = std::process::id();
        let started: chrono::DateTime<chrono::Utc> = tj_cli::process::agent::started_at(pid).unwrap().unwrap().into();
        let mut row = store::Row {
            launch: Launch {
                id: "probe".into(),
                host: Host::Claude,
                title: "probe".into(),
                cwd: std::env::temp_dir(),
                item: None,
            },
            closing: false,
            ended: false,
            closed: false,
            wrapper_pid: pid,
            wrapper_started_at: started.to_rfc3339(),
        };
        assert!(wrapper_alive(&row).unwrap());
        row.wrapper_started_at = "2020-01-01T00:00:00Z".into();
        assert!(!wrapper_alive(&row).unwrap());
        row.wrapper_started_at = "invalid".into();
        assert!(wrapper_alive(&row).is_err());
    }
}
