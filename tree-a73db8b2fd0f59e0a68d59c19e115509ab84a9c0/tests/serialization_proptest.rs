// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Property tests for the replayable serialization API (`src/grid/serialize.rs`).
//!
//! The hard invariant: serializing any `process()`-reachable state and
//! replaying the bytes into a fresh grid of the same dimensions reproduces
//! that state exactly (see `serialize.rs` for the contract's scope).

use proptest::prelude::*;
use termpane::DamageGrid;

/// One input fragment: text, wide/combining/ZWJ clusters, SGR, cursor moves,
/// erase/edit ops, DEC mode toggles, and control bytes — the full surface the
/// serialization contract covers.
fn arb_piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        4 => prop::sample::select(vec![
            b"abc".to_vec(),
            b"Hello".to_vec(),
            "界你".as_bytes().to_vec(),
            "🦀x".as_bytes().to_vec(),
            "a\u{301}".as_bytes().to_vec(),
            "\u{302}".as_bytes().to_vec(),
            "👨\u{200d}👩".as_bytes().to_vec(),
            "\u{200d}👧".as_bytes().to_vec(),
            "x\u{200d}".as_bytes().to_vec(),
        ]),
        3 => (0u16..=60).prop_map(|n| format!("\x1b[{n}m").into_bytes()),
        1 => prop::sample::select(vec![
            b"\x1b[4:2m".to_vec(),
            b"\x1b[4:3m".to_vec(),
            b"\x1b[38;2;10;20;30m".to_vec(),
            b"\x1b[48;5;200m".to_vec(),
            b"\x1b[58;5;9m".to_vec(),
            b"\x1b[m".to_vec(),
        ]),
        3 => prop::sample::select(vec![
            b"\x1b[H".to_vec(),
            b"\x1b[2;3H".to_vec(),
            b"\x1b[A".to_vec(),
            b"\x1b[2B".to_vec(),
            b"\x1b[C".to_vec(),
            b"\x1b[3D".to_vec(),
            b"\x1b[5G".to_vec(),
            b"\x1b[2d".to_vec(),
            b"\x1b7".to_vec(),
            b"\x1b8".to_vec(),
            b"\x1b[s".to_vec(),
            b"\x1b[u".to_vec(),
            b"\x1bM".to_vec(),
        ]),
        2 => prop::sample::select(vec![
            b"\x1b[J".to_vec(),
            b"\x1b[2J".to_vec(),
            b"\x1b[?J".to_vec(),
            b"\x1b[?2J".to_vec(),
            b"\x1b[K".to_vec(),
            b"\x1b[2K".to_vec(),
            b"\x1b[?K".to_vec(),
            b"\x1b[2X".to_vec(),
            b"\x1b[@".to_vec(),
            b"\x1b[P".to_vec(),
            b"\x1b[L".to_vec(),
            b"\x1b[M".to_vec(),
            b"\x1b[S".to_vec(),
            b"\x1b[T".to_vec(),
            b"\x1b[1;3r".to_vec(),
            b"\x1b[r".to_vec(),
        ]),
        2 => prop::sample::select(vec![
            b"\x1b[?7h".to_vec(),
            b"\x1b[?7l".to_vec(),
            b"\x1b[?25h".to_vec(),
            b"\x1b[?25l".to_vec(),
            b"\x1b[?1h".to_vec(),
            b"\x1b[?1l".to_vec(),
            b"\x1b[?12h".to_vec(),
            b"\x1b[?12l".to_vec(),
            b"\x1b=".to_vec(),
            b"\x1b>".to_vec(),
            b"\x1b[?2004h".to_vec(),
            b"\x1b[?2004l".to_vec(),
            b"\x1b[?2026h".to_vec(),
            b"\x1b[?2026l".to_vec(),
            b"\x1b[?1000h".to_vec(),
            b"\x1b[?1002h".to_vec(),
            b"\x1b[?1003h".to_vec(),
            b"\x1b[?1003l".to_vec(),
            b"\x1b[?1004h".to_vec(),
            b"\x1b[?1004l".to_vec(),
            b"\x1b[?1005h".to_vec(),
            b"\x1b[?1006h".to_vec(),
            b"\x1b[?1015h".to_vec(),
            b"\x1b[?47h".to_vec(),
            b"\x1b[?47l".to_vec(),
            b"\x1b[?1047h".to_vec(),
            b"\x1b[?1047l".to_vec(),
            b"\x1b[?1049h".to_vec(),
            b"\x1b[?1049l".to_vec(),
            b"\x1b[3 q".to_vec(),
            b"\x1b[0 q".to_vec(),
            b"\x1b[!p".to_vec(),
        ]),
        1 => prop::sample::select(vec![
            b"\r".to_vec(),
            b"\n".to_vec(),
            b"\x08".to_vec(),
            b"\t".to_vec(),
            b"\x07".to_vec(),
            b"\x0b".to_vec(),
        ]),
    ]
}

