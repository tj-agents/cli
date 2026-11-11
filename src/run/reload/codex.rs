use serde_json::json;

use super::gate::Activity;
use super::{Reload, ReloadError};
use crate::app_server::Server;

/// Asks the shared app-server to reload user config, which hot-reloads plugins, skills, hooks and config.toml
/// into every loaded thread.
pub(crate) struct Codex;

impl Reload for Codex {
    fn activity(&self, _agent: u32) -> Activity {
        Activity::Unknown
    }

    fn reload(&self) -> Result<(), ReloadError> {
        Server::connect()
            .and_then(|mut server| server.request("config/batchWrite", json!({"edits": [], "reloadUserConfig": true})))
            .map(drop)
            .map_err(|error| ReloadError::AppServer(error.into()))
    }
}
