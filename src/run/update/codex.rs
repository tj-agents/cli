use super::gate::Activity;
use super::{ApplyError, Update};

/// Managed Codex owns an embedded engine that the shared daemon cannot reload.
pub(crate) struct Codex;

impl Update for Codex {
    fn activity(&self, _agent: u32) -> Activity {
        Activity::Unknown
    }

    fn apply(&self) -> Result<(), ApplyError> {
        Err(ApplyError::NativeReloadUnavailable)
    }
}
