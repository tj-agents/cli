use std::time::Duration;

/// What the host says it is doing, as far as the wrapper can see from outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// The host exposes no turn state; it reloads through its own server, so a reload never waits.
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
    pub revision: bool,
    pub now: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Graces {
    pub reload: Duration,
    pub restart: Duration,
    pub input: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Apply {
    Reload,
    Restart,
}

pub(crate) fn decide(
    pending: Pending,
    activity: Activity,
    input_idle: Option<Duration>,
    graces: Graces,
) -> Option<Apply> {
    if pending.now {
        return Some(if pending.binary { Apply::Restart } else { Apply::Reload });
    }
    let input_quiet = |grace: Duration| input_idle.is_none_or(|idle| idle >= grace);
    match activity {
        Activity::Idle { since, background: 0 }
            if pending.binary && since >= graces.restart && input_quiet(graces.restart) =>
        {
            Some(Apply::Restart)
        }
        Activity::Idle { since, .. } if pending.revision && since >= graces.reload && input_quiet(graces.input) => {
            Some(Apply::Reload)
        }
        Activity::Unknown if pending.revision => Some(Apply::Reload),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Activity, Apply, Graces, Pending, decide};

    const GRACES: Graces =
        Graces { reload: Duration::from_secs(2), restart: Duration::from_secs(30), input: Duration::from_secs(2) };
    const QUIET: Option<Duration> = Some(Duration::from_secs(600));

    fn pending(binary: bool, revision: bool) -> Pending {
        Pending { binary, revision, now: false }
    }

    fn idle(seconds: u64, background: u32) -> Activity {
        Activity::Idle { since: Duration::from_secs(seconds), background }
    }

    #[test]
    fn a_published_update_reloads_two_seconds_after_the_turn_ends() {
        assert_eq!(decide(pending(false, true), idle(2, 3), QUIET, GRACES), Some(Apply::Reload));
        assert_eq!(decide(pending(false, true), idle(1, 0), QUIET, GRACES), None);
    }

    #[test]
    fn a_reload_never_types_over_the_keyboard_or_into_a_turn() {
        assert_eq!(decide(pending(false, true), idle(60, 0), Some(Duration::from_secs(1)), GRACES), None);
        assert_eq!(decide(pending(false, true), Activity::Busy, QUIET, GRACES), None);
        assert_eq!(decide(pending(false, true), idle(60, 0), None, GRACES), Some(Apply::Reload));
    }

    #[test]
    fn a_host_without_turn_state_reloads_through_its_server_at_once() {
        assert_eq!(decide(pending(false, true), Activity::Unknown, Some(Duration::ZERO), GRACES), Some(Apply::Reload));
        assert_eq!(decide(pending(true, false), Activity::Unknown, QUIET, GRACES), None);
    }

    #[test]
    fn a_new_binary_restarts_only_when_truly_idle() {
        assert_eq!(
            decide(pending(true, true), idle(30, 0), Some(Duration::from_secs(30)), GRACES),
            Some(Apply::Restart)
        );
        assert_eq!(decide(pending(true, false), idle(300, 1), QUIET, GRACES), None);
        assert_eq!(decide(pending(true, false), idle(29, 0), QUIET, GRACES), None);
        assert_eq!(decide(pending(true, false), idle(300, 0), Some(Duration::from_secs(29)), GRACES), None);
        assert_eq!(decide(pending(true, false), Activity::Busy, QUIET, GRACES), None);
    }

    #[test]
    fn refresh_applies_now() {
        let now = |binary| Pending { binary, revision: false, now: true };
        assert_eq!(decide(now(false), Activity::Busy, QUIET, GRACES), Some(Apply::Reload));
        assert_eq!(decide(now(true), Activity::Busy, QUIET, GRACES), Some(Apply::Restart));
    }
}
