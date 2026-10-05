use std::path::{MAIN_SEPARATOR_STR, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use walkdir::WalkDir;

use crate::{fzf, paths};

/// Folder names never worth navigating into.
const EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "bin",
    "obj",
    "build",
    "cmake-build-debug",
    "cmake-build-release",
    "out",
    "dist",
    ".vs",
    ".vscode",
    ".idea",
    "__pycache__",
    ".cache",
    ".next",
    ".venv",
    "venv",
    "packages",
    "Debug",
    "Release",
    ".pytest_cache",
    ".mypy_cache",
    "worktrees",
    ".worktrees",
    "CMakeFiles",
    "_deps",
];

#[derive(clap::Args)]
pub struct Args {
    /// Jump straight to the best match; with nothing typed, open the picker
    query: Vec<String>,
    /// Folder to search [default: $TJ_F_ROOT, else ~/projects, else ~/source/repos]
    #[arg(long)]
    root: Option<PathBuf>,
}

fn default_root() -> Result<PathBuf> {
    if let Some(r) = std::env::var_os("TJ_F_ROOT").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    let home = paths::home()?;
    for candidate in ["projects", "source/repos"] {
        let p = home.join(candidate);
        if p.is_dir() {
            return Ok(p);
        }
    }
    bail!("no project root found - set TJ_F_ROOT or pass --root")
}

/// Every folder under `root` as (relative path, absolute path), sorted by relative path.
fn candidates(root: &PathBuf) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && EXCLUDES.iter().any(|x| e.file_name().eq_ignore_ascii_case(x))))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_dir())
        .filter_map(|e| {
            let rel = e.path().strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((rel, e.into_path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\\' | '/' | ' ' | '_' | '-' | '.')).flat_map(char::to_lowercase).collect()
}

/// Higher is better, -1 is no match. Separators and case are ignored, so "cppnote" finds "cpp/note".
pub fn score(query: &str, rel: &str) -> i64 {
    let q = squash(query);
    if q.is_empty() {
        return -1;
    }
    let segs: Vec<&str> = rel.split(['/', '\\']).collect();
    let depth = segs.len() as i64;
    let full = squash(rel);
    let leaf = squash(segs.last().copied().unwrap_or(""));

    if full == q {
        return 1000 - depth;
    }
    if leaf == q {
        return 950 - depth;
    }
    if full.ends_with(&q) {
        return 850 - depth;
    }
    if leaf.starts_with(&q) {
        return 800 - depth;
    }
    if let Some(i) = full.find(&q) {
        return 700 - depth - i as i64;
    }
    // Last resort: the query's characters appear in order.
    let mut want = q.chars().peekable();
    for c in full.chars() {
        if want.peek() == Some(&c) {
            want.next();
        }
    }
    if want.peek().is_none() { 300 - depth - full.chars().count() as i64 } else { -1 }
}

pub fn run(args: Args) -> Result<ExitCode> {
    let root = match args.root {
        Some(r) => r,
        None => default_root()?,
    };
    if !root.is_dir() {
        bail!("root path not found: {}", root.display());
    }
    let root = dunce_canonical(&root);
    let mut choices = vec![(".".to_string(), root.clone())];
    choices.extend(candidates(&root));
    let query = args.query.join(" ").trim().to_string();

    let picked = if query.is_empty() {
        picker(&choices, None)?
    } else {
        let best = choices
            .iter()
            .map(|(rel, abs)| (score(&query, rel), rel, abs))
            .filter(|(s, _, _)| *s >= 0)
            .max_by_key(|(s, _, _)| *s);
        match best {
            Some((_, rel, abs)) => {
                eprintln!("\x1b[90m-> {rel}\x1b[0m");
                Some(abs.clone())
            }
            None => {
                eprintln!("\x1b[33mno match for '{query}' - opening picker\x1b[0m");
                picker(&choices, Some(&query))?
            }
        }
    };
    match picked {
        Some(path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        None => Ok(ExitCode::from(1)),
    }
}

fn picker(choices: &[(String, PathBuf)], query: Option<&str>) -> Result<Option<PathBuf>> {
    fzf::ensure()?;
    let mut args = vec!["--prompt", "project> ", "--height", "60%", "--border", "--reverse", "--scheme=path"];
    if let Some(q) = query {
        args.extend(["--query", q]);
    }
    let rows = choices.iter().map(|(rel, _)| rel.replace(['/', '\\'], MAIN_SEPARATOR_STR)).collect();
    let Some(sel) = fzf::pick(&args, rows)? else { return Ok(None) };
    let sel = sel.replace(['/', '\\'], "/");
    Ok(choices.iter().find(|(rel, _)| rel.replace('\\', "/") == sel).map(|(_, abs)| abs.clone()))
}

/// Canonical path without Windows' `\\?\` verbatim prefix, which `cd` in cmd and older tools reject.
fn dunce_canonical(p: &PathBuf) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_leaf_beats_substring() {
        assert!(score("core", "tj-agents/core") > score("core", "tj-agents/core-old"));
    }

    #[test]
    fn separators_and_case_are_ignored() {
        assert_eq!(score("TjAgentsCore", "tj-agents/core"), 1000 - 2);
        assert!(score("cppnote", r"cpp\note") > 0);
    }

    #[test]
    fn subsequence_is_last_resort_and_nonmatch_is_negative() {
        assert!(score("tjc", "tj-agents/cli") > 0);
        assert!(score("tjc", "tj-agents/cli") < score("cli", "tj-agents/cli"));
        assert_eq!(score("zzz", "tj-agents/cli"), -1);
    }

    #[test]
    fn walk_skips_excluded_folders() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["a/src", "a/node_modules/x", "b/.git/objects", "b/target/debug"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let rels: Vec<String> =
            candidates(&dir.path().to_path_buf()).into_iter().map(|(r, _)| r.replace('\\', "/")).collect();
        assert_eq!(rels, ["a", "a/src", "b"]);
    }
}
