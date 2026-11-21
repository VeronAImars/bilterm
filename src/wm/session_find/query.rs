//! The session-find vocabulary every surface lowers to (`Action::SessionFind`, UNI-R01):
//! [`FindQuery`] (the OS-attribute filters), [`FindAct`] (what to do with the matches) and
//! [`SessionFindOp`]. Pure data plus the one token grammar shared by `:find`, `:find-<act>`
//! and the CLI (which forwards the same flags as RPC params).

/// Max characters of any text filter or of text to send is handled by the executor; filters are
/// capped here so a query can never grow without bound.
pub const FILTER_CAP: usize = 128;
/// Largest `--limit`.
pub const LIMIT_MAX: usize = 200;
/// Default `--limit`.
pub const LIMIT_DEFAULT: usize = 50;

/// Filters; all given filters must match (AND). Empty query matches every pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindQuery {
    /// Shell (PTY child) pid or foreground process pid.
    pub pid: Option<u32>,
    /// Process group id of the shell or the foreground process.
    pub pgid: Option<u32>,
    /// Session id of the shell.
    pub sid: Option<u32>,
    /// Pane id.
    pub pane: Option<usize>,
    pub limit: Option<usize>,
    /// Controlling tty / pty path: `/dev/pts/3`, `pts/3` or just `3`.
    pub pty: Option<String>,
    /// Substring of the shell, foreground or tracked cwd.
    pub cwd: Option<String>,
    /// Case-insensitive substring of a process name or command line (secret-looking command lines excluded).
    pub cmd: Option<String>,
    /// Case-insensitive substring of a process name (shell or foreground).
    pub name: Option<String>,
    /// Owner user name or numeric uid.
    pub user: Option<String>,
    /// Environment marker `KEY` or `KEY=VALUE` of the shell or foreground process.
    pub env: Option<String>,
    /// Case-insensitive substring of the pane title.
    pub title: Option<String>,
    /// Case-insensitive text in the screen and scrollback.
    pub text: Option<String>,
    /// Session (workspace) name.
    pub session: Option<String>,
}

fn cap(s: &str) -> String {
    s.chars().take(FILTER_CAP).collect()
}

fn num<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse().map_err(|_| format!("--{flag} needs a number, got '{v}'"))
}

impl FindQuery {
    /// True when no filter is set.
    pub fn is_empty(&self) -> bool {
        *self == Self { limit: self.limit, ..Self::default() }
    }

    /// Sets filter `flag` (the long option name without `--`) to `value`.
    pub fn set(&mut self, flag: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        if v.is_empty() {
            return Err(format!("--{flag} needs a value"));
        }
        match flag {
            "pid" => self.pid = Some(num(flag, v)?),
            "pgid" => self.pgid = Some(num(flag, v)?),
            "sid" => self.sid = Some(num(flag, v)?),
            "pane" => self.pane = Some(num(flag, v)?),
            "limit" => self.limit = Some(num::<usize>(flag, v)?.clamp(1, LIMIT_MAX)),
            "pty" | "tty" => self.pty = Some(cap(v)),
            "cwd" => self.cwd = Some(cap(v)),
            "cmd" => self.cmd = Some(cap(v)),
            "name" => self.name = Some(cap(v)),
            "user" => self.user = Some(cap(v)),
            "env" => self.env = Some(cap(v)),
            "title" => self.title = Some(cap(v)),
            "text" => self.text = Some(cap(v)),
            "session" => self.session = Some(cap(v)),
            other => return Err(format!("unknown find option --{other}")),
        }
        Ok(())
    }

    /// Effective result limit.
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(LIMIT_DEFAULT)
    }

    /// The `--flag value ...` words that rebuild this query (round-trips through [`Tokens::parse`]).
    pub fn colon_args(&self) -> String {
        let mut out = Vec::new();
        let mut n = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                out.push(format!("--{k} {v}"));
            }
        };
        n("pid", self.pid.map(|v| v.to_string()));
        n("pgid", self.pgid.map(|v| v.to_string()));
        n("sid", self.sid.map(|v| v.to_string()));
        n("pane", self.pane.map(|v| v.to_string()));
        n("pty", self.pty.clone());
        n("cwd", self.cwd.clone());
        n("cmd", self.cmd.clone());
        n("name", self.name.clone());
        n("user", self.user.clone());
        n("env", self.env.clone());
        n("title", self.title.clone());
        n("text", self.text.clone());
        n("session", self.session.clone());
        n("limit", self.limit.map(|v| v.to_string()));
        out.join(" ")
    }
}

/// What to do with the matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindAct {
    /// Focus (and raise) the first match.
    Focus,
    /// Type `text` into every match (`enter` appends CR). Same safety gates as `pane.send`.
    Send { text: String, enter: bool },
    /// Ring the bell indicator on every match (and the host terminal).
    Beep,
    /// Briefly highlight every match.
    Flash,
    /// Mark every match as selected (persistent until `:find-select clear`).
    Select,
}

