//! `[ui]` section: pane-mail envelope indicator toggles.

use serde::{Deserialize, Serialize};

/// `[ui]` settings. `pane_mail_indicator` shows the envelope on a pane's title when another
/// session, a tool (RPC/MCP/CLI) or agent mail messages it; `pane_mail_flash` blinks it while unread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub pane_mail_indicator: bool,
    pub pane_mail_flash: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { pane_mail_indicator: true, pane_mail_flash: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_and_parses_toml() {
        let d = UiConfig::default();
        assert!(d.pane_mail_indicator && d.pane_mail_flash);
        let c: crate::config::BiltermConfig = toml::from_str("[ui]\npane_mail_flash = false\n").unwrap();
        assert!(c.ui.pane_mail_indicator && !c.ui.pane_mail_flash);
        let c: crate::config::BiltermConfig = toml::from_str("").unwrap();
        assert!(c.ui.pane_mail_indicator && c.ui.pane_mail_flash);
    }
}
