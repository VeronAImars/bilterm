//! T-A4 tests: wire grid losslessness, per-client diffing, state machine, size bounds. All
//! deterministic (xorshift PRNG), no sockets, no PTYs.

use std::collections::BTreeMap;
use std::time::Duration;

use super::diff::PaneFrame;
use super::frame::decode_one;
use super::layout::{WireLayout, WirePaneInfo};
use super::limits::*;
use super::msg::{HostMsg, Role};
use super::session::{AckError, ClientSession, HostFrames, SessionState};
use super::wire_grid::{WireDelta, WireGrid};
use crate::vt::snapshot::*;
use crate::vt::VtParser;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random but terminal-plausible byte soup: text, SGR, cursor moves, wide chars, alt screen.
fn soup(rng: &mut Rng, rows: u16, cols: u16) -> Vec<u8> {
    let mut s = String::new();
    for _ in 0..(1 + rng.below(40)) {
        match rng.below(12) {
            0 => s.push_str(&format!("\x1b[{};{}H", 1 + rng.below(rows as u64), 1 + rng.below(cols as u64))),
            1 => s.push_str(&format!("\x1b[{}m", [0, 1, 3, 4, 7, 31, 42, 91][rng.below(8) as usize])),
            2 => s.push_str(&format!("\x1b[38;2;{};{};{}m", rng.below(256), rng.below(256), rng.below(256))),
            3 => s.push_str(&format!("\x1b[38;5;{}m", rng.below(256))),
            4 => s.push_str("\u{4e2d}\u{6587}e\u{301}"),
            5 => s.push_str(if rng.below(2) == 0 { "\x1b[?1049h" } else { "\x1b[?1049l" }),
            6 => s.push_str("\r\n"),
            7 => s.push_str("\x1b[2J"),
            8 => s.push_str(["\x1b#6", "\x1b#3", "\x1b#5"][rng.below(3) as usize]),
            _ => {
                for _ in 0..rng.below(30) {
                    s.push((b'a' + rng.below(26) as u8) as char);
                }
            }
        }
    }
    s.into_bytes()
}

fn frames_of(parsers: &BTreeMap<usize, VtParser>) -> HostFrames {
    let mut layout = WireLayout::default();
    let mut panes = BTreeMap::new();
    for (&id, p) in parsers {
        let (rows, cols) = p.size();
        layout.panes.push(WirePaneInfo { id, title: format!("p{id}"), rows, cols, read_only: false });
        layout.z_order.push(id);
        panes.insert(id, PaneFrame::new(p.snapshot()));
    }
    layout.active = *parsers.keys().next().unwrap_or(&0);
    HostFrames { layout, panes }
}

/// Client-side mirror: applies host messages exactly as a remote renderer would.
#[derive(Default)]
struct Mirror {
    layout: Option<WireLayout>,
    grids: BTreeMap<usize, (WireGrid, u64)>,
    last_seq: BTreeMap<usize, u64>,
}

impl Mirror {
    fn apply(&mut self, m: &HostMsg) {
        match m {
            HostMsg::Layout { body } => self.layout = Some(WireLayout::from_body(body).unwrap()),
            HostMsg::Snapshot { pane, seq, generation, body } => {
                self.grids.insert(*pane, (WireGrid::from_body(body).unwrap(), *generation));
                self.last_seq.insert(*pane, *seq);
            }
            HostMsg::Delta { pane, base_gen, generation, seq, body } => {
                let (g, cur) = self.grids.get_mut(pane).expect("delta before snapshot");
                assert_eq!(*base_gen, *cur, "delta base_gen must be what the client holds");
                g.apply_delta(&WireDelta::from_body(body).unwrap()).unwrap();
                *cur = *generation;
                self.last_seq.insert(*pane, *seq);
            }
            _ => {}
        }
        // Every message must survive the real codec.
        let bytes = m.encode().unwrap();
        let (frame, _) = decode_one(&bytes, MAX_ATTACH_FRAME_BYTES).unwrap().unwrap();
        assert_eq!(&HostMsg::decode(&frame).unwrap(), m);
    }

    fn assert_matches(&self, parsers: &BTreeMap<usize, VtParser>) {
        for (id, p) in parsers {
            let host = p.snapshot();
            let (g, cur) = &self.grids[id];
            let mut got = g.to_snapshot(*cur).unwrap();
            let mut want = host.clone();
            for r in got.grid_rows.iter_mut().chain(want.grid_rows.iter_mut()) {
                r.dirty = true;
            }
            got.generation = want.generation;
            assert_eq!(got, want, "pane {id} diverged");
        }
    }
}