fn arb_stream() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(arb_piece(), 0..48).prop_map(|pieces| pieces.concat())
}

/// The `contents_formatted` contract: visible cells, cursor, pen, DECAWM, and
/// cursor visibility (everything except the input modes and the `?1049`
/// screen switch).
fn assert_visible_eq(replayed: &DamageGrid, expected: &DamageGrid) -> Result<(), TestCaseError> {
    let (rows, cols) = expected.size();
    for row in 0..rows {
        for col in 0..cols {
            let expected_cell = expected
                .cell(row, col)
                .ok_or_else(|| TestCaseError::fail("cell out of bounds"))?;
            let replayed_cell = replayed
                .cell(row, col)
                .ok_or_else(|| TestCaseError::fail("cell out of bounds"))?;
            prop_assert_eq!(
                replayed_cell.contents(),
                expected_cell.contents(),
                "cell ({},{})",
                row,
                col
            );
            prop_assert_eq!(replayed_cell.is_wide, expected_cell.is_wide);
            prop_assert_eq!(
                replayed_cell.is_wide_continuation,
                expected_cell.is_wide_continuation
            );
            prop_assert_eq!(
                &replayed_cell.attrs,
                &expected_cell.attrs,
                "cell ({},{}) attrs",
                row,
                col
            );
        }
    }
    prop_assert_eq!(replayed.cursor_position(), expected.cursor_position());
    prop_assert_eq!(replayed.current_attrs(), expected.current_attrs());
    prop_assert_eq!(replayed.autowrap(), expected.autowrap());
    prop_assert_eq!(replayed.hide_cursor(), expected.hide_cursor());
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn state_formatted_replays_exactly(stream in arb_stream(), rows in 2..=6u16, cols in 2..=12u16) {
        let mut grid = DamageGrid::new(rows, cols, 5);
        grid.process(&stream);
        let mut replayed = DamageGrid::new(rows, cols, 5);
        replayed.process(&grid.state_formatted());
        prop_assert!(
            grid.state_eq(&replayed),
            "state_formatted round-trip mismatch\nstream: {stream:?}\nstate: {:?}",
            grid.dump().to_text(),
        );
    }

    #[test]
    fn state_diff_replays_exactly(
        stream_a in arb_stream(),
        stream_b in arb_stream(),
        rows in 2..=6u16,
        cols in 2..=12u16,
    ) {
        let mut cur = DamageGrid::new(rows, cols, 5);
        cur.process(&stream_a);

        // The prev-state grid: a replay of cur's own serialization (this also
        // exercises the formatted property on the intermediate state).
        let mut prev = DamageGrid::new(rows, cols, 5);
        prev.process(&cur.state_formatted());
        prop_assert!(cur.state_eq(&prev), "intermediate state_formatted mismatch");

        cur.process(&stream_b);
        let diff = cur.state_diff(&prev);
        prev.process(&diff);
        prop_assert!(
            cur.state_eq(&prev),
            "state_diff round-trip mismatch\nstream_a: {stream_a:?}\nstream_b: {stream_b:?}\nstate: {:?}",
            cur.dump().to_text(),
        );
    }

    #[test]
    fn contents_formatted_replays_visible_state(stream in arb_stream(), rows in 2..=6u16, cols in 2..=12u16) {
        let mut grid = DamageGrid::new(rows, cols, 5);
        grid.process(&stream);
        let mut replayed = DamageGrid::new(rows, cols, 5);
        replayed.process(&grid.contents_formatted());
        assert_visible_eq(&replayed, &grid)?;
    }

    #[test]
    fn input_mode_formatted_replays_modes(stream in arb_stream(), rows in 2..=6u16, cols in 2..=12u16) {
        let mut grid = DamageGrid::new(rows, cols, 5);
        grid.process(&stream);
        let mut replayed = DamageGrid::new(rows, cols, 5);
        replayed.process(&grid.input_mode_formatted());
        prop_assert_eq!(replayed.autowrap(), grid.autowrap());
        prop_assert_eq!(replayed.application_keypad(), grid.application_keypad());
        prop_assert_eq!(replayed.application_cursor(), grid.application_cursor());
        prop_assert_eq!(replayed.bracketed_paste(), grid.bracketed_paste());
        prop_assert_eq!(replayed.alternate_screen(), grid.alternate_screen());
        prop_assert_eq!(replayed.hide_cursor(), grid.hide_cursor());
        prop_assert_eq!(replayed.cursor_style(), grid.cursor_style());
    }
}
