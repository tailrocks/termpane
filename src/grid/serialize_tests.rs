// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Tests for the replayable serialization API (`src/grid/serialize.rs`).
use super::*;

fn replay(bytes: &[u8], rows: u16, cols: u16) -> DamageGrid {
    let mut g = DamageGrid::new(rows, cols, 100);
    g.process(bytes);
    g
}

fn replay_pair(prev: &DamageGrid, encoded: &[u8], rows: u16, cols: u16) -> DamageGrid {
    let mut g = DamageGrid::new(rows, cols, 100);
    g.process(&prev.state_formatted());
    g.process(encoded);
    g
}

#[test]
fn formatted_intensity_roundtrip_clears_each_independent_flag() {
    // Pinned by tui-snap's tool_qualification.rs: independent bold/dim bits
    // must survive the serialization writer's 22-then-reassert rule.
    let mut parser = DamageGrid::new(2, 8, 0);
    parser.process(b"\x1b[1;2mX\x1b[22;1mB\x1b[22;2mD\x1b[0mN");
    let encoded = parser.contents_formatted();
    let replayed = replay(&encoded, 2, 8);
    for (col, bold, dim) in [
        (0, true, true),
        (1, true, false),
        (2, false, true),
        (3, false, false),
    ] {
        let cell = replayed.cell(0, col).expect("cell");
        assert_eq!((cell.bold(), cell.dim()), (bold, dim), "cell {col}");
    }
    assert!(replayed.state_eq(&parser));
}

#[test]
fn blink_conceal_strikethrough_roundtrip_distinctly() {
    // SGR 5 (slow) and 6 (rapid) must round-trip as DISTINCT states, along
    // with 8 (conceal) and 9 (strikethrough) and their resets 25/28/29.
    let mut parser = DamageGrid::new(2, 16, 0);
    parser.process(b"\x1b[5mS\x1b[25m\x1b[6mR\x1b[25m\x1b[8mC\x1b[28m\x1b[9mT\x1b[29m\x1b[0mN");
    let replayed = replay(&parser.contents_formatted(), 2, 16);
    let checks: [(u16, bool, bool, bool, bool); 5] = [
        // col, slow_blink, rapid_blink, conceal, strikethrough
        (0, true, false, false, false),
        (1, false, true, false, false),
        (2, false, false, true, false),
        (3, false, false, false, true),
        (4, false, false, false, false),
    ];
    for (col, slow, rapid, conceal, strike) in checks {
        let cell = replayed.cell(0, col).expect("cell");
        assert_eq!(
            (
                cell.slow_blink(),
                cell.rapid_blink(),
                cell.conceal(),
                cell.strikethrough()
            ),
            (slow, rapid, conceal, strike),
            "cell {col}"
        );
    }
    assert!(replayed.state_eq(&parser));
}

#[test]
fn full_attrs_and_colors_roundtrip() {
    let mut parser = DamageGrid::new(2, 24, 0);
    parser.process(
        b"\x1b[38;2;1;2;3m\x1b[48;5;200m\x1b[1;2;3;7;9;53mX\
          \x1b[4:3m\x1b[58;5;9mY\x1b[24m\x1b[4:2mZ\x1b[0m.",
    );
    let replayed = replay(&parser.state_formatted(), 2, 24);
    assert!(replayed.state_eq(&parser));
}

#[test]
fn serialized_contents_restore_wrap_before_painting() {
    // Pinned by tui-snap's tool_qualification.rs: state_formatted/state_diff
    // must restore wrap (?7h) before painting even when the destination
    // disabled it, and restore the target's own wrap state after.
    for target_disabled in [false, true] {
        let mut before = DamageGrid::new(3, 8, 0);
        before.process(b"\x1b[?7l");
        let mut after = DamageGrid::new(3, 8, 0);
        after.process(b"ABCDEFGHIJ");
        if target_disabled {
            after.process(b"\x1b[?7l");
        }
        for delta in [false, true] {
            let encoded = if delta {
                after.state_diff(&before)
            } else {
                after.state_formatted()
            };
            let replayed = replay_pair(&before, &encoded, 3, 8);
            assert!(
                replayed.state_eq(&after),
                "state mismatch (target_disabled={target_disabled}, delta={delta})\n\
                 replayed:\n{}\nafter:\n{}",
                replayed.dump().to_text(),
                after.dump().to_text(),
            );
        }
    }
}

#[test]
fn formatted_terminal_modes_preserve_autowrap_disable_and_restore() {
    // Pinned by tui-snap's tool_qualification.rs: input_mode_formatted
    // preserves a disabled autowrap; input_mode_diff restores it.
    let mut parser = DamageGrid::new(2, 8, 0);
    let mut original = DamageGrid::new(2, 8, 0);
    original.process(b"");
    parser.process(b"\x1b[?7l");
    let mut replayed = DamageGrid::new(2, 8, 0);
    replayed.process(&parser.input_mode_formatted());
    replayed.process(b"\x1b[1;8HABC");
    assert_eq!(replayed.cell(0, 7).expect("cell").contents(), "C");
    replayed.process(&original.input_mode_diff(&parser));
    replayed.process(b"D");
    assert_eq!(replayed.cell(1, 0).expect("cell").contents(), "D");
}

#[test]
fn contents_formatted_reproduces_pending_wrap_phantom() {
    let mut parser = DamageGrid::new(3, 8, 0);
    parser.process(b"ABCDEFGH"); // fills row 0, cursor parked at (0, 8)
    assert_eq!(parser.cursor_position(), (0, 8));
    let replayed = replay(&parser.contents_formatted(), 3, 8);
    assert_eq!(
        replayed.cursor_position(),
        (0, 8),
        "the pending-wrap phantom column is reproduced"
    );
    assert!(replayed.state_eq(&parser));
    // And the reproduced phantom behaves: the next printable wraps.
    let mut replayed = replayed;
    replayed.process(b"I");
    assert_eq!(replayed.cell(1, 0).expect("cell").contents(), "I");
}

