//! `[browser]` section: system-browser hand-off policy.

use serde::{Deserialize, Serialize};

/// `[browser]` settings. `offer_external` controls the non-blocking "Open in system browser?" offer
/// shown when a URL is opened in the in-pane browser canvas (default `true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    pub offer_external: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self { offer_external: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_and_parses_toml() {
        assert!(BrowserConfig::default().offer_external);
        let c: crate::config::BiltermConfig = toml::from_str("[browser]\noffer_external = false\n").unwrap();
        assert!(!c.browser.offer_external);
        let c: crate::config::BiltermConfig = toml::from_str("").unwrap();
        assert!(c.browser.offer_external);
    }
}
