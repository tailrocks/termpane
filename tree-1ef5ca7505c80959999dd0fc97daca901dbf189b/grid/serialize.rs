// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Replayable serialization of the grid state to terminal byte streams.
//!
//! Ported from the vt100 0.16.2 serializer (the tui-snap fork's
//! `grid.rs`/`row.rs`/`screen.rs` writers), adapted to the `DamageGrid`
//! model: deferred pending-wrap cursor, BCE erase blanks, and the full SGR
//! attribute set (independent bold/dim, slow *and* rapid blink, conceal,
//! strikethrough, underline styles and colors).
//!
//! # The replay contract
//!
//! Replaying [`DamageGrid::state_formatted`] into a fresh grid of the same
//! dimensions reproduces the state exactly, where "state" is:
//!
//! - visible cells: grapheme contents, `is_wide`/`is_wide_continuation`
//!   flags, and the full SGR attribute set (colors, bold/dim, italic,
//!   underline style/color, inverse, blink, conceal, strikethrough, overline);
//! - the cursor position, including a pending-wrap phantom column
//!   (`col == cols`);
//! - the current SGR pen ([`DamageGrid::current_attrs`]);
//! - DECAWM (`?7`), application keypad, application cursor keys, bracketed
//!   paste, mouse protocol mode/encoding, alternate screen (`?1049`), cursor
//!   visibility (`?25`), and the DECSCUSR cursor style.
//!
//! Deliberately outside the contract (matching upstream vt100): scrollback,
//! the scroll region, the saved cursor, row wrap provenance flags, OSC 8
//! hyperlink metadata, DEC 12 / DEC 2026 / focus-event state, and damage or
//! passthrough buffers. [`DamageGrid::contents_formatted`] reproduces only the
//! *visible* picture (cells, cursor, pen, DECAWM, cursor visibility); use the
//! `state_*` pair for full reproduction. The diff variants additionally assume
//! the destination parser is in the state described by `prev` — including its
//! dimensions and (for [`DamageGrid::contents_diff`]) its scroll region.
//!
//! # DECAWM sandwich
//!
//! Row painting relies on pending-wrap semantics, so every contents writer
//! prepends `\x1b[?7h` before painting and appends `\x1b[?7l` at the end when
//! the grid's own DECAWM is off. The input-mode writers emit the `?7` state
//! itself.
#[cfg_attr(
    not(test),
    expect(clippy::wildcard_imports, reason = "target-dependent")
)]
use super::*;

/// Cursor position during painting. `col` may be `== cols` (the pending-wrap
/// phantom column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pos {
    row: u16,
    col: u16,
}

// ── Terminal byte writers ────────────────────────────────────────────────────

fn write_move_to(out: &mut Vec<u8>, pos: Pos) {
    if pos.row == 0 && pos.col == 0 {
        out.extend_from_slice(b"\x1b[H");
    } else {
        out.extend_from_slice(b"\x1b[");
        write_u16(out, pos.row + 1);
        out.push(b';');
        write_u16(out, pos.col + 1);
        out.push(b'H');
    }
}

fn write_move_from_to(out: &mut Vec<u8>, from: Pos, to: Pos) {
    if to.row == from.row + 1 && to.col == 0 {
        out.extend_from_slice(b"\r\n");
    } else if from.row == to.row && from.col < to.col {
        let n = to.col - from.col;
        match n {
            // A zero-width move emits nothing.
            0 => {}
            1 => out.extend_from_slice(b"\x1b[C"),
            _ => {
                out.extend_from_slice(b"\x1b[");
                write_u16(out, n);
                out.push(b'C');
            }
        }
    } else if to != from {
        write_move_to(out, to);
    }
}

fn write_erase_chars(out: &mut Vec<u8>, n: u16) {
    match n {
        0 => {}
        1 => out.extend_from_slice(b"\x1b[X"),
        _ => {
            out.extend_from_slice(b"\x1b[");
            write_u16(out, n);
            out.push(b'X');
        }
    }
}

