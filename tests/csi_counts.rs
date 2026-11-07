// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! A count parameter cannot make one control sequence do unbounded work.
//!
//! Every test feeds bytes through `DamageGrid::process` and reads the grid
//! back through the public API.

use termpane::{DamageGrid, ScrollOp};

const ROWS: u16 = 60;
const COLS: u16 = 200;
const HUGE: &str = "65535";

fn filled() -> DamageGrid {
    let mut grid = DamageGrid::new(ROWS, COLS, 1_000);
    for row in 1..=ROWS {
        grid.process(format!("\x1b[{row};1Hrow{row:03}").as_bytes());
    }
    grid
}

fn text(grid: &DamageGrid, row: u16) -> String {
    (0..6)
        .filter_map(|col| grid.cell(row, col))
        .map(|cell| cell.contents().to_owned())
        .collect()
}

fn label(row: u16) -> String {
    format!("row{:03}", row + 1)
}

fn shifted_rows(ops: &[ScrollOp]) -> Vec<u16> {
    ops.iter()
        .map(|op| match *op {
            ScrollOp::Up { rows, .. } | ScrollOp::Down { rows, .. } => rows,
        })
        .collect()
}

fn assert_blank(grid: &DamageGrid, rows: std::ops::RangeInclusive<u16>) {
    for row in rows {
        assert_eq!(text(grid, row), "", "row {row} must be blank");
    }
}

fn assert_kept(grid: &DamageGrid, rows: std::ops::Range<u16>) {
    for row in rows {
        assert_eq!(text(grid, row), label(row), "row {row} must be untouched");
    }
}

#[test]
fn insert_lines_with_a_huge_count_blanks_only_cursor_to_bottom() {
    let mut grid = filled();
    grid.process(format!("\x1b[11;1H\x1b[{HUGE}L").as_bytes());
    assert_kept(&grid, 0..10);
    assert_blank(&grid, 10..=ROWS - 1);
    assert_eq!(shifted_rows(&grid.drain_scroll_ops()), vec![ROWS - 10]);
}

#[test]
fn delete_lines_with_a_huge_count_blanks_only_cursor_to_bottom() {
    let mut grid = filled();
    grid.process(format!("\x1b[11;1H\x1b[{HUGE}M").as_bytes());
    assert_kept(&grid, 0..10);
    assert_blank(&grid, 10..=ROWS - 1);
    assert_eq!(shifted_rows(&grid.drain_scroll_ops()), vec![ROWS - 10]);
}

#[test]
fn scroll_up_with_a_huge_count_scrolls_the_region_height_once() {
    let mut grid = filled();
    grid.process(format!("\x1b[{HUGE}S").as_bytes());
    assert_blank(&grid, 0..=ROWS - 1);
    assert_eq!(shifted_rows(&grid.drain_scroll_ops()), vec![ROWS]);
    assert_eq!(grid.scrollback_len(), usize::from(ROWS));
}

#[test]
fn scroll_down_with_a_huge_count_scrolls_the_region_height_once() {
    let mut grid = filled();
    grid.process(format!("\x1b[{HUGE}T").as_bytes());
    assert_blank(&grid, 0..=ROWS - 1);
    assert_eq!(shifted_rows(&grid.drain_scroll_ops()), vec![ROWS]);
}

#[test]
fn a_scroll_region_bounds_every_huge_count_to_the_region() {
    for (final_byte, expect_rows) in [("L", 21 - 5), ("M", 21 - 5), ("S", 21), ("T", 21)] {
        let mut grid = filled();
        grid.process(format!("\x1b[10;30r\x1b[15;1H\x1b[{HUGE}{final_byte}").as_bytes());
        assert_kept(&grid, 0..9);
        assert_kept(&grid, 30..ROWS);
        let (first_blank, last_blank) = match final_byte {
            "L" | "M" => (14, 29),
            _ => (9, 29),
        };
        assert_blank(&grid, first_blank..=last_blank);
        assert_eq!(
            shifted_rows(&grid.drain_scroll_ops()),
            vec![expect_rows],
            "{final_byte}"
        );
    }
}

#[test]
fn huge_counts_are_bounded_on_the_alternate_screen() {
    for (final_byte, expect_rows) in [("L", ROWS), ("M", ROWS), ("S", ROWS), ("T", ROWS)] {
        let mut grid = filled();
        grid.process(format!("\x1b[?1049h\x1b[1;1H\x1b[{HUGE}{final_byte}").as_bytes());
        assert_eq!(
            shifted_rows(&grid.drain_scroll_ops()),
            vec![expect_rows],
            "{final_byte}"
        );
    }
}

#[test]
fn a_huge_count_that_fits_the_region_exactly_is_unchanged() {
    let mut grid = filled();
    grid.process(b"\x1b[51;1H\x1b[10L");
    assert_kept(&grid, 0..50);
    assert_blank(&grid, 50..=ROWS - 1);
    let mut grid = filled();
    grid.process(b"\x1b[51;1H\x1b[3M");
    assert_kept(&grid, 0..50);
    assert_eq!(text(&grid, 50), label(53));
    assert_blank(&grid, ROWS - 3..=ROWS - 1);
}
