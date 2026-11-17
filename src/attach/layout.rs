//! Wire layout description (`Layout` message body): panes, z-order, focus. Pure data; the
//! host-side builder is `wm::host_view`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::msg::PaneId;

/// Titles are truncated to this many bytes before they hit the wire.
pub const MAX_LAYOUT_TITLE_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePaneInfo {
    pub id: PaneId,
    pub title: String,
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLayout {
    pub panes: Vec<WirePaneInfo>,
    /// Bottom-to-top.
    pub z_order: Vec<PaneId>,
    pub active: PaneId,
}

impl WireLayout {
    pub fn to_body(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    pub fn from_body(body: &Map<String, Value>) -> Result<Self, String> {
        serde_json::from_value(Value::Object(body.clone())).map_err(|e| e.to_string())
    }

    /// Restricts to `keep` (None = all panes).
    pub fn filtered(&self, keep: Option<&std::collections::BTreeSet<PaneId>>) -> Self {
        let Some(keep) = keep else { return self.clone() };
        Self {
            panes: self.panes.iter().filter(|p| keep.contains(&p.id)).cloned().collect(),
            z_order: self.z_order.iter().copied().filter(|id| keep.contains(id)).collect(),
            active: self.active,
        }
    }
}

/// Truncates on a char boundary.
pub fn clamp_title(t: &str) -> String {
    if t.len() <= MAX_LAYOUT_TITLE_BYTES {
        return t.to_string();
    }
    let mut end = MAX_LAYOUT_TITLE_BYTES;
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    t[..end].to_string()
}
