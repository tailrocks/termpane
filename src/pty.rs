//! PTY transport: master/slave allocation, spawn-into-PTY, resize.
//!
//! This module owns the PTY half of a terminal session: allocating the pair,
//! spawning [`SpawnParams`] into the slave,
//! splitting the master into a reader and a writer, and resizing the kernel
//! window size (which delivers `SIGWINCH`). It is Unix-only and available
//! only with the `pty` feature, which implies `process`.
//!
//! Two entry points:
//!
//! - [`spawn_pty`] does open + spawn + parent-slave-drop in one call under a
//!   single lifecycle lock. Prefer it.
//! - [`openpty`] plus [`Slave::spawn`] splits the steps for callers that need
//!   the slave handle in between. Drop the [`Slave`] before reading, or the
//!   master never sees EOF after child exit (Linux suppresses it while any
//!   slave fd stays open in the parent).
//!
//! EOF convention: the reader reports end-of-output as `Ok(0)` (macOS) or an
//! `EIO` error (Linux, after child exit). Consumers must treat both as EOF;
//! anything else is a real error.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::process::{ExitStatus, SignalError, SpawnParams};

/// Serializes PTY open/spawn process-wide.
///
/// This guards a kernel race (macOS `revoke()`), not an emulator bug: every
/// `portable-pty` consumer needs it. Held across open + spawn; see also the
/// transport spawn lock in [`crate::process`], which is always taken *after*
/// this one (lock order is fixed, so nesting cannot deadlock).
static PTY_LIFECYCLE: Mutex<()> = Mutex::new(());

