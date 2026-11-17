//! Wire mirror of `GridSnapshot` (plan section 7, T-A4): row-run-length JSON, lossless.
//!
//! `vt/snapshot.rs` stays renderer-facing; this module owns every wire concern. Per-row hashes
//! (`row_hash`) are the diff key (`RowSnapshot.dirty` is refuted for multi-client use, plan
//! section 12 #1) and deliberately ignore `dirty`.
//!
//! Encoding per cell run: see [`WireRun`] (compact positional array; blank runs carry no text).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::limits::validate_dims;
use crate::vt::snapshot::{
    CellColor, CellSnapshot, CursorShape, CursorSnapshot, GridSnapshot, LineSizeAttr, RgbTriplet, RowSnapshot,
    UnderlineKind,
};

/// Why a wire grid/delta was rejected (never panics on hostile input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    BadDims,
    RowCount,
    RowWidth,
    RowIndex,
    RunText,
    Json(String),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid wire grid: {self:?}")
    }
}

impl std::error::Error for WireError {}

const F_BOLD: u8 = 1;
const F_DIM: u8 = 2;
const F_ITALIC: u8 = 4;
const F_OVERLINE: u8 = 8;
const F_INVERSE: u8 = 16;
const F_STRIKE: u8 = 32;
const RGB_FLAG: u32 = 1 << 24;

fn pack_color(c: CellColor) -> Option<u32> {
    match c {
        CellColor::Default => None,
        CellColor::Indexed(i) => Some(u32::from(i)),
        CellColor::Rgb(r, g, b) => Some(RGB_FLAG | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)),
    }
}

fn unpack_color(v: Option<u32>) -> CellColor {
    match v {
        None => CellColor::Default,
        Some(v) if v & RGB_FLAG != 0 => CellColor::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
        Some(v) => CellColor::Indexed(v as u8),
    }
}

fn underline_to_u8(u: UnderlineKind) -> u8 {
    match u {
        UnderlineKind::None => 0,
        UnderlineKind::Single => 1,
        UnderlineKind::Double => 2,
        UnderlineKind::Curly => 3,
        UnderlineKind::Dotted => 4,
        UnderlineKind::Dashed => 5,
    }
}

fn underline_from_u8(v: u8) -> UnderlineKind {
    match v {
        1 => UnderlineKind::Single,
        2 => UnderlineKind::Double,
        3 => UnderlineKind::Curly,
        4 => UnderlineKind::Dotted,
        5 => UnderlineKind::Dashed,
        _ => UnderlineKind::None,
    }
}

fn shape_to_u8(s: CursorShape) -> u8 {
    match s {
        CursorShape::Block => 0,
        CursorShape::HollowBlock => 1,
        CursorShape::Underline => 2,
        CursorShape::Bar => 3,
    }
}

fn shape_from_u8(v: u8) -> CursorShape {
    match v {
        1 => CursorShape::HollowBlock,
        2 => CursorShape::Underline,
        3 => CursorShape::Bar,
        _ => CursorShape::Block,
    }
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn is_single(v: &LineSizeAttr) -> bool {
    *v == LineSizeAttr::Single
}

/// Cell attributes shared by a run. `None`/0 are defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WireStyle {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    /// Underline RGB (`SGR 58`), packed `r<<16|g<<8|b`.
    pub uc: Option<u32>,
    pub ul: u8,
    pub f: u8,
}

impl WireStyle {
    fn of(c: &CellSnapshot) -> Self {
        let mut f = 0;
        for (on, bit) in [
            (c.bold, F_BOLD),
            (c.dim, F_DIM),
            (c.italic, F_ITALIC),
            (c.overline, F_OVERLINE),
            (c.inverse, F_INVERSE),
            (c.strikethrough, F_STRIKE),
        ] {
            if on {
                f |= bit;
            }
        }
        Self {
            fg: pack_color(c.fg),
            bg: pack_color(c.bg),
            uc: c.underline_color.map(|t| u32::from(t.r) << 16 | u32::from(t.g) << 8 | u32::from(t.b)),
            ul: underline_to_u8(c.underline),
            f,
        }
    }
}

/// Text of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunText {
    /// One char per width-1 cell.
    Simple(String),
    /// Lossless fallback: `[grapheme, width]` per cell.
    Complex(Vec<(String, u8)>),
}

/// `n` consecutive cells sharing one style. Wire form is a compact positional array
/// `[n, text|null, fg, bg, x]` (`null` = `n` blank cells; `fg`/`bg`: 0 default, `i+1` indexed,
/// `1<<24|rgb`+1 truecolor; `x` = `flags | underline<<6 | (1<<9 | rgb<<10 if underline color)`),
/// sized so a worst-case 500x200 snapshot (every cell its own run) fits the 4 MiB cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireRun {
    pub n: u16,
    pub text: Option<RunText>,
    pub s: WireStyle,
}

