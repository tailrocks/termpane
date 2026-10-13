# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - 2026-09-11

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

## [0.6.4] - 2026-09-11

### Changed

- Extracted from the [`jackin-project/jackin`](https://github.com/jackin-project/jackin) monorepo, where the crate lived as `crates/jackin-term` (the owned terminal model of the jackin❯ Capsule PTY multiplexer). History preserved via `git filter-repo`.
- Renamed the crate `jackin-term` → `termpane` (lib `jackin_term` → `termpane`).
- No behavior changes: this is the first standalone release, bit-identical in logic to the monorepo source at extraction.