const T0: Duration = Duration::ZERO;

#[test]
fn wire_grid_roundtrip_is_lossless() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for _ in 0..60 {
        let (rows, cols) = (2 + rng.below(10) as u16, 3 + rng.below(30) as u16);
        let mut p = VtParser::new(rows, cols, 100);
        for _ in 0..5 {
            p.process(&soup(&mut rng, rows, cols));
        }
        let snap = p.snapshot();
        let wire = WireGrid::from(&snap);
        let json = serde_json::to_vec(&wire).unwrap();
        let back: WireGrid = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, wire);
        assert_eq!(back.to_snapshot(snap.generation).unwrap(), snap);
    }
}

#[test]
fn wire_grid_rejects_hostile_shapes() {
    let snap = GridSnapshot::empty(3, 4);
    let mut w = WireGrid::from(&snap);
    w.rows = 0;
    assert!(w.to_snapshot(0).is_err());
    let mut w = WireGrid::from(&snap);
    w.grid.pop();
    assert!(w.to_snapshot(0).is_err());
    let mut w = WireGrid::from(&snap);
    w.grid[0].r[0].n = 9; // wider than cols
    assert!(w.to_snapshot(0).is_err());
    let mut w = WireGrid::from(&snap);
    w.grid[0].r[0].text = Some(super::wire_grid::RunText::Simple("ab".into())); // text/count mismatch
    assert!(w.to_snapshot(0).is_err());
    let mut w = WireGrid::from(&snap);
    w.cols = u16::MAX; // would allocate: dims rejected first
    assert!(w.to_snapshot(0).is_err());
    let mut ok = WireGrid::from(&snap);
    let bad = WireDelta { rows: vec![(99, ok.grid[0].clone())], ..Default::default() };
    assert!(ok.apply_delta(&bad).is_err());
}

#[test]
fn snapshot_then_deltas_converge_for_multiple_independent_clients() {
    let mut rng = Rng(42);
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(8, 30, 100));
    parsers.insert(2, VtParser::new(5, 20, 100));
    let mut fast = (ClientSession::new(Role::Rw, None), Mirror::default());
    let mut slow = (ClientSession::new(Role::Ro, None), Mirror::default());
    let mut only2 = (ClientSession::new(Role::Rw, Some(&[2])), Mirror::default());
    for step in 0..120u32 {
        for p in parsers.values_mut() {
            if rng.below(3) != 0 {
                let (r, c) = p.size();
                p.process(&soup(&mut rng, r, c));
            }
        }
        let frames = frames_of(&parsers);
        let now = Duration::from_millis(u64::from(step) * 25);
        let out = fast.0.pump(&frames, now);
        for m in &out.msgs {
            fast.1.apply(m);
            if let HostMsg::Snapshot { pane, seq, .. } | HostMsg::Delta { pane, seq, .. } = m {
                fast.0.on_ack(*pane, *seq).unwrap();
            }
        }
        fast.1.assert_matches(&parsers);
        // Slow client pumps only every 7th tick (coalescing: sees fewer, larger deltas).
        if step % 7 == 0 {
            for m in &slow.0.pump(&frames, now).msgs {
                slow.1.apply(m);
                if let HostMsg::Snapshot { pane, seq, .. } | HostMsg::Delta { pane, seq, .. } = m {
                    slow.0.on_ack(*pane, *seq).unwrap();
                }
            }
            slow.1.assert_matches(&parsers);
        }
        for m in &only2.0.pump(&frames, now).msgs {
            only2.1.apply(m);
            if let HostMsg::Snapshot { pane, seq, .. } | HostMsg::Delta { pane, seq, .. } = m {
                only2.0.on_ack(*pane, *seq).unwrap();
            }
        }
    }
    assert!(only2.1.grids.contains_key(&2) && !only2.1.grids.contains_key(&1));
    assert_eq!(only2.1.layout.as_ref().unwrap().panes.len(), 1);
    assert_eq!(fast.1.layout.as_ref().unwrap().panes.len(), 2);
}

