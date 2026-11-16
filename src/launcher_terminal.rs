//! Pure host-terminal selection for the desktop launcher (`LAUNCH-01`).
//!
//! [`terminal_candidates`] decides, for a target OS, an environment snapshot and a "does this program
//! exist" predicate, which terminal commands to try and in which order. It touches no process state, so
//! the decision table for all three operating systems is unit-tested on any host. The thin spawning
//! layer is [`crate::launcher::ensure_terminal_or_relaunch`].
//!
//! Order per OS (an existing `BILTERM_TERMINAL`, then `TERMINAL`, always comes first):
//! - **Windows**: Windows Terminal (`wt.exe`), PowerShell 7 (`pwsh.exe`), Windows PowerShell
//!   (`powershell.exe`), then `cmd.exe` (always present).
//! - **macOS**: iTerm2 when installed, otherwise Terminal.app.
//! - **Linux**: `xdg-terminal-exec`, `x-terminal-emulator`, then gnome-terminal, kgx, ptyxis, konsole,
//!   xfce4-terminal, alacritty, kitty, foot, wezterm, ghostty, tilix, mate-terminal, urxvt, xterm. Only
//!   tried when a graphical session (`DISPLAY` or `WAYLAND_DISPLAY`) exists.

use std::path::Path;

/// Operating system the candidates are built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOs {
    Linux,
    Windows,
    MacOs,
}

impl TargetOs {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// Environment facts the choice depends on.
#[derive(Clone, Debug, Default)]
pub struct TermEnv {
    /// `BILTERM_TERMINAL` (owner override).
    pub bilterm_terminal: Option<String>,
    /// `TERMINAL` (the de-facto standard variable).
    pub terminal: Option<String>,
    /// Linux: `DISPLAY` or `WAYLAND_DISPLAY` is set, so a terminal emulator window can open.
    pub graphical_session: bool,
    /// `$HOME`, used for `~/Applications` on macOS.
    pub home: Option<String>,
}

impl TermEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            bilterm_terminal: var("BILTERM_TERMINAL"),
            terminal: var("TERMINAL"),
            graphical_session: var("DISPLAY").is_some() || var("WAYLAND_DISPLAY").is_some(),
            home: var("HOME"),
        }
    }
}

/// One terminal command to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCmd {
    pub program: String,
    pub args: Vec<String>,
    /// Windows: give the program its own console window (`CREATE_NEW_CONSOLE`).
    pub new_console: bool,
    /// Windows: `args` is one preformatted command line passed verbatim (cmd.exe quoting).
    pub raw_args: bool,
    /// Wait for the program and require exit status 0 (`osascript`, which hands off to the app and exits).
    pub wait: bool,
}

impl TerminalCmd {
    fn plain(program: &str, args: Vec<String>) -> Self {
        Self { program: program.to_string(), args, new_console: false, raw_args: false, wait: false }
    }
}

fn with_exe(prefix: &[&str], exe: &str, rest: &[String]) -> Vec<String> {
    let mut a: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    a.push(exe.to_string());
    a.extend(rest.iter().cloned());
    a
}

