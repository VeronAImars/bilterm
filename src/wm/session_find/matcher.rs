//! Gathers OS facts per pane and matches them against a [`FindQuery`].
//!
//! Everything shown is sanitised in `probe`/here; secret-looking command lines are neither shown
//! nor matched (no oracle), password-locked panes are never text-searched, and screen text is
//! only ever counted, never returned.

use super::probe::{self, ProcInfo};
use super::query::FindQuery;
use crate::pane::{CompositePane, PaneId};
use crate::pane::redact::classify;
use crate::vt::sanitize_display;
use crate::wm::WindowManager;
use crate::wm::coordinates::CoordDisplayStyle;

/// Panes examined per search.
pub const PANE_CAP: usize = 256;
/// Rows of screen plus scrollback scanned per pane for `--text`.
pub const TEXT_ROW_CAP: usize = 20_000;

/// OS facts about one pane.
#[derive(Clone, Debug, Default)]
pub struct PaneFacts {
    pub pane: PaneId,
    pub title: String,
    pub shell: Option<ProcInfo>,
    pub fg: Option<ProcInfo>,
    pub fg_pgid: Option<u32>,
    /// PTY slave path (`/dev/pts/N`).
    pub tty: Option<String>,
    /// Pane-tracked or OSC 7 reported cwd.
    pub pane_cwd: Option<String>,
    pub running_command: Option<String>,
}

/// One matching pane.
#[derive(Clone, Debug)]
pub struct Match {
    pub facts: PaneFacts,
    pub session: String,
    pub coord: Option<String>,
    /// Names of the filters that matched.
    pub matched: Vec<&'static str>,
    pub text_hits: usize,
}

/// Search outcome.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub matches: Vec<Match>,
    pub total: usize,
    pub truncated: bool,
    /// Capability notes (no process API, remote panes without facts, ...).
    pub degraded: Vec<String>,
}

fn gather(id: PaneId, pane: &CompositePane) -> PaneFacts {
    let shell = pane.child_pid().and_then(probe::probe);
    #[cfg(unix)]
    let fd = pane.pty_master_fd();
    #[cfg(unix)]
    let fg_pgid = fd.and_then(probe::foreground_pgid);
    #[cfg(not(unix))]
    let fg_pgid = None;
    let fg = fg_pgid.and_then(probe::probe);
    #[cfg(unix)]
    let pty = fd.and_then(probe::pty_path);
    #[cfg(not(unix))]
    let pty = None;
    let tty = pty.or_else(|| shell.as_ref().and_then(|s| s.tty.clone()));
    let pane_cwd = pane
        .vt()
        .reported_cwd()
        .or_else(|| pane.tracked_cwd())
        .map(|p| sanitize_display(&p.to_string_lossy(), probe::FIELD_CAP));
    let running = pane.prompt().running_command.as_deref().map(str::trim).filter(|c| !c.is_empty() && classify(c).is_none());
    PaneFacts {
        pane: id,
        title: sanitize_display(pane.title(), probe::FIELD_CAP),
        shell,
        fg,
        fg_pgid,
        tty,
        pane_cwd,
        running_command: running.map(|c| sanitize_display(c, probe::CMDLINE_CAP)),
    }
}

fn procs(f: &PaneFacts) -> impl Iterator<Item = &ProcInfo> {
    f.shell.iter().chain(f.fg.iter())
}

fn ci_contains(hay: &str, needle_lc: &str) -> bool {
    hay.to_lowercase().contains(needle_lc)
}

/// `/dev/pts/3` matches `/dev/pts/3`, `pts/3` and `3`.
pub fn pty_matches(tty: &str, want: &str) -> bool {
    let want = want.trim();
    tty == want || tty.ends_with(&format!("/{want}"))
}

/// Filters other than `text` (which needs the mutable pane). `None` when a filter fails.
fn match_attrs(f: &PaneFacts, session: &str, q: &FindQuery) -> Option<Vec<&'static str>> {
    let mut hit = Vec::new();
    if let Some(p) = q.pane {
        (f.pane == p).then_some(())?;
        hit.push("pane");
    }
    if let Some(p) = q.pid {
        procs(f).any(|i| i.pid == p).then_some(())?;
        hit.push("pid");
    }
    if let Some(g) = q.pgid {
        (procs(f).any(|i| i.pgid == Some(g)) || f.fg_pgid == Some(g)).then_some(())?;
        hit.push("pgid");
    }
    if let Some(s) = q.sid {
        procs(f).any(|i| i.sid == Some(s)).then_some(())?;
        hit.push("sid");
    }
    if let Some(want) = &q.pty {
        f.tty.as_deref().is_some_and(|t| pty_matches(t, want)).then_some(())?;
        hit.push("pty");
    }
    if let Some(want) = &q.cwd {
        let ok = procs(f).filter_map(|i| i.cwd.as_deref()).chain(f.pane_cwd.as_deref()).any(|c| c.contains(want.as_str()));
        ok.then_some(())?;
        hit.push("cwd");
    }
    if let Some(want) = &q.name {
        let w = want.to_lowercase();
        procs(f).any(|i| ci_contains(&i.name, &w)).then_some(())?;
        hit.push("name");
    }
    if let Some(want) = &q.cmd {
        let w = want.to_lowercase();
        let ok = procs(f).any(|i| {
            ci_contains(&i.name, &w) || (classify(&i.cmdline).is_none() && ci_contains(&i.cmdline, &w))
        }) || f.running_command.as_deref().is_some_and(|c| ci_contains(c, &w));
        ok.then_some(())?;
        hit.push("cmd");
    }
    if let Some(want) = &q.user {
        let ok = procs(f).any(|i| i.user.as_deref() == Some(want.as_str()) || i.uid.is_some_and(|u| u.to_string() == *want));
        ok.then_some(())?;
        hit.push("user");
    }
    if let Some(spec) = &q.env {
        procs(f).any(|i| probe::has_env(i.pid, spec)).then_some(())?;
        hit.push("env");
    }
    if let Some(want) = &q.title {
        ci_contains(&f.title, &want.to_lowercase()).then_some(())?;
        hit.push("title");
    }
    if let Some(want) = &q.session {
        session.eq_ignore_ascii_case(want).then_some(())?;
        hit.push("session");
    }
    Some(hit)
}

