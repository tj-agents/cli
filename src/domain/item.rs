use thiserror::Error;
use url::Url;

/// An absolute URL or qualified tracker reference. Agent Store owns provider-specific identity rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Reference(Value);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Value {
    Url(Url),
    Qualified(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ParseError {
    #[error("work item reference must be non-empty text without whitespace or control characters")]
    Text,
    #[error("work item reference must be an absolute URL or provider:scope:id")]
    Incomplete,
    #[error("invalid work item URL")]
    Url(#[source] url::ParseError),
}

impl Reference {
    pub(crate) fn parse(value: &str) -> Result<Self, ParseError> {
        if value.is_empty() || value.chars().any(|character| character.is_whitespace() || character.is_control()) {
            return Err(ParseError::Text);
        }
        let (provider, rest) = value.split_once(':').ok_or(ParseError::Incomplete)?;
        if provider.is_empty() || rest.is_empty() {
            return Err(ParseError::Incomplete);
        }
        if let Some((scope, id)) = rest.rsplit_once(':').filter(|_| !rest.starts_with("//")) {
            if scope.is_empty() || id.is_empty() {
                return Err(ParseError::Incomplete);
            }
            if provider == "url" && rest.split_once(':').is_some_and(|(_, path)| path.starts_with("//")) {
                Url::parse(rest).map_err(ParseError::Url)?;
            }
            return Ok(Self(Value::Qualified(value.to_owned())));
        }
        Url::parse(value).map(|url| Self(Value::Url(url))).map_err(ParseError::Url)
    }

    pub(crate) fn as_str(&self) -> &str {
        match &self.0 {
            Value::Url(url) => url.as_str(),
            Value::Qualified(value) => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reference;

    #[test]
    fn urls_use_the_url_library_and_tracker_references_keep_their_identity() {
        assert_eq!(
            Reference::parse("HTTPS://TRACKER.EXAMPLE/items/42").unwrap().as_str(),
            "https://tracker.example/items/42"
        );
        for value in [
            "github:github.com/owner/repository:42",
            "azure-devops:dev.azure.com/contoso:461",
            "jira:https://tracker.example:APP-12",
            "url:https://tracker.example:8443/items/42",
            "url:board.example:42",
            "mailto:owner@example.com",
        ] {
            assert_eq!(Reference::parse(value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn malformed_addresses_and_unresolved_ids_are_rejected() {
        for value in [
            "",
            "461",
            "#461",
            ":42",
            "https:",
            "github::42",
            "github:scope:",
            "https://[invalid",
            "url:https://[invalid",
            "https://tracker.example/a b",
            "github:scope:a\0b",
        ] {
            assert!(Reference::parse(value).is_err(), "accepted {value:?}");
        }
    }
}