fn write_u16(out: &mut Vec<u8>, n: u16) {
    let mut buf = [0u8; 5];
    let mut i = buf.len();
    let mut n = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

/// A single SGR parameter run; subparameters (styled underlines) use `:`.
struct SgrBuf {
    params: Vec<u8>,
    first: bool,
}

impl SgrBuf {
    fn new() -> Self {
        Self {
            params: Vec::new(),
            first: true,
        }
    }

    fn param(&mut self, n: u16) {
        if self.first {
            self.first = false;
        } else {
            self.params.push(b';');
        }
        write_u16(&mut self.params, n);
    }

    fn raw(&mut self, bytes: &[u8]) {
        if self.first {
            self.first = false;
        } else {
            self.params.push(b';');
        }
        self.params.extend_from_slice(bytes);
    }
}

/// Emit the SGR sequence that turns `prev` into `target`.
///
/// Bold and dim are independent bits, so the intensity group follows the
/// vt100-fork rule: clearing either bit requires SGR 22 (which clears both)
/// followed by re-asserting the surviving bit — Normal writes `22`, Bold
/// writes `22;1`, Dim writes `22;2`, and `BoldDim` writes `1;2` (no leading
/// 22). Slow and rapid blink are likewise distinct (`5`/`6`, cleared by `25`
/// when either was on).
fn write_attrs_diff(out: &mut Vec<u8>, target: &Attrs, prev: &Attrs) {
    if target == prev {
        return;
    }
    if target == &Attrs::default() {
        out.extend_from_slice(b"\x1b[m");
        return;
    }
    let mut sgr = SgrBuf::new();
    if target.foreground != prev.foreground {
        match target.foreground {
            Color::Default => sgr.param(39),
            Color::Idx(i) if i < 8 => sgr.param(u16::from(i) + 30),
            Color::Idx(i) if i < 16 => sgr.param(u16::from(i) + 82),
            Color::Idx(i) => {
                sgr.param(38);
                sgr.param(5);
                sgr.param(u16::from(i));
            }
            Color::Rgb(r, g, b) => {
                sgr.param(38);
                sgr.param(2);
                sgr.param(u16::from(r));
                sgr.param(u16::from(g));
                sgr.param(u16::from(b));
            }
        }
    }
    if target.background != prev.background {
        match target.background {
            Color::Default => sgr.param(49),
            Color::Idx(i) if i < 8 => sgr.param(u16::from(i) + 40),
            Color::Idx(i) if i < 16 => sgr.param(u16::from(i) + 92),
            Color::Idx(i) => {
                sgr.param(48);
                sgr.param(5);
                sgr.param(u16::from(i));
            }
            Color::Rgb(r, g, b) => {
                sgr.param(48);
                sgr.param(2);
                sgr.param(u16::from(r));
                sgr.param(u16::from(g));
                sgr.param(u16::from(b));
            }
        }
    }
    if target.bold != prev.bold || target.dim != prev.dim {
        match (target.bold, target.dim) {
            (false, false) => sgr.param(22),
            (true, false) => {
                sgr.param(22);
                sgr.param(1);
            }
            (false, true) => {
                sgr.param(22);
                sgr.param(2);
            }
            (true, true) => {
                sgr.param(1);
                sgr.param(2);
            }
        }
    }
    if target.italic != prev.italic {
        sgr.param(if target.italic { 3 } else { 23 });
    }
    if target.underline_style != prev.underline_style {
        match target.underline_style {
            UnderlineStyle::None => sgr.param(24),
            UnderlineStyle::Single => sgr.param(4),
            UnderlineStyle::Double => sgr.raw(b"4:2"),
            UnderlineStyle::Curly => sgr.raw(b"4:3"),
            UnderlineStyle::Dotted => sgr.raw(b"4:4"),
            UnderlineStyle::Dashed => sgr.raw(b"4:5"),
        }
    }
    if target.underline_color != prev.underline_color {
        match target.underline_color {
            Color::Default => sgr.param(59),
            Color::Idx(i) => {
                sgr.param(58);
                sgr.param(5);
                sgr.param(u16::from(i));
            }
            Color::Rgb(r, g, b) => {
                sgr.param(58);
                sgr.param(2);
                sgr.param(u16::from(r));
                sgr.param(u16::from(g));
                sgr.param(u16::from(b));
            }
        }
    }
    if target.inverse != prev.inverse {
        sgr.param(if target.inverse { 7 } else { 27 });
    }
    if target.strikethrough != prev.strikethrough {
        sgr.param(if target.strikethrough { 9 } else { 29 });
    }
    if target.slow_blink != prev.slow_blink || target.rapid_blink != prev.rapid_blink {
        if prev.slow_blink || prev.rapid_blink {
            sgr.param(25);
        }
        if target.slow_blink {
            sgr.param(5);
        }
        if target.rapid_blink {
            sgr.param(6);
        }
    }
    if target.conceal != prev.conceal {
        sgr.param(if target.conceal { 8 } else { 28 });
    }
    if target.overline != prev.overline {
        sgr.param(if target.overline { 53 } else { 55 });
    }
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(&sgr.params);
    out.push(b'm');
}

fn write_hide_cursor(out: &mut Vec<u8>, hidden: bool) {
    out.extend_from_slice(if hidden { b"\x1b[?25l" } else { b"\x1b[?25h" });
}

fn write_autowrap(out: &mut Vec<u8>, autowrap: bool) {
    out.extend_from_slice(if autowrap { b"\x1b[?7h" } else { b"\x1b[?7l" });
}

fn write_mouse_mode(out: &mut Vec<u8>, mode: MouseProtocolMode, prev: MouseProtocolMode) {
    let mode = mouse_mode_number(mode);
    let prev = mouse_mode_number(prev);
    if mode == prev {
        return;
    }
    if mode == 0 {
        out.extend_from_slice(b"\x1b[?");
        write_u16(out, prev);
        out.push(b'l');
    } else {
        out.extend_from_slice(b"\x1b[?");
        write_u16(out, mode);
        out.push(b'h');
    }
}

fn write_mouse_encoding(
    out: &mut Vec<u8>,
    encoding: MouseProtocolEncoding,
    prev: MouseProtocolEncoding,
) {
    let encoding = mouse_encoding_number(encoding);
    let prev = mouse_encoding_number(prev);
    if encoding == prev {
        return;
    }
    if encoding == 0 {
        out.extend_from_slice(b"\x1b[?");
        write_u16(out, prev);
        out.push(b'l');
    } else {
        out.extend_from_slice(b"\x1b[?");
        write_u16(out, encoding);
        out.push(b'h');
    }
}

fn write_cursor_style(out: &mut Vec<u8>, style: u16) {
    out.extend_from_slice(b"\x1b[");
    write_u16(out, style);
    out.extend_from_slice(b" q");
}

// ── Row painting ─────────────────────────────────────────────────────────────

/// Would a printable written at `col` of `row` join a ZWJ-final cluster to
/// its left? The serializer must break such joins with an explicit cursor
/// move (the join barrier only allows joining immediately after a print).
fn zwj_join_hazard(row: &[Cell], col: u16) -> bool {
    if col == 0 {
        return false;
    }
    let left = &row[usize::from(col - 1)];
    let cell = if left.is_wide_continuation && col >= 2 {
        &row[usize::from(col - 2)]
    } else {
        left
    };
    cell.contents().ends_with('\u{200d}')
}

/// Emit the cursor move that positions the replay cursor at `pos` before a
/// content cell write. Keeps the natural wrap (no move bytes) when the
/// previous row parked at the phantom column and this row continues the
/// logical line; an explicit CUP when the wrap write would join a ZWJ-final
/// cluster or when a cursor jog is needed to break the join barrier.
fn write_content_move(
    out: &mut Vec<u8>,
    pos: Pos,
    prev_pos: &mut Pos,
    cols: u16,
    wrapping: bool,
    prev_row: Option<&[Cell]>,
    cells: &[Cell],
) {
    if pos != *prev_pos {
        // Natural wrap: the previous row parked at the phantom column and this
        // row continues the line — the pending wrap carries the cursor here
        // with no move bytes. Only a true phantom qualifies: unlike vt100,
        // this grid never wraps a glyph written at a non-parked last column.
        let natural_wrap =
            wrapping && prev_pos.row + 1 == pos.row && prev_pos.col >= cols && pos.col == 0;
        if natural_wrap && !prev_row.is_some_and(zwj_wrap_hazard) {
            // No move: the pending wrap carries the cursor here.
        } else if natural_wrap {
            // The wrap write would join a ZWJ-final cluster: absolute move.
            write_move_to(out, pos);
        } else {
            write_move_from_to(out, *prev_pos, pos);
        }
        *prev_pos = pos;
    } else if zwj_join_hazard(cells, pos.col) {
        // Adjacent write after a ZWJ-final cell: jog the cursor to break the
        // cluster-continuation barrier check.
        write_move_to(out, pos);
    }
}

/// The ZWJ hazard for a natural wrap crossing: a printable consumed by the
/// pending wrap would join the previous row's last cell when that cell's
/// cluster ends with a ZWJ.
fn zwj_wrap_hazard(prev_row: &[Cell]) -> bool {
    let Some(last) = prev_row.last() else {
        return false;
    };
    let cell = if last.is_wide_continuation && prev_row.len() >= 2 {
        &prev_row[prev_row.len() - 2]
    } else {
        last
    };
    cell.contents().ends_with('\u{200d}')
}

/// Shared row-painting context: the row being serialized, its wrap
/// relationship with the previous row, and the previous row's cells (for
/// wrap-boundary ZWJ hazard checks).
struct RowCtx<'a> {
    cells: &'a [Cell],
    prev_row: Option<&'a [Cell]>,
    row: u16,
    cols: u16,
    wrapping: bool,
}

