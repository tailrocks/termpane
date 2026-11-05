// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! A resize keeps every wide cell paired with its continuation.
//!
//! Every test feeds bytes through `DamageGrid::process` and reads cells back
//! through the public API.

use termpane::DamageGrid;

fn is_lead(grid: &DamageGrid, row: u16, col: u16) -> bool {
    grid.cell(row, col).is_some_and(|cell| cell.is_wide)
}

fn is_continuation(grid: &DamageGrid, row: u16, col: u16) -> bool {
    grid.cell(row, col)
        .is_some_and(|cell| cell.is_wide_continuation)
}

fn assert_wide_cells_are_paired(grid: &DamageGrid) {
    let (rows, cols) = grid.size();
    for row in 0..rows {
        for col in 0..cols {
            assert_eq!(
                is_lead(grid, row, col),
                is_continuation(grid, row, col + 1),
                "wide lead at {row},{col} must be followed by its continuation"
            );
            assert_eq!(
                is_continuation(grid, row, col),
                col > 0 && is_lead(grid, row, col - 1),
                "continuation at {row},{col} must follow a wide lead"
            );
        }
    }
}

#[test]
fn widening_the_grid_completes_a_wide_cell_at_the_old_last_column() {
    for new_cols in [4, 6] {
        let mut grid = DamageGrid::new(1, 3, 0);
        grid.process("\x1b[1;3H\u{2601}\u{fe0f}".as_bytes());
        assert!(is_lead(&grid, 0, 2));

        grid.set_size(1, new_cols);

        assert_eq!(
            grid.cell(0, 2).map(|cell| cell.contents().to_owned()),
            Some("\u{2601}\u{fe0f}".to_owned())
        );
        assert_wide_cells_are_paired(&grid);
    }
}
