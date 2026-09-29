// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Live PTY sessions: a child process, its emulator grid, and the pump threads.
//!
//! [`PtySession::spawn`] builds on the [`crate::pty`] transport: it spawns
//! [`SpawnParams`](crate::process::SpawnParams) into a PTY and starts two
//! threads: a **reader thread** that blocks on the PTY master and forwards
//! byte batches to the worker, and a **worker thread** that owns the
//! [`DamageGrid`](crate::grid::DamageGrid), the PTY writer, and the child
//! handle. All grid access happens on the worker; the handle only sends ops
//! and receives replies over channels, so the session is `Send + Sync` with
//! no `unsafe` anywhere.
//!
//! The worker routes emulator replies ([`Reply`](crate::PassthroughEvent::Reply)
//! events: DA/DSR/DECRQM answers) back to PTY stdin automatically and stashes
//! every other passthrough event for [`PtySession::drain_events`].
//!
//! ## Cleanup
//!
//! [`PtySession::finish`] (graceful: EOF stdin, wait, reap) and
//! [`PtySession::close`] (forceful, idempotent) return teardown errors.
//! `Drop` reaps children and joins threads without double-panicking.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use crate::grid::DamageGrid;
use crate::passthrough::PassthroughEvent;
use crate::process::{ExitStatus, SpawnParams};
use crate::pty::{Master, PtyChild, PtyReader, PtyWriter, spawn_pty};
use crate::snapshot::GridSnapshot;
use crate::width::VirtualTerminalProfile;

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

/// How long after child exit the worker still accepts trailing reader bytes.
const DRAIN_GRACE: Duration = Duration::from_millis(500);
/// Worker tick: child-exit polling cadence.
const WORKER_TICK: Duration = Duration::from_millis(25);
/// Grace for SIGKILL-triggered reap during teardown.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Bound for joining one session thread during teardown. Must exceed the
/// worker's worst case (`KILL_GRACE` + tick). Past this grace the thread is
/// detached, never joined forever.
const JOIN_GRACE: Duration = Duration::from_secs(5);
/// Bound for one worker round-trip (resize, snapshot, ...).
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

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
/// [`SpawnParams`](crate::process::SpawnParams); these options cover only
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
    /// Record every pumped output byte for [`PtySession::output_log`].
    /// Off by default: the log grows without bound.
    pub record_output: bool,
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
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct SharedState {
    exit: Option<ExitStatus>,
    closed: bool,
    teardown_error: Option<String>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<SharedState>,
    changed: Condvar,
}

