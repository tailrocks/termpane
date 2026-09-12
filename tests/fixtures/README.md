# Corpus Fixtures — termpane Conformance Harness

Each file in the subdirectories below is a raw byte sequence fed to the conformance harness
(`tests/conformance.rs`). The harness feeds identical bytes to `DamageGrid` in one chunk and
byte-by-byte, then asserts identical final grids (cells, full attrs, cursor, alt-screen flag,
and tracked DEC modes — see Oracle coverage below).

## Format

- `.bin` — raw bytes (binary PTY capture)
- `.vt` — VT/ANSI escape sequences in a text-safe encoding: LF-delimited lines,
  kebab-case filenames, geometry-safe at 24x80, `\xNN` hex escapes for non-printable
  bytes (e.g. `\x1b` for ESC, `\x07` for BEL), printable ASCII/UTF-8 inline
  (CJK `界` stays inline UTF-8)
- `.cast` — asciinema v2 JSONL; the harness replays every output (`"o"`) event

## Parity follow-up fixtures (0.1.0)

| Fixture | Concern |
|---|---|
| `basic/decawm-off-overwrite.vt` | `?7l` + `1;80HABC` overwrite at the clamped right column, plus a `?7h` re-enable variant line |
| `wide_chars/decawm-off-wide-suppress.vt` | `?7l` + `2;80H界` suppressed wide glyph (no fit), plus fit-control `2;79H界` |
| `basic/decsed-decsel.vt` | Text + `?K` / `?2J` plain-erase parity, plus invalid `?5J` / `?5K` / `5J` / `5K` (silent-swallow; correctness pinned by unit test, harness asserts determinism only) |
| `basic/bell.vt` | `a BEL b BEL BEL` — Bell is passthrough-only, no cell side effect |
| `basic/text-cursor-enable.vt` | `?12h X ?12l Y` tri-state tracking |
| `basic/serialization-modes.vt` | Mode cocktail (`?7l`, `ESC =`, `?1h`, `?2004h`, `?1002h`, `?1006h`, `?1049h`, `?25l`, DECSCUSR `3q`) + SGR pen + margin write |

The `bell` / `?12` / `modes` fixtures passed vacuously under the old 4-attr oracle
(cells/cursor/alt only); they are determinism guards, with correctness pinned by
unit tests (`bel_emits_typed_event_without_cell_side_effects`,
`text_cursor_enable_is_tri_state`, serialization round-trips).

## Oracle coverage

- Cells: contents, `is_wide` / `is_wide_continuation`, fg/bg colors.
- Attrs: full 10-bit SGR set — `bold`, `conceal`, `dim`, `inverse`, `italic`,
  `overline`, `rapid_blink`, `slow_blink`, `strikethrough`, `underline`
  (was 4-attr: `bold` / `italic` / `underline` / `inverse`).
- Modes: `autowrap`, `application_keypad`, `application_cursor`,
  `bracketed_paste`, `mouse_mode` (`mouse_protocol_mode`),
  `mouse_encoding` (`mouse_protocol_encoding`), `focus_events`,
  `text_cursor_enable`, `in_synchronized_update`, `cursor_style`,
  `scrollback_len`, `mid_sequence`, plus `alternate_screen`.
- Limits: the harness asserts one-shot vs byte-split equality only. It does not
  assert Bell counts (covered by the fuzz target's `drain_passthrough` equality),
  nor replay-form scope (`?12` / `2026` / focus are tracked + queryable but
  intentionally outside every replay form and `state_eq` — the harness reads live
  getters).

## Corpus categories

| Directory | Coverage goal |
|---|---|
| `basic/` | Plain text, cursor movement, SGR colors, line/screen clear |
| `wide_chars/` | CJK ideographs, emoji, combining marks, wide-char continuation cells |
| `resize/` | Content under resize (Defect 44 regression class) |
| `scrollback/` | Scrollback fill, clear-scrollback (`CSI 3J`), alternate screen |
| `alt_screen/` | Alternate screen enter/exit, content in both screens |
| `vttest/` | Representative VT conformance sequences derived from vttest classes |
| `esctest/` | Representative CSI/DEC/SGR sequences derived from esctest classes |
| `real/` | Real CLI/TUI PTY-output captures that are geometry-safe at 24x80 (`claude`, `codex`, `vim`, `htop`, `tmux`) |
| `asciinema/` | Asciinema v2 `.cast` files; output events are replayed through the harness |
| `tool_archetypes/` | Tool-shaped fixtures for binaries unavailable in this environment |
| `pathological/` | High-volume: `seq 1 100000` tail windows, `yes` flood, full-screen redraw storms |

## Adding fixtures

Capture a real PTY stream during a `claude` / `codex` / `vim` / `htop` / `tmux` session:

```sh
# Record a PTY session to a binary file.
script -q -F ~/fixtures/session.bin
# ... do the thing ...
exit

# Or capture just the output bytes:
strace -e write -p <pty-pid> -o /tmp/trace.txt
```

Name the fixture descriptively (`claude-compact-mode.bin`, `vim-syntax-heavy.bin`, etc.)
and commit it to the appropriate subdirectory.

## vttest / esctest sequences

The `vttest/` and `esctest/` directories hold committed representative sequences from those
conformance families. When importing larger upstream slices, keep each fixture geometry-safe at
24x80 or add a dedicated test with the required geometry.
