// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Live PTY sessions: a child process, its emulator grid, and the pump threads.
//!
//! [`PtySession::spawn`] builds on the [`crate::pty`] transport: it spawns
//! [`SpawnParams`] into a PTY and starts two
//! threads: a **reader thread** that blocks on the PTY master and forwards
//! byte batches over a bounded data channel, and a **worker thread** (see the
//! crate-private `session_worker` module) that owns the
//! [`DamageGrid`](crate::grid::DamageGrid) and the PTY writer. All grid access
//! happens on the worker; the handle sends small ops over a separate control
//! channel and receives replies, so the session is `Send + Sync` with no
//! `unsafe` anywhere.
//!
//! The worker routes emulator replies ([`Reply`](crate::PassthroughEvent::Reply)
//! events: DA/DSR/DECRQM answers) back to PTY stdin automatically, counts and
//! retains any routing failures, and stashes every other passthrough event
//! (bounded, oldest dropped first) for [`PtySession::drain_events`].
//!
//! ## Cleanup
//!
//! [`PtySession::finish`] (graceful: EOF stdin, wait, reap) and
//! [`PtySession::close`] (forceful, idempotent) return teardown errors.
//! Teardown sets a shutdown flag, kills the child through the shared handle —
//! which unblocks any in-flight PTY write with `EIO` — and then really joins
//! both threads: a stuck thread is never detached. `Drop` runs the same
//! teardown without double-panicking.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Instant;

use crate::passthrough::PassthroughEvent;
use crate::process::{ExitStatus, SpawnParams};
use crate::pty::spawn_pty;
use crate::session_worker::{
    CtlOp, DATA_QUEUE_BATCHES, DEFAULT_EVENT_CAP, DEFAULT_OUTPUT_CAP_BYTES, DataMsg, REPLY_TIMEOUT,
    Shared, SharedChild, StartupFaults, StartupGuard, WorkerConfig, run_reader, run_worker,
};
use crate::snapshot::GridSnapshot;
use crate::width::VirtualTerminalProfile;

pub use crate::session_observe::{
    Capabilities, CaptureOutcome, ColorState, Completeness, CursorState, Diagnostics, ModeState,
    Observation, StreamState,
};

// ---------------------------------------------------------------------------
// Limits and timing
// ---------------------------------------------------------------------------

/// Smallest session width, in columns.
pub const MIN_COLS: u16 = 1;
/// Smallest session height, in rows.
pub const MIN_ROWS: u16 = 1;
/// Largest session width, in columns.
pub const MAX_COLS: u16 = 1000;
/// Largest session height, in rows.
pub const MAX_ROWS: u16 = 1000;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Session failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    /// Spawn failed (empty argv, bad geometry, PTY/worker startup, ...).
    Spawn(String),
    /// Invalid argument (bad size, empty input, ...).
    InvalidInput(String),
    /// PTY I/O failure.
    Io(String),
    /// Deadlines: child still alive / worker unresponsive.
    Timeout(String),
    /// Input or observation refused: the child already exited.
    ChildExited(String),
    /// Session is closed (or its worker is gone).
    Closed(String),
    /// Teardown itself failed (kill/reap/join errors from finish/close).
    Teardown(String),
    /// Signal delivery failed.
    Signal(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "spawn failed: {m}"),
            Self::InvalidInput(m) => write!(f, "invalid input: {m}"),
            Self::Io(m) => write!(f, "pty io: {m}"),
            Self::Timeout(m) => write!(f, "timeout: {m}"),
            Self::ChildExited(m) => write!(f, "child exited: {m}"),
            Self::Closed(m) => write!(f, "session closed: {m}"),
            Self::Teardown(m) => write!(f, "teardown failed: {m}"),
            Self::Signal(m) => write!(f, "signal failed: {m}"),
        }
    }
}

impl std::error::Error for ProcessError {}

// ---------------------------------------------------------------------------
// Signal
// ---------------------------------------------------------------------------

/// Process signal for [`PtySession::signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGINT`.
    Int,
    /// `SIGTERM`.
    Term,
    /// `SIGKILL`.
    Kill,
    /// `SIGQUIT`.
    Quit,
    /// `SIGHUP`.
    Hup,
}