impl FindAct {
    /// Verb used in `:find-<verb>`, RPC `find.<verb>` and CLI `--act`.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::Send { .. } => "send",
            Self::Beep => "beep",
            Self::Flash => "flash",
            Self::Select => "select",
        }
    }
}

/// One session-find operation; every surface lowers to exactly one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionFindOp {
    /// `:find --pid N ...` - list matches (no side effects besides remembering them).
    Locate(Box<FindQuery>),
    /// `:find-<act> [filters]` - run `query` (or reuse the last matches when it is empty) and act.
    Act { act: FindAct, query: Box<FindQuery> },
    /// `Ctrl+Alt+D` - put `:find --text ` on the prompt.
    Prefill,
    /// `:find-select clear` - drop the selection.
    ClearSelection,
}

impl SessionFindOp {
    /// The `:` form (parses back to an equal op).
    pub fn colon_form(&self) -> String {
        let join = |head: String, q: &FindQuery| {
            let args = q.colon_args();
            if args.is_empty() { head } else { format!("{head} {args}") }
        };
        match self {
            Self::Locate(q) if q.is_empty() => ":find --all".to_string(),
            Self::Locate(q) => join(":find".to_string(), q),
            Self::Act { act, query } => {
                let mut s = join(format!(":find-{}", act.verb()), query);
                if let FindAct::Send { text, enter } = act {
                    s.push_str(&format!(" --send {text}"));
                    if *enter {
                        s.push_str(" --enter");
                    }
                }
                s
            }
            Self::Prefill => ":find-prefill".to_string(),
            Self::ClearSelection => ":find-select clear".to_string(),
        }
    }
}

/// Result of tokenising `--flag value...` words.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub query: FindQuery,
    /// `--send TEXT`.
    pub send: Option<String>,
    pub enter: bool,
    /// Bare word `clear` (for `:find-select clear`).
    pub clear: bool,
}

impl Tokens {
    /// Parses words: a value runs to the next `--flag`, so `--text hello world` works unquoted.
    pub fn parse(words: &[&str]) -> Result<Self, String> {
        let mut out = Self::default();
        let mut i = 0;
        while i < words.len() {
            let w = words[i];
            let Some(flag) = w.strip_prefix("--") else {
                if w == "clear" && words.len() == 1 {
                    out.clear = true;
                    i += 1;
                    continue;
                }
                return Err(format!("unexpected '{w}' (filters are --pid --pgid --sid --pty --cwd --cmd --name --user --env --title --text --session --pane --limit)"));
            };
            if flag == "enter" || flag == "all" {
                out.enter |= flag == "enter";
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut end = start;
            while end < words.len() && !words[end].starts_with("--") {
                end += 1;
            }
            let value = words[start..end].join(" ");
            if flag == "send" {
                if value.is_empty() {
                    return Err("--send needs text".to_string());
                }
                out.send = Some(value);
            } else {
                out.query.set(flag, &value)?;
            }
            i = end;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_collect_multiword_values_and_flags() {
        let t = Tokens::parse(&["--pid", "42", "--text", "hello", "world", "--cwd", "/tmp"]).unwrap();
        assert_eq!(t.query.pid, Some(42));
        assert_eq!(t.query.text.as_deref(), Some("hello world"));
        assert_eq!(t.query.cwd.as_deref(), Some("/tmp"));
        let t = Tokens::parse(&["--send", "ls", "-l", "--enter"]).unwrap();
        assert_eq!((t.send.as_deref(), t.enter), (Some("ls -l"), true));
    }

    #[test]
    fn tokens_reject_bad_input() {
        assert!(Tokens::parse(&["--pid", "x"]).is_err());
        assert!(Tokens::parse(&["--pid"]).is_err());
        assert!(Tokens::parse(&["--bogus", "1"]).is_err());
        assert!(Tokens::parse(&["stray"]).is_err());
        assert!(Tokens::parse(&["--send"]).is_err());
    }

    #[test]
    fn limit_is_clamped_and_filters_capped() {
        let mut q = FindQuery::default();
        q.set("limit", "99999").unwrap();
        assert_eq!(q.limit(), LIMIT_MAX);
        q.set("text", &"x".repeat(1000)).unwrap();
        assert_eq!(q.text.unwrap().chars().count(), FILTER_CAP);
    }

    #[test]
    fn colon_form_round_trips() {
        let mut q = FindQuery::default();
        q.set("pid", "7").unwrap();
        q.set("cmd", "vim foo").unwrap();
        let t = Tokens::parse(&q.colon_args().split_whitespace().collect::<Vec<_>>()).unwrap();
        assert_eq!(t.query, q);
    }
}