/// Flush a pending erase run starting at the recorded column: move to the run
/// start, switch to the run's (uniform blank) attributes, then erase — ECH up
/// to `end_col` mid-row, or EL (clear to row end) for the trailing run.
fn flush_erase_run(
    out: &mut Vec<u8>,
    erase: Option<(u16, Attrs)>,
    row: u16,
    end_col: Option<u16>,
    prev_pos: &mut Pos,
    prev_attrs: &mut Attrs,
) {
    let Some((start, attrs)) = erase else {
        return;
    };
    let new_pos = Pos { row, col: start };
    write_move_from_to(out, *prev_pos, new_pos);
    *prev_pos = new_pos;
    if *prev_attrs != attrs {
        write_attrs_diff(out, &attrs, prev_attrs);
        *prev_attrs = attrs;
    }
    match end_col {
        Some(end) => write_erase_chars(out, end - start),
        None => out.extend_from_slice(b"\x1b[K"),
    }
}

/// Paint one row's contents, vt100-fork style: blank-with-attributes runs are
/// erased with ECH/EL (BCE), content cells are written with SGR diffs, and
/// cursor moves are minimized (CRLF / move-right where possible), including
/// natural-wrap continuation when the previous row parked at the phantom
/// column.
///
/// Returns the cursor position and active SGR attributes after the row.
fn write_row_formatted(
    out: &mut Vec<u8>,
    ctx: &RowCtx<'_>,
    mut prev_pos: Pos,
    mut prev_attrs: Attrs,
) -> (Pos, Attrs) {
    let RowCtx {
        cells,
        prev_row,
        row: row_idx,
        cols,
        wrapping,
    } = *ctx;
    let default_cell = Cell::default();
    let mut prev_was_wide = false;

    let first_cell = &cells[0];
    if wrapping && *first_cell == default_cell {
        let parked_at_phantom = prev_pos.row + 1 == row_idx && prev_pos.col >= cols;
        let hazard = prev_row.is_some_and(zwj_wrap_hazard);
        if parked_at_phantom && !hazard {
            // Perform the pending wrap with a space and erase it, positioning
            // the cursor at the row start through the wrap (reproducing the
            // soft wrap on the replay side).
            if prev_attrs != Attrs::default() {
                write_attrs_diff(out, &Attrs::default(), &prev_attrs);
                prev_attrs = Attrs::default();
            }
            out.push(b' ');
            out.push(0x08);
            write_erase_chars(out, 1);
            prev_pos = Pos {
                row: row_idx,
                col: 0,
            };
        } else {
            // Not actually parked (the previous row's tail was erased after it
            // wrapped), or the wrap write would join a ZWJ-final cluster:
            // position explicitly. The cell is already default after the
            // clear-screen preamble.
            write_move_to(
                out,
                Pos {
                    row: row_idx,
                    col: 0,
                },
            );
            prev_pos = Pos {
                row: row_idx,
                col: 0,
            };
        }
    }

    // A pending erase run: start column plus the uniform blank attributes.
    let mut erase: Option<(u16, Attrs)> = None;
    for (col, cell) in cells.iter().enumerate() {
        if prev_was_wide {
            prev_was_wide = false;
            continue;
        }
        prev_was_wide = cell.is_wide;

        let col = col as u16;
        let pos = Pos { row: row_idx, col };

        let flush =
            matches!(&erase, Some((_, attrs)) if cell.has_contents() || cell.attrs != *attrs);
        if flush {
            flush_erase_run(
                out,
                erase.take(),
                row_idx,
                Some(pos.col),
                &mut prev_pos,
                &mut prev_attrs,
            );
        }

        if cell != &default_cell {
            if cell.has_contents() {
                write_content_move(out, pos, &mut prev_pos, cols, wrapping, prev_row, cells);

                if prev_attrs != cell.attrs {
                    write_attrs_diff(out, &cell.attrs, &prev_attrs);
                    prev_attrs = cell.attrs.clone();
                }

                let width = u16::from(cell.is_wide);
                out.extend_from_slice(cell.contents().as_bytes());
                prev_pos.col = (pos.col + 1 + width).min(cols);
            } else if erase.is_none() {
                erase = Some((pos.col, cell.attrs.clone()));
            }
        }
    }
    flush_erase_run(
        out,
        erase.take(),
        row_idx,
        None,
        &mut prev_pos,
        &mut prev_attrs,
    );

    (prev_pos, prev_attrs)
}

