use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn home() -> Result<PathBuf> {
    dirs::home_dir().context("cannot determine the home directory")
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Claude Code's config root, honouring the same override Claude Code does.
pub fn claude_home() -> Result<PathBuf> {
    Ok(env_dir("CLAUDE_CONFIG_DIR").unwrap_or(home()?.join(".claude")))
}

pub fn codex_home() -> Result<PathBuf> {
    Ok(env_dir("CODEX_HOME").unwrap_or(home()?.join(".codex")))
}

/// Where indexes and rendered preview panes live. Never inside an agent's own config directory.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = env_dir("TJ_CACHE_DIR") {
        return Ok(dir);
    }
    Ok(dirs::cache_dir().context("cannot determine the cache directory")?.join("tj"))
}

/// A path as a comparable string: forward slashes, no trailing slash, and case-folded on Windows
/// where the filesystem is case-insensitive. Git, the shell and transcripts disagree on all three.
pub fn norm(path: &str) -> String {
    let s = path.replace('\\', "/");
    let s = s.trim_end_matches('/');
    let s = if s.is_empty() { "/" } else { s };
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// True when `path` is `base` or anything beneath it.
pub fn within(path: &str, base: &str) -> bool {
    let (p, b) = (norm(path), norm(base));
    p == b || p.starts_with(&format!("{}/", b.trim_end_matches('/')))
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_matches_children_but_not_siblings() {
        assert!(within("/a/repo", "/a/repo"));
        assert!(within("/a/repo/src", "/a/repo/"));
        assert!(!within("/a/repo-other", "/a/repo"));
        assert!(within(r"C:\x\repo\sub", "C:/x/repo"));
    }
}
