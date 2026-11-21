//! Pane/session origin: is this pane local, or does it come from somewhere else?
//!
//! Derived (never stored per pane) from state that already exists: `remote_target` (set by
//! `:ssh` and `:ssh-tmux`), the tmux link bridge, the fleet registry and the pane transport.
//! Anything that cannot be shown to be remote is [`OriginKind::Local`]. The badge is text first
//! (`[R:host]`), with a glyph only for the unicode/nerd packs, so it never relies on colour.

use serde::{Deserialize, Serialize};

use crate::api::JsonValue;
use crate::config::BiltermConfig;
use crate::pane::PaneId;
use crate::remote::FleetNodeKind;
use crate::ui::glyphs::GlyphPack;
use crate::wm::WindowManager;

/// Longest host kept in a badge.
const HOST_CAP: usize = 24;
/// Unicode glyph in front of the `[R:host]` tag (U+21C4, width 1); ascii/cp437 get the tag only.
pub const GLYPH: &str = "\u{21C4}";

/// Where a pane (or an attached session) really lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OriginKind {
    Local,
    /// `:ssh` pane (a local `ssh` process).
    Ssh,
    /// Pane mirrored from a remote `tmux -CC` link (`:ssh-tmux`).
    TmuxLink,
    /// This process is a `bilterm attach` client of a remote session.
    AttachClient,
    /// A federated `bilterm --headless` daemon (fleet node with an RPC endpoint).
    RemoteDaemon,
    /// Remote, but the mechanism is not identifiable.
    Other,
}

impl OriginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ssh => "ssh",
            Self::TmuxLink => "tmux-link",
            Self::AttachClient => "attach-client",
            Self::RemoteDaemon => "remote-daemon",
            Self::Other => "other",
        }
    }
}

/// Origin of one pane plus the host name where known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneOrigin {
    pub kind: OriginKind,
    pub host: Option<String>,
}

impl PaneOrigin {
    pub fn local() -> Self {
        Self { kind: OriginKind::Local, host: None }
    }

    pub fn new(kind: OriginKind, host: Option<String>) -> Self {
        Self { kind, host: host.map(|h| crate::vt::sanitize_display(&h, HOST_CAP)).filter(|h| !h.is_empty()) }
    }

    pub fn is_remote(&self) -> bool {
        self.kind != OriginKind::Local
    }

    /// `[R:host]` (`[R:?]` when the host is unknown); `None` for local.
    pub fn tag(&self) -> Option<String> {
        self.is_remote().then(|| format!("[R:{}]", self.host.as_deref().unwrap_or("?")))
    }

    /// Short title badge: glyph (unicode packs only) + tag, e.g. `⇄[R:web1]`.
    pub fn badge(&self, cfg: &BiltermConfig) -> Option<String> {
        let tag = self.tag()?;
        Some(if ascii_pack(cfg) { tag } else { format!("{GLYPH}{tag}") })
    }

    /// Status-line text: badge plus mechanism, e.g. `⇄[R:web1] ssh`.
    pub fn oia_text(&self, cfg: &BiltermConfig) -> Option<String> {
        Some(format!("{} {}", self.badge(cfg)?, self.kind.as_str()))
    }

    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj([
            ("remote", JsonValue::Bool(self.is_remote())),
            ("kind", JsonValue::String(self.kind.as_str().to_string())),
            ("host", self.host.clone().map_or(JsonValue::Null, JsonValue::String)),
            ("tag", self.tag().map_or(JsonValue::Null, JsonValue::String)),
        ])
    }
}

fn ascii_pack(cfg: &BiltermConfig) -> bool {
    matches!(cfg.glyphs, GlyphPack::Ascii | GlyphPack::Cp437)
}

/// `title` with the origin badge in front (front so a narrow frame truncates the title, never the badge).
pub fn with_badge(title: &str, badge: &str) -> String {
    if title.is_empty() { badge.to_string() } else { format!("{badge} {title}") }
}

/// Host part of an SSH destination or `dest/session` badge (`user@host:22/s` -> `host`).
fn host_of(target: &str) -> String {
    let dest = target.split('/').next().unwrap_or(target);
    dest.rsplit('@').next().unwrap_or(dest).to_string()
}