/// Paint one row as a diff against `prev_cells` (same semantics as
/// [`write_row_formatted`], emitting only what changed).
///
/// `prev_wrapping` is the previous state's wrap-continuation flag for this
/// row. Row wrap provenance is outside the replay contract, so unlike vt100
/// this does not redraw the last cell on wrap-flag transitions; the cursor
/// bookkeeping still tracks where the replay cursor actually is.
fn write_row_diff(
    out: &mut Vec<u8>,
    ctx: &RowCtx<'_>,
    prev_cells: &[Cell],
    prev_wrapping: bool,
    mut prev_pos: Pos,
    mut prev_attrs: Attrs,
) -> (Pos, Attrs) {
    let RowCtx {
        cells,
        prev_row,
        row: row_idx,
        cols,
        wrapping,
    } = *ctx;
    let mut prev_was_wide = false;

    let first_cell = &cells[0];
    let prev_first_cell = &prev_cells[0];
    if wrapping
        && !prev_wrapping
        && first_cell == prev_first_cell
        && prev_pos.row + 1 == row_idx
        && prev_pos.col >= cols
        && !prev_row.is_some_and(zwj_wrap_hazard)
    {
        // The previous row now wraps into this one: perform the pending wrap
        // by redrawing the first cell, then back onto the row start.
        if prev_attrs != first_cell.attrs {
            write_attrs_diff(out, &first_cell.attrs, &prev_attrs);
            prev_attrs = first_cell.attrs.clone();
        }
        let mut contents = prev_first_cell.contents();
        let need_erase = contents.is_empty();
        if need_erase {
            contents = " ";
        }
        out.extend_from_slice(contents.as_bytes());
        out.push(0x08);
        if prev_first_cell.is_wide {
            out.push(0x08);
        }
        if need_erase {
            write_erase_chars(out, 1);
        }
        prev_pos = Pos {
            row: row_idx,
            col: 0,
        };
    }

    let mut erase: Option<(u16, Attrs)> = None;
    for (col, (cell, prev_cell)) in cells.iter().zip(prev_cells.iter()).enumerate() {
        if prev_was_wide {
            prev_was_wide = false;
            continue;
        }
        prev_was_wide = cell.is_wide;

        let col = col as u16;
        let pos = Pos { row: row_idx, col };

        let flush =
            matches!(&erase, Some((_, attrs)) if cell.has_contents() || cell.attrs != *attrs);
        if flush {
            flush_erase_run(
                out,
                erase.take(),
                row_idx,
                Some(pos.col),
                &mut prev_pos,
                &mut prev_attrs,
            );
        }

        if cell != prev_cell {
            if cell.has_contents() {
                write_content_move(out, pos, &mut prev_pos, cols, wrapping, prev_row, cells);

                if prev_attrs != cell.attrs {
                    write_attrs_diff(out, &cell.attrs, &prev_attrs);
                    prev_attrs = cell.attrs.clone();
                }

                let width = u16::from(cell.is_wide);
                out.extend_from_slice(cell.contents().as_bytes());
                prev_pos.col = (pos.col + 1 + width).min(cols);
            } else if erase.is_none() {
                erase = Some((pos.col, cell.attrs.clone()));
            }
        }
    }
    flush_erase_run(
        out,
        erase.take(),
        row_idx,
        None,
        &mut prev_pos,
        &mut prev_attrs,
    );

    (prev_pos, prev_attrs)
}

