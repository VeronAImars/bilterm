//! Desktop Launcher, Non-TTY Terminal Self-Relaunch & Working Directory Normalization (`LAUNCH-01`).
//!
//! Enables `bilterm` to be launched directly from OS application launchers without first opening a
//! terminal window:
//! - **Windows Run (`Win+R` -> `bilterm` or `bilterm --gui`) & Start Menu `.lnk` shortcuts**
//! - **Linux Ubuntu GNOME Application Launcher / Rofi / dmenu (`bilterm.desktop`)**
//! - **macOS Spotlight / Launchpad / Finder (`Bilterm.app`)**
//!
//! Which terminal hosts the TUI is decided by the pure [`crate::launcher_terminal`] module.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use crate::launcher_terminal::{TargetOs, TermEnv, TerminalCmd, terminal_candidates, tui_fallback_args};
use crate::launcher_terminal::program_exists;

/// Environment variable guarding against infinite relaunch recursion when no terminal emulator succeeds.
pub const RELAUNCH_GUARD_ENV: &str = "BILTERM_RELAUNCHED";

/// Returns `true` when both `stdin` and `stdout` are attached to an interactive controlling TTY/console.
pub fn has_controlling_tty() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Returns `true` if `dir` looks like an OS system/root directory commonly inherited from desktop
/// launchers (`Win+R` -> `C:\Windows\System32`, GNOME/macOS launcher -> `/`).
pub fn is_system_launch_dir(dir: &Path) -> bool {
    let s = dir.to_string_lossy();
    if s == "/" || s == "\\" {
        return true;
    }
    let lower = s.replace('/', "\\").to_ascii_lowercase();
    let trimmed = lower.trim_end_matches('\\');
    trimmed.ends_with("\\windows\\system32")
        || trimmed.ends_with("\\windows\\syswow64")
        || trimmed.ends_with("\\windows")
}

/// Normalizes the process working directory to the user's home directory (`$HOME` / `%USERPROFILE%`)
/// when `bilterm` is started from an OS Run dialog or desktop launcher whose initial working
/// directory is `/` or `C:\Windows\System32`.
pub fn normalize_desktop_working_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let needs_home = match &cwd {
        Some(dir) => is_system_launch_dir(dir),
        None => true,
    };
    if !needs_home {
        return cwd;
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    if home.is_dir() && std::env::set_current_dir(&home).is_ok() {
        return Some(home);
    }
    cwd
}

/// Resolves a writable directory for `bilterm.log` so launching from `Win+R` (`C:\Windows\System32`),
/// GNOME Launcher (`/`), or macOS Spotlight never fails or pollutes system directories.
pub fn resolve_log_directory() -> PathBuf {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        let dir = PathBuf::from(state_home).join("bilterm");
        if crate::secure_fs::create_private_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let dir = PathBuf::from(local_app_data).join("bilterm").join("logs");
        if crate::secure_fs::create_private_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("bilterm");
        if crate::secure_fs::create_private_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    if let Ok(runtime) = crate::secure_fs::private_runtime_dir() {
        return runtime;
    }
    std::env::temp_dir()
}

/// Ordered terminal commands for this machine (see [`crate::launcher_terminal::terminal_candidates`]).
pub fn candidate_terminal_commands(
    exe_path: &Path,
    passthrough_args: &[String],
) -> Vec<TerminalCmd> {
    terminal_candidates(
        TargetOs::current(),
        &TermEnv::from_process(),
        &program_exists,
        &exe_path.to_string_lossy(),
        passthrough_args,
    )
}

/// Starts one terminal candidate. `true` once the terminal was handed the command.
fn spawn_terminal(cmd: &TerminalCmd) -> bool {
    let mut c = Command::new(&cmd.program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if cmd.raw_args {
            for a in &cmd.args {
                c.raw_arg(a);
            }
        } else {
            c.args(&cmd.args);
        }
        if cmd.new_console {
            c.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
        }
    }
    #[cfg(not(windows))]
    c.args(&cmd.args);
    c.env(RELAUNCH_GUARD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if cmd.wait {
        return c.status().is_ok_and(|s| s.success());
    }
    c.spawn().is_ok()
}

/// Facts that decide whether a bare `bilterm` should open the native GUI window instead of the TUI.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchContext {
    /// stdin and stdout are both attached to a terminal.
    pub has_tty: bool,
    /// The native GUI can reach a display (Linux: `DISPLAY` set, i.e. X11 or XWayland; a Wayland-only
    /// session without `DISPLAY` has no GUI and goes to a terminal; macOS/Windows: always).
    pub display: bool,
    /// Windows only: this process is the sole owner of its console (started from Run/Explorer, not a shell).
    pub private_console: bool,
    /// `--tui` or `BILTERM_GUI=0`.
    pub force_tui: bool,
    /// Already relaunched inside a terminal emulator by [`ensure_terminal_or_relaunch`].
    pub relaunched: bool,
}