type RunTuple = (u16, Option<RunText>, u32, u32, u64);

fn color_wire(v: Option<u32>) -> u32 {
    v.map_or(0, |v| v + 1)
}
fn color_unwire(v: u32) -> Option<u32> {
    v.checked_sub(1)
}

impl Serialize for WireRun {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let st = &self.s;
        let x = u64::from(st.f) | u64::from(st.ul) << 6 | st.uc.map_or(0, |u| 1u64 << 9 | u64::from(u) << 10);
        (self.n, &self.text, color_wire(st.fg), color_wire(st.bg), x).serialize(ser)
    }
}

impl<'de> Deserialize<'de> for WireRun {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let (n, text, fg, bg, x): RunTuple = Deserialize::deserialize(de)?;
        let uc = (x & (1 << 9) != 0).then_some(((x >> 10) & 0xff_ffff) as u32);
        Ok(Self {
            n,
            text,
            s: WireStyle { fg: color_unwire(fg), bg: color_unwire(bg), uc, ul: ((x >> 6) & 7) as u8, f: (x & 63) as u8 },
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRow {
    pub r: Vec<WireRun>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub w: bool,
    #[serde(default, skip_serializing_if = "is_single")]
    pub ls: LineSizeAttr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub blink: bool,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub shape: u8,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wide_tail: bool,
}

impl From<&CursorSnapshot> for WireCursor {
    fn from(c: &CursorSnapshot) -> Self {
        Self {
            row: c.row,
            col: c.col,
            visible: c.visible,
            blink: c.blinking,
            shape: shape_to_u8(c.shape),
            wide_tail: c.at_wide_tail,
        }
    }
}

impl From<WireCursor> for CursorSnapshot {
    fn from(c: WireCursor) -> Self {
        Self {
            row: c.row,
            col: c.col,
            visible: c.visible,
            blinking: c.blink,
            shape: shape_from_u8(c.shape),
            at_wide_tail: c.wide_tail,
        }
    }
}

impl WireRow {
    pub fn from_row(row: &RowSnapshot) -> Self {
        let mut runs: Vec<WireRun> = Vec::new();
        let mut cur: Vec<&CellSnapshot> = Vec::new();
        let mut cur_style = WireStyle::default();
        for cell in &row.cells {
            let st = WireStyle::of(cell);
            if !cur.is_empty() && (st != cur_style || cur.len() >= usize::from(u16::MAX)) {
                runs.push(finish_run(&cur, cur_style));
                cur.clear();
            }
            cur_style = st;
            cur.push(cell);
        }
        if !cur.is_empty() {
            runs.push(finish_run(&cur, cur_style));
        }
        Self { r: runs, w: row.wrapped, ls: row.line_size }
    }

    /// Rebuilds the cells; `cols` is the expected width.
    pub fn to_row(&self, cols: u16) -> Result<RowSnapshot, WireError> {
        let mut cells = Vec::with_capacity(usize::from(cols));
        for run in &self.r {
            let n = usize::from(run.n);
            if cells.len() + n > usize::from(cols) {
                return Err(WireError::RowWidth);
            }
            let st = run.s;
            let mk = |grapheme: String, width: u8| CellSnapshot {
                grapheme,
                width,
                fg: unpack_color(st.fg),
                bg: unpack_color(st.bg),
                underline_color: st.uc.map(|v| RgbTriplet::new((v >> 16) as u8, (v >> 8) as u8, v as u8)),
                bold: st.f & F_BOLD != 0,
                dim: st.f & F_DIM != 0,
                italic: st.f & F_ITALIC != 0,
                underline: underline_from_u8(st.ul),
                overline: st.f & F_OVERLINE != 0,
                inverse: st.f & F_INVERSE != 0,
                strikethrough: st.f & F_STRIKE != 0,
            };
            match &run.text {
                Some(RunText::Complex(c)) => {
                    if c.len() != n {
                        return Err(WireError::RunText);
                    }
                    cells.extend(c.iter().map(|(g, w)| mk(g.clone(), *w)));
                }
                Some(RunText::Simple(t)) => {
                    let mut count = 0;
                    for ch in t.chars() {
                        cells.push(mk(ch.to_string(), 1));
                        count += 1;
                    }
                    if count != n {
                        return Err(WireError::RunText);
                    }
                }
                None => cells.extend((0..n).map(|_| mk(" ".to_string(), 1))),
            }
        }
        if cells.len() != usize::from(cols) {
            return Err(WireError::RowWidth);
        }
        Ok(RowSnapshot { cells, dirty: true, wrapped: self.w, line_size: self.ls })
    }
}

fn finish_run(cells: &[&CellSnapshot], style: WireStyle) -> WireRun {
    let n = cells.len() as u16;
    let blank = cells.iter().all(|c| c.width == 1 && c.grapheme == " ");
    if blank {
        return WireRun { n, text: None, s: style };
    }
    let simple = cells.iter().all(|c| c.width == 1 && c.grapheme.chars().count() == 1);
    if simple {
        let t: String = cells.iter().map(|c| c.grapheme.as_str()).collect();
        return WireRun { n, text: Some(RunText::Simple(t)), s: style };
    }
    WireRun { n, text: Some(RunText::Complex(cells.iter().map(|c| (c.grapheme.clone(), c.width)).collect())), s: style }
}

/// Full-state grid body of a `Snapshot` message (generation travels in the message header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGrid {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<WireRow>,
    pub cursor: WireCursor,
    #[serde(default, skip_serializing_if = "is_false")]
    pub alt: bool,
    pub sb_off: usize,
    pub sb_total: usize,
}

impl From<&GridSnapshot> for WireGrid {
    fn from(g: &GridSnapshot) -> Self {
        Self {
            rows: g.rows,
            cols: g.cols,
            grid: g.grid_rows.iter().map(WireRow::from_row).collect(),
            cursor: WireCursor::from(&g.cursor),
            alt: g.alternate_screen,
            sb_off: g.scrollback_offset,
            sb_total: g.total_scrollback_rows,
        }
    }
}

impl WireGrid {
    /// Client side: rebuilds a `GridSnapshot` (dims validated before anything is allocated).
    pub fn to_snapshot(&self, generation: u64) -> Result<GridSnapshot, WireError> {
        validate_dims(self.cols, self.rows).map_err(|_| WireError::BadDims)?;
        if self.grid.len() != usize::from(self.rows) {
            return Err(WireError::RowCount);
        }
        let grid_rows = self.grid.iter().map(|r| r.to_row(self.cols)).collect::<Result<Vec<_>, _>>()?;
        Ok(GridSnapshot {
            rows: self.rows,
            cols: self.cols,
            grid_rows,
            cursor: self.cursor.into(),
            alternate_screen: self.alt,
            scrollback_offset: self.sb_off,
            total_scrollback_rows: self.sb_total,
            generation,
        })
    }

    /// Client side: applies a delta in place (same dims only; dims changes arrive as `Snapshot`).
    pub fn apply_delta(&mut self, d: &WireDelta) -> Result<(), WireError> {
        for (y, row) in &d.rows {
            if usize::from(*y) >= self.grid.len() {
                return Err(WireError::RowIndex);
            }
            // Validate width before committing.
            row.to_row(self.cols)?;
        }
        for (y, row) in &d.rows {
            self.grid[usize::from(*y)] = row.clone();
        }
        if let Some(c) = d.cursor {
            self.cursor = c;
        }
        if let Some(a) = d.alt {
            self.alt = a;
        }
        if let Some((o, t)) = d.sb {
            self.sb_off = o;
            self.sb_total = t;
        }
        Ok(())
    }

    /// `Snapshot.body` map (everything but `pane`/`seq`/`gen`).
    pub fn to_body(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    pub fn from_body(body: &Map<String, Value>) -> Result<Self, WireError> {
        serde_json::from_value(Value::Object(body.clone())).map_err(|e| WireError::Json(e.to_string()))
    }
}

/// Row-level diff body of a `Delta` message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDelta {
    /// `(y, row)` for each changed row, ascending `y`.
    pub rows: Vec<(u16, WireRow)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<WireCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sb: Option<(usize, usize)>,
}

impl WireDelta {
    pub fn to_body(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    pub fn from_body(body: &Map<String, Value>) -> Result<Self, WireError> {
        serde_json::from_value(Value::Object(body.clone())).map_err(|e| WireError::Json(e.to_string()))
    }
}

fn hash_color<H: Hasher>(h: &mut H, c: CellColor) {
    match c {
        CellColor::Default => 0u8.hash(h),
        CellColor::Indexed(i) => (1u8, i).hash(h),
        CellColor::Rgb(r, g, b) => (2u8, r, g, b).hash(h),
    }
}

/// Deterministic 64-bit content hash of everything a row puts on the wire (ignores `dirty`).
/// `DefaultHasher::new()` uses fixed keys, so equal rows hash equal across clients and ticks.
pub fn row_hash(row: &RowSnapshot) -> u64 {
    let mut h = DefaultHasher::new();
    row.wrapped.hash(&mut h);
    (row.line_size as u8).hash(&mut h);
    row.cells.len().hash(&mut h);
    for c in &row.cells {
        c.grapheme.hash(&mut h);
        c.width.hash(&mut h);
        hash_color(&mut h, c.fg);
        hash_color(&mut h, c.bg);
        c.underline_color.map(|t| (t.r, t.g, t.b)).hash(&mut h);
        (c.bold, c.dim, c.italic, c.overline, c.inverse, c.strikethrough).hash(&mut h);
        underline_to_u8(c.underline).hash(&mut h);
    }
    h.finish()
}