impl WindowManager {
    /// Origin of pane `id`; `Local` for unknown panes and for anything not provably remote.
    pub fn pane_origin(&self, id: PaneId) -> PaneOrigin {
        let Some(pane) = self.panes.get(&id) else { return PaneOrigin::local() };
        if let Some(t) = pane.remote_target() {
            let kind = if t.contains('/') || self.tmux_bridge.bilterm_to_tmux.contains_key(&id) { OriginKind::TmuxLink } else { OriginKind::Ssh };
            return PaneOrigin::new(kind, Some(host_of(t)));
        }
        for node in self.fleet.nodes.values() {
            match &node.kind {
                FleetNodeKind::SshPane { pane_id, destination } if *pane_id == id => return PaneOrigin::new(OriginKind::Ssh, Some(host_of(destination))),
                FleetNodeKind::SshTmux { pane_ids, destination, .. } if pane_ids.contains(&id) => {
                    return PaneOrigin::new(OriginKind::TmuxLink, Some(host_of(destination)));
                }
                _ => {}
            }
        }
        // A pane mapped to a tmux link has the link's `dest/session` badge; a plain local tmux does not.
        if self.tmux_bridge.bilterm_to_tmux.contains_key(&id)
            && let Some(b) = self.tmux_bridge.badge()
        {
            return PaneOrigin::new(OriginKind::TmuxLink, Some(host_of(b)));
        }
        if pane.remote_input_tap() {
            return PaneOrigin::new(OriginKind::Other, None);
        }
        PaneOrigin::local()
    }

    /// Fleet daemon peers (federated `bilterm --headless` nodes), as `(name, host)`.
    pub fn remote_daemon_peers(&self) -> Vec<(String, String)> {
        self.fleet
            .nodes
            .iter()
            .filter_map(|(name, n)| match &n.kind {
                FleetNodeKind::HostProfile { destination, rpc_endpoint: Some(_) } => Some((name.clone(), host_of(destination))),
                _ => None,
            })
            .collect()
    }

    /// Pushes each pane's origin text to the pane so the status line can show it. `true` on change.
    pub fn sync_origin_badges(&mut self) -> bool {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut changed = false;
        for id in ids {
            let text = self.pane_origin(id).oia_text(self.config());
            if let Some(p) = self.panes.get_mut(&id)
                && p.origin_badge() != text.as_deref()
            {
                p.set_origin_badge(text);
                changed = true;
            }
        }
        changed
    }

    /// Drawer badge text (`[R:host]`) for pane `id`, `None` when local.
    pub fn pane_origin_tag(&self, id: PaneId) -> Option<String> {
        self.pane_origin(id).tag()
    }

    /// `{panes:[{pane_id,remote,kind,host,tag}], remote_panes, daemons:[{name,host}]}`.
    pub fn remote_status_json(&self, only: Option<PaneId>) -> JsonValue {
        let ids: Vec<PaneId> = match only {
            Some(p) => vec![p],
            None => self.window_panes().map(|(i, _)| *i).collect(),
        };
        let mut remote = 0;
        let rows: Vec<JsonValue> = ids
            .iter()
            .map(|&id| {
                let o = self.pane_origin(id);
                remote += i64::from(o.is_remote());
                let mut j = std::collections::BTreeMap::new();
                j.insert("pane_id".to_string(), JsonValue::Number(id as i64));
                if let JsonValue::Object(kv) = o.to_json() {
                    j.extend(kv);
                }
                JsonValue::Object(j)
            })
            .collect();
        let daemons = self.remote_daemon_peers().into_iter().map(|(n, h)| JsonValue::obj([("name", JsonValue::String(n)), ("host", JsonValue::String(h)), ("kind", JsonValue::String("remote-daemon".into()))])).collect();
        JsonValue::obj([("panes", JsonValue::Array(rows)), ("remote_panes", JsonValue::Number(remote)), ("daemons", JsonValue::Array(daemons))])
    }

    /// Applies `:remote status [pane]`; the JSON result is left in `command_data`.
    pub fn remote_status_apply(&mut self, pane: Option<PaneId>) -> Result<String, String> {
        if let Some(p) = pane
            && !self.panes.contains_key(&p)
        {
            return Err(format!("Pane #{p} not found"));
        }
        let json = self.remote_status_json(pane);
        let n = json.get("remote_panes").and_then(JsonValue::as_i64).unwrap_or(0);
        let msg = match pane {
            Some(p) => match self.pane_origin(p).tag() {
                Some(t) => format!("Pane #{p} is remote {t} ({})", self.pane_origin(p).kind.as_str()),
                None => format!("Pane #{p} is local"),
            },
            None => format!("{n} remote pane(s) of {}", self.window_count()),
        };
        self.set_command_data(json.to_json_string());
        Ok(msg)
    }
}