#[test]
fn contents_formatted_reproduces_phantom_with_erased_last_cell() {
    // Pending wrap armed, then the last cell is erased without moving the
    // cursor (EL is not a cursor-moving op): the phantom must still round-trip.
    let mut parser = DamageGrid::new(3, 8, 0);
    parser.process(b"ABCDEFGH\x1b[2K");
    assert_eq!(parser.cursor_position(), (0, 8));
    assert_eq!(parser.cell(0, 7).expect("cell").contents(), "");
    let replayed = replay(&parser.contents_formatted(), 3, 8);
    assert_eq!(replayed.cursor_position(), (0, 8));
    assert!(replayed.state_eq(&parser));
    let mut replayed = replayed;
    replayed.process(b"Z");
    assert_eq!(
        replayed.cell(1, 0).expect("cell").contents(),
        "Z",
        "the restored phantom wraps on the next printable"
    );
}

#[test]
fn state_formatted_roundtrips_alt_screen_and_cursor_style() {
    let mut parser = DamageGrid::new(4, 10, 0);
    parser.process(b"primary\x1b[?1049h\x1b[2;2Halt\x1b[?25l\x1b[3 q\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b=\x1b[?1h");
    let replayed = replay(&parser.state_formatted(), 4, 10);
    assert!(replayed.state_eq(&parser));
    assert!(replayed.alternate_screen());
    assert!(replayed.hide_cursor());
    assert_eq!(replayed.cursor_style(), 3);
    assert!(replayed.application_keypad() && replayed.application_cursor());
    assert!(replayed.bracketed_paste());
}

#[test]
fn state_diff_roundtrips_alt_screen_switch_both_ways() {
    let mut before = DamageGrid::new(3, 8, 0);
    before.process(b"base");
    let mut after = DamageGrid::new(3, 8, 0);
    after.process(b"base");
    after.process(b"\x1b[?1049h\x1b[1;1H\x1b[31mred");
    let replayed = replay_pair(&before, &after.state_diff(&before), 3, 8);
    assert!(replayed.state_eq(&after), "entering alt screen");

    let replayed = replay_pair(&after, &before.state_diff(&after), 3, 8);
    assert!(replayed.state_eq(&before), "leaving alt screen");
}

#[test]
fn wide_chars_combining_and_zwj_roundtrip() {
    let mut parser = DamageGrid::new(3, 10, 0);
    parser.process("a界b\u{301}c👨\u{200d}👩\u{200d}👧d".as_bytes());
    let replayed = replay(&parser.state_formatted(), 3, 10);
    assert!(replayed.state_eq(&parser));
}

#[test]
fn zwj_final_cell_before_content_roundtrips() {
    // A ZWJ-final cluster with a later-written neighbor: the serializer must
    // jog the cursor so the neighbor does not join the cluster on replay.
    let mut parser = DamageGrid::new(3, 8, 0);
    parser.process("\x1b[1;2Hb\x1b[1;1Ha\u{200d}".as_bytes());
    assert_eq!(parser.cell(0, 0).expect("cell").contents(), "a\u{200d}");
    assert_eq!(parser.cell(0, 1).expect("cell").contents(), "b");
    let replayed = replay(&parser.state_formatted(), 3, 8);
    assert!(replayed.state_eq(&parser));
}

#[test]
fn wide_char_at_margin_roundtrips() {
    // A wide glyph written into the last column (orphan lead + phantom).
    let mut parser = DamageGrid::new(3, 8, 0);
    parser.process("\x1b[1;8H界".as_bytes());
    assert_eq!(parser.cursor_position(), (0, 8));
    let replayed = replay(&parser.state_formatted(), 3, 8);
    assert!(replayed.state_eq(&parser));
}

#[test]
fn blank_cells_with_background_roundtrip_via_erase_runs() {
    // BCE: blanks carrying a background color, including mid-row runs.
    let mut parser = DamageGrid::new(3, 10, 0);
    parser.process(b"\x1b[41mab\x1b[Kcd\x1b[0m");
    let replayed = replay(&parser.state_formatted(), 3, 10);
    assert!(replayed.state_eq(&parser));
    assert_eq!(
        replayed.cell(0, 3).expect("cell").bgcolor(),
        Color::Idx(1),
        "the erased run keeps its background"
    );
}

#[test]
fn scroll_content_and_soft_wraps_roundtrip() {
    let mut parser = DamageGrid::new(3, 8, 5);
    parser.process(b"aaaaaaaaaaaaaaaabbbb"); // wraps and scrolls
    let replayed = replay(&parser.state_formatted(), 3, 8);
    assert!(replayed.state_eq(&parser));
}

#[test]
fn decawm_off_state_roundtrips_through_input_mode() {
    let mut parser = DamageGrid::new(3, 8, 0);
    parser.process(b"\x1b[?7l\x1b[1;8HAB");
    let replayed = replay(&parser.input_mode_formatted(), 3, 8);
    assert!(
        !replayed.autowrap(),
        "input_mode_formatted preserves wrap-off"
    );
    let restored = replay(&parser.input_mode_diff(&replayed), 3, 8);
    assert!(
        restored.autowrap(),
        "diff against the off state restores wrap"
    );
}

#[test]
fn empty_grid_serializes_to_a_replayable_preamble() {
    let parser = DamageGrid::new(2, 4, 0);
    let replayed = replay(&parser.state_formatted(), 2, 4);
    assert!(replayed.state_eq(&parser));
}