impl Signal {
    /// Signal number, via the transport constants (values taken from `nix`,
    /// never handwritten).
    fn number(self) -> i32 {
        match self {
            Self::Int => crate::process::SIGINT,
            Self::Term => crate::process::SIGTERM,
            Self::Kill => crate::process::SIGKILL,
            Self::Quit => crate::process::SIGQUIT,
            Self::Hup => crate::process::SIGHUP,
        }
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Spawn options for [`PtySession::spawn`]: geometry, grid, and recording.
///
/// The child command itself (argv, environment, cwd) travels on
/// [`SpawnParams`]; these options cover only
/// what the session layer adds on top of the transport.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// Initial PTY/emulator width, in columns.
    pub cols: u16,
    /// Initial PTY/emulator height, in rows.
    pub rows: u16,
    /// `TERM` value exported to the child when the spawn params carry no
    /// `TERM` override.
    pub term: String,
    /// `COLORTERM` value exported to the child when the spawn params carry
    /// no `COLORTERM` override.
    pub colorterm: String,
    /// Grid scrollback limit, in lines.
    pub scrollback: usize,
    /// Record pumped output bytes for [`PtySession::output_log`].
    /// Off by default: the log retains up to `output_cap_bytes`.
    pub record_output: bool,
    /// Cap on stashed passthrough events for [`PtySession::drain_events`]:
    /// past this many undrained events the oldest are dropped first. Every
    /// drop is counted in [`Diagnostics::events_dropped`], so the bound is
    /// never silent. At least 1 is always kept.
    pub event_cap: usize,
    /// Cap on recorded output-log bytes: the log keeps the oldest prefix up
    /// to this size, then freezes with [`Diagnostics::output_truncated`] set.
    /// [`Diagnostics::output_bytes_total`] still reports every byte seen.
    pub output_cap_bytes: usize,
}

impl Default for SessionOptions {
    fn default() -> Self {
        let profile = VirtualTerminalProfile::default();
        Self {
            cols: 80,
            rows: 24,
            term: profile.agent_term.to_owned(),
            colorterm: profile.agent_colorterm.to_owned(),
            scrollback: 1000,
            record_output: false,
            event_cap: DEFAULT_EVENT_CAP,
            output_cap_bytes: DEFAULT_OUTPUT_CAP_BYTES,
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Owned PTY session: the child, its emulator grid, and both I/O threads.
/// `Send + Sync`; concurrent sessions are fully independent.
#[derive(Debug)]
pub struct PtySession {
    ctl_tx: Mutex<Option<mpsc::Sender<CtlOp>>>,
    shutdown: Arc<AtomicBool>,
    killer: SharedChild,
    shared: Arc<Shared>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    closed: AtomicBool,
    pid: Option<u32>,
}

/// True when the spawn params already override `key` (an ASCII name, so a
/// non-UTF-8 override key can never match).
fn overrides_key(params: &SpawnParams, key: &str) -> bool {
    params
        .env_overrides()
        .iter()
        .any(|(k, _)| k.to_str() == Some(key))
}

impl PtySession {
    /// Spawn `params` in a new PTY and start the session threads.
    ///
    /// `TERM`/`COLORTERM` default from `options` unless the params already
    /// override them; `detached` on the params is accepted and implied (PTY
    /// children always start a new session with a controlling terminal).
    ///
    /// Every post-spawn failure — reader, writer, initial poll, thread spawn,
    /// worker handshake — rolls back through one guard: the child is killed
    /// and reaped and any started thread is joined, so a failed `spawn` never
    /// leaks a process or a thread.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Spawn`] when the geometry is out of range,
    /// `argv` is empty, or the PTY/child/threads fail to start.
    pub fn spawn(params: &SpawnParams, options: SessionOptions) -> Result<Self, ProcessError> {
        let mut pid_report = None;
        Self::spawn_with_faults(params, options, StartupFaults::none(), &mut pid_report)
    }

    /// Spawn with fault injection and pid reporting, for rollback tests.
    ///
    /// `spawned_pid` receives the child's pid as soon as `spawn_pty` succeeds,
    /// so tests can prove no-survivors after an injected failure. Production
    /// callers use [`PtySession::spawn`].
    pub(crate) fn spawn_with_faults(
        params: &SpawnParams,
        options: SessionOptions,
        faults: StartupFaults,
        spawned_pid: &mut Option<u32>,
    ) -> Result<Self, ProcessError> {
        let (cols, rows) = (options.cols, options.rows);
        if !(MIN_COLS..=MAX_COLS).contains(&cols) {
            return Err(ProcessError::Spawn(format!(
                "cols {cols} outside backend range {MIN_COLS}..={MAX_COLS}"
            )));
        }
        if !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            return Err(ProcessError::Spawn(format!(
                "rows {rows} outside backend range {MIN_ROWS}..={MAX_ROWS}"
            )));
        }
        if params.argv().is_empty() {
            return Err(ProcessError::Spawn("empty argv".to_owned()));
        }
        let mut effective = params.clone();
        if !overrides_key(params, "TERM") {
            effective = effective.env("TERM", &options.term);
        }
        if !overrides_key(params, "COLORTERM") {
            effective = effective.env("COLORTERM", &options.colorterm);
        }

        // One transport call: open + spawn + parent-slave-drop under the
        // lifecycle lock, so EOF discipline is correct from the start.
        let (master, child) = spawn_pty(&effective, cols, rows)
            .map_err(|e| ProcessError::Spawn(format!("pty spawn failed: {e}")))?;
        let killer = SharedChild::new(child);
        *spawned_pid = killer.pid();
        // From here on, every early return drops the guard, which kills and
        // reaps the child (and joins the worker once it runs).
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut guard = StartupGuard::armed(killer.clone(), Arc::clone(&shutdown));

        // Drain discipline: take I/O handles before any wait can run.
        if faults.fail_reader {
            return Err(ProcessError::Spawn("injected reader failure".to_owned()));
        }
        let reader = master
            .try_clone_reader()
            .map_err(|e| ProcessError::Spawn(format!("pty reader failed: {e}")))?;
        if faults.fail_writer {
            return Err(ProcessError::Spawn("injected writer failure".to_owned()));
        }
        let writer = master
            .take_writer()
            .map_err(|e| ProcessError::Spawn(format!("pty writer failed: {e}")))?;
        if faults.fail_poll {
            return Err(ProcessError::Spawn(
                "injected child-poll failure".to_owned(),
            ));
        }
        killer
            .poll()
            .map_err(|e| ProcessError::Spawn(format!("child poll failed: {e}")))?;
        let pid = killer.pid();

        let (ctl_tx, ctl_rx) = mpsc::channel::<CtlOp>();
        let (data_tx, data_rx) = mpsc::sync_channel::<DataMsg>(DATA_QUEUE_BATCHES);
        let inflight = Arc::new(AtomicUsize::new(0));
        let worker_inflight = Arc::clone(&inflight);
        let shared = Arc::new(Shared::default());
        if faults.fail_worker_thread {
            return Err(ProcessError::Spawn(
                "injected worker-thread failure".to_owned(),
            ));
        }
        let worker_shared = Arc::clone(&shared);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_killer = killer.clone();
        let worker = std::thread::Builder::new()
            .name("termpane-session-worker".to_owned())
            .spawn(move || {
                run_worker(WorkerConfig {
                    master,
                    killer: worker_killer,
                    writer,
                    cols,
                    rows,
                    scrollback: options.scrollback,
                    record_output: options.record_output,
                    event_cap: options.event_cap,
                    output_cap_bytes: options.output_cap_bytes,
                    data_rx,
                    inflight: worker_inflight,
                    ctl_rx,
                    shutdown: worker_shutdown,
                    shared: worker_shared,
                });
            })
            .map_err(|e| ProcessError::Spawn(format!("worker spawn failed: {e}")))?;
        guard.set_worker(ctl_tx.clone(), worker);

        if faults.fail_reader_thread {
            return Err(ProcessError::Spawn(
                "injected reader-thread failure".to_owned(),
            ));
        }
        let reader_thread = std::thread::Builder::new()
            .name("termpane-session-reader".to_owned())
            .spawn(move || run_reader(reader, data_tx, inflight))
            .map_err(|e| ProcessError::Spawn(format!("reader spawn failed: {e}")))?;

        let worker = guard.take_worker();
        guard.defuse();
        if faults.fail_setup {
            // Simulate a dead worker before the handshake: the handshake must
            // fail and full teardown must still reap the child.
            drop(ctl_tx);
            let mut session = Self {
                ctl_tx: Mutex::new(None),
                shutdown,
                killer,
                shared,
                worker: Mutex::new(worker),
                reader: Mutex::new(Some(reader_thread)),
                closed: AtomicBool::new(false),
                pid,
            };
            let _ignored = session.close();
            return Err(ProcessError::Spawn(
                "injected setup failure: worker did not answer".to_owned(),
            ));
        }
        let session = Self {
            ctl_tx: Mutex::new(Some(ctl_tx)),
            shutdown,
            killer,
            shared,
            worker: Mutex::new(worker),
            reader: Mutex::new(Some(reader_thread)),
            closed: AtomicBool::new(false),
            pid,
        };
        // Round-trip the worker: the session is usable only once the worker
        // owns the grid and answers ops.
        if let Err(e) = session.snapshot() {
            let mut session = session;
            let _ignored = session.close();
            return Err(ProcessError::Spawn(format!("worker did not answer: {e}")));
        }
        Ok(session)
    }

    /// Direct child's PID, when the transport reports one.
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }

    /// True once teardown started ([`PtySession::close`] or `Drop`).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.shared.is_closed()
    }