/// Lock [`PTY_LIFECYCLE`], recovering from poison (mutual exclusion only, no
/// protected invariant, so recovery is safe).
fn lock_lifecycle() -> std::sync::MutexGuard<'static, ()> {
    PTY_LIFECYCLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure to allocate, spawn into, or drive a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyError {
    /// Spawn with no argv element 0.
    EmptyArgv,
    /// A zero column or row count. Sizes are `(cols, rows)` everywhere in
    /// this module — note the order differs from grid `(rows, cols)` APIs.
    InvalidSize {
        /// Requested columns.
        cols: u16,
        /// Requested rows.
        rows: u16,
    },
    /// The PTY backend failed; the message carries its report.
    Backend(String),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyArgv => write!(f, "pty spawn: empty argv"),
            Self::InvalidSize { cols, rows } => {
                write!(
                    f,
                    "pty size {cols}x{rows}: columns and rows must be nonzero"
                )
            }
            Self::Backend(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PtyError {}

/// Backend failure into [`PtyError::Backend`], keeping the error chain.
fn backend(err: impl std::fmt::Display) -> PtyError {
    PtyError::Backend(format!("{err:#}"))
}

fn check_size(cols: u16, rows: u16) -> Result<(), PtyError> {
    if cols == 0 || rows == 0 {
        return Err(PtyError::InvalidSize { cols, rows });
    }
    Ok(())
}

/// [`SpawnParams`] into a `portable-pty` command: verbatim argv, inherited
/// environment plus the child-only edits in the documented order (clear,
/// then removals, then overrides), optional cwd. `detached` is accepted
/// and implied — PTY children always start a new session with a controlling
/// terminal.
///
/// Backend caveat: `portable-pty` always sets `SHELL` at spawn (from the
/// builder env, else the passwd-database shell), so `env_clear` still leaves
/// `SHELL` in the child and `env_remove("SHELL")` is ineffective — but a
/// `SHELL` override wins.
fn command_builder(params: &SpawnParams) -> Result<portable_pty::CommandBuilder, PtyError> {
    if params.argv().is_empty() {
        return Err(PtyError::EmptyArgv);
    }
    let mut cmd = portable_pty::CommandBuilder::from_argv(params.argv().to_vec());
    if params.is_env_cleared() {
        cmd.env_clear();
    }
    for key in params.env_removed() {
        cmd.env_remove(key);
    }
    for (key, value) in params.env_overrides() {
        cmd.env(key, value);
    }
    if let Some(cwd) = params.cwd() {
        cmd.cwd(cwd);
    }
    Ok(cmd)
}

fn convert_status(status: portable_pty::ExitStatus) -> ExitStatus {
    // Same convention as piped children: signal death carries code 1 plus a
    // human-readable (opaque) signal description.
    ExitStatus::from_pty(status.exit_code(), status.signal())
}

// ---------------------------------------------------------------------------
// Allocation and spawn
// ---------------------------------------------------------------------------

/// Allocate a PTY pair at `cols` x `rows` (pixel dimensions are 0).
///
/// Returns the parent-side [`Master`] and the [`Slave`] to spawn into. Drop
/// the slave before reading the master (or use [`spawn_pty`], which does both
/// plus the spawn under one lock).
///
/// # Errors
///
/// Returns [`PtyError`] when the size is degenerate or allocation fails.
pub fn openpty(cols: u16, rows: u16) -> Result<(Master, Slave), PtyError> {
    check_size(cols, rows)?;
    let _lifecycle = lock_lifecycle();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(backend)?;
    Ok((Master { inner: pair.master }, Slave { inner: pair.slave }))
}

/// Allocate a PTY, spawn `params` into it, and drop the parent slave handle.
///
/// One call, one lifecycle lock, correct EOF discipline: the slave is dropped
/// before this returns, so the master reports EOF/EIO once the child exits
/// instead of hanging (Linux holds EOF while any parent slave fd is open).
/// Take the reader/writer from the [`Master`] before polling the child.
///
/// Environment: inherited entries plus the child-only edits in the
/// [`SpawnParams`](crate::process::SpawnParams) order (clear, removals,
/// overrides). Backend caveat: `portable-pty` always re-adds `SHELL` at
/// spawn, so a cleared child still sees `SHELL`, removing `SHELL` is
/// ineffective, and only a `SHELL` override changes it.
///
/// # Errors
///
/// Returns [`PtyError`] when argv is empty, the size is degenerate, or the
/// backend fails to allocate or spawn.
pub fn spawn_pty(
    params: &SpawnParams,
    cols: u16,
    rows: u16,
) -> Result<(Master, PtyChild), PtyError> {
    check_size(cols, rows)?;
    let cmd = command_builder(params)?;
    let _lifecycle = lock_lifecycle();
    let _transport = crate::process::lock_transport_spawn();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(backend)?;
    let child = pair.slave.spawn_command(cmd).map_err(backend)?;
    drop(pair.slave);
    Ok((
        Master { inner: pair.master },
        PtyChild {
            inner: child,
            reaped: AtomicBool::new(false),
        },
    ))
}

// ---------------------------------------------------------------------------
// Master / Slave
// ---------------------------------------------------------------------------

/// The parent side of a PTY: resize, size readback, reader/writer splitting.
///
/// `Send` (safe to hand to a reader thread); see [`PtyReader`] and
/// [`PtyWriter`] for the split handles.
pub struct Master {
    inner: Box<dyn portable_pty::MasterPty>,
}

impl std::fmt::Debug for Master {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Master").finish_non_exhaustive()
    }
}

impl Master {
    /// Split off a blocking reader for child output (`Send`).
    ///
    /// Reads return child bytes; end-of-output is `Ok(0)` (macOS) or an `EIO`
    /// error (Linux, after child exit) — treat both as EOF.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when the backend cannot clone the reader.
    pub fn try_clone_reader(&self) -> Result<PtyReader, PtyError> {
        self.inner
            .try_clone_reader()
            .map(|inner| PtyReader { inner })
            .map_err(backend)
    }

    /// Take the writer for child input (`Send`).
    ///
    /// Valid to take only once; dropping the writer asks the backend to inject
    /// its EOF sequence (newline + `VEOF`) — a canonical-mode EOF request,
    /// not a universal half-close (see [`PtyWriter`).
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when the backend cannot hand out the writer
    /// (including a second take).
    pub fn take_writer(&self) -> Result<PtyWriter, PtyError> {
        self.inner
            .take_writer()
            .map(|inner| PtyWriter { inner })
            .map_err(backend)
    }

    /// Resize the kernel window to `cols` x `rows` and deliver `SIGWINCH`.
    ///
    /// This is the PTY half of a resize; the emulator grid half (if any)
    /// stays with the caller.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when the size is degenerate or the ioctl fails.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        check_size(cols, rows)?;
        self.inner
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(backend)
    }

    /// Kernel window size as `(cols, rows)`; used to verify a [`Master::resize`].
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when the backend cannot report the size.
    pub fn size(&self) -> Result<(u16, u16), PtyError> {
        self.inner
            .get_size()
            .map_err(backend)
            .map(|size| (size.cols, size.rows))
    }
}

