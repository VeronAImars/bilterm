//! `Action::Dos*` execution core (D3a): maps the action payloads onto the CLI [`Cmd`] and runs
//! them with the real host context. Used by colon, RPC and MCP so no surface has its own logic.
//!
//! Consent: these paths have no terminal, so stdin is empty. Anything that asks for confirmation
//! (setup download, remove, reset, restore) is declined unless `--yes` / `yes: true` was given.

use super::cli::{self, Cmd};
use super::image::cmd::ImgCmd;
use crate::api::json::JsonValue;

/// Result of running one [`Cmd`] without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpResult {
    pub code: i32,
    pub output: String,
}

impl OpResult {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Short operation name for labels and JSON.
pub fn op_name(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::Setup { .. } => "setup",
        Cmd::Remove { .. } => "remove",
        Cmd::Doctor => "doctor",
        Cmd::Status => "status",
        Cmd::Run { .. } => "run",
        Cmd::Image(ImgCmd::Reset { .. }) => "reset",
        Cmd::Image(ImgCmd::Restore { .. }) => "restore",
        Cmd::Image(_) => "snapshot",
    }
}

/// Operations that touch the network or copy large files: run off the UI thread.
pub fn is_long_running(cmd: &Cmd) -> bool {
    !matches!(cmd, Cmd::Doctor | Cmd::Status | Cmd::Run { .. })
}

/// Parses the image-lifecycle subcommands (`reset`, `snapshot`, `restore`) with the CLI grammar.
pub fn parse_image(sub: &str, args: &[String]) -> Result<Cmd, String> {
    let mut v = vec![sub.to_string()];
    v.extend(args.iter().cloned());
    cli::parse(&v)
}

/// Runs `cmd` against the real host (empty stdin: confirmations are declined).
pub fn run_cmd(cmd: &Cmd) -> OpResult {
    match cli::with_host_ctx(|ctx| {
        let mut out = String::new();
        let code = cli::execute(cmd, ctx, &mut std::io::BufReader::new(std::io::empty()), &mut out);
        OpResult { code, output: out }
    }) {
        Ok(r) => r,
        Err(e) => OpResult { code: 1, output: format!("{e}\n") },
    }
}

/// JSON form of a finished operation (`dos.*` RPC results).
pub fn result_json(op: &str, r: &OpResult) -> JsonValue {
    JsonValue::obj([
        ("op", JsonValue::String(op.to_string())),
        ("status", JsonValue::String(if r.ok() { "ok" } else { "error" }.to_string())),
        ("exit_code", JsonValue::Number(i64::from(r.code))),
        ("output", JsonValue::String(r.output.clone())),
    ])
}

/// Structured `dos.status` payload (same view model as the text form).
pub fn status_json() -> Result<JsonValue, String> {
    cli::with_host_ctx(|ctx| super::status::to_json(&super::status::gather(ctx)))
}
