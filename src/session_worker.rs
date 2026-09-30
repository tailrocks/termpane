// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Session worker: bounded pump, truthful capture, and startup rollback.
//!
//! The worker thread owns the [`DamageGrid`](crate::grid::DamageGrid), the PTY
//! writer, and the shared child handle. Two channels feed it:
//!
//! - a **bounded data channel** ([`DATA_QUEUE_BATCHES`] × [`READ_BATCH`] bytes)
//!   carrying child output from the reader thread, drained with a per-tick
//!   byte budget so a flooding child cannot starve control;
//! - a separate **control channel** carrying only small handle ops, so
//!   shutdown, resize, and observation bypass queued output entirely.
//!
//! Teardown never depends on either queue draining: it sets the shutdown flag,
//! kills the child through the shared handle (which unblocks any in-flight
//! PTY write with `EIO`), then joins both threads. Joins are real joins — a
//! stuck thread is never detached.
//!
//! F05 capture rules live here too: `Interrupted` reads are retried, only
//! qualified platform EOF (`Ok(0)` / `EIO`) ends the stream, every other I/O
//! failure is retained, and the drain grace is a deadline that yields
//! [`StreamState::DrainExpired`](crate::session::StreamState), never a proof
//! of EOF.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use crate::grid::DamageGrid;
use crate::passthrough::PassthroughEvent;
use crate::process::{ExitStatus, SignalError};
use crate::pty::{Master, PtyChild, PtyReader, PtyWriter};
use crate::session::ProcessError;
use crate::session_observe::{
    Capabilities, CaptureOutcome, ColorState, Completeness, CursorState, Diagnostics, ModeState,
    Observation, StreamState,
};
use crate::snapshot::GridSnapshot;
use crate::width::VirtualTerminalProfile;

// ---------------------------------------------------------------------------
// Bounds and timing
// ---------------------------------------------------------------------------

/// One reader batch: bytes per `read()` into the data queue.
pub(crate) const READ_BATCH: usize = 8192;
/// Data-queue depth in batches: at most 8 × 8 KiB = 64 KiB of child output is
/// ever queued behind the worker. The reader blocks past this (it always
/// unblocks: the worker drains every tick, and worker exit disconnects).
pub(crate) const DATA_QUEUE_BATCHES: usize = 8;
/// Per-tick feed budget: after each control op (or tick) the worker processes
/// at most this many queued output bytes before re-checking control, so a
/// flooding child cannot starve shutdown/resize/observation.
pub(crate) const FEED_BUDGET_PER_TICK: usize = 256 * 1024;
/// Stdin/reply writes are chunked so the shutdown flag is re-checked at least
/// this often; a single chunk write still blocks until the child reads, the
/// child dies, or teardown kills it (kill unblocks with `EIO`).
pub(crate) const WRITE_CHUNK: usize = 4096;
/// Default cap on stashed passthrough events (oldest dropped first, counted).
pub(crate) const DEFAULT_EVENT_CAP: usize = 1024;
/// Default cap on recorded output-log bytes (oldest prefix kept, counted).
pub(crate) const DEFAULT_OUTPUT_CAP_BYTES: usize = 1024 * 1024;

/// How long after child exit the worker still accepts trailing reader bytes
/// before declaring [`StreamState::DrainExpired`].
pub(crate) const DRAIN_GRACE: Duration = Duration::from_millis(500);
/// Worker tick: control poll cadence, data-drain cadence, exit-poll cadence.
const WORKER_TICK: Duration = Duration::from_millis(25);
/// Grace for SIGKILL-triggered reap during worker-side teardown.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Bound for one worker round-trip (resize, snapshot, ...).
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Channels: bounded data, separate control
// ---------------------------------------------------------------------------

/// Terminal stream ending seen by the reader thread.
#[derive(Debug, Clone)]
pub(crate) enum StreamEndKind {
    /// Qualified end-of-output: `Ok(0)` or platform `EIO`.
    CleanEof,
    /// Any other read failure; the message carries the I/O error.
    ReadFailed(String),
}

/// One message on the bounded reader→worker data channel. FIFO order preserves
/// terminal byte order; control ops travel on their own channel and may
/// overtake still-queued output (documented on the handle methods).
#[derive(Debug)]
pub(crate) enum DataMsg {
    /// One batch of child output bytes (never empty).
    Bytes(Vec<u8>),
    /// The reader observed a terminal stream condition and exited.
    StreamEnd(StreamEndKind),
}

