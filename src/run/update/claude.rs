use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tj_cli::process::agent;

use super::gate::Activity;
use super::{ApplyError, Update};
use crate::paths;

const COMMAND: &str = "/reload-plugins";
const SHELLS: &[&str] = &["bash", "sh", "zsh", "fish", "pwsh", "powershell"];

/// Types `/reload-plugins` into the console Claude shares with the wrapper.
pub(crate) struct Claude;

/// Claude's own per-process record, `<claude home>/sessions/<pid>.json`; its format belongs to Claude Code.
fn activity(record: &str, now: SystemTime, background: u32) -> Activity {
    let Ok(record) = serde_json::from_str::<Value>(record) else { return Activity::Busy };
    let since = record["statusUpdatedAt"]
        .as_u64()
        .and_then(|millis| now.duration_since(UNIX_EPOCH + Duration::from_millis(millis)).ok());
    match (record["status"].as_str(), since) {
        (Some("idle"), Some(since)) => Activity::Idle { since, background },
        _ => Activity::Busy,
    }
}

fn shells(pid: u32) -> u32 {
    let Ok(images) = agent::descendants(pid) else { return u32::MAX };
    let shell = |image: &String| {
        let stem = image.rsplit_once('.').map_or(image.as_str(), |(stem, _)| stem);
        SHELLS.iter().any(|name| stem.eq_ignore_ascii_case(name))
    };
    u32::try_from(images.iter().filter(|image| shell(image)).count()).unwrap_or(u32::MAX)
}

impl Update for Claude {
    fn activity(&self, agent: u32) -> Activity {
        let Ok(home) = paths::claude_home() else { return Activity::Busy };
        let Ok(record) = std::fs::read_to_string(home.join("sessions").join(format!("{agent}.json"))) else {
            return Activity::Busy;
        };
        activity(&record, SystemTime::now(), shells(agent))
    }

    fn apply(&self) -> Result<(), ApplyError> {
        agent::type_text(COMMAND).map_err(ApplyError::Typing)?;
        std::thread::sleep(Duration::from_millis(300));
        agent::press_enter().map_err(ApplyError::Typing)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::activity;
    use crate::run::update::gate::Activity;

    #[test]
    fn only_an_idle_status_counts_and_its_age_comes_from_claude() {
        let now = UNIX_EPOCH + Duration::from_millis(10_000);
        let record = |status: &str| format!(r#"{{"pid":1,"status":"{status}","statusUpdatedAt":7000}}"#);
        assert_eq!(activity(&record("idle"), now, 2), Activity::Idle { since: Duration::from_secs(3), background: 2 });
        assert_eq!(activity(&record("busy"), now, 0), Activity::Busy);
        assert_eq!(activity(&record("shell"), now, 0), Activity::Busy);
        assert_eq!(activity(r#"{"status":"idle"}"#, now, 0), Activity::Busy);
        assert_eq!(activity(r#"{"status":"idle","statusUpdat"#, now, 0), Activity::Busy);
    }
}
