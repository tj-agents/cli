use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::host::Host;
use crate::paths;

/// A marketplace commit: forty hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Commit(String);

#[derive(Debug, thiserror::Error)]
#[error("not a forty-digit commit")]
pub(crate) struct CommitError;

impl Commit {
    /// The commit on the first line of `text`.
    pub(crate) fn parse(text: &str) -> Result<Self, CommitError> {
        text.lines()
            .next()
            .map(str::trim)
            .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map(|sha| Self(sha.to_owned()))
            .ok_or(CommitError)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AnnounceError {
    #[error("could not create the published updates folder")]
    Folder(#[source] std::io::Error),
    #[error("could not stage the published update")]
    Stage(#[source] std::io::Error),
    #[error("could not publish the update")]
    Publish(#[source] std::io::Error),
}

/// The record `tj update` announces a host's plugin updates in; every running tab of that host watches it.
#[derive(Debug, Clone)]
pub(crate) struct Publication {
    host: Host,
    path: PathBuf,
}

impl Publication {
    pub(crate) fn new(cache: &Path, host: Host) -> Self {
        Self { host, path: cache.join("published").join(host.name()) }
    }

    pub(crate) fn of(host: Host) -> Result<Self> {
        Ok(Self::new(&paths::cache_dir()?, host))
    }

    /// The whole record. Each announcement is distinct, even of the same revision.
    pub(crate) fn announcement(&self) -> Option<String> {
        fs::read_to_string(&self.path).ok()
    }

    /// The revision last announced: the record's first line.
    pub(crate) fn revision(&self) -> Option<String> {
        self.announcement()?.lines().next().map(|line| line.trim().to_owned())
    }

    /// The marketplace commit this host's plugins last converged to, when the revision is one.
    pub(crate) fn commit(&self) -> Option<Commit> {
        Commit::parse(&self.announcement()?).ok()
    }

    pub(crate) fn announce(&self, revision: &str) -> Result<(), AnnounceError> {
        let folder = self.path.parent().unwrap_or(&self.path);
        fs::create_dir_all(folder).map_err(AnnounceError::Folder)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let process = std::process::id();
        let staged = folder.join(format!("{}.{process}", self.host.name()));
        fs::write(&staged, format!("{revision}\n{stamp}.{process}")).map_err(AnnounceError::Stage)?;
        fs::rename(&staged, &self.path).map_err(AnnounceError::Publish)
    }
}

#[cfg(test)]
mod tests {
    use super::{Commit, Publication};
    use crate::host::Host;

    const SHA: &str = "e555013e5e7bab455193e79be12a15e588bdd221";

    #[test]
    fn a_commit_is_the_first_line_only_when_it_is_a_sha() {
        assert_eq!(Commit::parse(&format!("{SHA}\n123.4")).unwrap().as_str(), SHA);
        assert!(Commit::parse(r#"{"agent":"abc"}"#).is_err());
        assert!(Commit::parse("").is_err());
    }

    #[test]
    fn each_announcement_is_distinct_and_carries_its_revision() {
        let cache = tempfile::tempdir().unwrap();
        let publication = Publication::new(cache.path(), Host::Claude);
        assert_eq!(publication.revision(), None);
        publication.announce(SHA).unwrap();
        let first = publication.announcement().unwrap();
        publication.announce(SHA).unwrap();
        assert_ne!(publication.announcement().unwrap(), first);
        assert_eq!(publication.revision().as_deref(), Some(SHA));
        assert_eq!(publication.commit().unwrap().as_str(), SHA);
    }
}
