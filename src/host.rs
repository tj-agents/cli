//! Agent's Python `Host` and this enum both assert against `packages/tj-cli/src/tj_cli/hosts.json`;
//! change all three together.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub(crate) enum Host {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Equals(&'static str),
    NonEmpty,
}

impl Marker {
    fn matches(self, value: &str) -> bool {
        match self {
            Self::Equals(expected) => value == expected,
            Self::NonEmpty => !value.is_empty(),
        }
    }

    #[cfg(test)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Equals(value) => value,
            Self::NonEmpty => "nonempty",
        }
    }
}

impl Host {
    const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|host| host.name() == name)
    }

    fn marker_variable(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDECODE",
            Self::Codex => "CODEX_THREAD_ID",
        }
    }

    fn marker(self) -> Marker {
        match self {
            Self::Claude => Marker::Equals("1"),
            Self::Codex => Marker::NonEmpty,
        }
    }

    fn session_id_variable(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CODE_SESSION_ID",
            Self::Codex => "CODEX_THREAD_ID",
        }
    }

    fn active(self, env: &impl Fn(&str) -> Option<String>) -> bool {
        env(self.marker_variable()).is_some_and(|value| self.marker().matches(&value))
    }

    pub(crate) fn session_id(self) -> Option<String> {
        std::env::var(self.session_id_variable()).ok().filter(|value| !value.is_empty())
    }

    pub(crate) fn handle(self) -> Option<String> {
        self.session_id().map(|id| format!("{}:{id}", self.name()))
    }

    pub(crate) fn from_env(env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        Self::ALL.into_iter().find(|host| host.active(&env))
    }

    pub(crate) fn current() -> Option<Self> {
        Self::from_env(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::Host;

    #[test]
    fn claude_marker_outranks_codex_when_both_are_set() {
        let env = |name: &str| match name {
            "CLAUDECODE" => Some("1".to_owned()),
            "CODEX_THREAD_ID" => Some("t".to_owned()),
            _ => None,
        };
        assert_eq!(Host::from_env(env), Some(Host::Claude));
    }

    #[test]
    fn the_codex_marker_alone_selects_codex() {
        let env = |name: &str| (name == "CODEX_THREAD_ID").then(|| "t".to_owned());
        assert_eq!(Host::from_env(env), Some(Host::Codex));
    }

    #[test]
    fn neither_marker_set_is_no_host() {
        assert_eq!(Host::from_env(|_| None), None);
    }

    #[test]
    fn empty_marker_values_are_ignored() {
        let env = |name: &str| match name {
            "CLAUDECODE" | "CODEX_THREAD_ID" => Some(String::new()),
            _ => None,
        };
        assert_eq!(Host::from_env(env), None);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Contract {
        name: String,
        marker_variable: String,
        marker_value: String,
        session_id_variable: String,
    }

    impl Contract {
        fn from_value(value: &Value) -> Option<Self> {
            let text = |field: &str| value[field].as_str().map(str::to_owned);
            Some(Self {
                name: text("name")?,
                marker_variable: text("marker_variable")?,
                marker_value: text("marker_value")?,
                session_id_variable: text("session_id_variable")?,
            })
        }
    }

    #[test]
    fn matches_the_python_hosts_contract_in_precedence_order() {
        const JSON: &str = include_str!("../packages/tj-cli/src/tj_cli/hosts.json");
        let rows = serde_json::from_str::<Value>(JSON)
            .expect("hosts.json must parse")
            .as_array()
            .expect("hosts.json must be an array")
            .iter()
            .map(|row| Contract::from_value(row).expect("each host entry needs all four contract fields"))
            .collect::<Vec<_>>();
        let expected = Host::ALL
            .iter()
            .map(|host| Contract {
                name: host.name().to_owned(),
                marker_variable: host.marker_variable().to_owned(),
                marker_value: host.marker().as_str().to_owned(),
                session_id_variable: host.session_id_variable().to_owned(),
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, expected);
    }
}
