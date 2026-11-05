# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking:** an OSC 8 hyperlink is shared by every cell written under it.
  `Cell::hyperlink` is now `Option<Arc<Hyperlink>>`, `SnapCell` carries the
  same `Option<Arc<Hyperlink>>` in `hyperlink` in place of `hyperlink_id` and
  `hyperlink_uri`, and `Cell::hyperlink_id` is removed.
  `DamageGrid::hyperlink_target_at_content_row` reads the cell's own link, so a
  link no longer disappears when many other links have been written. A switch
  to or from the alternate screen ends the open link, as it already did for hover.
- `unicode-width` 0.2.3 is the minimum. It gives the halfwidth katakana voiced
  marks U+FF9E and U+FF9F a width of one column (0.2.2 gave them zero), so
  `display_width` no longer adds a column for each mark. A mark now prints in
  its own cell after its base, as xterm does, instead of joining the base and
  widening it to two columns. `display_width("\u{ff9e}")` is 1, not 2.

### Fixed

- `CSI Ps L`, `M`, `S` and `T` with a huge count (`CSI 65535 L`) ran one pass
  per unit of the count, so eight bytes held a pane for about a tenth of a
  second in a debug build. The count is now clamped to the rows from the cursor to the bottom
  margin (`L`, `M`) or to the scroll region height (`S`, `T`), as xterm does,
  and the recorded `ScrollOp` carries the clamped count. A scroll in a partial
  region (DECSTBM) now moves each row once instead of cloning every row of the
  region per scrolled line.
- Each cell used to hold its own copy of an OSC 8 hyperlink, so one 140 KB URI
  cost about 144 KB per printed byte. A cell now holds a pointer to one shared
  link. A link whose URI is longer than 8 KiB or whose `id=` is longer than 256
  bytes is refused: the text still prints, without a link, and any link that was
  open ends.
- A program that reopened the same OSC 8 link before each glyph or span still
  gave every cell its own copy. The grid now remembers its eight most recently
  opened links and reuses the matching one, so those cells share one link too.
- A cell now holds at most 16 characters after its base (combining marks,
  variation selectors, joiners, and the characters a joiner pulls in). Output
  that never stops sending them grew one cell, and the cost of every join, without
  bound. Marks past the limit are dropped; a printable that follows a joiner in
  a full cluster starts the next cell. The longest emoji ZWJ sequence (10
  characters) is unchanged.
- `CSI ! p` (DECSTR) at the right margin, and a wide glyph suppressed by
  `CSI ? 7 l`, no longer leave the cursor in the phantom column without a
  deferred wrap, where the next printable was dropped and later marks joined
  the last cell.

## [0.1.1] - 2026-11-03

### Added

- `EraseDisplayPolicy` and `DamageGrid::set_erase_display_policy` /
  `erase_display_policy`: choose whether a full-screen erase (`CSI 2 J`, and
  `CSI J` at the home cursor) on the primary screen saves the cleared rows into
  scrollback. The default, `SaveToScrollback`, keeps the 0.1.0 behaviour;
  `Discard` clears without pushing, as xterm and `vt100` do. `CSI 3 J`, the
  alternate screen and normal scrolling are unchanged.

## [0.1.0] - 2026-09-12

First standalone release. Extracted from the [`jackin-project/jackin`](https://github.com/jackin-project/jackin) monorepo, where the crate lived as `crates/jackin-term` (the owned terminal model of the jackin❯ Capsule PTY multiplexer); history preserved via `git filter-repo`. Renamed `jackin-term` → `termpane` (lib `jackin_term` → `termpane`). The extraction itself carries no behavior changes vs the monorepo source; this release additionally lands the tui-snap parity surface listed below.

### Added

- DECAWM (`CSI ?7 h/l`) modeled with vt100-fork print semantics: wrap-off
  overwrites at the clamped right column, a wide glyph that cannot fit is
  suppressed, combining marks still join, re-enabling restores wrap.
  Serialized in every replay form (`contents_*`, `input_mode_*`, `state_*`).
- DECSED/DECSEL (`CSI ? J` / `CSI ? K`) distinct parse arms with plain-erase
  parity (upstream vt100 has no protection model; documented as parity).
- Bell: BEL emits typed `PassthroughEvent::Bell` (one per byte, in order),
  never a cell side effect.
- `CSI ?12` text-cursor-enable tracked as tri-state `Option<bool>`
  (`text_cursor_enable()`), plus application keypad (`ESC =` / `ESC >`) and
  DEC 2026 synchronized-update (`in_synchronized_update()`) tracking.
- Replayable serialization API: `contents_formatted`, `contents_diff`,
  `input_mode_formatted`, `input_mode_diff`, `state_formatted`, `state_diff`,
  and `state_eq` replay contract (exact reproduction incl. DECAWM, phantom
  pending-wrap cursor, bracketed paste, mouse mode/encoding, app cursor
  keys, alt screen, cursor visibility/style). Covered by unit pins and a
  proptest property; fuzz target asserts one-shot vs byte-split equality
  (incl. `mid_sequence`) and `state_formatted`/`state_diff` round-trips.
- Harness surface: `mid_sequence()` parser ground-state scanner (vte 0.15
  exposes none), public `decrqm_status()` for every tracked DEC mode
  (declines 2027, 0 for untracked/ANSI), `set_size()` content-preserving
  resize, scrollback offset views, documented phantom pending-wrap
  `cursor_position()` (adapter clamps to `cols-1`).
- `proptest` dev-dependency for the serialization round-trip property.
- Live PTY sessions (`termpane::session`, Unix-only `pty` feature): spawns
  `SpawnParams` into a PTY, pumps output through `DamageGrid` on a worker
  thread, and routes emulator replies (DA/DSR/DECRQM answers) back to PTY
  stdin automatically. Covers `TERM`/`COLORTERM` defaulting from
  `VirtualTerminalProfile`, atomic PTY+grid resize, signals via the
  transport, `poll_exit`/`wait_exit` with trailing-output drain,
  `close_input` (EOF), graceful `finish` and forceful idempotent `close`
  (all teardown bounded; `Drop` never hangs), plus `output_log` recording so
  live state can be verified against byte replay via
  `state_formatted`/`state_eq`. Hermetic conformance suite in
  `tests/session.rs`.

### Clarified

- `CSI ?12` (text-cursor-enable), DEC 2026 (synchronized update), and DEC 1004
  (focus events) are tracked + queryable via live getters but intentionally
  outside every replay form (`contents_*`, `input_mode_*`, `state_*`) and
  `state_eq` — by design; the conformance/fuzz harnesses read the live getters.
- Invalid ED/EL/DECSED/DECSEL modes (`CSI 5J`, `CSI ?5J`, `CSI 5K`, `CSI ?5K`)
  are swallowed silently (upstream unhandled-hook parity, no `DroppedCsi`);
  pinned by unit test, exercised for determinism by
  `tests/fixtures/basic/decsed-decsel.vt`.
- DECRQM on never-set `?12` reports reset (tri-state `None` reads as reset).
- `?6` DECOM is absorbed as a non-goal (no origin-mode emulation).
- Scrollback rows keep their capture widths across `set_size` (resize preserves
  content, does not re-wrap history).