/// POSIX single-quote quoting for `sh -c` / AppleScript `exec` strings.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn sh_command(exe: &str, args: &[String]) -> String {
    std::iter::once(exe).chain(args.iter().map(String::as_str)).map(sh_quote).collect::<Vec<_>>().join(" ")
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// PowerShell single-quoted literal.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Ordered terminal commands to host `exe args...`. `exists` answers whether a program (a bare name found
/// on `PATH`, or an absolute path / `.app` bundle) is installed. Pure.
pub fn terminal_candidates(
    os: TargetOs,
    env: &TermEnv,
    exists: &dyn Fn(&str) -> bool,
    exe: &str,
    args: &[String],
) -> Vec<TerminalCmd> {
    let mut out: Vec<TerminalCmd> = Vec::new();

    for custom in [&env.bilterm_terminal, &env.terminal].into_iter().flatten() {
        let mut words = custom.split_whitespace();
        if let Some(prog) = words.next() {
            let mut a: Vec<String> = words.map(String::from).collect();
            a.push("-e".to_string());
            a.push(exe.to_string());
            a.extend(args.iter().cloned());
            out.push(TerminalCmd::plain(prog, a));
        }
    }

    match os {
        TargetOs::Windows => {
            if exists("wt.exe") {
                out.push(TerminalCmd::plain(
                    "wt.exe",
                    with_exe(&["new-tab", "--title", "Bilterm", "--"], exe, args),
                ));
            }
            let ps_cmd = format!(
                "& {}",
                std::iter::once(exe).chain(args.iter().map(String::as_str)).map(ps_quote).collect::<Vec<_>>().join(" ")
            );
            for ps in ["pwsh.exe", "powershell.exe"] {
                if exists(ps) {
                    let mut c = TerminalCmd::plain(
                        ps,
                        vec!["-NoLogo".into(), "-Command".into(), ps_cmd.clone()],
                    );
                    c.new_console = true;
                    out.push(c);
                }
            }
            // cmd.exe ships with every Windows install, so it is the unconditional last resort.
            let line = std::iter::once(exe)
                .chain(args.iter().map(String::as_str))
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(" ");
            let mut c = TerminalCmd::plain("cmd.exe", vec![format!("/d /c \"{line}\"")]);
            c.new_console = true;
            c.raw_args = true;
            out.push(c);
        }
        TargetOs::MacOs => {
            let cmd = applescript_escape(&format!("exec {}", sh_command(exe, args)));
            let home_iterm = env.home.as_deref().map(|h| format!("{h}/Applications/iTerm.app"));
            let has_iterm = exists("/Applications/iTerm.app") || home_iterm.as_deref().is_some_and(exists);
            if has_iterm {
                let script = format!(
                    "tell application \"iTerm\"\nactivate\ncreate window with default profile command \"{cmd}\"\nend tell"
                );
                let mut c = TerminalCmd::plain("osascript", vec!["-e".into(), script]);
                c.wait = true;
                out.push(c);
            }
            let script = format!("tell application \"Terminal\"\nactivate\ndo script \"{cmd}\"\nend tell");
            let mut c = TerminalCmd::plain("osascript", vec!["-e".into(), script]);
            c.wait = true;
            out.push(c);
        }
        TargetOs::Linux => {
            if !env.graphical_session {
                return out;
            }
            // (program, args placed before the command)
            let table: [(&str, &[&str]); 17] = [
                ("xdg-terminal-exec", &[]),
                ("x-terminal-emulator", &["-e"]),
                ("gnome-terminal", &["--"]),
                ("kgx", &["-e"]),
                ("ptyxis", &["--"]),
                ("konsole", &["-e"]),
                ("xfce4-terminal", &["-x"]),
                ("alacritty", &["-e"]),
                ("kitty", &["--"]),
                ("foot", &["--"]),
                ("wezterm", &["start", "--"]),
                ("ghostty", &["-e"]),
                ("tilix", &["-e"]),
                ("mate-terminal", &["-x"]),
                ("lxterminal", &["-e"]),
                ("urxvt", &["-e"]),
                ("xterm", &["-e"]),
            ];
            for (prog, prefix) in table {
                if exists(prog) {
                    out.push(TerminalCmd::plain(prog, with_exe(prefix, exe, args)));
                }
            }
        }
    }
    out
}

/// `true` if `name` is a file on `PATH` (Windows: also tries `.exe`; reparse-point aliases such as
/// `wt.exe` count), or `name` is an existing absolute path / bundle.
pub fn program_exists(name: &str) -> bool {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.exists();
    }
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| {
        let f = d.join(name);
        f.symlink_metadata().is_ok_and(|m| !m.is_dir())
    })
}

