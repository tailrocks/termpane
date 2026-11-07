// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! `?1049` follows xterm: entering saves the cursor with its deferred wrap,
//! leaves the cursor in place and cancels the wrap; leaving restores both.
//!
//! Every test feeds bytes through `DamageGrid::process` and reads cells back
//! through the public API.

use termpane::DamageGrid;

fn text(grid: &DamageGrid, row: u16, col: u16) -> String {
    grid.cell(row, col)
        .map(|cell| cell.contents().to_owned())
        .unwrap_or_default()
}

#[test]
fn entering_the_alternate_screen_leaves_the_cursor_where_it_is() {
    let mut grid = DamageGrid::new(3, 5, 0);
    grid.process(b"abc\x1b[?1049hX");

    assert_eq!(text(&grid, 0, 3), "X");
    assert_eq!(text(&grid, 0, 0), "");
}

#[test]
fn a_pending_wrap_does_not_survive_entering_the_alternate_screen() {
    let mut grid = DamageGrid::new(3, 5, 0);
    grid.process(b"abcde\x1b[?1049hX");

    assert_eq!(text(&grid, 0, 4), "X");
    assert_eq!(text(&grid, 1, 0), "");
}

#[test]
fn leaving_the_alternate_screen_restores_a_pending_wrap() {
    let mut grid = DamageGrid::new(3, 5, 0);
    grid.process(b"abcde\x1b[?1049h\x1b[1;1Hq\x1b[?1049lX");

    assert_eq!(text(&grid, 0, 4), "e");
    assert_eq!(text(&grid, 1, 0), "X");
}
