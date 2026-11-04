#![cfg(feature = "dhat-heap")]

// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

use termpane::DamageGrid;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[test]
fn focused_process_dirty_patch_path_allocates_zero_after_warmup() {
    let mut grid = DamageGrid::new(24, 80, 1_000);
    // Warm the exact measured path twice so first-touch setup (parsers,
    // scratch buffers) is not charged to the zero-alloc window. CI runners
    // otherwise sometimes report a single residual block.
    for byte in [b"A", b"B"] {
        grid.process(byte);
        let patch = grid.dump_dirty_patch();
        let changed_cells = patch
            .changed_spans()
            .map(|(_, _, cells)| cells.iter().filter(|cell| cell.has_contents()).count())
            .sum::<usize>();
        std::hint::black_box(changed_cells);
    }

    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();

    grid.process(b"C");
    let patch = grid.dump_dirty_patch();
    let changed_cells = patch
        .changed_spans()
        .map(|(_, _, cells)| cells.iter().filter(|cell| cell.has_contents()).count())
        .sum::<usize>();
    std::hint::black_box(changed_cells);

    let after = dhat::HeapStats::get();
    dhat::assert_eq!(after.total_blocks - before.total_blocks, 0);
    dhat::assert_eq!(after.total_bytes - before.total_bytes, 0);
}

#[test]
fn same_size_resize_allocates_zero_after_warmup() {
    let mut grid = DamageGrid::new(24, 80, 1_000);
    grid.process(b"A");
    grid.set_size(24, 80); // warm up: absorb any first-call setup cost
    drop(grid.dump_dirty_patch());

    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();

    grid.set_size(24, 80); // same dims: RowStore::resize must be a pure no-op

    let after = dhat::HeapStats::get();
    dhat::assert_eq!(after.total_blocks - before.total_blocks, 0);
    dhat::assert_eq!(after.total_bytes - before.total_bytes, 0);
}

#[test]
fn huge_csi_counts_allocate_at_most_one_row_per_region_row() {
    const SEQUENCES: [&[u8]; 4] = [
        b"\x1b[65535L",
        b"\x1b[65535M",
        b"\x1b[65535S",
        b"\x1b[65535T",
    ];
    const REGION_ROWS: u64 = 31;
    let mut grid = DamageGrid::new(60, 200, 1_000);
    grid.process(b"\x1b[10;40r\x1b[12;1H");
    for sequence in SEQUENCES {
        grid.process(sequence);
        grid.clear_scroll_ops();
    }

    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();

    for sequence in SEQUENCES {
        grid.process(sequence);
        grid.clear_scroll_ops();
    }

    let after = dhat::HeapStats::get();
    dhat::assert!(after.total_blocks - before.total_blocks <= 4 * REGION_ROWS);
}

#[test]
fn printing_under_a_long_link_allocates_nothing_per_cell() {
    let mut grid = DamageGrid::new(24, 80, 0);
    let open = format!("\x1b]8;id=a;http://x/{}\x1b\\", "A".repeat(8_000));
    let text = "x".repeat(24 * 80);
    grid.process(open.as_bytes());

    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();

    grid.process(text.as_bytes());

    let after = dhat::HeapStats::get();
    dhat::assert!(after.total_bytes - before.total_bytes <= 64 * 1024);
}