    /// Current grid size as `(cols, rows)`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn size(&self) -> Result<(u16, u16), ProcessError> {
        let snapshot = self.snapshot()?;
        Ok((snapshot.cols, snapshot.rows))
    }

    /// Full grid snapshot: cells, cursor, and modes at one instant.
    ///
    /// See [`PtySession::observe`] for the complete single-revision
    /// observation (cursor, color, mode, capability, completeness, and
    /// diagnostic facts alongside the grid).
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn snapshot(&self) -> Result<GridSnapshot, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::Snapshot(tx))?;
        recv_value(rx, "snapshot")
    }

    /// One complete owned observation at one worker revision: grid, cursor,
    /// color, mode, capabilities, completeness, and diagnostics, all read
    /// without an interleaving pump. No terminal parsing is needed to consume
    /// it; pair with [`PtySession::wait_revision`] to observe across resizes
    /// and repaints without races.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn observe(&self) -> Result<Observation, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::Observe(tx))?;
        recv_value(rx, "observation")
    }

    /// Wait until the worker revision reaches `target` (see
    /// [`Observation::revision`]), the session closes, or `deadline` passes.
    /// Returns the current revision on success.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] on expiry, [`ProcessError::Closed`]
    /// when the session shut down first.
    pub fn wait_revision(&self, target: u64, deadline: Instant) -> Result<u64, ProcessError> {
        match self.shared.wait_revision(target, deadline) {
            Some(revision) => Ok(revision),
            None => {
                if self.shared.is_closed() {
                    Err(ProcessError::Closed(
                        "session closed before revision {target}".to_owned(),
                    ))
                } else {
                    Err(ProcessError::Timeout(format!(
                        "revision {target} not reached"
                    )))
                }
            }
        }
    }

    /// Wait until the worker completes a synchronized-update frame (DEC 2026
    /// on→off) after this call, the session closes, or `deadline` passes.
    /// Returns the completed-frame count on success.
    ///
    /// Only a completed frame satisfies this wait: a child that never opens a
    /// synchronized update (mode currently off the whole time) yields
    /// [`ProcessError::Timeout`], never a spurious success.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] on expiry, [`ProcessError::Closed`]
    /// when the session shut down first.
    pub fn wait_frame(&self, deadline: Instant) -> Result<u64, ProcessError> {
        let baseline = self.shared.frames();
        match self.shared.wait_frames_above(baseline, deadline) {
            Some(frames) => Ok(frames),
            None => {
                if self.shared.is_closed() {
                    Err(ProcessError::Closed(
                        "session closed before synced frame".to_owned(),
                    ))
                } else {
                    Err(ProcessError::Timeout(
                        "no synchronized-update frame completed".to_owned(),
                    ))
                }
            }
        }
    }

    /// Escape codes reproducing the entire live grid state (see
    /// [`DamageGrid::state_formatted`](crate::grid::DamageGrid::state_formatted)):
    /// replay into a same-size fresh grid for an exact copy.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn state_formatted(&self) -> Result<Vec<u8>, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::StateBytes(tx))?;
        recv_value(rx, "state snapshot")
    }

    /// Drain and return the grid's dirty-row set.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn dirty_spans(&self) -> Result<crate::damage::DirtySpans, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::DirtySpans(tx))?;
        recv_value(rx, "dirty spans")
    }

    /// Drain passthrough events the emulator produced (title, bell, clipboard,
    /// ...) since the last call. `Reply` events never appear here: the worker
    /// routes them to PTY stdin automatically.
    ///
    /// At most `event_cap` events are stashed; past that the oldest are
    /// dropped first and counted in [`Diagnostics::events_dropped`].
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn drain_events(&self) -> Result<Vec<PassthroughEvent>, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::Events(tx))?;
        recv_value(rx, "events")
    }

    /// Output bytes pumped so far, oldest prefix up to `output_cap_bytes`.
    /// Empty unless [`SessionOptions::record_output`] was set: replay into a
    /// same-size fresh grid to reproduce the live state byte-for-byte.
    ///
    /// The log freezes once the final outcome is declared or the cap is hit
    /// ([`Diagnostics::output_truncated`]); [`Diagnostics::output_bytes_total`]
    /// reports every byte seen regardless.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn output_log(&self) -> Result<Vec<u8>, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::OutputLog(tx))?;
        recv_value(rx, "output log")
    }

    /// Write raw bytes to the child's stdin.
    ///
    /// The write completes when the child consumes the bytes; it is chunked
    /// so teardown stays responsive, but one chunk still blocks while the
    /// child neither reads nor dies. Cancel via [`PtySession::close`]: the
    /// teardown kill unblocks the write with an I/O error.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ChildExited`] when the child already exited,
    /// [`ProcessError::Closed`] when stdin was closed, [`ProcessError::Io`]
    /// on PTY write failure.
    pub fn write_stdin(&self, bytes: &[u8]) -> Result<(), ProcessError> {
        if bytes.is_empty() {
            return Err(ProcessError::InvalidInput("empty stdin write".to_owned()));
        }
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::Write {
            bytes: bytes.to_vec(),
            reply: tx,
        })?;
        recv_reply(rx, "stdin write")
    }

    /// Resize the PTY and the emulator together (PTY ioctl first: if the
    /// kernel refuses, the emulator stays consistent).
    ///
    /// The resize applies between pumped output batches: it may overtake bytes
    /// still queued behind the worker. Pair with [`PtySession::wait_revision`]
    /// to order observations against it.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidInput`] for out-of-range geometry,
    /// [`ProcessError::Io`] when the PTY ioctl fails.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ProcessError> {
        if !(MIN_COLS..=MAX_COLS).contains(&cols) {
            return Err(ProcessError::InvalidInput(format!(
                "cols {cols} outside backend range {MIN_COLS}..={MAX_COLS}"
            )));
        }
        if !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            return Err(ProcessError::InvalidInput(format!(
                "rows {rows} outside backend range {MIN_ROWS}..={MAX_ROWS}"
            )));
        }
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::Resize {
            cols,
            rows,
            reply: tx,
        })?;
        recv_reply(rx, "resize")
    }

    /// Update the OSC 10/11 colors the emulator reports to color queries.
    /// `None` keeps the current value. Power-on defaults come from the model
    /// profile; every update advances the worker revision.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn set_reported_colors(
        &self,
        fg: Option<(u8, u8, u8)>,
        bg: Option<(u8, u8, u8)>,
    ) -> Result<(), ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::SetColors { fg, bg, reply: tx })?;
        recv_reply(rx, "set reported colors")
    }

    /// Deliver a signal to the direct child.
    ///
    /// The reaped check and the `kill` run atomically against the worker's
    /// reap polling (F03): once the child is reaped, no signal can be
    /// addressed at the pid again, so kernel pid reuse can never redirect it
    /// at an unrelated process.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ChildExited`] when the child already exited,
    /// [`ProcessError::Signal`] when delivery failed.
    pub fn signal(&self, signal: Signal) -> Result<(), ProcessError> {
        if self.shared.exit().is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        let pid = self.pid;
        match self.killer.signal_locked(signal.number()) {
            Ok(()) => Ok(()),
            Err(crate::process::SignalError::NotFound { .. }) => Err(ProcessError::ChildExited(
                format!("child {} no longer exists", pid.unwrap_or(0)),
            )),
            Err(crate::process::SignalError::UnknownPid) => {
                Err(ProcessError::Signal("child PID unknown".to_owned()))
            }
            Err(err) => Err(ProcessError::Signal(format!(
                "kill({}) failed: {err}",
                pid.unwrap_or(0)
            ))),
        }
    }

    /// Close the child's stdin: drops the PTY writer, which asks the backend
    /// to inject its EOF sequence (newline + `VEOF`). A second call reports
    /// [`ProcessError::Closed`].
    ///
    /// This is a canonical-mode EOF request, not a universal half-close:
    /// a child in canonical mode (`cat`) reads EOF and typically exits, but
    /// a child in raw mode sees the injected bytes as ordinary input and may
    /// keep running. Portable PTYs offer no true half-close; use
    /// [`PtySession::close`] to guarantee termination.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ChildExited`] when the child already exited,
    /// [`ProcessError::Closed`] when stdin is already closed.
    pub fn close_input(&self) -> Result<(), ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(CtlOp::CloseInput { reply: tx })?;
        recv_reply(rx, "close stdin")
    }

    /// Non-blocking exit poll: `Some` once the final outcome is declared
    /// (child reaped plus stream resolved), `None` while it runs. See
    /// [`PtySession::poll_outcome`] for the full outcome.
    #[must_use]
    pub fn poll_exit(&self) -> Option<ExitStatus> {
        self.shared.exit()
    }

    /// Non-blocking outcome poll: `Some` once the final outcome is declared —
    /// child exit plus stream completeness — `None` while capture runs.
    #[must_use]
    pub fn poll_outcome(&self) -> Option<CaptureOutcome> {
        self.shared.outcome()
    }

    /// Wait until the direct child exits, is reaped, and the output stream
    /// resolves (clean EOF, read failure, or drain-grace expiry) — or
    /// `deadline` passes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] on expiry,
    /// [`ProcessError::Closed`] when the session shut down without an exit.
    pub fn wait_exit(&self, deadline: Instant) -> Result<ExitStatus, ProcessError> {
        Ok(self.wait_outcome(deadline)?.exit)
    }

    /// Wait until the final capture outcome is declared — child reaped plus
    /// stream resolved — or `deadline` passes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] on expiry,
    /// [`ProcessError::Closed`] when the session shut down without an exit.
    pub fn wait_outcome(&self, deadline: Instant) -> Result<CaptureOutcome, ProcessError> {
        loop {
            if let Some(outcome) = self.shared.outcome() {
                return Ok(outcome);
            }
            if self.shared.is_closed() {
                return Err(ProcessError::Closed(
                    "session closed before child exit".to_owned(),
                ));
            }
            if Instant::now() >= deadline {
                return Err(ProcessError::Timeout("child still alive".to_owned()));
            }
            self.shared.wait_outcome_changed(deadline);
        }
    }

    /// Graceful shutdown: EOF stdin, wait for natural exit until `deadline`,
    /// reap. On timeout the child is killed and a timeout error is returned;
    /// teardown still completes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] when the child outlived `deadline`
    /// (it was killed during teardown), or the first teardown error.
    pub fn finish(mut self, deadline: Instant) -> Result<ExitStatus, ProcessError> {
        match self.close_input() {
            Ok(()) => {}
            Err(ProcessError::ChildExited(_)) => {}
            Err(e) => return Err(e),
        }
        match self.wait_exit(deadline) {
            Ok(status) => {
                self.close()?;
                Ok(status)
            }
            Err(e @ ProcessError::Timeout(_)) => {
                let _ignored = self.close();
                Err(ProcessError::Timeout(format!(
                    "finish: {e}; child killed during teardown"
                )))
            }
            Err(e) => {
                let _ignored = self.close();
                Err(e)
            }
        }
    }

    /// Forceful idempotent teardown: flag the worker, kill a living child
    /// (which unblocks any in-flight PTY I/O), reap, really join both threads.
    /// Returns the first teardown error, if any.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Teardown`] when kill/reap/join failed.
    pub fn close(&mut self) -> Result<(), ProcessError> {
        self.teardown();
        if let Some(msg) = self.shared.teardown_error() {
            return Err(ProcessError::Teardown(msg));
        }
        Ok(())
    }

    // -- internals ---------------------------------------------------------

    fn send(&self, op: CtlOp) -> Result<(), ProcessError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ProcessError::Closed("session is closed".to_owned()));
        }
        let tx = self.ctl_tx.lock().unwrap_or_else(PoisonError::into_inner);
        match tx.as_ref() {
            Some(tx) => tx
                .send(op)
                .map_err(|_| ProcessError::Closed("worker is gone".to_owned())),
            None => Err(ProcessError::Closed("session is closed".to_owned())),
        }
    }

    /// Run teardown exactly once; never panics (safe from `Drop`).
    ///
    /// Kill-first discipline: the shutdown flag aborts in-flight writes, the
    /// kill unblocks any PTY read/write sitting in the kernel with EOF/`EIO`,
    /// and only then are the threads joined — really joined, never detached.
    /// After the worker is gone the child is reaped here (normally a cached
    /// no-op: the worker already reaped it).
    fn teardown(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            // A previous close/drop already shut down; still join in case a
            // concurrent teardown is in flight.
            self.join_threads();
            return;
        }
        self.shutdown.store(true, Ordering::SeqCst);
        if let Err(e) = self.killer.kill_locked() {
            self.shared
                .record_teardown(&format!("child kill during teardown failed: {e}"));
        }
        {
            let mut tx = self.ctl_tx.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(tx) = tx.take() {
                let _ignored = tx.send(CtlOp::Shutdown);
            }
        }
        self.join_threads();
        // Reap here: the worker normally already did (cached, no syscall),
        // but a panicked worker may not have, and the pid must not leak as a
        // zombie owned by us.
        if let Err(e) = self.killer.wait_locked() {
            self.shared
                .record_teardown(&format!("child reap during teardown failed: {e}"));
        }
    }

    fn join_threads(&mut self) {
        let worker = self.worker.lock().map_or(None, |mut g| g.take());
        let reader = self.reader.lock().map_or(None, |mut g| g.take());
        // Worker first: its exit drops the data channel, which releases a
        // reader blocked on a full bounded queue.
        if let Some(h) = worker
            && h.join().is_err()
        {
            self.shared.record_teardown("worker thread panicked");
        }
        if let Some(h) = reader
            && h.join().is_err()
        {
            self.shared.record_teardown("reader thread panicked");
        }
        self.shared.mark_closed();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Never panic from Drop: teardown paths only record errors.
        self.teardown();
    }
}

fn recv_reply(
    rx: mpsc::Receiver<Result<(), ProcessError>>,
    what: &str,
) -> Result<(), ProcessError> {
    rx.recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| ProcessError::Timeout(format!("{what}: worker unresponsive")))?
}

fn recv_value<T>(rx: mpsc::Receiver<T>, what: &str) -> Result<T, ProcessError> {
    rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| {
        if what == "snapshot" {
            // Matches the Closed contract: a dead worker means shutdown.
            ProcessError::Closed("worker is gone".to_owned())
        } else {
            ProcessError::Timeout(format!("{what}: worker unresponsive"))
        }
    })
}