// ── Cursor-position restore ──────────────────────────────────────────────────

/// Emit bytes leaving the replay cursor at the grid's cursor position,
/// reproducing a pending-wrap phantom column (`col == cols`) when one is
/// armed.
///
/// A phantom is re-armed by redrawing the row's last cell. When that cell is
/// blank (its content was erased without moving the cursor, or the row was
/// scrolled under a parked cursor), the cell cannot carry the wrap directly:
/// write a space with the erase cell's attributes, then save the parked
/// cursor, erase the space, and restore — DECSC/DECRC round-trip the phantom
/// (xterm parity).
fn write_cursor_position_formatted(
    out: &mut Vec<u8>,
    screen: &RowStore,
    cursor: Pos,
    cols: u16,
    prev_pos: Option<Pos>,
    prev_attrs: Option<&Attrs>,
) {
    let prev_attrs = prev_attrs.cloned().unwrap_or_default();
    if prev_pos != Some(cursor) && cursor.col >= cols {
        let mut lead = Pos {
            row: cursor.row,
            col: cols.saturating_sub(1),
        };
        let last_cell = screen
            .get(usize::from(cursor.row))
            .and_then(|row| row.get(usize::from(lead.col)));
        if cols >= 2 && last_cell.is_some_and(|cell| cell.is_wide_continuation) {
            lead.col = cols - 2;
        }
        let cell = screen
            .get(usize::from(cursor.row))
            .and_then(|row| row.get(usize::from(lead.col)));
        if cell.is_some_and(Cell::has_contents) {
            // Redraw the cell with the content; the write re-parks the cursor.
            // Absolute move even when already in place: an explicit CUP breaks
            // the ZWJ cluster-continuation barrier.
            write_move_to(out, lead);
            if let Some(cell) = cell {
                write_attrs_diff(out, &cell.attrs, &prev_attrs);
                out.extend_from_slice(cell.contents().as_bytes());
                write_attrs_diff(out, &prev_attrs, &cell.attrs);
            }
        } else if let Some(end_cell) = last_cell {
            // The last cell is blank: paint and erase a space there instead.
            write_move_to(
                out,
                Pos {
                    row: cursor.row,
                    col: cols.saturating_sub(1),
                },
            );
            out.push(b' ');
            write_attrs_diff(out, &end_cell.attrs, &prev_attrs);
            out.extend_from_slice(b"\x1b7");
            out.push(0x08);
            write_erase_chars(out, 1);
            out.extend_from_slice(b"\x1b8");
            write_attrs_diff(out, &prev_attrs, &end_cell.attrs);
        }
    } else if let Some(prev_pos) = prev_pos {
        write_move_from_to(out, prev_pos, cursor);
    } else {
        write_move_to(out, cursor);
    }
}

