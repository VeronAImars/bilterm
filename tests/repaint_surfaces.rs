//! `Action::Repaint` surface parity (`UNI-R01`) and draw-loop invalidation: one action reachable as
//! colon command (`:repaint` / `:redraw`), leader key, palette entry, `ui.repaint` RPC, `bilterm
//! repaint` CLI and `bilterm_repaint` MCP tool; consuming it forces every host cell to be rewritten.

use bilterm::action::Action;
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::app::{repaint_if_requested, App};
use bilterm::colon_command::parse_colon_command;
use bilterm::input::DEFAULT_LEADER_BINDINGS;
use bilterm::pane::CompositePane;
use bilterm::ui::modals::PALETTE_CATALOG;
use bilterm::ui::WorkspaceWidget;
use crossterm::event::KeyCode;
use ratatui::backend::{Backend, TestBackend};
use ratatui::Terminal;

fn app() -> App {
    App::new(CompositePane::new_headless(1, 10, 40))
}

#[test]
fn colon_aliases_lower_to_repaint() {
    for c in [":repaint", ":redraw"] {
        assert_eq!(parse_colon_command(c).unwrap().unwrap(), Action::Repaint, "{c}");
    }
    assert!(parse_colon_command(":repaint now").unwrap().is_err());
}

#[test]
fn leader_key_palette_rpc_mcp_and_cli_are_wired() {
    assert!(DEFAULT_LEADER_BINDINGS.iter().any(|(k, a)| *k == KeyCode::Char(')') && *a == Action::Repaint));
    assert!(PALETTE_CATALOG.iter().any(|e| e.0 == ":repaint"));
    assert!(bilterm::api::rpc::RPC_METHODS.contains(&"ui.repaint"));
    let tools = AutomationControlPlane::mcp_tools_list().to_json_string();
    assert!(tools.contains("bilterm_repaint"));
    let env = bilterm::cli::CliEnv::default();
    let call = bilterm::cli::parse(&["repaint".to_string()], &env).unwrap();
    assert_eq!(call.method, "ui.repaint");
    assert!(bilterm::cli::CLI_COMMANDS.contains(&"repaint"));
}

#[test]
fn rpc_and_mcp_set_the_repaint_request() {
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    let none = JsonValue::parse("{}").unwrap();
    let r = cp.dispatch_method("ui.repaint", &none).unwrap();
    assert_eq!(r.get("status").and_then(JsonValue::as_str), Some("ok"));
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_repaint","arguments":{}}}"#;
    let out = cp.handle_mcp_line(req).unwrap_or_default();
    assert!(!out.contains("\"error\""), "{out}");
}

#[test]
fn repaint_invalidates_the_diff_and_is_consumed_once() {
    let mut app = app();
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let draw = |app: &App, term: &mut Terminal<TestBackend>| {
        term.draw(|f| f.render_widget(WorkspaceWidget::new(&app.wm), f.area())).unwrap();
    };
    draw(&app, &mut term);
    let good = term.backend().buffer().clone();

    // The host screen now disagrees with the model; an unchanged frame cannot fix it (diff is empty).
    let sentinel = |term: &mut Terminal<TestBackend>| {
        // Written behind ratatui's back, like a host terminal that drew a wide glyph at another width.
        let cell = ratatui::buffer::Cell::new("#");
        let cells: Vec<(u16, u16, &ratatui::buffer::Cell)> = (0..40u16).map(|x| (x, 5, &cell)).collect();
        term.backend_mut().draw(cells.into_iter()).unwrap();
    };
    sentinel(&mut term);
    assert!(!repaint_if_requested(&mut app, &mut term), "no request pending");
    draw(&app, &mut term);
    assert_eq!(term.backend().buffer()[(0, 5)].symbol(), "#", "diff alone leaves stale cells");

    app.wm.apply(Action::Repaint).unwrap();
    assert!(repaint_if_requested(&mut app, &mut term), "request consumed");
    assert!(app.needs_redraw);
    draw(&app, &mut term);
    assert_eq!(term.backend().buffer(), &good, "full repaint restores every cell");
    assert!(!repaint_if_requested(&mut app, &mut term), "flag is one-shot");
}