/// `true` when a bare `bilterm` was started by the desktop (GNOME/KDE launcher, Windows Run, Finder)
/// rather than by a shell, so the native GUI window is the right default. Pure; see [`LaunchContext::detect`].
pub fn should_default_to_gui(ctx: LaunchContext) -> bool {
    if ctx.force_tui || ctx.relaunched {
        return false;
    }
    (!ctx.has_tty && ctx.display) || ctx.private_console
}

impl LaunchContext {
    /// Reads the live process environment. `args` are the CLI arguments after the program name.
    pub fn detect(args: &[String]) -> Self {
        let force_tui = args.iter().any(|a| a == "--tui")
            || std::env::var("BILTERM_GUI").as_deref() == Ok("0");
        Self {
            has_tty: has_controlling_tty(),
            display: display_available(),
            private_console: owns_private_console(),
            force_tui,
            relaunched: std::env::var(RELAUNCH_GUARD_ENV).as_deref() == Ok("1"),
        }
    }
}

fn display_available() -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    {
        true
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
    }
}

/// Windows: a console exe started from `Win+R`/Explorer gets a fresh console that only it uses; one started
/// from `cmd`/PowerShell/Windows Terminal shares the console with the shell (process list >= 2).
fn owns_private_console() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        }
        let mut pids = [0u32; 4];
        // SAFETY: `pids` is a valid writable buffer of `count` u32s.
        let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
        n == 1
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Checks whether the current process has a controlling TTY; if not, attempts to relaunch `bilterm`
/// inside an available host terminal emulator.
///
/// Returns `Ok(true)` if a child terminal emulator was spawned (caller should exit `0`), or
/// `Ok(false)` if a TTY is already attached and normal TUI startup should proceed.
pub fn ensure_terminal_or_relaunch(passthrough_args: &[String]) -> io::Result<bool> {
    if has_controlling_tty() {
        return Ok(false);
    }
    if std::env::var(RELAUNCH_GUARD_ENV).as_deref() == Ok("1") {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    for cmd in candidate_terminal_commands(&exe, passthrough_args) {
        if spawn_terminal(&cmd) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The native GUI could not start (`BiltermError::GuiUnavailable`). A launcher click has no visible
/// stderr, so never exit silently: continue as the terminal UI. From a shell (TTY present) that runs
/// `bilterm --tui` in the same terminal; otherwise it opens a native terminal (see
/// [`crate::launcher_terminal`]). Returns `Ok(Some(code))` with the exit code to use once the TUI ended,
/// or `Ok(None)` when it was handed to a new terminal window or no terminal could be opened.
/// `BILTERM_GUI=0` and `--tui` keep the child from trying the GUI again, so this cannot loop.
pub fn fall_back_to_tui(args: &[String]) -> io::Result<Option<i32>> {
    let tui_args = tui_fallback_args(args);
    if has_controlling_tty() {
        let status = Command::new(std::env::current_exe()?)
            .args(&tui_args)
            .env("BILTERM_GUI", "0")
            .status()?;
        return Ok(Some(status.code().unwrap_or(1)));
    }
    if std::env::var(RELAUNCH_GUARD_ENV).as_deref() == Ok("1") {
        return Ok(None);
    }
    let exe = std::env::current_exe()?;
    for cmd in candidate_terminal_commands(&exe, &tui_args) {
        if spawn_terminal(&cmd) {
            return Ok(None);
        }
    }
    Ok(None)
}

/// Generates a freedesktop.org `.desktop` launcher entry for Linux application launchers
/// (Ubuntu GNOME Activities, KDE KRunner, Rofi, Wofi, dmenu) with both default TUI and
/// Native GUI (`--gui`) actions (`LAUNCH-01`).
pub fn desktop_entry_contents(exec_path: &str, icon_path: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Version=1.0\n\
         Type=Application\n\
         Name=Bilterm\n\
         GenericName=Composite Terminal Multiplexer\n\
         Comment=IBM 3270 Mainframe & Borland Turbo Vision Composite Terminal Multiplexer\n\
         Exec={exec_path} --gui\n\
         TryExec={exec_path}\n\
         Icon={icon_path}\n\
         Terminal=false\n\
         Categories=System;TerminalEmulator;Utility;\n\
         Keywords=terminal;tmux;3270;borland;multiplexer;shell;ssh;\n\
         StartupNotify=true\n\
         StartupWMClass=bilterm\n\
         Actions=Terminal;QuickSession;\n\
         \n\
         [Desktop Action Terminal]\n\
         Name=Open in a Terminal (TUI)\n\
         Exec={exec_path} --tui\n\
         Terminal=true\n\
         \n\
         [Desktop Action QuickSession]\n\
         Name=New Persistent Session\n\
         Exec={exec_path} --tui -s main\n\
         Terminal=true\n"
    )
}

/// Generates the `Contents/Info.plist` for a macOS `Bilterm.app` bundle (`LAUNCH-01`).
pub fn macos_info_plist_contents(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>CFBundleName</key>\n\
           <string>Bilterm</string>\n\
           <key>CFBundleDisplayName</key>\n\
           <string>Bilterm</string>\n\
           <key>CFBundleIdentifier</key>\n\
           <string>dev.bilterm.app</string>\n\
           <key>CFBundleVersion</key>\n\
           <string>{version}</string>\n\
           <key>CFBundleShortVersionString</key>\n\
           <string>{version}</string>\n\
           <key>CFBundlePackageType</key>\n\
           <string>APPL</string>\n\
           <key>CFBundleExecutable</key>\n\
           <string>bilterm-launcher</string>\n\
           <key>LSMinimumSystemVersion</key>\n\
           <string>11.0</string>\n\
           <key>NSHighResolutionCapable</key>\n\
           <true/>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Generates the `Contents/MacOS/bilterm-launcher` script for a macOS `Bilterm.app` bundle,
/// defaulting to standalone native GUI mode (`bilterm --gui`).
pub fn macos_launcher_script_contents(exec_path: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\"\n\
         exec \"{exec_path}\" --gui \"$@\"\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_system_launch_directories() {
        assert!(is_system_launch_dir(Path::new("/")));
        assert!(is_system_launch_dir(Path::new(r"C:\Windows\System32")));
        assert!(is_system_launch_dir(Path::new(r"c:\windows\syswow64")));
        assert!(is_system_launch_dir(Path::new(r"C:\Windows")));
        assert!(!is_system_launch_dir(Path::new(
            r"C:\Users\bills\src\terminals\bilterm"
        )));
        assert!(!is_system_launch_dir(Path::new("/home/bills/src/bilterm")));
    }

    #[test]
    fn candidate_terminal_commands_carry_passthrough_args() {
        let exe = Path::new("/usr/local/bin/bilterm");
        let args = vec!["-s".to_string(), "ops".to_string()];
        // Windows and macOS always have a last-resort terminal; Linux depends on what is installed.
        for c in candidate_terminal_commands(exe, &args) {
            assert!(c.args.iter().any(|a| a.contains("-s")), "{c:?}");
        }
    }

    #[test]
    fn desktop_launch_defaults_to_gui_but_shell_and_opt_out_do_not() {
        let base = LaunchContext { display: true, ..Default::default() };
        assert!(should_default_to_gui(base), "no tty + display = desktop launcher");
        assert!(!should_default_to_gui(LaunchContext { has_tty: true, ..base }), "shell keeps the TUI");
        assert!(!should_default_to_gui(LaunchContext { display: false, ..base }), "no display: terminal relaunch");
        assert!(!should_default_to_gui(LaunchContext { force_tui: true, ..base }), "--tui / BILTERM_GUI=0");
        assert!(!should_default_to_gui(LaunchContext { relaunched: true, ..base }));
        assert!(should_default_to_gui(LaunchContext { has_tty: true, private_console: true, display: true, ..Default::default() }));
        assert!(!should_default_to_gui(LaunchContext { private_console: true, force_tui: true, ..base }));
    }

    #[test]
    fn generates_valid_xdg_desktop_entry_and_macos_plist() {
        let desktop = desktop_entry_contents("/home/user/.local/bin/bilterm", "bilterm");
        assert!(desktop.contains("[Desktop Entry]"));
        assert!(desktop.contains("Exec=/home/user/.local/bin/bilterm --gui\n"));
        assert!(desktop.contains("Terminal=false"));
        assert!(desktop.contains("Exec=/home/user/.local/bin/bilterm --tui\n"));

        let plist = macos_info_plist_contents("1.10.2");
        assert!(plist.contains("<string>dev.bilterm.app</string>"));
        assert!(plist.contains("<string>bilterm-launcher</string>"));

        let mac_launcher = macos_launcher_script_contents("/Users/bill/.local/bin/bilterm");
        assert!(mac_launcher.contains("exec \"/Users/bill/.local/bin/bilterm\" --gui \"$@\""));
    }
}