// ── DamageGrid serialization API ─────────────────────────────────────────────

impl DamageGrid {
    fn visible_screen(&self) -> &RowStore {
        if self.mode_flags & ALT_SCREEN != 0 {
            &self.alternate
        } else {
            &self.primary
        }
    }

    /// The visible screen's soft-wrap continuation flag for `row`
    /// (true = `row` continues the logical line of the row above).
    fn row_is_continuation(&self, row: u16) -> bool {
        row > 0 && self.visible_screen().wrap(usize::from(row)) == Some(RowWrap::Soft)
    }

    fn write_contents_formatted(&self, out: &mut Vec<u8>) {
        // Row serialization uses pending-wrap semantics. Establish its mode
        // before painting, even when the destination previously disabled it.
        write_autowrap(out, true);
        write_hide_cursor(out, self.hide_cursor());
        self.write_contents_full_paint(out);
        if !self.autowrap() {
            write_autowrap(out, false);
        }
    }

    /// `ClearAttrs` + clear-screen preamble followed by a full paint of the
    /// visible rows and the cursor-position restore. Shared by
    /// `contents_formatted` and the non-diffable `contents_diff` fallback.
    fn write_contents_full_paint(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[m");
        out.extend_from_slice(b"\x1b[H\x1b[J");

        let screen = self.visible_screen();
        let cols = self.cols;
        let mut prev_attrs = Attrs::default();
        let mut prev_pos = Pos { row: 0, col: 0 };
        let mut wrapping = false;
        for i in 0..self.rows {
            let Some(cells) = screen.get(usize::from(i)) else {
                break;
            };
            let prev_row_cells = (i > 0).then(|| screen.get(usize::from(i - 1))).flatten();
            let ctx = RowCtx {
                cells,
                prev_row: prev_row_cells.map(Vec::as_slice),
                row: i,
                cols,
                wrapping,
            };
            let (new_pos, new_attrs) = write_row_formatted(out, &ctx, prev_pos, prev_attrs);
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = self.row_is_continuation(i + 1);
        }

        let cursor = Pos {
            row: self.cursor_row,
            col: self.cursor_col,
        };
        write_cursor_position_formatted(
            out,
            screen,
            cursor,
            cols,
            Some(prev_pos),
            Some(&prev_attrs),
        );
        write_attrs_diff(out, &self.current_attrs, &prev_attrs);
    }

    fn write_contents_diff(&self, out: &mut Vec<u8>, prev: &DamageGrid) {
        write_autowrap(out, true);
        if self.hide_cursor() != prev.hide_cursor() {
            write_hide_cursor(out, self.hide_cursor());
        }
        if self.size() != prev.size() || self.alternate_screen() != prev.alternate_screen() {
            // A screen switch or a dimension change cannot be diffed: repaint
            // fully (the input-mode writer performs the `?1049` switch first).
            self.write_contents_full_paint(out);
            if !self.autowrap() {
                write_autowrap(out, false);
            }
            return;
        }

        let screen = self.visible_screen();
        let prev_screen = prev.visible_screen();
        let cols = self.cols;
        let mut prev_attrs = prev.current_attrs.clone();
        let mut prev_pos = Pos {
            row: prev.cursor_row,
            col: prev.cursor_col,
        };
        let mut wrapping = false;
        let mut prev_wrapping = false;
        for i in 0..self.rows {
            let (Some(cells), Some(prev_cells)) =
                (screen.get(usize::from(i)), prev_screen.get(usize::from(i)))
            else {
                break;
            };
            let prev_row_cells = (i > 0).then(|| screen.get(usize::from(i - 1))).flatten();
            let ctx = RowCtx {
                cells,
                prev_row: prev_row_cells.map(Vec::as_slice),
                row: i,
                cols,
                wrapping,
            };
            let (new_pos, new_attrs) =
                write_row_diff(out, &ctx, prev_cells, prev_wrapping, prev_pos, prev_attrs);
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = self.row_is_continuation(i + 1);
            prev_wrapping = prev.row_is_continuation(i + 1);
        }

        let cursor = Pos {
            row: self.cursor_row,
            col: self.cursor_col,
        };
        write_cursor_position_formatted(
            out,
            screen,
            cursor,
            cols,
            Some(prev_pos),
            Some(&prev_attrs),
        );
        write_attrs_diff(out, &self.current_attrs, &prev_attrs);
        if !self.autowrap() {
            write_autowrap(out, false);
        }
    }

    fn write_input_mode_formatted(&self, out: &mut Vec<u8>) {
        write_autowrap(out, self.autowrap());
        out.extend_from_slice(if self.application_keypad() {
            b"\x1b="
        } else {
            b"\x1b>"
        });
        out.extend_from_slice(if self.application_cursor() {
            b"\x1b[?1h"
        } else {
            b"\x1b[?1l"
        });
        out.extend_from_slice(if self.bracketed_paste() {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        });
        write_mouse_mode(out, self.mouse_mode, MouseProtocolMode::None);
        write_mouse_encoding(out, self.mouse_encoding, MouseProtocolEncoding::Default);
        if self.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        write_hide_cursor(out, self.hide_cursor());
        write_cursor_style(out, self.cursor_style);
    }

    fn write_input_mode_diff(&self, out: &mut Vec<u8>, prev: &DamageGrid) {
        if self.autowrap() != prev.autowrap() {
            write_autowrap(out, self.autowrap());
        }
        if self.application_keypad() != prev.application_keypad() {
            out.extend_from_slice(if self.application_keypad() {
                b"\x1b="
            } else {
                b"\x1b>"
            });
        }
        if self.application_cursor() != prev.application_cursor() {
            out.extend_from_slice(if self.application_cursor() {
                b"\x1b[?1h"
            } else {
                b"\x1b[?1l"
            });
        }
        if self.bracketed_paste() != prev.bracketed_paste() {
            out.extend_from_slice(if self.bracketed_paste() {
                b"\x1b[?2004h"
            } else {
                b"\x1b[?2004l"
            });
        }
        write_mouse_mode(out, self.mouse_mode, prev.mouse_mode);
        write_mouse_encoding(out, self.mouse_encoding, prev.mouse_encoding);
        if self.alternate_screen() != prev.alternate_screen() {
            out.extend_from_slice(if self.alternate_screen() {
                b"\x1b[?1049h"
            } else {
                b"\x1b[?1049l"
            });
        }
        if self.hide_cursor() != prev.hide_cursor() {
            write_hide_cursor(out, self.hide_cursor());
        }
        if self.cursor_style != prev.cursor_style {
            write_cursor_style(out, self.cursor_style);
        }
    }

    /// Terminal escape sequences reproducing the visible contents of the
    /// grid: cells (with attributes), the cursor position including a
    /// pending-wrap phantom column, the current SGR pen, DECAWM, and cursor
    /// visibility. See the module docs for the replay contract.
    #[must_use]
    pub fn contents_formatted(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_contents_formatted(&mut out);
        out
    }

    /// Terminal escape sequences turning the visible contents described by
    /// `prev` into the visible contents described by `self`. Replaying
    /// `prev.contents_formatted()` followed by `self.contents_diff(prev)`
    /// produces the same visible state as `self.contents_formatted()`.
    #[must_use]
    pub fn contents_diff(&self, prev: &Self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_contents_diff(&mut out, prev);
        out
    }

    /// Terminal escape sequences setting the grid's input modes: DECAWM,
    /// application keypad, application cursor keys, bracketed paste, mouse
    /// protocol mode and encoding, alternate screen (`?1049`), cursor
    /// visibility, and the DECSCUSR cursor style.
    #[must_use]
    pub fn input_mode_formatted(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_input_mode_formatted(&mut out);
        out
    }

    /// Terminal escape sequences changing the input modes described by `prev`
    /// to the input modes of `self`.
    #[must_use]
    pub fn input_mode_diff(&self, prev: &Self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_input_mode_diff(&mut out, prev);
        out
    }

    /// Escape codes sufficient to reproduce the entire terminal state: the
    /// input modes (including the `?1049` screen switch, which must precede
    /// painting) followed by the visible contents. Replaying into a fresh
    /// grid of the same dimensions reproduces the state exactly — see the
    /// module docs for the contract.
    #[must_use]
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_input_mode_formatted(&mut out);
        self.write_contents_formatted(&mut out);
        out
    }