/// The child side of a PTY: spawn into it, then drop it before reading.
///
/// Not `Send`: use (and drop) on the thread that allocated it. The drop is
/// load-bearing — a parent-held slave fd suppresses master EOF/EIO on Linux.
pub struct Slave {
    inner: Box<dyn portable_pty::SlavePty>,
}

impl std::fmt::Debug for Slave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slave").finish_non_exhaustive()
    }
}

impl Slave {
    /// Spawn `params` into this PTY.
    ///
    /// Same environment contract as [`spawn_pty`]: clear, removals, then
    /// overrides, except the backend always re-adds `SHELL`.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] when argv is empty or the backend spawn fails.
    pub fn spawn(&self, params: &SpawnParams) -> Result<PtyChild, PtyError> {
        let cmd = command_builder(params)?;
        let _lifecycle = lock_lifecycle();
        let _transport = crate::process::lock_transport_spawn();
        self.inner
            .spawn_command(cmd)
            .map(|inner| PtyChild {
                inner,
                reaped: AtomicBool::new(false),
            })
            .map_err(backend)
    }
}

// ---------------------------------------------------------------------------
// Reader / writer
// ---------------------------------------------------------------------------

/// Blocking reader for child output (`Send`, hand to a reader thread).
///
/// See [`Master::try_clone_reader`] for the EOF convention.
pub struct PtyReader {
    inner: Box<dyn std::io::Read + Send>,
}

impl std::fmt::Debug for PtyReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyReader").finish_non_exhaustive()
    }
}

impl std::io::Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

/// Writer for child input (`Send`).
///
/// Dropping the writer asks the backend to inject a newline plus the `VEOF`
/// character before closing the fd, so the reader may observe line-discipline
/// echo artifacts (e.g. `^D`). That is a canonical-mode EOF request only: a
/// child in raw mode reads the injected bytes as ordinary input and may keep
/// running. Portable PTYs offer no true half-close; take only once per
/// master — see [`Master::take_writer`].
pub struct PtyWriter {
    inner: Box<dyn std::io::Write + Send>,
}

impl std::fmt::Debug for PtyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyWriter").finish_non_exhaustive()
    }
}

impl std::io::Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Child
// ---------------------------------------------------------------------------

/// A child running in a PTY: poll, wait, kill, signal.
///
/// Dropping the handle does not kill or wait for the child; reap with
/// [`PtyChild::wait`] or [`PtyChild::try_wait`]. Once reaped, the pid is never
/// signalled again (see [`PtyChild::signal`]).
pub struct PtyChild {
    inner: Box<dyn portable_pty::Child + Send + Sync>,
    /// Set once [`PtyChild::try_wait`] or [`PtyChild::wait`] observes the
    /// exit. The flag is read by `signal`/`kill` and written only under `&mut`
    /// access, so safe callers cannot race the transition — the same shape as
    /// the piped transport's post-reap rule.
    reaped: AtomicBool,
}

impl std::fmt::Debug for PtyChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyChild")
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