/// The arguments for the terminal-UI fallback: `--gui`/`-G` removed, `--tui` added.
pub fn tui_fallback_args(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = args.iter().filter(|a| !matches!(a.as_str(), "-G" | "--gui")).cloned().collect();
    if !out.iter().any(|a| a == "--tui") {
        out.insert(0, "--tui".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(list: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n| list.contains(&n)
    }
    fn progs(v: &[TerminalCmd]) -> Vec<&str> {
        v.iter().map(|c| c.program.as_str()).collect()
    }
    const EXE: &str = "/opt/bilterm";

    #[test]
    fn windows_order_is_wt_pwsh_powershell_cmd() {
        let all = on(&["wt.exe", "pwsh.exe", "powershell.exe"]);
        let v = terminal_candidates(TargetOs::Windows, &TermEnv::default(), &all, EXE, &[]);
        assert_eq!(progs(&v), ["wt.exe", "pwsh.exe", "powershell.exe", "cmd.exe"]);
        let v = terminal_candidates(TargetOs::Windows, &TermEnv::default(), &on(&["powershell.exe"]), EXE, &[]);
        assert_eq!(progs(&v), ["powershell.exe", "cmd.exe"]);
        let v = terminal_candidates(TargetOs::Windows, &TermEnv::default(), &on(&[]), EXE, &[]);
        assert_eq!(progs(&v), ["cmd.exe"], "cmd.exe is the unconditional last resort");
        assert!(v[0].new_console && v[0].raw_args);
    }

    #[test]
    fn windows_commands_carry_exe_and_args_quoted() {
        let a = vec!["-s".to_string(), "o'ps".to_string()];
        let v = terminal_candidates(TargetOs::Windows, &TermEnv::default(), &on(&["wt.exe", "pwsh.exe"]), EXE, &a);
        assert_eq!(v[0].args, ["new-tab", "--title", "Bilterm", "--", EXE, "-s", "o'ps"]);
        assert_eq!(v[1].args[2], "& '/opt/bilterm' '-s' 'o''ps'");
        assert_eq!(v[2].args[0], "/d /c \"\"/opt/bilterm\" \"-s\" \"o'ps\"\"");
    }

    #[test]
    fn macos_prefers_iterm_when_installed_else_terminal_app() {
        let home = TermEnv { home: Some("/Users/b".into()), ..Default::default() };
        let v = terminal_candidates(TargetOs::MacOs, &home, &on(&["/Applications/iTerm.app"]), EXE, &[]);
        assert_eq!(v.len(), 2);
        assert!(v[0].args[1].contains("tell application \"iTerm\""));
        assert!(v[1].args[1].contains("tell application \"Terminal\""));
        let v = terminal_candidates(TargetOs::MacOs, &home, &on(&["/Users/b/Applications/iTerm.app"]), EXE, &[]);
        assert!(v[0].args[1].contains("iTerm"));
        let v = terminal_candidates(TargetOs::MacOs, &home, &on(&[]), EXE, &[]);
        assert_eq!(v.len(), 1);
        assert!(v[0].wait && v[0].args[1].contains("do script \"exec '/opt/bilterm'\""));
    }

    #[test]
    fn macos_script_escapes_quotes_in_arguments() {
        let a = vec!["a\"b".to_string()];
        let v = terminal_candidates(TargetOs::MacOs, &TermEnv::default(), &on(&[]), EXE, &a);
        assert!(v[0].args[1].contains("'a\\\"b'"));
    }

    #[test]
    fn linux_order_and_only_installed_programs() {
        let env = TermEnv { graphical_session: true, ..Default::default() };
        let have = on(&["xterm", "gnome-terminal", "kgx", "xdg-terminal-exec", "konsole"]);
        let v = terminal_candidates(TargetOs::Linux, &env, &have, EXE, &["x".to_string()]);
        assert_eq!(progs(&v), ["xdg-terminal-exec", "gnome-terminal", "kgx", "konsole", "xterm"]);
        assert_eq!(v[0].args, [EXE, "x"]);
        assert_eq!(v[1].args, ["--", EXE, "x"]);
    }

    #[test]
    fn linux_wayland_only_session_counts_but_no_session_does_not() {
        let have = on(&["foot", "wezterm"]);
        let wl = TermEnv { graphical_session: true, ..Default::default() };
        assert_eq!(progs(&terminal_candidates(TargetOs::Linux, &wl, &have, EXE, &[])), ["foot", "wezterm"]);
        let none = TermEnv::default();
        assert!(terminal_candidates(TargetOs::Linux, &none, &have, EXE, &[]).is_empty());
    }

    #[test]
    fn overrides_come_first_on_every_os_and_split_words() {
        let env = TermEnv {
            bilterm_terminal: Some("mine --flag".into()),
            terminal: Some("kitty".into()),
            graphical_session: true,
            ..Default::default()
        };
        for os in [TargetOs::Linux, TargetOs::Windows, TargetOs::MacOs] {
            let v = terminal_candidates(os, &env, &on(&["xterm"]), EXE, &[]);
            assert_eq!(v[0].program, "mine");
            assert_eq!(v[0].args, ["--flag", "-e", EXE]);
            assert_eq!(v[1].program, "kitty");
        }
    }

    #[test]
    fn tui_fallback_args_drop_gui_flags_and_add_tui() {
        let a: Vec<String> = ["--gui", "-s", "ops", "-G"].iter().map(|s| s.to_string()).collect();
        assert_eq!(tui_fallback_args(&a), ["--tui", "-s", "ops"]);
        assert_eq!(tui_fallback_args(&["--tui".to_string()]), ["--tui"]);
    }
}