/// Case-insensitive occurrences of `needle_lc` in the pane's screen and scrollback (bounded).
pub fn count_text(pane: &mut CompositePane, needle_lc: &str) -> usize {
    if needle_lc.is_empty() || pane.is_injection_password_locked() {
        return 0;
    }
    let total = pane.vt_mut().total_available_rows().min(TEXT_ROW_CAP);
    let mut hits = 0;
    for row in 0..total {
        if let Some(text) = pane.vt_mut().abs_row_text(row) {
            hits += text.to_lowercase().matches(needle_lc).count();
        }
    }
    hits
}

impl WindowManager {
    /// Runs `q` over the live panes. Read-only apart from scrolling bookkeeping inside the VT.
    pub fn session_find_run(&mut self, q: &FindQuery) -> Outcome {
        let session = sanitize_display(&self.config.history_session, 64);
        let mut out = Outcome::default();
        if probe::platform() == "none" {
            out.degraded.push("no process API on this platform: pid/pgid/sid/pty/user/env filters never match; title, cwd (tracked) and text still work".to_string());
        }
        let ids: Vec<PaneId> = self.panes.keys().copied().take(PANE_CAP).collect();
        let needle = q.text.as_ref().map(|t| t.to_lowercase());
        let mut remote = 0;
        for id in ids {
            let Some(pane) = self.panes.get(&id) else { continue };
            if pane.child_pid().is_none() {
                remote += 1;
            }
            let facts = gather(id, pane);
            let Some(mut matched) = match_attrs(&facts, &session, q) else { continue };
            let mut text_hits = 0;
            if let Some(n) = &needle {
                let Some(pane) = self.panes.get_mut(&id) else { continue };
                text_hits = count_text(pane, n);
                if text_hits == 0 {
                    continue;
                }
                matched.push("text");
            }
            out.total += 1;
            if out.matches.len() >= q.limit() {
                out.truncated = true;
                continue;
            }
            let coord = self.coord_of(id).and_then(|c| c.format(CoordDisplayStyle::Compact));
            out.matches.push(Match { facts, session: session.clone(), coord, matched, text_hits });
        }
        if remote > 0 {
            out.degraded.push(format!("{remote} pane(s) have no local PTY child (remote/none transport): OS process filters cannot match them"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> PaneFacts {
        PaneFacts {
            pane: 3,
            title: "Build Box".into(),
            shell: Some(ProcInfo { pid: 100, pgid: Some(100), sid: Some(100), name: "bash".into(), cwd: Some("/srv/app".into()), user: Some("bill".into()), uid: Some(1000), ..Default::default() }),
            fg: Some(ProcInfo { pid: 200, pgid: Some(200), name: "vim".into(), cmdline: "vim notes.txt".into(), ..Default::default() }),
            fg_pgid: Some(200),
            tty: Some("/dev/pts/7".into()),
            ..Default::default()
        }
    }

    fn q(f: impl FnOnce(&mut FindQuery)) -> FindQuery {
        let mut q = FindQuery::default();
        f(&mut q);
        q
    }

    #[test]
    fn attribute_filters_and_together() {
        let f = facts();
        let m = |q: &FindQuery| match_attrs(&f, "default", q);
        assert!(m(&FindQuery::default()).unwrap().is_empty());
        assert_eq!(m(&q(|q| q.pid = Some(200))).unwrap(), vec!["pid"]);
        assert!(m(&q(|q| q.pid = Some(1))).is_none());
        assert!(m(&q(|q| q.pgid = Some(200))).is_some());
        assert!(m(&q(|q| q.sid = Some(100))).is_some());
        assert!(m(&q(|q| q.pty = Some("pts/7".into()))).is_some());
        assert!(m(&q(|q| q.pty = Some("7".into()))).is_some());
        assert!(m(&q(|q| q.pty = Some("17".into()))).is_none());
        assert!(m(&q(|q| q.cwd = Some("/srv".into()))).is_some());
        assert!(m(&q(|q| q.cmd = Some("NOTES".into()))).is_some());
        assert!(m(&q(|q| q.name = Some("VIM".into()))).is_some());
        assert!(m(&q(|q| q.user = Some("1000".into()))).is_some());
        assert!(m(&q(|q| q.title = Some("build".into()))).is_some());
        assert!(m(&q(|q| q.session = Some("DEFAULT".into()))).is_some());
        assert!(m(&q(|q| { q.pid = Some(200); q.cwd = Some("/nope".into()); })).is_none());
    }

    #[test]
    fn secret_looking_command_lines_are_not_an_oracle() {
        let mut f = facts();
        f.fg.as_mut().unwrap().name = "tool".into();
        f.fg.as_mut().unwrap().cmdline = "tool --password=hunter2".into();
        let hit = match_attrs(&f, "d", &q(|q| q.cmd = Some("hunter2".into())));
        assert!(hit.is_none(), "classified command line must not match: {hit:?}");
    }

    #[test]
    fn pty_suffix_matching() {
        assert!(pty_matches("/dev/pts/3", "/dev/pts/3"));
        assert!(pty_matches("/dev/pts/3", "pts/3"));
        assert!(!pty_matches("/dev/pts/13", "3"));
    }
}
