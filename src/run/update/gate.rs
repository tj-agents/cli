use std::time::Duration;

/// What the host says it is doing, as far as the wrapper can see from outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// The host exposes no turn state; its update handler must avoid composer input.
    Unknown,
    Busy,
    Idle {
        since: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Graces {
    pub plugins: Duration,
    pub input: Duration,
}

pub(crate) fn decide(activity: Activity, input_idle: Option<Duration>, graces: Graces) -> bool {
    match activity {
        Activity::Idle { since } => since >= graces.plugins && input_idle.is_none_or(|idle| idle >= graces.input),
        Activity::Unknown => true,
        Activity::Busy => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Activity, Duration, Graces, decide};

    #[test]
    fn claude_waits_for_the_turn_and_input_to_settle() {
        let grace = Duration::from_secs(2);
        let graces = Graces { plugins: grace, input: grace };
        let idle = Activity::Idle { since: grace };
        assert!(decide(idle, Some(grace), graces));
        assert!(decide(idle, None, graces));
        assert!(!decide(idle, Some(Duration::ZERO), graces));
        assert!(!decide(Activity::Idle { since: Duration::ZERO }, None, graces));
        assert!(!decide(Activity::Busy, None, graces));
        assert!(decide(Activity::Unknown, Some(Duration::ZERO), graces));
    }
}
