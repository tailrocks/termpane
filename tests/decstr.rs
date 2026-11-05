// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! DECSTR (`CSI ! p`) follows xterm: it restores auto-wrap, keeps a pending
//! deferred wrap, the cursor and bracketed paste where they are, and homes the
//! saved cursor.
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
fn a_soft_reset_turns_auto_wrap_back_on() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[?7l");
    assert!(!grid.autowrap());

    grid.process(b"\x1b[!p");
    assert!(grid.autowrap());
}

#[test]
fn auto_wrap_restored_at_the_margin_overwrites_once_then_wraps() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[?7l\x1b[1;10Hab\x1b[!pcd");

    assert_eq!(text(&grid, 0, 9), "c");
    assert_eq!(text(&grid, 1, 0), "d");
}

#[test]
fn a_soft_reset_leaves_the_cursor_and_its_pending_wrap_alone() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[2;3H\x1b[!p");
    assert_eq!(grid.cursor_position(), (1, 2));

    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[1;10Ha\x1b[!p");
    assert_eq!(grid.cursor_position(), (0, 10));
}

#[test]
fn a_soft_reset_homes_the_saved_cursor() {
    let mut grid = DamageGrid::new(5, 10, 0);
    grid.process(b"\x1b[3;5H\x1b7\x1b[!p\x1b[4;4H\x1b8X");

    assert_eq!(text(&grid, 0, 0), "X");
    assert_eq!(text(&grid, 2, 4), "");
}

#[test]
fn a_saved_cursor_at_the_margin_is_homed_without_a_wrap() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[1;10Ha\x1b7\x1b[!p\x1b8X");

    assert_eq!(text(&grid, 0, 0), "X");
    assert_eq!(text(&grid, 1, 0), "");
}

#[test]
fn a_soft_reset_ends_an_open_zwj_cluster() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process("\u{1f468}\u{200d}\x1b[!p\u{1f469}".as_bytes());

    assert_eq!(text(&grid, 0, 0), "\u{1f468}\u{200d}");
    assert_eq!(text(&grid, 0, 2), "\u{1f469}");
}

#[test]
fn a_soft_reset_keeps_bracketed_paste() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b[?2004h\x1b[!p");

    assert!(grid.bracketed_paste());
}

#[test]
fn a_soft_reset_turns_application_keypad_and_cursor_keys_off() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(b"\x1b=\x1b[?1h");
    assert!(grid.application_keypad());
    assert!(grid.application_cursor());

    grid.process(b"\x1b[!p");
    assert!(!grid.application_keypad());
    assert!(!grid.application_cursor());
}
