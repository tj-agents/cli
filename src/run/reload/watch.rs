use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: SystemTime,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = path.metadata().ok()?;
        Some(Self { length: meta.len(), modified: meta.modified().ok()? })
    }
}

/// What this tab is running against: the wrapper's own executable and the last published plugin revision.
pub(crate) struct Updates {
    exe: PathBuf,
    running: Option<Stamp>,
    candidate: Option<(Stamp, Instant)>,
    published: PathBuf,
    applied: Option<String>,
}

impl Updates {
    pub(crate) fn new(exe: PathBuf, published: PathBuf) -> Self {
        let running = Stamp::of(&exe);
        let applied = std::fs::read_to_string(&published).ok();
        Self { exe, running, candidate: None, published, applied }
    }

    pub(crate) fn exe(&self) -> &Path {
        &self.exe
    }

    /// A replaced executable counts once it has stopped changing and runs.
    pub(crate) fn binary(&mut self, settle: Duration) -> bool {
        let Some(now) = Stamp::of(&self.exe) else { return false };
        if Some(now) == self.running {
            self.candidate = None;
            return false;
        }
        match self.candidate {
            Some((seen, since)) if seen == now => {
                since.elapsed() >= settle
                    && Command::new(&self.exe)
                        .arg("--version")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|status| status.success())
            }
            _ => {
                self.candidate = Some((now, Instant::now()));
                false
            }
        }
    }

    pub(crate) fn revision(&self) -> Option<String> {
        std::fs::read_to_string(&self.published).ok().filter(|revision| Some(revision) != self.applied.as_ref())
    }

    pub(crate) fn applied(&mut self, revision: Option<String>) {
        if revision.is_some() {
            self.applied = revision;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Updates;

    #[test]
    fn a_revision_is_pending_only_once_published_and_until_applied() {
        let directory = tempfile::tempdir().unwrap();
        let published = directory.path().join("published");
        let exe = directory.path().join("tj");
        std::fs::write(&exe, "old").unwrap();
        let mut updates = Updates::new(exe, published.clone());
        assert_eq!(updates.revision(), None);
        std::fs::write(&published, "b").unwrap();
        assert_eq!(updates.revision().as_deref(), Some("b"));
        updates.applied(updates.revision());
        assert_eq!(updates.revision(), None);
    }

    #[test]
    fn an_unchanged_or_unrunnable_binary_is_never_an_update() {
        let directory = tempfile::tempdir().unwrap();
        let exe = directory.path().join("tj");
        std::fs::write(&exe, "old").unwrap();
        let mut updates = Updates::new(exe.clone(), directory.path().join("published"));
        assert!(!updates.binary(Duration::ZERO));
        std::fs::write(&exe, "a newer build").unwrap();
        assert!(!updates.binary(Duration::ZERO));
        assert!(!updates.binary(Duration::ZERO), "a file that cannot run was taken for a new binary");
    }
}
