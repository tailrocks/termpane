# termpane

Deterministic terminal screen-state emulator: a VT-parsed cell grid with typed attributes, scrollback, snapshots, and damage tracking.

`termpane` turns a byte stream from a terminal program into an owned, in-memory model of the screen: feed bytes in, read the grid, the cursor, the modes, and exactly which rows changed. It is the model layer for terminal multiplexers, snapshot tests, and headless renderers — it does not draw pixels and it never talks to a host terminal.

The pipeline is:

```text
program bytes
  -> vte::Parser            (the canonical VT/ANSI parser state machine)
  -> termpane::DamageGrid   (parser-perform sink: grid mutation + damage)
  -> GridView / GridSnapshot / GridPatch observation APIs
```

## What it owns

- **Parser-perform sink over `vte` 0.15**: bytes → grid mutation + typed passthrough events. The parser is persistent across `process()` calls and buffers incomplete UTF-8 sequences split across chunk boundaries.
- **`DamageGrid` cell model**: cursor, modes, styles, alternate screen, scrollback, and dirty-row damage recorded at mutation time — no snapshot diffing to discover what changed.
- **Typed cell attributes**: bold/italic/underline/blink/conceal/overline family, underline style *and* underline color, and `Color` as `Default` / `Idx` / `Rgb`.
- **Unicode-correct cells**: grapheme clustering (combining marks, variation selectors, ZWJ join the previous cell) and wide-cell handling with orphaned-continuation blanking.
- **Scrollback**: primary/alternate screens, preserve-on-clear retention with exact dedupe, and clamped offset views (`scrollback_view`, `scrollback_rows_at_offset`).
- **OSC 8 hyperlinks** as cell metadata, with a policy hook for filtering unsafe URIs.
- **Terminal replies a real program expects**: DECRQM, DA, and DSR are answered from grid state; OSC 10/11 default-color queries are answered with configurable reported colors.
- **Snapshots + damage tracking**: `GridSnapshot` (owned, with `to_text()`), borrowed `GridView`, and `GridPatch` dirty-span dumps.

## Guarantees

- **Zero `unsafe`** (`#![forbid(unsafe_code)]` via workspace lints), pure Rust, no FFI.
- **Deterministic**: no wall-clock reads, no RNG, no host-side effects — identical input bytes produce identical grid state, every run.
- **Chunk-boundary safe**: the conformance harness replays every corpus fixture both one-shot and byte-by-byte and asserts identical final screen state.
- **Fuzz-covered**: the `damage_grid_process` target feeds arbitrary bytes to the grid asserting no panic and stable invariants.

## Quick start

```rust
use termpane::DamageGrid;

let mut grid = DamageGrid::new(24, 80, 1_000); // rows, cols, scrollback limit
grid.process(b"hello\x1b[1;31m red \x1b[0mworld\r\n");
grid.process(b"\x1b[3;10Hhere"); // cursor to row 3, col 10

let (row, col) = grid.cursor_position();
println!("cursor at {row}:{col}");

let snapshot = grid.dump();          // owned GridSnapshot
let text = snapshot.to_text();       // plain-text rendering
let dirty = grid.dump_dirty_patch(); // GridPatch: only what changed
```

## Requirements

Rust 1.97 or newer (MSRV, pinned by `rust-toolchain.toml`).

## Verify

```sh
cargo nextest run                                        # 161 tests (114 unit + 43 conformance + 4 serialization_proptest)
cargo nextest run --all-features                         # +2 dhat allocation tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fuzz run damage_grid_process -- -max_total_time=30 # bounded fuzz smoke
cargo bench --bench resize_storm -- --quick
cargo bench --bench scroll_throughput -- --quick
cargo bench --bench present_frame -- --quick
cargo bench --bench preserve_scrollback -- --quick
```

The conformance corpus lives in [`tests/fixtures/`](tests/fixtures) (vttest/esctest excerpts, real tool captures, pathological streams); the replay harness is [`tests/conformance.rs`](tests/conformance.rs).

## History

`termpane` was extracted from the [`jackin-project/jackin`](https://github.com/jackin-project/jackin) monorepo, where it lived as `crates/jackin-term` — the owned terminal model of the jackin❯ Capsule re-emitting PTY multiplexer. The git history is preserved (extraction via `git filter-repo`); design rationale and the retire-`vt100` record live in that repository's `docs/content/reference/capsule/terminal-model.mdx` (link kept; vendoring declined). The 0.1.0 release is the first standalone release; the extraction itself carried no behavior changes.

## License

Apache-2.0. Copyright 2026 Alexey Zhokhov. See [LICENSE](LICENSE); this repository is [REUSE](https://reuse.software/)-compliant.
