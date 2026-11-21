//! `WindowManageOp::Place`: the one Action a window drop lowers to (and `:window place`, RPC
//! `window.place` and MCP use directly). Tiled layout only for swap/split; `float` switches to Stacked.

use serde::{Deserialize, Serialize};

use super::split_tree::SplitDirection;
use super::{WindowManager, WorkspaceLayoutMode};
use crate::pane::PaneId;

/// Where a window lands relative to a target window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaceHow {
    /// Exchange positions with the target.
    Swap,
    /// Split the target and take its left half.
    Left,
    /// Split the target and take its right half.
    Right,
    /// Split the target and take its top half.
    Up,
    /// Split the target and take its bottom half.
    Down,
    /// Leave the tiles: Stacked layout with this window raised on top (no target).
    Float,
}

impl PlaceHow {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "swap" => Some(Self::Swap),
            "left" | "l" => Some(Self::Left),
            "right" | "r" => Some(Self::Right),
            "up" | "top" | "above" => Some(Self::Up),
            "down" | "bottom" | "below" => Some(Self::Down),
            "float" => Some(Self::Float),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::Float => "float",
        }
    }
}

impl WindowManager {
    /// Places `window_id` (default: active) relative to `target` per `how`. Errors leave the layout unchanged.
    pub(crate) fn place_window(&mut self, window_id: Option<PaneId>, target: Option<PaneId>, how: PlaceHow) -> Result<String, String> {
        let src = window_id.unwrap_or_else(|| self.active_pane_id());
        if !self.panes.contains_key(&src) {
            return Err(format!("No window #{src}"));
        }
        if how == PlaceHow::Float {
            self.set_layout_mode(WorkspaceLayoutMode::Stacked);
            self.focus_and_raise(src);
            return Ok(format!("Window #{src} floated (Stacked layout)"));
        }
        if self.layout_mode != WorkspaceLayoutMode::Tiled {
            return Err("Swap/split placement needs the Tiled layout (:layout tiled)".to_string());
        }
        let Some(target) = target else { return Err(format!("Usage: :window place [id] {} <target>", how.name())) };
        if target == src {
            return Err("Cannot place a window onto itself".to_string());
        }
        let ok = match how {
            PlaceHow::Swap => self.split_tree.swap_leaves(src, target),
            PlaceHow::Left => self.split_tree.move_leaf_beside(src, target, SplitDirection::Vertical, true),
            PlaceHow::Right => self.split_tree.move_leaf_beside(src, target, SplitDirection::Vertical, false),
            PlaceHow::Up => self.split_tree.move_leaf_beside(src, target, SplitDirection::Horizontal, true),
            PlaceHow::Down => self.split_tree.move_leaf_beside(src, target, SplitDirection::Horizontal, false),
            PlaceHow::Float => false,
        };
        if !ok {
            return Err(format!("Cannot place #{src} {} #{target} (both must be tiled in this session)", how.name()));
        }
        self.reflow_default_viewport();
        self.maybe_autosave_layout();
        Ok(format!("Window #{src} placed {} #{target}", how.name()))
    }
}