/// One message on the control channel. Small by construction (the largest,
/// [`CtlOp::Write`], is chunked by the worker); the reader never sends here.
#[derive(Debug)]
pub(crate) enum CtlOp {
    /// Full grid snapshot.
    Snapshot(mpsc::Sender<GridSnapshot>),
    /// Escape codes reproducing the entire live grid state.
    StateBytes(mpsc::Sender<Vec<u8>>),
    /// Drain the grid's dirty-row set.
    DirtySpans(mpsc::Sender<crate::damage::DirtySpans>),
    /// Drain stashed passthrough events.
    Events(mpsc::Sender<Vec<PassthroughEvent>>),
    /// Clone the recorded output log.
    OutputLog(mpsc::Sender<Vec<u8>>),
    /// Assemble one atomic observation.
    Observe(mpsc::Sender<Observation>),
    /// Write raw bytes to the child's stdin.
    Write {
        /// Bytes to write.
        bytes: Vec<u8>,
        /// Write outcome.
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    /// Resize the PTY and the emulator together.
    Resize {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
        /// Resize outcome.
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    /// Close the child's stdin (EOF request).
    CloseInput {
        /// Close outcome.
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    /// Update the OSC 10/11 reported colors (`None` keeps the current value).
    SetColors {
        /// New reported foreground, if any.
        fg: Option<(u8, u8, u8)>,
        /// New reported background, if any.
        bg: Option<(u8, u8, u8)>,
        /// Update outcome (always `Ok`; fallible for channel symmetry).
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    /// Abort capture, reap the child, publish the outcome, exit.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Shared child: race-free poll/signal/kill across worker and handle (F03)
// ---------------------------------------------------------------------------

/// Reaped status plus the reaped flag, guarded by one mutex.
#[derive(Debug)]
struct SharedChildState {
    child: PtyChild,
    reaped: bool,
    status: Option<ExitStatus>,
}

/// The session child, shared between the worker (exit polling, teardown reap)
/// and the handle (out-of-band signal/kill).
///
/// Every state transition — reap observation, signal delivery, kill — happens
/// under one mutex, so the F03 rule holds structurally: once any thread has
/// observed the reap, no thread can address the pid again, and the check and
/// the `kill` syscall cannot be interleaved by a reap on another thread. The
/// mutex is held only across non-blocking syscalls (`try_wait`, `kill(pid)`,
/// backend `kill`), except [`SharedChild::wait_locked`], whose callers (spawn
/// rollback, post-join teardown) provably hold no worker concurrency.
#[derive(Clone, Debug)]
pub(crate) struct SharedChild {
    pid: Option<u32>,
    state: Arc<Mutex<SharedChildState>>,
}

impl SharedChild {
    /// Share `child`. The pid is read once here; every later use goes through
    /// the reaped check.
    pub(crate) fn new(child: PtyChild) -> Self {
        let pid = child.pid();
        Self {
            pid,
            state: Arc::new(Mutex::new(SharedChildState {
                child,
                reaped: false,
                status: None,
            })),
        }
    }

    /// Direct child's PID, when the transport reports one.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn lock(&self) -> MutexGuard<'_, SharedChildState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Poll for exit without blocking; records the reap atomically.
    pub(crate) fn poll(&self) -> std::io::Result<Option<ExitStatus>> {
        let mut state = self.lock();
        if state.reaped {
            return Ok(state.status.clone());
        }
        let status = state.child.try_wait()?;
        if let Some(status) = &status {
            state.reaped = true;
            state.status = Some(status.clone());
        }
        Ok(status)
    }

    /// Deliver `signo` unless the child was reaped: the check and the `kill`
    /// run under one lock, so a concurrent reap cannot slip between them and
    /// redirect the signal at a recycled pid.
    pub(crate) fn signal_locked(&self, signo: i32) -> Result<(), SignalError> {
        let state = self.lock();
        let Some(pid) = self.pid else {
            return Err(SignalError::UnknownPid);
        };
        if state.reaped {
            return Err(SignalError::NotFound { pid });
        }
        crate::process::signal(pid, signo)
    }

    /// Kill unless already reaped (silent no-op then, like `std`).
    pub(crate) fn kill_locked(&self) -> std::io::Result<()> {
        let mut state = self.lock();
        if state.reaped {
            return Ok(());
        }
        state.child.kill()
    }

    /// Block until reaped. Call only where the worker cannot poll concurrently
    /// (spawn rollback before the worker exists, teardown after joining it):
    /// the mutex is held across the blocking wait.
    pub(crate) fn wait_locked(&self) -> std::io::Result<ExitStatus> {
        let mut state = self.lock();
        if let Some(status) = state.status.clone() {
            return Ok(status);
        }
        let status = state.child.wait()?;
        state.reaped = true;
        state.status = Some(status.clone());
        Ok(status)
    }
}

// ---------------------------------------------------------------------------
// Shared rendezvous: outcome, revision, frames, teardown
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct SharedState {
    outcome: Option<CaptureOutcome>,
    revision: u64,
    frames: u64,
    closed: bool,
    teardown_error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    state: Mutex<SharedState>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publish the final outcome exactly once; later declarations are dropped.
    pub(crate) fn publish_outcome(&self, outcome: CaptureOutcome) {
        let mut state = self.lock();
        if state.outcome.is_none() {
            state.outcome = Some(outcome);
            self.changed.notify_all();
        }
    }

    /// The final outcome, once declared.
    pub(crate) fn outcome(&self) -> Option<CaptureOutcome> {
        self.lock().outcome.clone()
    }

    /// The direct child's exit, once declared.
    pub(crate) fn exit(&self) -> Option<ExitStatus> {
        self.lock().outcome.as_ref().map(|o| o.exit.clone())
    }

    /// Publish the current worker revision.
    pub(crate) fn set_revision(&self, revision: u64) {
        let mut state = self.lock();
        if state.revision != revision {
            state.revision = revision;
            self.changed.notify_all();
        }
    }

    /// Publish the completed-sync-frame count.
    pub(crate) fn set_frames(&self, frames: u64) {
        let mut state = self.lock();
        if state.frames != frames {
            state.frames = frames;
            self.changed.notify_all();
        }
    }

    /// Completed sync frames observed so far.
    pub(crate) fn frames(&self) -> u64 {
        self.lock().frames
    }

    pub(crate) fn mark_closed(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    pub(crate) fn record_teardown(&self, msg: &str) {
        let mut state = self.lock();
        if state.teardown_error.is_none() {
            state.teardown_error = Some(msg.to_owned());
        }
    }

    pub(crate) fn teardown_error(&self) -> Option<String> {
        self.lock().teardown_error.clone()
    }

    /// Wait until the outcome is published, the session closes, or `deadline`
    /// passes.
    pub(crate) fn wait_outcome_changed(&self, deadline: Instant) {
        let mut state = self.lock();
        while state.outcome.is_none() && !state.closed {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wait until `revision >= target`, the session closes, or `deadline`
    /// passes. Returns the current revision when the target was reached.
    pub(crate) fn wait_revision(&self, target: u64, deadline: Instant) -> Option<u64> {
        let mut state = self.lock();
        loop {
            if state.revision >= target {
                return Some(state.revision);
            }
            if state.closed {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wait until the completed-frame count exceeds `baseline`, the session
    /// closes, or `deadline` passes. Returns the new count on success.
    pub(crate) fn wait_frames_above(&self, baseline: u64, deadline: Instant) -> Option<u64> {
        let mut state = self.lock();
        loop {
            if state.frames > baseline {
                return Some(state.frames);
            }
            if state.closed {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

// ---------------------------------------------------------------------------
// Worker thread: sole owner of grid and writer
// ---------------------------------------------------------------------------

/// Everything the worker thread owns. Bundled so the entry point takes one
/// argument instead of a dozen.
pub(crate) struct WorkerConfig {
    /// PTY master (resize only; reads/writes use the split handles).
    pub(crate) master: Master,
    /// Shared child handle (exit polling; the handle signals/kills).
    pub(crate) killer: SharedChild,
    /// Child-stdin writer; `None` once closed.
    pub(crate) writer: PtyWriter,
    /// Initial width in columns.
    pub(crate) cols: u16,
    /// Initial height in rows.
    pub(crate) rows: u16,
    /// Grid scrollback limit in lines.
    pub(crate) scrollback: usize,
    /// Record pumped bytes for the output log.
    pub(crate) record_output: bool,
    /// Cap on stashed passthrough events.
    pub(crate) event_cap: usize,
    /// Cap on recorded output-log bytes.
    pub(crate) output_cap_bytes: usize,
    /// Bounded reader→worker data channel.
    pub(crate) data_rx: mpsc::Receiver<DataMsg>,
    /// Batches sent but not yet received: exact data-queue depth, maintained
    /// by the reader (increment after each successful send) and the worker
    /// (decrement after each successful receive).
    pub(crate) inflight: Arc<AtomicUsize>,
    /// Handle→worker control channel.
    pub(crate) ctl_rx: mpsc::Receiver<CtlOp>,
    /// Set by teardown: abort in-flight writes and exit at the next tick even
    /// if the control op never arrives.
    pub(crate) shutdown: Arc<AtomicBool>,
    /// Rendezvous with the handle.
    pub(crate) shared: Arc<Shared>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Four independent worker flags (record, truncated, finalized, \
              in-sync-sample): orthogonal lifecycle bits, not a state machine."
)]
struct Worker {
    master: Master,
    killer: SharedChild,
    writer: Option<PtyWriter>,
    grid: DamageGrid,
    events: VecDeque<PassthroughEvent>,
    events_dropped: u64,
    event_cap: usize,
    output_log: Vec<u8>,
    output_bytes_total: u64,
    output_truncated: bool,
    output_cap_bytes: usize,
    record_output: bool,
    replies_routed: u64,
    replies_dropped_no_writer: u64,
    reply_write_errors: u64,
    reply_first_error: Option<String>,
    bytes_pumped: u64,
    feeds: u64,
    dropped_after_drain: u64,
    resizes: u64,
    revision: u64,
    frames: u64,
    was_in_sync: bool,
    reported_fg: (u8, u8, u8),
    reported_bg: (u8, u8, u8),
    stream_end: Option<StreamEndKind>,
    final_stream: Option<StreamState>,
    exited: Option<ExitStatus>,
    exit_seen_at: Option<Instant>,
    finalized: bool,
    data_rx: mpsc::Receiver<DataMsg>,
    inflight: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

pub(crate) fn run_worker(config: WorkerConfig) {
    let profile = VirtualTerminalProfile::default();
    let WorkerConfig {
        master,
        killer,
        writer,
        cols,
        rows,
        scrollback,
        record_output,
        event_cap,
        output_cap_bytes,
        data_rx,
        inflight,
        ctl_rx,
        shutdown,
        shared,
    } = config;
    // The grid seeds its reported colors from the same profile defaults the
    // worker tracks below, so cursor/color/mode observations agree from
    // revision 0 without any round trip.
    let mut worker = Worker {
        master,
        killer,
        writer: Some(writer),
        grid: DamageGrid::new(rows, cols, scrollback),
        events: VecDeque::new(),
        events_dropped: 0,
        event_cap,
        output_log: Vec::new(),
        output_bytes_total: 0,
        output_truncated: false,
        output_cap_bytes,
        record_output,
        replies_routed: 0,
        replies_dropped_no_writer: 0,
        reply_write_errors: 0,
        reply_first_error: None,
        bytes_pumped: 0,
        feeds: 0,
        dropped_after_drain: 0,
        resizes: 0,
        revision: 0,
        frames: 0,
        was_in_sync: false,
        reported_fg: profile.default_reported_fg,
        reported_bg: profile.default_reported_bg,
        stream_end: None,
        final_stream: None,
        exited: None,
        exit_seen_at: None,
        finalized: false,
        data_rx,
        inflight,
        shutdown,
        shared,
    };

    loop {
        // The flag is the control path of last resort: teardown sets it
        // before killing, so even a worker that never receives its Shutdown
        // op (orphaned sender) still exits on the next tick.
        if worker.shutdown.load(Ordering::SeqCst) {
            worker.shutdown_and_exit();
            return;
        }
        match ctl_rx.recv_timeout(WORKER_TICK) {
            Ok(op) => {
                if worker.handle_ctl(op) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Handle gone without teardown: reap and go away quietly.
                worker.shutdown_and_exit();
                return;
            }
        }
        worker.drain_data();
        worker.poll_exit_state();
    }
}

impl Worker {
    /// Handle one control op. Returns true when the worker must exit.
    fn handle_ctl(&mut self, op: CtlOp) -> bool {
        match op {
            CtlOp::Snapshot(reply) => {
                let _ignored = reply.send(self.grid.dump());
            }
            CtlOp::StateBytes(reply) => {
                let _ignored = reply.send(self.grid.state_formatted());
            }
            CtlOp::DirtySpans(reply) => {
                let _ignored = reply.send(self.grid.dirty_spans());
            }
            CtlOp::Events(reply) => {
                let _ignored = reply.send(self.events.drain(..).collect());
            }
            CtlOp::OutputLog(reply) => {
                let _ignored = reply.send(self.output_log.clone());
            }
            CtlOp::Observe(reply) => {
                let _ignored = reply.send(self.observe());
            }
            CtlOp::Write { bytes, reply } => {
                let r = self.apply_write(&bytes);
                let _ignored = reply.send(r);
            }
            CtlOp::Resize { cols, rows, reply } => {
                let r = self.apply_resize(cols, rows);
                let _ignored = reply.send(r);
            }
            CtlOp::CloseInput { reply } => {
                let r = self.close_input();
                let _ignored = reply.send(r);
            }
            CtlOp::SetColors { fg, bg, reply } => {
                self.grid.set_reported_colors(fg, bg);
                if let Some(fg) = fg {
                    self.reported_fg = fg;
                }
                if let Some(bg) = bg {
                    self.reported_bg = bg;
                }
                self.bump_revision();
                let _ignored = reply.send(Ok(()));
            }
            CtlOp::Shutdown => {
                self.shutdown_and_exit();
                return true;
            }
        }
        false
    }

    /// Drain queued output with a per-tick byte budget, preserving FIFO order.
    fn drain_data(&mut self) {
        let mut budget = FEED_BUDGET_PER_TICK;
        loop {
            let msg = self.data_rx.try_recv();
            if msg.is_ok() {
                self.inflight.fetch_sub(1, Ordering::SeqCst);
            }
            match msg {
                Ok(DataMsg::Bytes(bytes)) => {
                    budget = budget.saturating_sub(bytes.len());
                    self.feed(bytes);
                    if budget == 0 {
                        return;
                    }
                }
                Ok(DataMsg::StreamEnd(end)) => {
                    // First declaration wins: a stream end racing the drain
                    // grace never rewrites an already-finalized outcome.
                    if self.stream_end.is_none() {
                        self.stream_end = Some(end);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The reader sends exactly one terminal message before
                    // exiting; a bare disconnect means it died without one.
                    if self.stream_end.is_none() {
                        self.stream_end =
                            Some(StreamEndKind::ReadFailed("reader thread lost".to_owned()));
                    }
                    return;
                }
            }
        }
    }

    fn feed(&mut self, bytes: Vec<u8>) {
        // Declared-drained output is immutable: late bytes are counted, never
        // applied, so the grid and the output log cannot drift past the final
        // outcome.
        if self.finalized {
            self.dropped_after_drain += 1;
            return;
        }
        if bytes.contains(&0x1b) {
            // Control-bearing batch: feed byte-by-byte so synchronized-update
            // (DEC 2026) on→off transitions that complete inside the batch
            // are all observed — a whole frame in one batch shows no net
            // state change afterwards. Batches without ESC cannot move the
            // parser out of ground state, so they take the fast path.
            for byte in &bytes {
                self.grid.process(std::slice::from_ref(byte));
                self.note_sync_sample();
            }
        } else {
            self.grid.process(&bytes);
            self.note_sync_sample();
        }
        self.bytes_pumped += bytes.len() as u64;
        self.feeds += 1;
        if self.record_output {
            // Total counts every pumped byte even past truncation: the gap to
            // `output_bytes_kept` is exactly what was cut.
            self.output_bytes_total += bytes.len() as u64;
            if !self.output_truncated {
                let room = self.output_cap_bytes.saturating_sub(self.output_log.len());
                let take = room.min(bytes.len());
                self.output_log.extend_from_slice(&bytes[..take]);
                if take < bytes.len() {
                    self.output_truncated = true;
                }
            }
        }
        for event in self.grid.drain_passthrough() {
            match event {
                PassthroughEvent::Reply(reply) => self.route_reply(&reply),
                other => self.stash_event(other),
            }
        }
        self.bump_revision();
    }

    /// Sample the synchronized-update bit after pumped input: only an on→off
    /// transition completes a frame. Sampling `in_synchronized_update` alone
    /// can never prove a frame was observed.
    fn note_sync_sample(&mut self) {
        let now_in = self.grid.in_synchronized_update();
        if self.was_in_sync && !now_in {
            self.frames += 1;
            self.shared.set_frames(self.frames);
        }
        self.was_in_sync = now_in;
    }

    fn stash_event(&mut self, event: PassthroughEvent) {
        while self.events.len() >= self.event_cap.max(1) {
            self.events.pop_front();
            self.events_dropped += 1;
        }
        self.events.push_back(event);
    }

    /// Route one emulator reply (DA/DSR/DECRQM answer) to PTY stdin. Failures
    /// are counted and the first is retained — never ignored, never fatal.
    fn route_reply(&mut self, reply: &[u8]) {
        let Some(writer) = self.writer.as_mut() else {
            self.replies_dropped_no_writer += 1;
            return;
        };
        if self.shutdown.load(Ordering::SeqCst) {
            self.replies_dropped_no_writer += 1;
            return;
        }
        match writer.write_all(reply) {
            Ok(()) => self.replies_routed += 1,
            Err(err) => {
                self.reply_write_errors += 1;
                if self.reply_first_error.is_none() {
                    self.reply_first_error = Some(format!("pty reply write failed: {err}"));
                }
            }
        }
    }

    /// Chunked stdin write with shutdown checks between chunks. One chunk
    /// still blocks until the child reads, the child dies, or teardown kills
    /// it (the kill unblocks the write with `EIO`); the flag checks keep a
    /// large write cancellable at 4 KiB granularity past that.
    fn apply_write(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.exited.is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(ProcessError::Closed("session is closed".to_owned()));
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| ProcessError::Closed("stdin is closed".to_owned()))?;
        for chunk in bytes.chunks(WRITE_CHUNK) {
            if self.shutdown.load(Ordering::SeqCst) {
                return Err(ProcessError::Closed("session is closed".to_owned()));
            }
            writer
                .write_all(chunk)
                .map_err(|e| ProcessError::Io(format!("pty write failed: {e}")))?;
        }
        Ok(())
    }

    fn apply_resize(&mut self, cols: u16, rows: u16) -> Result<(), ProcessError> {
        // PTY first: if the kernel refuses, the emulator stays consistent.
        self.master
            .resize(cols, rows)
            .map_err(|e| ProcessError::Io(format!("pty resize failed: {e}")))?;
        self.grid.set_size(rows, cols);
        self.resizes += 1;
        self.bump_revision();
        Ok(())
    }

    fn close_input(&mut self) -> Result<(), ProcessError> {
        if self.exited.is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        if self.writer.take().is_some() {
            // Dropping the writer runs the backend's EOF injection (newline +
            // VEOF): a canonical-mode EOF request, not a universal half-close
            // — see the handle's `close_input` docs.
            self.bump_revision();
            Ok(())
        } else {
            Err(ProcessError::Closed("stdin already closed".to_owned()))
        }
    }

    fn bump_revision(&mut self) {
        self.revision += 1;
        self.shared.set_revision(self.revision);
    }

    /// Assemble one atomic observation: every field is read here, on the
    /// worker, with no interleaving pump, so all facts share one revision.
    fn observe(&self) -> Observation {
        let grid = self.grid.dump();
        let attrs = self.grid.current_attrs();
        Observation {
            revision: self.revision,
            grid,
            cursor: CursorState {
                position: self.grid.cursor_position(),
                visible: !self.grid.hide_cursor(),
                style: self.grid.cursor_style(),
                text_cursor_enable: self.grid.text_cursor_enable(),
            },
            colors: ColorState {
                reported_fg: self.reported_fg,
                reported_bg: self.reported_bg,
                current_fg: attrs.foreground,
                current_bg: attrs.background,
            },
            modes: ModeState {
                alternate_screen: self.grid.alternate_screen(),
                autowrap: self.grid.autowrap(),
                application_cursor: self.grid.application_cursor(),
                application_keypad: self.grid.application_keypad(),
                bracketed_paste: self.grid.bracketed_paste(),
                focus_events: self.grid.focus_events(),
                mouse_mode: self.grid.mouse_protocol_mode(),
                mouse_encoding: self.grid.mouse_protocol_encoding(),
                kitty_keyboard: self.grid.kitty_kb_flags(),
                in_synchronized_update: self.grid.in_synchronized_update(),
                mid_sequence: self.grid.mid_sequence(),
            },
            capabilities: Capabilities::model_defaults(),
            completeness: Completeness {
                exit: self.exited.clone(),
                stream: self.stream_state(),
                finalized: self.finalized,
            },
            diagnostics: Diagnostics {
                bytes_pumped: self.bytes_pumped,
                feeds: self.feeds,
                dropped_after_drain: self.dropped_after_drain,
                events_stashed: self.events.len() as u64,
                events_dropped: self.events_dropped,
                output_bytes_kept: self.output_log.len() as u64,
                output_bytes_total: self.output_bytes_total,
                output_truncated: self.output_truncated,
                replies_routed: self.replies_routed,
                replies_dropped_no_writer: self.replies_dropped_no_writer,
                reply_write_errors: self.reply_write_errors,
                reply_first_error: self.reply_first_error.clone(),
                sync_frames_completed: self.frames,
                resizes: self.resizes,
                stdin_open: self.writer.is_some(),
                queued_batches: self.inflight.load(Ordering::SeqCst),
            },
        }
    }

    /// Current stream completeness: interim while running, frozen at finalize.
    fn stream_state(&self) -> StreamState {
        if let Some(end) = &self.stream_end {
            return match end {
                StreamEndKind::CleanEof => StreamState::CleanEof,
                StreamEndKind::ReadFailed(msg) => StreamState::ReadFailed(msg.clone()),
            };
        }
        if let Some(final_stream) = &self.final_stream {
            return final_stream.clone();
        }
        StreamState::Streaming
    }

    /// Reap promptly, but give trailing output `DRAIN_GRACE` after the child
    /// dies before publishing the final outcome. The grace is a deadline: if
    /// it expires first, the outcome records `DrainExpired`, never EOF.
    fn poll_exit_state(&mut self) {
        if self.finalized {
            return;
        }
        if self.exited.is_none() {
            // A failed poll is transient (the backend retries next tick);
            // only a reaped status counts as an exit sighting.
            if let Some(status) = self.killer.poll().ok().flatten() {
                self.exited = Some(status);
                self.exit_seen_at = Some(Instant::now());
            }
        }
        if self.exited.is_none() {
            return;
        }
        let grace_over = self
            .exit_seen_at
            .is_some_and(|t| t.elapsed() >= DRAIN_GRACE);
        if self.stream_end.is_none() && !grace_over {
            return;
        }
        let stream = match self.stream_end.clone() {
            Some(StreamEndKind::CleanEof) => StreamState::CleanEof,
            Some(StreamEndKind::ReadFailed(msg)) => StreamState::ReadFailed(msg),
            None => StreamState::DrainExpired,
        };
        self.finalized = true;
        self.final_stream = Some(stream.clone());
        if let Some(exit) = self.exited.clone() {
            self.shared.publish_outcome(CaptureOutcome {
                exit,
                stream,
                revision: self.revision,
                bytes_pumped: self.bytes_pumped,
                dropped_after_drain: 0,
            });
        }
    }

    /// Abort path: preserve trailing queued bytes in order, reap the child,
    /// publish the outcome unless already finalized, mark closed.
    fn shutdown_and_exit(&mut self) {
        if !self.finalized {
            // Best-effort final drain: bytes already queued were captured
            // before the abort, so they still belong in the grid and the log.
            self.drain_data();
        }
        shutdown_child(&self.killer, &self.shared);
        if self.exited.is_none() {
            self.exited = self.killer.poll().ok().flatten();
        }
        if !self.finalized {
            self.finalized = true;
            let stream = match self.stream_end.clone() {
                Some(StreamEndKind::CleanEof) => StreamState::CleanEof,
                Some(StreamEndKind::ReadFailed(msg)) => StreamState::ReadFailed(msg),
                // Nobody waited for a drain here: teardown aborted the
                // capture, and the outcome says so.
                None => StreamState::Aborted,
            };
            self.final_stream = Some(stream.clone());
            self.shared.publish_outcome(CaptureOutcome {
                exit: self.exited.clone().unwrap_or_else(ExitStatus::unknown),
                stream,
                revision: self.revision,
                bytes_pumped: self.bytes_pumped,
                dropped_after_drain: 0,
            });
        }
        self.shared.mark_closed();
    }
}

/// Bounded kill + reap. Records teardown errors instead of failing; never
/// blocks past `KILL_GRACE`.
fn shutdown_child(killer: &SharedChild, shared: &Shared) {
    match killer.poll() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => {
            shared.record_teardown(&format!("child poll during teardown failed: {e}"));
        }
    }
    if let Err(e) = killer.kill_locked() {
        shared.record_teardown(&format!("child kill during teardown failed: {e}"));
    }
    let deadline = Instant::now() + KILL_GRACE;
    loop {
        match killer.poll() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(e) => {
                shared.record_teardown(&format!("child reap during teardown failed: {e}"));
                return;
            }
        }
        if Instant::now() >= deadline {
            shared.record_teardown("child still alive after kill grace");
            return;
        }
        // The teardown poll sleeps on a worker-owned OS thread (never a
        // render/runtime thread), which the disallowed-methods policy
        // explicitly permits.
        #[expect(
            clippy::disallowed_methods,
            reason = "reap poll on a worker-owned OS thread — exactly the \
                      carve-out the policy names"
        )]
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Reader thread: blocking pump into the bounded data queue
// ---------------------------------------------------------------------------

/// True only for the qualified platform EOF error: `EIO` (Linux reports it on
/// the master once the child is gone). `Ok(0)` is the other EOF shape and is
/// matched at the call site; every other failure is retained, never coerced.
fn is_qualified_eof(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(nix::errno::Errno::EIO as i32)
}

/// Bounded send: blocks while the worker is behind (it drains every tick),
/// fails once the worker is gone. Counts the batch as queued on success.
fn send_counted(tx: &mpsc::SyncSender<DataMsg>, inflight: &Arc<AtomicUsize>, msg: DataMsg) -> bool {
    match tx.send(msg) {
        Ok(()) => {
            inflight.fetch_add(1, Ordering::SeqCst);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn run_reader(
    mut reader: PtyReader,
    tx: mpsc::SyncSender<DataMsg>,
    inflight: Arc<AtomicUsize>,
) {
    let mut buf = vec![0u8; READ_BATCH];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                send_counted(&tx, &inflight, DataMsg::StreamEnd(StreamEndKind::CleanEof));
                return;
            }
            Ok(n) => {
                if !send_counted(&tx, &inflight, DataMsg::Bytes(buf[..n].to_vec())) {
                    return;
                }
            }
            // A signal that landed mid-read is not a stream condition.
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if is_qualified_eof(&err) => {
                send_counted(&tx, &inflight, DataMsg::StreamEnd(StreamEndKind::CleanEof));
                return;
            }
            Err(err) => {
                send_counted(
                    &tx,
                    &inflight,
                    DataMsg::StreamEnd(StreamEndKind::ReadFailed(format!(
                        "pty read failed: {err}"
                    ))),
                );
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Startup rollback: one guard owns the child until the session does
// ---------------------------------------------------------------------------

/// Fault-injection stages for spawn-rollback tests. Each flag simulates a
/// failure at that stage *after* the rollback guard is armed, so the test
/// proves the guard cleans up; unit tests drive these through the crate's
/// spawn entry point with real children.
#[derive(Debug, Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Six independent fault-injection switches for rollback tests: \
              each stage fails separately, so a single enum would lose \
              combinations."
)]
pub(crate) struct StartupFaults {
    /// Fail before taking the PTY reader.
    pub(crate) fail_reader: bool,
    /// Fail before taking the PTY writer.
    pub(crate) fail_writer: bool,
    /// Fail before the initial child poll.
    pub(crate) fail_poll: bool,
    /// Fail before spawning the worker thread.
    pub(crate) fail_worker_thread: bool,
    /// Fail before spawning the reader thread (worker already runs).
    pub(crate) fail_reader_thread: bool,
    /// Fail the worker handshake after the session is assembled.
    pub(crate) fail_setup: bool,
}

impl StartupFaults {
    /// No faults: the production path.
    pub(crate) fn none() -> Self {
        Self::default()
    }
}

/// Owns an incompletely started session until the handle takes over.
///
/// Armed immediately after `spawn_pty`: while armed, dropping the guard kills
/// and reaps the child (plus shuts down and joins the worker once it exists),
/// so no spawn failure — reader, writer, poll, thread spawn, or handshake —
/// can leak a child or a thread. The session defuses the guard once both
/// threads run and (in `PtySession::spawn`) the handshake succeeds.
#[derive(Debug)]
pub(crate) struct StartupGuard {
    killer: SharedChild,
    shutdown: Arc<AtomicBool>,
    ctl_tx: Option<mpsc::Sender<CtlOp>>,
    worker: Option<std::thread::JoinHandle<()>>,
    defused: bool,
}

impl StartupGuard {
    /// Arm over a just-spawned child. From here until [`Self::defuse`], every
    /// early return kills and reaps.
    pub(crate) fn armed(killer: SharedChild, shutdown: Arc<AtomicBool>) -> Self {
        Self {
            killer,
            shutdown,
            ctl_tx: None,
            worker: None,
            defused: false,
        }
    }

    /// Record the running worker so rollback shuts it down as well.
    pub(crate) fn set_worker(
        &mut self,
        ctl_tx: mpsc::Sender<CtlOp>,
        handle: std::thread::JoinHandle<()>,
    ) {
        self.ctl_tx = Some(ctl_tx);
        self.worker = Some(handle);
    }

    /// Release the worker handle to the finished session.
    pub(crate) fn take_worker(&mut self) -> Option<std::thread::JoinHandle<()>> {
        self.worker.take()
    }

    /// Disarm: the session owns the child and the worker now.
    pub(crate) fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }
        // Same order as session teardown: flag first (aborts in-flight
        // writes), then kill (unblocks the worker), then join, then reap.
        // Failures are unreportable from Drop; the spawn already failed with
        // the real error.
        self.shutdown.store(true, Ordering::SeqCst);
        let _ignored = self.killer.kill_locked();
        if let Some(tx) = self.ctl_tx.take() {
            let _ignored = tx.send(CtlOp::Shutdown);
        }
        if let Some(handle) = self.worker.take() {
            let _ignored = handle.join();
        }
        let _ignored = self.killer.wait_locked();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_eio_qualifies_as_eof() {
        let eio = nix::errno::Errno::EIO as i32;
        assert!(is_qualified_eof(&std::io::Error::from_raw_os_error(eio)));
        assert!(!is_qualified_eof(&std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        )));
        assert!(!is_qualified_eof(&std::io::Error::from(
            std::io::ErrorKind::Interrupted
        )));
        assert!(!is_qualified_eof(&std::io::Error::from_raw_os_error(
            nix::errno::Errno::EPIPE as i32
        )));
    }

    #[test]
    fn data_queue_bound_is_64kib() {
        assert_eq!(DATA_QUEUE_BATCHES * READ_BATCH, 64 * 1024);
    }

    /// Reap-check: `ECHILD` proves the pid is reaped, not merely dead (a
    /// zombie would still answer `waitpid`).
    fn assert_reaped(pid: u32) {
        let target = nix::unistd::Pid::from_raw(i32::try_from(pid).expect("test pid fits"));
        match nix::sys::wait::waitpid(target, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Err(nix::errno::Errno::ECHILD) => {}
            other => panic!("expected ECHILD for reaped {pid}, got {other:?}"),
        }
    }

    #[test]
    fn guard_drop_kills_and_reaps_a_live_child() {
        let params = crate::process::SpawnParams::new("sleep").arg("60");
        let (_master, child) =
            crate::pty::spawn_pty(&params, 80, 24).expect("spawn sleep for guard test");
        let pid = child.pid().expect("sleep has a pid");
        let killer = SharedChild::new(child);
        drop(StartupGuard::armed(
            killer,
            Arc::new(AtomicBool::new(false)),
        ));
        assert!(
            !crate::process::pid_alive(pid),
            "guard must kill child {pid}"
        );
        assert_reaped(pid);
    }

    fn spawn_with_fault(
        faults: StartupFaults,
    ) -> (
        Result<crate::session::PtySession, ProcessError>,
        Option<u32>,
    ) {
        let params = crate::process::SpawnParams::new("sleep").arg("60");
        let mut pid = None;
        let session = crate::session::PtySession::spawn_with_faults(
            &params,
            crate::session::SessionOptions::default(),
            faults,
            &mut pid,
        );
        (
            session.map(|mut s| {
                s.close().expect("fault-free session must close");
                s
            }),
            pid,
        )
    }

    fn assert_fault_rolls_back(faults: StartupFaults, stage: &str) {
        let (result, pid) = spawn_with_fault(faults);
        let err = result.expect_err(&format!("{stage} fault must fail spawn"));
        assert!(
            matches!(err, ProcessError::Spawn(_)),
            "{stage}: expected Spawn error, got {err}"
        );
        let pid = pid.expect("child must have spawned before the fault");
        assert!(
            !crate::process::pid_alive(pid),
            "{stage}: child {pid} leaked"
        );
        assert_reaped(pid);
    }

    #[test]
    fn rollback_at_reader_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_reader: true,
                ..StartupFaults::none()
            },
            "reader",
        );
    }

    #[test]
    fn rollback_at_writer_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_writer: true,
                ..StartupFaults::none()
            },
            "writer",
        );
    }

    #[test]
    fn rollback_at_initial_poll_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_poll: true,
                ..StartupFaults::none()
            },
            "initial-poll",
        );
    }

    #[test]
    fn rollback_at_worker_thread_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_worker_thread: true,
                ..StartupFaults::none()
            },
            "worker-thread",
        );
    }

    #[test]
    fn rollback_at_reader_thread_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_reader_thread: true,
                ..StartupFaults::none()
            },
            "reader-thread",
        );
    }

    #[test]
    fn rollback_at_setup_handshake_stage() {
        assert_fault_rolls_back(
            StartupFaults {
                fail_setup: true,
                ..StartupFaults::none()
            },
            "setup",
        );
    }

    /// Test-only poll pacing on an owned test thread.
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only poll pacing on an owned test thread, never a render/runtime thread"
    )]
    fn test_sleep_ms(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    #[test]
    fn drain_grace_expiry_without_stream_end_is_not_eof() {
        // Deterministic DrainExpired proof: a real worker and a real child,
        // but the test holds the data channel and never sends a stream end,
        // so the grace MUST expire. Also proves the F03 window: between the
        // reap and the outcome publication, the locked signal refuses the pid.
        let params = crate::process::SpawnParams::new("true");
        let (master, child) =
            crate::pty::spawn_pty(&params, 80, 24).expect("spawn true for drain test");
        // No reader thread: drop the reader so the stream can never resolve.
        drop(master.try_clone_reader().expect("reader takes"));
        let writer = master.take_writer().expect("writer takes");
        let killer = SharedChild::new(child);
        let pid = killer.pid().expect("true has a pid");
        let (ctl_tx, ctl_rx) = mpsc::channel::<CtlOp>();
        let (data_tx, data_rx) = mpsc::sync_channel::<DataMsg>(DATA_QUEUE_BATCHES);
        let shared = Arc::new(Shared::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_killer = killer.clone();
        let worker = std::thread::Builder::new()
            .name("drain-test-worker".to_owned())
            .spawn(move || {
                run_worker(WorkerConfig {
                    master,
                    killer: worker_killer,
                    writer,
                    cols: 80,
                    rows: 24,
                    scrollback: 1000,
                    record_output: false,
                    event_cap: DEFAULT_EVENT_CAP,
                    output_cap_bytes: DEFAULT_OUTPUT_CAP_BYTES,
                    data_rx,
                    inflight: Arc::new(AtomicUsize::new(0)),
                    ctl_rx,
                    shutdown: worker_shutdown,
                    shared: worker_shared,
                });
            })
            .expect("worker spawns");
        // Handshake: the worker owns the grid once it answers.
        let (tx, rx) = mpsc::channel();
        ctl_tx.send(CtlOp::Snapshot(tx)).expect("worker alive");
        rx.recv_timeout(Duration::from_secs(10))
            .expect("snapshot answers");
        // Wait for the reap (fast, deterministic); the outcome cannot publish
        // before sighting + grace, so it is still None here by deadline math.
        let start = Instant::now();
        loop {
            if killer.poll().expect("poll").is_some() {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "true never reaped"
            );
            test_sleep_ms(5);
        }
        assert!(shared.outcome().is_none());
        assert_eq!(
            killer.signal_locked(crate::process::SIGTERM),
            Err(SignalError::NotFound { pid }),
            "reaped pid must refuse signals before publication"
        );
        // The grace expires with no stream end: DrainExpired, never EOF.
        let start = Instant::now();
        let outcome = loop {
            if let Some(outcome) = shared.outcome() {
                break outcome;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "grace never expired"
            );
            test_sleep_ms(25);
        };
        assert!(outcome.exit.success());
        assert_eq!(outcome.stream, StreamState::DrainExpired);
        // Cleanup: real shutdown, real join, no reader to wait for.
        shutdown.store(true, Ordering::SeqCst);
        ctl_tx.send(CtlOp::Shutdown).expect("shutdown delivers");
        worker.join().expect("worker joins");
        drop(data_tx);
        assert!(shared.teardown_error().is_none());
    }
}