#[test]
fn unchanged_state_sends_nothing_and_alt_resize_work() {
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(6, 20, 100));
    let mut s = ClientSession::new(Role::Rw, None);
    let mut mirror = Mirror::default();
    let first = s.pump(&frames_of(&parsers), T0);
    assert_eq!(first.msgs.len(), 2); // Layout + Snapshot
    first.msgs.iter().for_each(|m| mirror.apply(m));
    s.on_ack(1, 1).unwrap();
    assert!(s.pump(&frames_of(&parsers), T0).msgs.is_empty());
    // Alt screen toggle then a size change (dims change must produce a Snapshot, not a Delta).
    parsers.get_mut(&1).unwrap().process(b"\x1b[?1049hhi");
    let out = s.pump(&frames_of(&parsers), T0);
    assert!(matches!(out.msgs.as_slice(), [HostMsg::Delta { .. }]));
    out.msgs.iter().for_each(|m| mirror.apply(m));
    mirror.assert_matches(&parsers);
    s.on_ack(1, 2).unwrap();
    parsers.insert(1, VtParser::new(7, 25, 100)); // resized pane
    let out = s.pump(&frames_of(&parsers), T0);
    assert!(out.msgs.iter().any(|m| matches!(m, HostMsg::Snapshot { .. })));
    out.msgs.iter().for_each(|m| mirror.apply(m));
    mirror.assert_matches(&parsers);
}

#[test]
fn flow_control_blocks_then_resyncs_then_kicks() {
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(4, 20, 100));
    let mut s = ClientSession::new(Role::Rw, None);
    let mut sent = 0;
    for i in 0..10 {
        parsers.get_mut(&1).unwrap().process(format!("line{i}\r\n").as_bytes());
        sent += s.pump(&frames_of(&parsers), T0).msgs.iter().filter(|m| matches!(m, HostMsg::Snapshot { .. } | HostMsg::Delta { .. })).count();
    }
    assert_eq!(sent, MAX_UNACKED_FRAMES, "window caps in-flight frames; nothing queued");
    assert_eq!(s.inflight(1), MAX_UNACKED_FRAMES as u64);
    let bytes_blocked = s.state_bytes();
    // Past LAG_DEADLINE: forced to resync (needs-snapshot), reported once.
    let p = s.pump(&frames_of(&parsers), LAG_DEADLINE + Duration::from_millis(1));
    assert!(p.msgs.is_empty() && !p.kick);
    let p = s.pump(&frames_of(&parsers), LAG_DEADLINE + Duration::from_secs(1));
    assert!(p.resynced.is_empty(), "resync is reported once");
    // Past KICK_DEADLINE: kick.
    assert!(s.pump(&frames_of(&parsers), KICK_DEADLINE + Duration::from_secs(1)).kick);
    // Ack frees the window; the next frame is a Snapshot (resync), not a Delta.
    s.on_ack(1, 4).unwrap();
    let out = s.pump(&frames_of(&parsers), KICK_DEADLINE + Duration::from_secs(2));
    assert!(matches!(out.msgs.as_slice(), [HostMsg::Snapshot { .. }]));
    assert!(s.state_bytes() <= bytes_blocked + 8 * 4, "state stays O(rows)");
}

#[test]
fn resync_reported_on_the_pump_that_crosses_lag_deadline() {
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(4, 20, 100));
    let mut s = ClientSession::new(Role::Rw, None);
    for i in 0..6 {
        parsers.get_mut(&1).unwrap().process(format!("x{i}\r\n").as_bytes());
        s.pump(&frames_of(&parsers), T0);
    }
    assert!(s.pump(&frames_of(&parsers), Duration::from_secs(1)).resynced.is_empty());
    // blocked_since was set at the first blocked pump (T0): +LAG_DEADLINE crosses.
    assert_eq!(s.pump(&frames_of(&parsers), LAG_DEADLINE).resynced, vec![1]);
}

#[test]
fn ack_validation_and_detach() {
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(3, 10, 10));
    let mut s = ClientSession::new(Role::Rw, None);
    assert_eq!(s.on_ack(1, 1), Err(AckError::UnknownPane));
    s.pump(&frames_of(&parsers), T0);
    assert_eq!(s.on_ack(1, 99), Err(AckError::FromFuture));
    assert_eq!(s.on_ack(1, 1), Ok(()));
    assert_eq!(s.on_ack(1, 0), Ok(())); // stale ack is harmless
    s.detach();
    assert_eq!(s.state(), SessionState::Detached);
    assert!(s.pump(&frames_of(&parsers), T0).msgs.is_empty());
    assert_eq!(s.state_bytes(), 0);
}

