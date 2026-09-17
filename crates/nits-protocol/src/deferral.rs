//! Validated explanation and external reference for an unfixed finding.
use core::{fmt, str::FromStr};
use serde::{Deserialize, Serialize};

/// A nonempty explanation. Leading/trailing whitespace is removed at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct DeferralReason(String);

impl TryFrom<String> for DeferralReason {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.is_empty()
            || value
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(
                "deferral reason must contain text and no unsupported control characters".into(),
            );
        }
        Ok(Self(value.to_owned()))
    }
}

/// An absolute HTTP(S) URL without credentials, whitespace or control characters.
/// Kept as supplied so external issue links remain recognisable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct TrackingUrl(String);

impl TryFrom<String> for TrackingUrl {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parsed = url::Url::parse(&value).map_err(|error| {
            format!("Invalid tracking URL. Enter a complete http:// or https:// URL ({error}).")
        })?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
            || value.contains('\\')
            || !(value.starts_with("https://") || value.starts_with("http://"))
        {
            return Err(
                "tracking URL must be an absolute HTTP(S) URL without credentials or whitespace"
                    .into(),
            );
        }
        Ok(Self(value))
    }
}

/// Implements string parsing, display and owned conversion for validated string types.
macro_rules! string_access {
    ($name:ident) => {
        impl FromStr for $name {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.to_owned().try_into()
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}
string_access!(DeferralReason);
string_access!(TrackingUrl);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_reject_empty_reasons_and_unsafe_links() {
        for reason in ["", " \n\t", "bad\0reason"] {
            assert!(serde_json::from_value::<DeferralReason>(serde_json::json!(reason)).is_err());
        }
        assert_eq!(
            " scoped out "
                .parse::<DeferralReason>()
                .unwrap()
                .to_string(),
            "scoped out"
        );
        for link in [
            "javascript:alert(1)",
            "data:text/plain,x",
            "/issue/288",
            "https://",
            "https://u:p@example.com/288",
            " https://example.com",
            "https://example.com/\n288",
            "https:example.com",
            "https://example.com\\evil",
        ] {
            assert!(link.parse::<TrackingUrl>().is_err(), "{link}");
        }
        for link in [
            "https://example.com/issues/288",
            "http://localhost:3000/issues/288?q=x#note",
        ] {
            let parsed: TrackingUrl = link.parse().unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), link);
        }
    }
}
