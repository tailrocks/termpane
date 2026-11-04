// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! A cell's grapheme cluster is bounded whatever bytes the program writes.
//!
//! Every test feeds bytes through `DamageGrid::process` and reads cells back
//! through the public API.

use termpane::DamageGrid;

/// One base character plus the 16 characters that may join it.
const CELL_CHARS: usize = 17;

fn cell_chars(grid: &DamageGrid, row: u16, col: u16) -> usize {
    grid.cell(row, col)
        .map_or(0, |cell| cell.contents().chars().count())
}

fn longest_cell(grid: &DamageGrid, rows: u16, cols: u16) -> usize {
    (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .map(|(row, col)| cell_chars(grid, row, col))
        .max()
        .unwrap_or(0)
}

fn marks(n: usize) -> String {
    "\u{301}".repeat(n)
}

#[test]
fn combining_marks_stop_at_the_cap() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(format!("a{}", marks(5_000)).as_bytes());
    assert_eq!(cell_chars(&grid, 0, 0), CELL_CHARS);
}

#[test]
fn a_printable_after_a_joiner_cannot_grow_a_cell_without_bound() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process(format!("a{}", "\u{200d}b".repeat(5_000)).as_bytes());
    assert_eq!(cell_chars(&grid, 0, 0), CELL_CHARS);
    assert!(longest_cell(&grid, 2, 10) <= CELL_CHARS);
}

#[test]
fn a_printable_after_a_full_cluster_starts_a_new_cell() {
    let mut grid = DamageGrid::new(2, 40, 0);
    grid.process(format!("a{}\u{200d}", marks(15)).as_bytes());
    assert_eq!(
        cell_chars(&grid, 0, 0),
        CELL_CHARS,
        "15 marks and the joiner"
    );
    grid.process(b"z");
    assert_eq!(
        grid.cell(0, 1).map(|cell| cell.contents().to_owned()),
        Some("z".to_owned()),
        "the printable that cannot join prints in the next cell"
    );
}

#[test]
fn a_wide_base_that_does_not_print_cannot_collect_marks_through_the_phantom_column() {
    let mut grid = DamageGrid::new(2, 10, 0);
    let mut bytes = String::from("\x1b[?7l\x1b[1;10Ha\u{4e2d}\x1b[?7h");
    for _ in 0..200 {
        bytes.push('b');
        bytes.push_str(&marks(20));
    }
    grid.process(bytes.as_bytes());
    assert!(longest_cell(&grid, 2, 10) <= CELL_CHARS);
}

#[test]
fn marks_after_a_soft_reset_cannot_grow_the_last_cell_without_bound() {
    let mut grid = DamageGrid::new(2, 10, 0);
    let mut bytes = String::from("\x1b[1;10Ha\x1b[!p");
    for _ in 0..200 {
        bytes.push('b');
        bytes.push_str(&marks(20));
    }
    grid.process(bytes.as_bytes());
    assert!(longest_cell(&grid, 2, 10) <= CELL_CHARS);
}

#[test]
fn legitimate_clusters_are_unchanged() {
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
    let kiss = "\u{1f469}\u{200d}\u{2764}\u{fe0f}\u{200d}\u{1f48b}\u{200d}\u{1f468}";
    let england = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
    let flag = "\u{1f1fa}\u{1f1f8}";
    let sixteen = format!("a{}", marks(16));

    let mut grid = DamageGrid::new(8, 40, 0);
    for (row, text) in [family, kiss, england, flag, sixteen.as_str()]
        .into_iter()
        .enumerate()
    {
        grid.process(format!("\x1b[{};1H{text}", row + 1).as_bytes());
    }
    let row_text = |row: u16| -> String {
        (0..40)
            .filter_map(|col| grid.cell(row, col))
            .filter(|cell| !cell.is_wide_continuation)
            .map(|cell| cell.contents().to_owned())
            .collect()
    };
    assert_eq!(row_text(0), family);
    assert_eq!(row_text(1), kiss);
    assert_eq!(row_text(2), england);
    assert_eq!(row_text(3), flag);
    assert_eq!(row_text(4), sixteen);
    assert_eq!(cell_chars(&grid, 4, 0), CELL_CHARS);
}

fn text(grid: &DamageGrid, row: u16, col: u16) -> String {
    grid.cell(row, col)
        .map(|cell| cell.contents().to_owned())
        .unwrap_or_default()
}

#[test]
fn a_soft_reset_at_the_margin_leaves_the_cursor_on_the_last_cell() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process("\x1b[1;10Ha\x1b[!pb\u{301}".as_bytes());
    assert_eq!(
        text(&grid, 0, 9),
        "b\u{301}",
        "the printable is not dropped"
    );
    assert_eq!(text(&grid, 1, 0), "");
}

#[test]
fn a_wide_glyph_that_does_not_fit_keeps_the_deferred_wrap_for_the_next_printable() {
    let mut grid = DamageGrid::new(2, 10, 0);
    grid.process("\x1b[?7l\x1b[1;10Ha\u{4e2d}\x1b[?7hb".as_bytes());
    assert_eq!(text(&grid, 0, 9), "a");
    assert_eq!(text(&grid, 1, 0), "b", "the printable is not dropped");
}
