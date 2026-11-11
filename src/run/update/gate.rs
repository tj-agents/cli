use std::time::Duration;

/// What the host says it is doing, as far as the wrapper can see from outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// The host exposes no turn state; it takes updates through its own server, so they never wait.
    Unknown,
    Busy,
    Idle {
        since: Duration,
        background: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub binary: bool,
    pub plugins: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Graces {
    pub plugins: Duration,
    pub restart: Duration,
    pub input: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Apply {
    /// Load the updated plugins into the running agent.
    Plugins,
    /// Restart the agent under the updated tj binary, resuming its conversation.
    Binary,
}

pub(crate) fn decide(
    pending: Pending,
    activity: Activity,
    input_idle: Option<Duration>,
    graces: Graces,
) -> Option<Apply> {
    let input_quiet = |grace: Duration| input_idle.is_none_or(|idle| idle >= grace);
    match activity {
        Activity::Idle { since, background: 0 }
            if pending.binary && since >= graces.restart && input_quiet(graces.restart) =>
        {
            Some(Apply::Binary)
        }
        Activity::Idle { since, .. } if pending.plugins && since >= graces.plugins && input_quiet(graces.input) => {
            Some(Apply::Plugins)
        }
        Activity::Unknown if pending.plugins => Some(Apply::Plugins),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Activity, Apply, Graces, Pending, decide};

    const GRACES: Graces =
        Graces { plugins: Duration::from_secs(2), restart: Duration::from_secs(30), input: Duration::from_secs(2) };
    const QUIET: Option<Duration> = Some(Duration::from_secs(600));

    fn pending(binary: bool, plugins: bool) -> Pending {
        Pending { binary, plugins }
    }

    fn idle(seconds: u64, background: u32) -> Activity {
        Activity::Idle { since: Duration::from_secs(seconds), background }
    }

    #[test]
    fn a_published_plugin_update_applies_two_seconds_after_the_turn_ends() {
        assert_eq!(decide(pending(false, true), idle(2, 3), QUIET, GRACES), Some(Apply::Plugins));
        assert_eq!(decide(pending(false, true), idle(1, 0), QUIET, GRACES), None);
    }

    #[test]
    fn a_plugin_update_never_types_over_the_keyboard_or_into_a_turn() {
        assert_eq!(decide(pending(false, true), idle(60, 0), Some(Duration::from_secs(1)), GRACES), None);
        assert_eq!(decide(pending(false, true), Activity::Busy, QUIET, GRACES), None);
        assert_eq!(decide(pending(false, true), idle(60, 0), None, GRACES), Some(Apply::Plugins));
    }

    #[test]
    fn a_host_without_turn_state_takes_updates_through_its_server_at_once() {
        assert_eq!(decide(pending(false, true), Activity::Unknown, Some(Duration::ZERO), GRACES), Some(Apply::Plugins));
        assert_eq!(decide(pending(true, false), Activity::Unknown, QUIET, GRACES), None);
    }

    #[test]
    fn a_new_binary_restarts_only_when_truly_idle() {
        assert_eq!(
            decide(pending(true, true), idle(30, 0), Some(Duration::from_secs(30)), GRACES),
            Some(Apply::Binary)
        );
        assert_eq!(decide(pending(true, false), idle(300, 1), QUIET, GRACES), None);
        assert_eq!(decide(pending(true, false), idle(29, 0), QUIET, GRACES), None);
        assert_eq!(decide(pending(true, false), idle(300, 0), Some(Duration::from_secs(29)), GRACES), None);
        assert_eq!(decide(pending(true, false), Activity::Busy, QUIET, GRACES), None);
    }
}
