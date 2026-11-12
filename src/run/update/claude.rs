use std::time::{Duration, SystemTime};

use tj_cli::process::agent;

use super::gate::Activity;
use super::{ApplyError, Update};
use crate::run::turn::{self, Turn};

const COMMAND: &str = "/reload-plugins";

/// Types `/reload-plugins` into the console Claude shares with the wrapper.
pub(crate) struct Claude;

fn activity(turn: Option<Turn>, now: SystemTime, background: u32) -> Activity {
    match turn {
        Some(Turn { idle: true, changed }) => {
            now.duration_since(changed).map_or(Activity::Busy, |since| Activity::Idle { since, background })
        }
        _ => Activity::Busy,
    }
}

impl Update for Claude {
    fn activity(&self, agent: u32) -> Activity {
        activity(turn::claude(agent), SystemTime::now(), turn::shells(agent))
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
    use crate::run::turn::Turn;
    use crate::run::update::gate::Activity;

    #[test]
    fn only_an_idle_turn_counts_and_its_age_comes_from_claude() {
        let now = UNIX_EPOCH + Duration::from_millis(10_000);
        let turn = |idle: bool, millis: u64| Some(Turn { idle, changed: UNIX_EPOCH + Duration::from_millis(millis) });
        assert_eq!(activity(turn(true, 7000), now, 2), Activity::Idle { since: Duration::from_secs(3), background: 2 });
        assert_eq!(activity(turn(false, 7000), now, 0), Activity::Busy);
        assert_eq!(activity(turn(true, 11_000), now, 0), Activity::Busy);
        assert_eq!(activity(None, now, 0), Activity::Busy);
    }
}