    /// Escape codes turning the terminal state described by `prev` into the
    /// state described by `self` (input-mode diff, then contents diff).
    /// Replaying `prev.state_formatted()` followed by `self.state_diff(prev)`
    /// into a fresh grid reproduces `self`'s state exactly.
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_input_mode_diff(&mut out, prev);
        self.write_contents_diff(&mut out, prev);
        out
    }

    /// Whether `self` and `other` are equal over the serialization replay
    /// contract (see the module docs): visible cells with full attributes,
    /// cursor position including the pending-wrap phantom column, the current
    /// SGR pen, DECAWM, application keypad/cursor keys, bracketed paste, mouse
    /// protocol mode/encoding, alternate screen, cursor visibility, and
    /// DECSCUSR cursor style.
    #[must_use]
    pub fn state_eq(&self, other: &Self) -> bool {
        if self.size() != other.size() {
            return false;
        }
        let (rows, cols) = self.size();
        let screen = self.visible_screen();
        let other_screen = other.visible_screen();
        for r in 0..rows {
            let (Some(row), Some(other_row)) =
                (screen.get(usize::from(r)), other_screen.get(usize::from(r)))
            else {
                return false;
            };
            for c in 0..cols as usize {
                let (Some(cell), Some(other_cell)) = (row.get(c), other_row.get(c)) else {
                    return false;
                };
                if cell.contents() != other_cell.contents()
                    || cell.is_wide != other_cell.is_wide
                    || cell.is_wide_continuation != other_cell.is_wide_continuation
                    || cell.attrs != other_cell.attrs
                {
                    return false;
                }
            }
        }
        self.cursor_position() == other.cursor_position()
            && self.current_attrs == other.current_attrs
            && self.autowrap() == other.autowrap()
            && self.application_keypad() == other.application_keypad()
            && self.application_cursor() == other.application_cursor()
            && self.bracketed_paste() == other.bracketed_paste()
            && mouse_mode_number(self.mouse_mode) == mouse_mode_number(other.mouse_mode)
            && mouse_encoding_number(self.mouse_encoding)
                == mouse_encoding_number(other.mouse_encoding)
            && self.alternate_screen() == other.alternate_screen()
            && self.hide_cursor() == other.hide_cursor()
            && self.cursor_style == other.cursor_style
    }
}

#[cfg(test)]
#[path = "serialize_tests.rs"]
mod tests;