impl PtyChild {
    /// Process id of the child, when the backend knows it.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.inner.process_id()
    }

    /// True once [`PtyChild::try_wait`] or [`PtyChild::wait`] observed the
    /// exit: the pid is dead to this handle from here on.
    #[must_use]
    pub fn is_reaped(&self) -> bool {
        self.reaped.load(Ordering::SeqCst)
    }

    fn mark_reaped(&self) {
        self.reaped.store(true, Ordering::SeqCst);
    }

    /// Poll for exit without blocking: `Ok(None)` means still running.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the underlying poll fails.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self.inner.try_wait()?.map(convert_status);
        if status.is_some() {
            self.mark_reaped();
        }
        Ok(status)
    }

    /// Block until the child exits and return its status.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the underlying wait fails.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let status = self.inner.wait().map(convert_status);
        if status.is_ok() {
            self.mark_reaped();
        }
        status
    }

    /// Kill the child (`SIGKILL`).
    ///
    /// A kill on an already-reaped child is a silent no-op returning `Ok`
    /// (matching `std`): the pid is never signalled after reaping, so kernel
    /// pid reuse cannot redirect the signal at an unrelated process.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the backend kill fails.
    pub fn kill(&mut self) -> std::io::Result<()> {
        if self.is_reaped() {
            return Ok(());
        }
        self.inner.kill()
    }

    /// Deliver `signo` to the child; see [`crate::process::signal`].
    ///
    /// Never targets the pid after the child was reaped (via
    /// [`PtyChild::wait`] or [`PtyChild::try_wait`]): the kernel may already
    /// have recycled the pid for an unrelated process, so a reaped handle
    /// reports [`SignalError::NotFound`] without a syscall. An exited but
    /// still unreaped child keeps its pid (zombie), so signalling it stays a
    /// harmless delivered-or-ESRCH round trip.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError::UnknownPid`] when the backend does not know the
    /// pid, [`SignalError::NotFound`] for a reaped child without signalling,
    /// else exactly like [`crate::process::signal`].
    pub fn signal(&self, signo: i32) -> Result<(), SignalError> {
        let Some(pid) = self.pid() else {
            return Err(SignalError::UnknownPid);
        };
        if self.is_reaped() {
            return Err(SignalError::NotFound { pid });
        }
        crate::process::signal(pid, signo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_params_map_to_command_builder_verbatim() {
        let params = SpawnParams::new("echo")
            .arg("hi")
            .arg("a b")
            .env("TP2_PTY", "1")
            .current_dir("/tmp");
        let cmd = command_builder(&params).expect("valid params convert");
        assert_eq!(
            cmd.get_argv(),
            &[
                std::ffi::OsString::from("echo"),
                std::ffi::OsString::from("hi"),
                std::ffi::OsString::from("a b"),
            ]
        );
        assert_eq!(cmd.get_env("TP2_PTY"), Some(std::ffi::OsStr::new("1")));
        assert_eq!(cmd.get_cwd(), Some(&std::ffi::OsString::from("/tmp")));
    }

    #[test]
    fn env_clear_remove_override_forward_in_documented_order() {
        // PATH is inherited in every test environment, so its absence
        // proves the edit reached the builder (not a vacuous pass).
        assert!(std::env::var_os("PATH").is_some());
        let cleared = SpawnParams::new("true").env_clear().env("TP2_KEPT", "1");
        let cmd = command_builder(&cleared).expect("valid params convert");
        assert_eq!(cmd.get_env("PATH"), None);
        assert_eq!(cmd.get_env("TP2_KEPT"), Some(std::ffi::OsStr::new("1")));

        let removed = SpawnParams::new("true").env_remove("PATH");
        let cmd = command_builder(&removed).expect("valid params convert");
        assert_eq!(cmd.get_env("PATH"), None);

        // Overrides win over removals for the same key.
        let revived = SpawnParams::new("true")
            .env_remove("PATH")
            .env("PATH", "/custom-tp2");
        let cmd = command_builder(&revived).expect("valid params convert");
        assert_eq!(
            cmd.get_env("PATH"),
            Some(std::ffi::OsStr::new("/custom-tp2"))
        );
    }

    #[test]
    fn empty_argv_and_zero_sizes_are_rejected() {
        let empty = SpawnParams::default();
        assert!(matches!(command_builder(&empty), Err(PtyError::EmptyArgv)));
        assert_eq!(
            check_size(0, 24),
            Err(PtyError::InvalidSize { cols: 0, rows: 24 })
        );
        assert_eq!(
            check_size(80, 0),
            Err(PtyError::InvalidSize { cols: 80, rows: 0 })
        );
        assert_eq!(check_size(80, 24), Ok(()));
    }

    #[test]
    fn pty_error_display() {
        assert_eq!(PtyError::EmptyArgv.to_string(), "pty spawn: empty argv");
        assert_eq!(
            PtyError::InvalidSize { cols: 0, rows: 24 }.to_string(),
            "pty size 0x24: columns and rows must be nonzero"
        );
        assert_eq!(PtyError::Backend(String::from("boom")).to_string(), "boom");
    }

    #[test]
    fn pty_status_conversion_keeps_code_and_signal() {
        let clean = convert_status(portable_pty::ExitStatus::with_exit_code(3));
        assert_eq!(clean.exit_code(), 3);
        assert_eq!(clean.signal(), None);
        assert!(!clean.success());

        let signaled = convert_status(portable_pty::ExitStatus::with_signal("Terminated"));
        assert_eq!(signaled.exit_code(), 1);
        assert_eq!(signaled.signal(), Some("Terminated"));
        assert!(!signaled.success());
    }

    #[test]
    fn pty_handles_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Master>();
        assert_send::<PtyReader>();
        assert_send::<PtyWriter>();
        assert_send::<PtyChild>();
    }
}
