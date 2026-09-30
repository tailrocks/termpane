// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Atomic session observation: one complete owned view at one worker revision.
//!
//! [`Observation`] is the F06 answer to `GridSnapshot`'s partial state (cells
//! plus cursor position and one mode bit) and to the grid's one-fact-at-a-time
//! accessors: the worker assembles grid, cursor, color, mode, capability,
//! completeness, and diagnostic facts into a single owned value without
//! interleaving pumps, so every field describes the same [`DamageGrid`] moment.
//! Consumers never parse terminal bytes themselves.
//!
//! [`DamageGrid`]: crate::grid::DamageGrid

use crate::grid::{MouseProtocolEncoding, MouseProtocolMode};
use crate::process::ExitStatus;
use crate::snapshot::GridSnapshot;
use crate::{Color, Osc8Policy, SupportedSgr, VirtualTerminalProfile};

/// Full cursor state at the observation revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Cursor position `(row, col)`.
    ///
    /// Raw parser state, like
    /// [`DamageGrid::cursor_position`](crate::grid::DamageGrid::cursor_position):
    /// while a deferred DECAWM wrap is pending the column is the *phantom*
    /// column `== cols`. Clamp with `col.min(cols - 1)` for a physical cell.
    pub position: (u16, u16),
    /// Whether the cursor is visible (DECTCEM on).
    pub visible: bool,
    /// DECSCUSR cursor style requested by the program (`0` = default).
    pub style: u16,
    /// DEC mode 12 (text cursor enable/blink) tri-state: `None` until the
    /// program sets or resets it.
    pub text_cursor_enable: Option<bool>,
}

/// Full color state at the observation revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorState {
    /// OSC 10 foreground RGB the emulator reports to color queries.
    pub reported_fg: (u8, u8, u8),
    /// OSC 11 background RGB the emulator reports to color queries.
    pub reported_bg: (u8, u8, u8),
    /// Foreground color applied to newly written cells.
    pub current_fg: Color,
    /// Background color applied to newly written cells.
    pub current_bg: Color,
}

/// Full DEC/private mode state at the observation revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Eight orthogonal DEC private-mode bits plus a tri-state and two \
              small enums: the terminal mode surface is intrinsically a flag \
              set, and named fields keep observation assertions readable."
)]
pub struct ModeState {
    /// Alternate screen active (`?1049h`).
    pub alternate_screen: bool,
    /// Auto-wrap enabled (DECAWM, DEC 7 — the power-on default).
    pub autowrap: bool,
    /// Application cursor keys (DEC 1).
    pub application_cursor: bool,
    /// Application keypad (DECKPAM).
    pub application_keypad: bool,
    /// Bracketed paste (DEC 2004).
    pub bracketed_paste: bool,
    /// Focus-event reporting (DEC 1004).
    pub focus_events: bool,
    /// Mouse reporting mode (DEC 1000/1002/1003).
    pub mouse_mode: MouseProtocolMode,
    /// Mouse coordinate encoding (1005/1006/1015).
    pub mouse_encoding: MouseProtocolEncoding,
    /// Top of the kitty-keyboard enhancement stack (`0` when empty).
    pub kitty_keyboard: u32,
    /// Inside a synchronized update (DEC 2026, `?2026h`) *right now*.
    ///
    /// A point sample, not a frame proof: use
    /// [`Diagnostics::sync_frames_completed`] plus
    /// [`crate::session::PtySession::wait_frame`] to observe a complete
    /// on→off frame. "Mode currently off" never implies "a frame completed".
    pub in_synchronized_update: bool,
    /// The parser may be mid-escape-sequence (or hold an incomplete UTF-8
    /// tail): more input is expected to complete the pending sequence.
    pub mid_sequence: bool,
}

/// Static emulator capabilities: the deterministic model-only defaults.
///
/// Always equal to [`VirtualTerminalProfile::default`]; live program output
/// never changes it. Carried on every observation so consumers can branch on
/// capability facts without importing the model profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Width-table source identifier (crate + version).
    pub unicode_version: &'static str,
    /// Mode-2027 grapheme-cluster width active (inert, legacy default).
    pub grapheme_cluster_width_mode: bool,
    /// East Asian Ambiguous code points treated as two columns.
    pub ambiguous_width_is_wide: bool,
    /// DECRQM reply for private mode 2027.
    pub decrqm_mode_2027_status: u16,
    /// `$TERM` advertised to the child PTY environment.
    pub agent_term: &'static str,
    /// `$COLORTERM` advertised to the child PTY environment.
    pub agent_colorterm: &'static str,
    /// How OSC 8 hyperlinks are modeled.
    pub osc8_policy: Osc8Policy,
    /// SGR features the virtual terminal claims to support.
    pub supported_sgr: SupportedSgr,
}

impl Capabilities {
    /// The model-only defaults every observation carries.
    #[must_use]
    pub fn model_defaults() -> Self {
        Self::from(VirtualTerminalProfile::default())
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::model_defaults()
    }
}

impl From<VirtualTerminalProfile> for Capabilities {
    fn from(profile: VirtualTerminalProfile) -> Self {
        Self {
            unicode_version: profile.unicode_version,
            grapheme_cluster_width_mode: profile.grapheme_cluster_width_mode,
            ambiguous_width_is_wide: profile.ambiguous_width_is_wide,
            decrqm_mode_2027_status: profile.decrqm_mode_2027_status,
            agent_term: profile.agent_term,
            agent_colorterm: profile.agent_colorterm,
            osc8_policy: profile.osc8_policy,
            supported_sgr: profile.supported_sgr,
        }
    }
}

