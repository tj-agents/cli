use std::process::Command;

use anyhow::Result;

use crate::fzf;
use crate::paths::{norm, within};

pub enum Choice {
    All,
    Main,
    Worktree(String),
}

pub struct Worktree {
    pub path: String,
    pub branch: String,
}

/// Which sessions `cr` searches: the repo's main checkout path, whether we are in a repo at all, and
/// which worktree to search. It only asks when sitting in the main checkout with other worktrees
/// present - otherwise a repo with several in-flight branches would get mixed together by default.
pub struct Scope {
    pub base: String,
    pub is_repo: bool,
    pub choice: Choice,
    pub others: Vec<Worktree>,
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `<repo>/.worktrees/<name>` belongs to `<repo>`.
fn main_checkout(path: &str) -> String {
    let n = path.replace('\\', "/");
    match n.find("/.worktrees/") {
        Some(i) => path[..i].to_string(),
        None => path.trim_end_matches(['/', '\\']).to_string(),
    }
}

impl Scope {
    pub fn select(all: bool) -> Result<Self> {
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        let top = git(&["rev-parse", "--show-toplevel"]).filter(|t| !t.is_empty());
        let is_repo = top.is_some();
        let top = top.unwrap_or(cwd);
        let in_main = is_repo && !top.replace('\\', "/").contains("/.worktrees/");
        let base = main_checkout(&top);

        let mut scope = Scope { base, is_repo, choice: Choice::All, others: Vec::new() };
        if !in_main || all {
            return Ok(scope);
        }

        let porcelain = git(&["worktree", "list", "--porcelain"]).unwrap_or_default();
        for record in porcelain.split("\n\n") {
            let mut path = None;
            let mut branch = "(detached)".to_string();
            for line in record.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(p.to_string());
                } else if let Some(b) = line.strip_prefix("branch ") {
                    branch = b.trim_start_matches("refs/heads/").to_string();
                }
            }
            let Some(path) = path else { continue };
            if norm(&path) == norm(&scope.base) || norm(&path).contains("/.claude/worktrees/") {
                continue;
            }
            scope.others.push(Worktree { path, branch });
        }
        if scope.others.is_empty() {
            return Ok(scope);
        }

        fzf::ensure()?;
        let leaf =
            std::path::Path::new(&scope.base).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut items = vec!["All worktrees\tALL".to_string(), format!("{leaf} (main)\tMAIN")];
        items.extend(scope.others.iter().enumerate().map(|(i, w)| format!("{}\t{i}", w.branch)));
        let args = [
            "--prompt",
            "scope> ",
            "--height",
            "40%",
            "--border",
            "--reverse",
            "--delimiter",
            "\t",
            "--with-nth",
            "1",
            "--header",
            "which worktree to search? ENTER picks / ESC = all",
        ];
        if let Some(pick) = fzf::pick(&args, items)? {
            scope.choice = match pick.rsplit('\t').next().unwrap_or("ALL") {
                "MAIN" => Choice::Main,
                "ALL" => Choice::All,
                i => i
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| scope.others.get(i))
                    .map_or(Choice::All, |w| Choice::Worktree(w.path.clone())),
            };
        }
        Ok(scope)
    }

    /// Codex records each session's cwd; this says whether one belongs to the chosen scope.
    pub fn contains_cwd(&self, cwd: &str) -> bool {
        if cwd.is_empty() {
            return false;
        }
        let in_other = || self.others.iter().any(|o| within(cwd, &o.path));
        match &self.choice {
            Choice::All => within(cwd, &self.base) || in_other(),
            Choice::Main => within(cwd, &self.base) && !in_other(),
            Choice::Worktree(p) => within(cwd, p),
        }
    }

    pub fn leaf(&self) -> String {
        std::path::Path::new(&self.base)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.base.clone())
    }
}

/// Claude buckets sessions under a folder named for the cwd with every non-alphanumeric character
/// flattened to a dash, so the bucket name is computed the same way.
pub fn project_key(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_matches_claude_bucket_names() {
        assert_eq!(project_key("/home/tommy/projects/tj-agents/core"), "-home-tommy-projects-tj-agents-core");
        assert_eq!(project_key(r"C:\Users\T\source\repos\a.b"), "C--Users-T-source-repos-a-b");
    }

    #[test]
    fn worktrees_fold_into_their_main_checkout() {
        assert_eq!(main_checkout("/r/app/.worktrees/Fix-X"), "/r/app");
        assert_eq!(main_checkout("/r/app/"), "/r/app");
    }
}