#[test]
fn removed_panes_drop_state_and_state_is_independent_of_throughput() {
    let mut parsers = BTreeMap::new();
    parsers.insert(1, VtParser::new(10, 40, 100));
    parsers.insert(2, VtParser::new(10, 40, 100));
    let mut s = ClientSession::new(Role::Rw, None);
    s.pump(&frames_of(&parsers), T0);
    s.on_ack(1, 1).unwrap();
    s.on_ack(2, 1).unwrap();
    let base = s.state_bytes();
    let mut rng = Rng(7);
    for _ in 0..500 {
        parsers.get_mut(&1).unwrap().process(&soup(&mut rng, 10, 40));
        for m in s.pump(&frames_of(&parsers), T0).msgs {
            if let HostMsg::Snapshot { pane, seq, .. } | HostMsg::Delta { pane, seq, .. } = m {
                s.on_ack(pane, seq).unwrap();
            }
        }
        if rng.below(10) == 0 {
            s.request_resync(1);
        }
    }
    for m in s.pump(&frames_of(&parsers), T0).msgs {
        if let HostMsg::Snapshot { pane, seq, .. } | HostMsg::Delta { pane, seq, .. } = m {
            s.on_ack(pane, seq).unwrap();
        }
    }
    assert_eq!(s.state_bytes(), base, "per-client state does not grow with output volume");
    parsers.remove(&2);
    s.pump(&frames_of(&parsers), T0);
    assert!(s.state_bytes() < base, "removed pane state is dropped");
}

fn worst_case_grid(rows: u16, cols: u16, rng: &mut Rng) -> GridSnapshot {
    let mut g = GridSnapshot::empty(rows, cols);
    for row in &mut g.grid_rows {
        for c in &mut row.cells {
            c.grapheme = ((b'!' + rng.below(90) as u8) as char).to_string();
            c.fg = CellColor::Rgb(rng.next() as u8, rng.next() as u8, rng.next() as u8);
            c.bg = CellColor::Rgb(rng.next() as u8, rng.next() as u8, rng.next() as u8);
            c.bold = true;
        }
    }
    g
}

#[test]
fn snapshot_sizes_fit_under_the_4mib_cap_at_500x200() {
    let msg = |g: &GridSnapshot| HostMsg::Snapshot { pane: 1, seq: 1, generation: 1, body: WireGrid::from(g).to_body() };
    // Blank grid: tiny.
    let blank = msg(&GridSnapshot::empty(MAX_ATTACH_ROWS, MAX_ATTACH_COLS)).encode().unwrap();
    assert!(blank.len() < 128 * 1024, "blank 500x200 = {}", blank.len());
    // Worst case: random RGB fg+bg and bold on every cell (every cell its own run).
    let mut rng = Rng(1);
    let worst = worst_case_grid(MAX_ATTACH_ROWS, MAX_ATTACH_COLS, &mut rng);
    let bytes = msg(&worst).encode().expect("worst-case 500x200 snapshot must fit MAX_ATTACH_FRAME_BYTES");
    eprintln!("sizes: blank={} worst={}", blank.len(), bytes.len());
    assert!(bytes.len() < MAX_ATTACH_FRAME_BYTES, "worst 500x200 = {}", bytes.len());
    // Wide/combining cells take the lossless `c` path; still bounded.
    let mut wide = worst_case_grid(MAX_ATTACH_ROWS, MAX_ATTACH_COLS, &mut rng);
    for row in &mut wide.grid_rows {
        for (i, c) in row.cells.iter_mut().enumerate() {
            c.grapheme = if i % 2 == 0 { "\u{4e2d}".into() } else { String::new() };
            c.width = if i % 2 == 0 { 2 } else { 0 };
        }
    }
    let bytes = msg(&wide).encode().expect("wide-char 500x200 snapshot must fit");
    assert!(bytes.len() < MAX_ATTACH_FRAME_BYTES, "wide 500x200 = {}", bytes.len());
    // And the worst case round-trips.
    let (frame, _) = decode_one(&msg(&worst).encode().unwrap(), MAX_ATTACH_FRAME_BYTES).unwrap().unwrap();
    let HostMsg::Snapshot { body, generation, .. } = HostMsg::decode(&frame).unwrap() else { panic!() };
    assert_eq!(WireGrid::from_body(&body).unwrap().to_snapshot(generation).unwrap(), {
        let mut w = worst;
        w.generation = generation;
        w
    });
}

#[test]
fn oversized_pane_is_never_exported_to_a_client() {
    let mut frames = HostFrames::default();
    frames.layout.active = 1;
    frames.panes.insert(1, PaneFrame::new(GridSnapshot::empty(MAX_ATTACH_ROWS + 1, 10)));
    let mut s = ClientSession::new(Role::Rw, None);
    let out = s.pump(&frames, T0);
    assert!(out.msgs.iter().all(|m| !matches!(m, HostMsg::Snapshot { .. })));
    assert_eq!(s.state_bytes(), 0);
}