impl Shared {
    fn publish_exit(&self, status: ExitStatus) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exit.is_none() {
            state.exit = Some(status);
            self.changed.notify_all();
        }
    }

    fn exit(&self) -> Option<ExitStatus> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exit
            .clone()
    }

    fn mark_closed(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.changed.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed
    }

    fn record_teardown(&self, msg: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.teardown_error.is_none() {
            state.teardown_error = Some(msg.to_owned());
        }
    }

    fn teardown_error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .teardown_error
            .clone()
    }

    /// Wait until exit is published, the session closes, or `deadline` passes.
    fn wait_exit_changed(&self, deadline: Instant) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exit.is_some() || state.closed {
            return;
        }
        let now = Instant::now();
        if now < deadline {
            let _guard = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
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
    op_tx: Mutex<Option<mpsc::Sender<Op>>>,
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
    /// # Errors
    ///
    /// Returns [`ProcessError::Spawn`] when the geometry is out of range,
    /// `argv` is empty, or the PTY/child/threads fail to start.
    pub fn spawn(params: &SpawnParams, options: SessionOptions) -> Result<Self, ProcessError> {
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
        let (master, mut child) = spawn_pty(&effective, cols, rows)
            .map_err(|e| ProcessError::Spawn(format!("pty spawn failed: {e}")))?;
        // Drain discipline: take I/O handles before any wait can run.
        let reader = master
            .try_clone_reader()
            .map_err(|e| ProcessError::Spawn(format!("pty reader failed: {e}")))?;
        let writer = master
            .take_writer()
            .map_err(|e| ProcessError::Spawn(format!("pty writer failed: {e}")))?;
        child
            .try_wait()
            .map_err(|e| ProcessError::Spawn(format!("child poll failed: {e}")))?;
        let pid = child.pid();

        let (op_tx, op_rx) = mpsc::channel::<Op>();
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("termpane-session-worker".to_owned())
            .spawn(move || {
                run_worker(
                    master,
                    child,
                    writer,
                    cols,
                    rows,
                    options.scrollback,
                    options.record_output,
                    op_rx,
                    worker_shared,
                );
            })
            .map_err(|e| ProcessError::Spawn(format!("worker spawn failed: {e}")))?;

        let feed_tx = op_tx.clone();
        let reader_thread = std::thread::Builder::new()
            .name("termpane-session-reader".to_owned())
            .spawn(move || run_reader(reader, feed_tx))
            .map_err(|e| ProcessError::Spawn(format!("reader spawn failed: {e}")))?;

        let session = Self {
            op_tx: Mutex::new(Some(op_tx)),
            shared,
            worker: Mutex::new(Some(worker)),
            reader: Mutex::new(Some(reader_thread)),
            closed: AtomicBool::new(false),
            pid,
        };
        // Round-trip the worker: the session is usable only once the worker
        // owns the grid and answers ops.
        session
            .snapshot()
            .map_err(|e| ProcessError::Spawn(format!("worker did not answer: {e}")))?;
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
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn snapshot(&self) -> Result<GridSnapshot, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(Op::Snapshot(tx))?;
        recv_value(rx, "snapshot")
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
        self.send(Op::StateBytes(tx))?;
        recv_value(rx, "state snapshot")
    }

    /// Drain and return the grid's dirty-row set.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn dirty_spans(&self) -> Result<crate::damage::DirtySpans, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(Op::DirtySpans(tx))?;
        recv_value(rx, "dirty spans")
    }

    /// Drain passthrough events the emulator produced (title, bell, clipboard,
    /// ...) since the last call. `Reply` events never appear here: the worker
    /// routes them to PTY stdin automatically.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn drain_events(&self) -> Result<Vec<PassthroughEvent>, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(Op::Events(tx))?;
        recv_value(rx, "events")
    }

    /// Every output byte pumped so far. Empty unless
    /// [`SessionOptions::record_output`] was set: replay into a same-size fresh
    /// grid to reproduce the live state byte-for-byte.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Closed`] when the session is shut down.
    pub fn output_log(&self) -> Result<Vec<u8>, ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(Op::OutputLog(tx))?;
        recv_value(rx, "output log")
    }

    /// Write raw bytes to the child's stdin.
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
        self.send(Op::Write {
            bytes: bytes.to_vec(),
            reply: tx,
        })?;
        recv_reply(rx, "stdin write")
    }

    /// Resize the PTY and the emulator together (PTY ioctl first: if the
    /// kernel refuses, the emulator stays consistent).
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
        self.send(Op::Resize {
            cols,
            rows,
            reply: tx,
        })?;
        recv_reply(rx, "resize")
    }

    /// Deliver a signal to the direct child.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ChildExited`] when the child already exited,
    /// [`ProcessError::Signal`] when delivery failed.
    pub fn signal(&self, signal: Signal) -> Result<(), ProcessError> {
        let pid = self
            .pid
            .ok_or_else(|| ProcessError::Signal("child PID unknown".to_owned()))?;
        if self.shared.exit().is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        match crate::process::signal(pid, signal.number()) {
            Ok(()) => Ok(()),
            Err(crate::process::SignalError::NotFound { .. }) => Err(ProcessError::ChildExited(
                format!("child {pid} no longer exists"),
            )),
            Err(err) => Err(ProcessError::Signal(format!("kill({pid}) failed: {err}"))),
        }
    }

    /// Close the child's stdin (EOF). A second call reports
    /// [`ProcessError::Closed`].
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ChildExited`] when the child already exited,
    /// [`ProcessError::Closed`] when stdin is already closed.
    pub fn close_input(&self) -> Result<(), ProcessError> {
        let (tx, rx) = mpsc::channel();
        self.send(Op::CloseInput { reply: tx })?;
        recv_reply(rx, "close stdin")
    }

    /// Non-blocking exit poll: `Some` once the child is reaped and trailing
    /// output drained, `None` while it runs.
    #[must_use]
    pub fn poll_exit(&self) -> Option<ExitStatus> {
        self.shared.exit()
    }

    /// Wait until the direct child exits, is reaped, and trailing output is
    /// drained — or `deadline` passes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Timeout`] on expiry,
    /// [`ProcessError::Closed`] when the session shut down without an exit.
    pub fn wait_exit(&self, deadline: Instant) -> Result<ExitStatus, ProcessError> {
        loop {
            if let Some(status) = self.shared.exit() {
                return Ok(status);
            }
            if self.shared.is_closed() {
                return Err(ProcessError::Closed(
                    "session closed before child exit".to_owned(),
                ));
            }
            if Instant::now() >= deadline {
                return Err(ProcessError::Timeout("child still alive".to_owned()));
            }
            self.shared.wait_exit_changed(deadline);
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

    /// Forceful idempotent teardown: kill a living child (bounded grace),
    /// reap, join threads. Returns the first teardown error, if any.
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

    fn send(&self, op: Op) -> Result<(), ProcessError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ProcessError::Closed("session is closed".to_owned()));
        }
        let tx = self.op_tx.lock().unwrap_or_else(PoisonError::into_inner);
        match tx.as_ref() {
            Some(tx) => tx
                .send(op)
                .map_err(|_| ProcessError::Closed("worker is gone".to_owned())),
            None => Err(ProcessError::Closed("session is closed".to_owned())),
        }
    }

    /// Run teardown exactly once; never panics (safe from `Drop`).
    fn teardown(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            // A previous close/drop already shut down; still join in case a
            // concurrent teardown is in flight.
            self.join_threads();
            return;
        }
        {
            let mut tx = self.op_tx.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(tx) = tx.take() {
                let _ignored = tx.send(Op::Shutdown);
            }
        }
        self.join_threads();
    }

    fn join_threads(&mut self) {
        let worker = self.worker.lock().map_or(None, |mut g| g.take());
        let reader = self.reader.lock().map_or(None, |mut g| g.take());
        if let Some(h) = worker {
            join_one(h, &self.shared, "worker", JOIN_GRACE);
        }
        if let Some(h) = reader {
            join_one(h, &self.shared, "reader", JOIN_GRACE);
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

/// Bounded join of one session thread; never blocks past `grace`, never
/// panics. On timeout the handle is detached and a teardown diagnostic is
/// recorded, so `Drop` can never hang on a reader blocked in `read()`
/// after a failed child kill.
fn join_one(h: std::thread::JoinHandle<()>, shared: &Shared, name: &str, grace: Duration) {
    let (tx, rx) = mpsc::channel::<bool>();
    let waiter = std::thread::Builder::new()
        .name(format!("termpane-session-join-{name}"))
        .spawn(move || {
            let panicked = h.join().is_err();
            let _ignored = tx.send(panicked);
        });
    match waiter {
        Ok(_waiter) => match rx.recv_timeout(grace) {
            Ok(true) => shared.record_teardown(&format!("{name} thread panicked")),
            Ok(false) => {}
            Err(_) => shared.record_teardown(&format!(
                "{name} thread did not exit within {grace:?}; detached"
            )),
        },
        Err(e) => shared.record_teardown(&format!("join waiter spawn failed for {name}: {e}")),
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

// ---------------------------------------------------------------------------
// Worker thread: sole owner of grid, writer, and child
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Op {
    Feed(Vec<u8>),
    Eof,
    Snapshot(mpsc::Sender<GridSnapshot>),
    StateBytes(mpsc::Sender<Vec<u8>>),
    DirtySpans(mpsc::Sender<crate::damage::DirtySpans>),
    Events(mpsc::Sender<Vec<PassthroughEvent>>),
    OutputLog(mpsc::Sender<Vec<u8>>),
    Write {
        bytes: Vec<u8>,
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    Resize {
        cols: u16,
        rows: u16,
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    CloseInput {
        reply: mpsc::Sender<Result<(), ProcessError>>,
    },
    Shutdown,
}

struct Worker {
    master: Master,
    child: PtyChild,
    writer: Option<PtyWriter>,
    grid: DamageGrid,
    events: Vec<PassthroughEvent>,
    output_log: Vec<u8>,
    record_output: bool,
    eof: bool,
    exited: Option<ExitStatus>,
    exit_seen_at: Option<Instant>,
    finalized: bool,
    shared: Arc<Shared>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "worker entry takes its owned resources explicitly; a parameter \
              struct would only rename the same nine fields"
)]
fn run_worker(
    master: Master,
    child: PtyChild,
    writer: PtyWriter,
    cols: u16,
    rows: u16,
    scrollback: usize,
    record_output: bool,
    op_rx: mpsc::Receiver<Op>,
    shared: Arc<Shared>,
) {
    let mut worker = Worker {
        master,
        child,
        writer: Some(writer),
        grid: DamageGrid::new(rows, cols, scrollback),
        events: Vec::new(),
        output_log: Vec::new(),
        record_output,
        eof: false,
        exited: None,
        exit_seen_at: None,
        finalized: false,
        shared,
    };

    loop {
        match op_rx.recv_timeout(WORKER_TICK) {
            Ok(op) => {
                if worker.handle_op(op) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Handle and reader both gone: reap and go away quietly.
                shutdown_child(&mut worker.child, &worker.shared);
                worker.shared.mark_closed();
                return;
            }
        }
        worker.poll_exit_state();
    }
}

impl Worker {
    /// Handle one op. Returns true when the worker must exit.
    fn handle_op(&mut self, op: Op) -> bool {
        match op {
            Op::Feed(bytes) => self.feed(bytes),
            Op::Eof => self.eof = true,
            Op::Snapshot(reply) => {
                let _ignored = reply.send(self.grid.dump());
            }
            Op::StateBytes(reply) => {
                let _ignored = reply.send(self.grid.state_formatted());
            }
            Op::DirtySpans(reply) => {
                let _ignored = reply.send(self.grid.dirty_spans());
            }
            Op::Events(reply) => {
                let _ignored = reply.send(std::mem::take(&mut self.events));
            }
            Op::OutputLog(reply) => {
                let _ignored = reply.send(self.output_log.clone());
            }
            Op::Write { bytes, reply } => {
                let r = self.apply_write(&bytes);
                let _ignored = reply.send(r);
            }
            Op::Resize { cols, rows, reply } => {
                let r = self.apply_resize(cols, rows);
                let _ignored = reply.send(r);
            }
            Op::CloseInput { reply } => {
                let r = self.close_input();
                let _ignored = reply.send(r);
            }
            Op::Shutdown => {
                shutdown_child(&mut self.child, &self.shared);
                if let Ok(Some(status)) = self.child.try_wait() {
                    self.exited = Some(status);
                }
                if !self.finalized {
                    self.shared
                        .publish_exit(self.exited.clone().unwrap_or_else(ExitStatus::unknown));
                }
                self.shared.mark_closed();
                return true;
            }
        }
        false
    }

    fn feed(&mut self, bytes: Vec<u8>) {
        self.grid.process(&bytes);
        if self.record_output {
            self.output_log.extend_from_slice(&bytes);
        }
        for event in self.grid.drain_passthrough() {
            match event {
                PassthroughEvent::Reply(reply) => {
                    // Route emulator answers (DA/DSR/DECRQM/kitty queries)
                    // to PTY stdin. Best-effort: a failed write means the
                    // child is dying anyway.
                    if let Some(w) = self.writer.as_mut() {
                        let _ignored = w.write_all(&reply);
                    }
                }
                other => self.events.push(other),
            }
        }
    }

    fn apply_write(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.exited.is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| ProcessError::Closed("stdin is closed".to_owned()))?;
        writer
            .write_all(bytes)
            .map_err(|e| ProcessError::Io(format!("pty write failed: {e}")))
    }

    fn apply_resize(&mut self, cols: u16, rows: u16) -> Result<(), ProcessError> {
        // PTY first: if the kernel refuses, the emulator stays consistent.
        self.master
            .resize(cols, rows)
            .map_err(|e| ProcessError::Io(format!("pty resize failed: {e}")))?;
        self.grid.set_size(rows, cols);
        Ok(())
    }

    fn close_input(&mut self) -> Result<(), ProcessError> {
        if self.exited.is_some() {
            return Err(ProcessError::ChildExited("child already exited".to_owned()));
        }
        if self.writer.take().is_some() {
            Ok(())
        } else {
            Err(ProcessError::Closed("stdin already closed".to_owned()))
        }
    }

    /// Reap promptly, but give trailing output `DRAIN_GRACE` after the child
    /// dies before publishing the final exit.
    fn poll_exit_state(&mut self) {
        if self.finalized {
            return;
        }
        if self.exited.is_none() {
            // A failed poll is transient (the backend retries next tick);
            // only a reaped status counts as an exit sighting.
            if let Some(status) = self.child.try_wait().ok().flatten() {
                self.exited = Some(status);
                self.exit_seen_at = Some(Instant::now());
            }
        }
        let drained = self.eof
            || self
                .exit_seen_at
                .is_some_and(|t| t.elapsed() >= DRAIN_GRACE);
        if self.exited.is_some() && drained {
            self.finalized = true;
            if let Some(status) = self.exited.clone() {
                self.shared.publish_exit(status);
            }
        }
    }
}

/// Bounded kill + reap on a worker-owned thread. Records teardown errors
/// instead of failing; never blocks past `KILL_GRACE`.
fn shutdown_child(child: &mut PtyChild, shared: &Shared) {
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => {
            shared.record_teardown(&format!("child poll during teardown failed: {e}"));
        }
    }
    if let Err(e) = child.kill() {
        shared.record_teardown(&format!("child kill during teardown failed: {e}"));
    }
    let deadline = Instant::now() + KILL_GRACE;
    loop {
        match child.try_wait() {
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

fn run_reader(mut reader: PtyReader, tx: mpsc::Sender<Op>) {
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ignored = tx.send(Op::Eof);
                return;
            }
            Ok(n) => {
                if tx.send(Op::Feed(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            // A read error at EOF (e.g. Linux EIO after child death) ends
            // the pump; the exit status is authoritative, not the error.
            Err(_) => {
                let _ignored = tx.send(Op::Eof);
                return;
            }
        }
    }
}
