//! `session_on_empty`: what happens when the last window of a session exits or is closed.

use serde::{Deserialize, Serialize};

/// Policy for a session whose last window exits or is closed (`:session on-empty end|keep`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionOnEmpty {
    /// End the session and move to another one; bilterm quits only when none is left (default).
    #[default]
    End,
    /// Keep the session alive with zero windows (an empty canvas) until it is refilled or ended.
    Keep,
}

impl SessionOnEmpty {
    /// Parses a case-insensitive name from `:session on-empty` or `config.toml`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "end" | "close" | "kill" | "quit" => Some(Self::End),
            "keep" | "empty" | "leave" | "stay" => Some(Self::Keep),
            _ => None,
        }
    }

    /// Canonical lowercase name (`end` / `keep`).
    pub fn label(self) -> &'static str {
        match self {
            Self::End => "end",
            Self::Keep => "keep",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_defaults_to_end() {
        assert_eq!(SessionOnEmpty::default(), SessionOnEmpty::End);
        assert_eq!(SessionOnEmpty::from_name("KEEP"), Some(SessionOnEmpty::Keep));
        assert_eq!(SessionOnEmpty::from_name("end"), Some(SessionOnEmpty::End));
        assert_eq!(SessionOnEmpty::from_name("nope"), None);
    }
}
