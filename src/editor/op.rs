//! `Action::Editor` vocabulary shared by `:edit`/`:scratch`/`:editor`, `editor.*` RPC, the
//! `bilterm_editor` MCP tool and the key chord (`UNI-R01`).

use super::menu::{MenuCmd, menu_by_name};

pub const EDITOR_USAGE: &str = "Usage: :edit [path] | :scratch [name] | :editor save [--force] | saveas <path> [--force] | wrap [on|off] | find <text> | findprev <text> | undo | redo | goto <line> | revert | insert <text> | text | status | close [--force] | menu [open [file|edit|search|view|help] | close | do <item>]";

/// Menu bar verbs (`:editor menu ...`, `editor.menu`): the keyboard menu lowers to these too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuOp {
    /// Open a menu by title (`None` = the first one, like `F10`).
    Open(Option<String>),
    Close,
    /// Run a menu item by its wire name (see `MenuCmd::NAMES`).
    Do(MenuCmd),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorOp {
    /// Open `path` (or a new unnamed buffer) in an editor window.
    Open { path: Option<String> },
    /// Open or focus the scratchpad `name` (default `scratch`).
    Scratch { name: Option<String> },
    Save { force: bool },
    SaveAs { path: String, force: bool },
    Wrap(Option<bool>),
    Find { query: String, backward: bool },
    Undo,
    Redo,
    Goto(usize),
    Revert,
    Insert(String),
    /// Report the buffer text (command data).
    Text,
    /// Report path, dirty flag, cursor and options (command data).
    Status,
    Close { force: bool },
    /// Menu bar: open, close or run an item.
    Menu(MenuOp),
}

impl EditorOp {
    /// Canonical colon form.
    pub fn colon_form(&self) -> String {
        let f = |force: &bool| if *force { " --force" } else { "" };
        match self {
            Self::Open { path: Some(p) } => format!(":edit {p}"),
            Self::Open { path: None } => ":edit".into(),
            Self::Scratch { name: Some(n) } => format!(":scratch {n}"),
            Self::Scratch { name: None } => ":scratch".into(),
            Self::Save { force } => format!(":editor save{}", f(force)),
            Self::SaveAs { path, force } => format!(":editor saveas {path}{}", f(force)),
            Self::Wrap(None) => ":editor wrap".into(),
            Self::Wrap(Some(b)) => format!(":editor wrap {}", if *b { "on" } else { "off" }),
            Self::Find { query, backward } => format!(":editor {} {query}", if *backward { "findprev" } else { "find" }),
            Self::Undo => ":editor undo".into(),
            Self::Redo => ":editor redo".into(),
            Self::Goto(n) => format!(":editor goto {n}"),
            Self::Revert => ":editor revert".into(),
            Self::Insert(t) => format!(":editor insert {t}"),
            Self::Text => ":editor text".into(),
            Self::Status => ":editor status".into(),
            Self::Close { force } => format!(":editor close{}", f(force)),
            Self::Menu(MenuOp::Open(None)) => ":editor menu open".into(),
            Self::Menu(MenuOp::Open(Some(n))) => format!(":editor menu open {n}"),
            Self::Menu(MenuOp::Close) => ":editor menu close".into(),
            Self::Menu(MenuOp::Do(c)) => format!(":editor menu do {}", c.name()),
        }
    }

    /// Parses `:edit ...`, `:scratch ...` and `:editor <verb> ...`; `None` when `head` is not ours.
    pub fn parse(head: &str, parts: &[&str]) -> Option<Result<Self, String>> {
        let rest = |from: usize| parts.get(from..).unwrap_or(&[]).to_vec();
        let joined = |from: usize| rest(from).join(" ");
        let opt = |s: String| (!s.is_empty()).then_some(s);
        let bad = || Err(EDITOR_USAGE.to_string());
        Some(match head {
            "edit" => Ok(Self::Open { path: opt(joined(1)) }),
            "scratch" => match rest(1).as_slice() {
                [] => Ok(Self::Scratch { name: None }),
                [n] => Ok(Self::Scratch { name: Some((*n).to_string()) }),
                _ => bad(),
            },
            "editor" => {
                let verb = parts.get(1).copied().unwrap_or("");
                let force = rest(2).contains(&"--force");
                let args: Vec<&str> = rest(2).into_iter().filter(|a| *a != "--force").collect();
                match (verb, args.as_slice()) {
                    ("save", []) => Ok(Self::Save { force }),
                    ("saveas" | "save-as", a) if !a.is_empty() => Ok(Self::SaveAs { path: a.join(" "), force }),
                    ("wrap", []) => Ok(Self::Wrap(None)),
                    ("wrap", ["on"]) => Ok(Self::Wrap(Some(true))),
                    ("wrap", ["off"]) => Ok(Self::Wrap(Some(false))),
                    ("find", a) if !a.is_empty() => Ok(Self::Find { query: a.join(" "), backward: false }),
                    ("findprev", a) if !a.is_empty() => Ok(Self::Find { query: a.join(" "), backward: true }),
                    ("undo", []) => Ok(Self::Undo),
                    ("redo", []) => Ok(Self::Redo),
                    ("goto", [n]) => n.parse().map(Self::Goto).map_err(|_| EDITOR_USAGE.to_string()),
                    ("revert", []) => Ok(Self::Revert),
                    ("insert", _) if parts.len() > 2 => Ok(Self::Insert(joined(2))),
                    ("text", []) => Ok(Self::Text),
                    ("status", []) => Ok(Self::Status),
                    ("close", []) => Ok(Self::Close { force }),
                    ("menu", a) => Self::parse_menu(a),
                    _ => bad(),
                }
            }
            _ => return None,
        })
    }

    fn parse_menu(args: &[&str]) -> Result<Self, String> {
        match args {
            [] | ["open"] => Ok(Self::Menu(MenuOp::Open(None))),
            ["open", n] if menu_by_name(n).is_some() => Ok(Self::Menu(MenuOp::Open(Some((*n).to_ascii_lowercase())))),
            ["close"] => Ok(Self::Menu(MenuOp::Close)),
            ["do", n] => MenuCmd::from_name(n).map(|c| Self::Menu(MenuOp::Do(c))).ok_or_else(|| format!("unknown menu item '{n}' (see Help > Keys)")),
            _ => Err(EDITOR_USAGE.to_string()),
        }
    }

    /// `true` for verbs that read or write host files (gated for API callers).
    pub fn touches_files(&self) -> bool {
        matches!(self, Self::Open { path: Some(_) } | Self::Save { .. } | Self::SaveAs { .. } | Self::Revert | Self::Menu(MenuOp::Do(MenuCmd::Save | MenuCmd::SaveAs | MenuCmd::Revert)))
    }
}