/// How the child-output byte stream ended — distinct from the exit status.
///
/// The direct child's exit ([`ExitStatus`]) says how the *process* ended; this
/// says how much of its *output* the session captured. A clean exit with
/// [`Self::DrainExpired`] means bytes may still have been in flight when the
/// session stopped waiting — never read it as end-of-output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// Bytes may still arrive; no terminal stream condition observed.
    Streaming,
    /// Qualified end-of-output: PTY `Ok(0)` or platform `EIO` after child
    /// exit, and nothing else. The only state that proves completeness.
    CleanEof,
    /// A non-EOF read failure ended the pump; the message carries the I/O
    /// error. `Interrupted` is retried, never reported here.
    ReadFailed(String),
    /// The drain grace elapsed after child exit without end-of-output: a
    /// deadline, not proof of EOF. Trailing bytes may exist but were not
    /// captured.
    DrainExpired,
    /// Teardown aborted the capture before any stream condition was observed.
    Aborted,
}

/// Liveness/completeness facts at the observation revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completeness {
    /// Direct child's exit status once the worker reaped it.
    pub exit: Option<ExitStatus>,
    /// Output-stream completeness — see [`StreamState`].
    pub stream: StreamState,
    /// True once the final outcome was declared: the output log and stream
    /// state are frozen from here on (further reader bytes are dropped and
    /// counted, never applied). The grid itself stays resizable.
    pub finalized: bool,
}

/// Final capture outcome: exit, stream completeness, and counters, declared
/// once and immutable afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// How the direct child terminated.
    pub exit: ExitStatus,
    /// How the output stream ended — see [`StreamState`].
    pub stream: StreamState,
    /// Worker revision at declaration; the output log never changes past this
    /// revision (the grid stays resizable, which advances the revision).
    pub revision: u64,
    /// Output bytes pumped into the grid over the session lifetime.
    pub bytes_pumped: u64,
    /// Reader batches dropped after finalization (always 0 here: the outcome
    /// is declared before any post-final byte can arrive).
    pub dropped_after_drain: u64,
}

/// Lifetime I/O and truncation diagnostics at the observation revision.
///
/// Every bound the session enforces reports here; nothing is silently lost.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagnostics {
    /// Output bytes pumped into the grid so far.
    pub bytes_pumped: u64,
    /// Reader batches (`Feed` messages) processed so far.
    pub feeds: u64,
    /// Reader batches dropped after finalization (declared-drained output
    /// stays immutable, so late bytes are counted, never applied).
    pub dropped_after_drain: u64,
    /// Passthrough events currently stashed for `drain_events`.
    pub events_stashed: u64,
    /// Passthrough events dropped by the event cap (oldest first).
    pub events_dropped: u64,
    /// Output-log bytes retained (0 unless recording was enabled).
    pub output_bytes_kept: u64,
    /// Output bytes seen by the recorder (equals `bytes_pumped` while
    /// recording; the gap to `output_bytes_kept` is truncation).
    pub output_bytes_total: u64,
    /// True once the output log hit its byte cap: the log holds the oldest
    /// prefix only and never grows again.
    pub output_truncated: bool,
    /// Terminal replies (DA/DSR/DECRQM answers) routed to PTY stdin.
    pub replies_routed: u64,
    /// Replies dropped because stdin was already closed.
    pub replies_dropped_no_writer: u64,
    /// Failed terminal-reply writes; the first error is retained below.
    pub reply_write_errors: u64,
    /// First terminal-reply write failure, if any.
    pub reply_first_error: Option<String>,
    /// Completed synchronized-update frames (DEC 2026 on→off transitions).
    /// Monotonic; the only honest frame counter — see
    /// [`ModeState::in_synchronized_update`].
    pub sync_frames_completed: u64,
    /// Resize operations applied so far.
    pub resizes: u64,
    /// Whether the child's stdin writer is still open.
    pub stdin_open: bool,
    /// Batches currently queued behind the worker (bounded by the data
    /// channel depth; proves backpressure instead of unbounded growth).
    pub queued_batches: usize,
}

/// One complete owned observation of a live session at one worker revision.
///
/// Assembled by the worker without interleaving pumps: `grid`, `cursor`,
/// `colors`, and `modes` all describe revision [`Self::revision`]. Pair with
/// [`crate::session::PtySession::wait_revision`] to observe across resizes and
/// repaints without races.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Worker revision: bumped on every feed batch, resize, color update, and
    /// stdin close. Monotonic within a session.
    pub revision: u64,
    /// Cells, cursor position, and alternate-screen flag at this revision.
    pub grid: GridSnapshot,
    /// Full cursor state at this revision.
    pub cursor: CursorState,
    /// Full color state at this revision.
    pub colors: ColorState,
    /// Full mode state at this revision.
    pub modes: ModeState,
    /// Static emulator capabilities (model-only defaults).
    pub capabilities: Capabilities,
    /// Liveness/completeness facts at this revision.
    pub completeness: Completeness,
    /// Lifetime I/O and truncation diagnostics at this revision.
    pub diagnostics: Diagnostics,
}
