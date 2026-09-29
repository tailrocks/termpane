//! Piped-process transport: spawn parameters, piped spawn, and identity probes.
//!
//! This module owns the fork/exec dance for piped (non-PTY) children plus the
//! small Unix identity surface saturation harnesses need: liveness probes,
//! signal delivery with distinct "no such process" errors, and
//! pid/pgid/sid/uid wrappers. It is Unix-only and available only with the
//! `process` feature; the default model-only build has no host effects.
//!
//! PTY children live in [`crate::pty`], which consumes spawn parameters so
//! both transports share one argv/env/cwd vocabulary.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes every transport spawn in this process.
///
/// macOS has no atomic close-on-exec pipe (`pipe2`), so detached spawn sets
/// `FD_CLOEXEC` with `fcntl` after `pipe()`. The lock keeps that window from
/// racing another termpane spawn in a sibling thread (whose child would
/// otherwise inherit the pipe end and suppress EOF). It doubles as the
/// process-wide spawn serialization point alongside the PTY lifecycle guard.
pub(crate) static TRANSPORT_SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Lock [`TRANSPORT_SPAWN_LOCK`], recovering from poison.
///
/// A poisoned mutex means a previous spawn panicked while holding it; the
/// lock guards no invariant beyond mutual exclusion, so recovery is safe.
pub(crate) fn lock_transport_spawn() -> std::sync::MutexGuard<'static, ()> {
    TRANSPORT_SPAWN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Signal numbers
// ---------------------------------------------------------------------------

/// `SIGHUP` (1): hangup. Value taken from `nix`, never handwritten.
pub const SIGHUP: i32 = nix::sys::signal::Signal::SIGHUP as i32;
/// `SIGINT` (2): interrupt. Value taken from `nix`, never handwritten.
pub const SIGINT: i32 = nix::sys::signal::Signal::SIGINT as i32;
/// `SIGQUIT` (3): quit. Value taken from `nix`, never handwritten.
pub const SIGQUIT: i32 = nix::sys::signal::Signal::SIGQUIT as i32;
/// `SIGKILL` (9): kill, uncatchable. Value taken from `nix`, never handwritten.
pub const SIGKILL: i32 = nix::sys::signal::Signal::SIGKILL as i32;
/// `SIGTERM` (15): termination request. Value taken from `nix`, never handwritten.
pub const SIGTERM: i32 = nix::sys::signal::Signal::SIGTERM as i32;

// ---------------------------------------------------------------------------
// Spawn parameters
// ---------------------------------------------------------------------------

/// How to start a child process: argv, environment overrides, cwd, detach flag.
///
/// Shared by piped spawn ([`spawn_piped`]) and PTY spawn
/// ([`crate::pty::spawn_pty`]) so both transports agree on the vocabulary:
///
/// - `argv` is passed verbatim (no shell, no splitting, no globbing).
/// - The child inherits the parent environment plus these overrides; the
///   parent environment is never modified. There is intentionally no
///   unset/clear operation: fixtures that need scrubbed variables (e.g. temp
///   `HOME`) express them as overrides.
/// - `TERM` is *not* defaulted here: the caller sets it when the child needs
///   one (PTY sessions) and leaves piped children alone.
/// - `cwd` defaults to inheriting the parent working directory.
/// - `detached` requests a new session for piped children (see
///   [`spawn_piped`]); PTY children always start a new session, so the flag
///   is accepted and implied on that path.
#[derive(Debug, Clone, Default)]
pub struct SpawnParams {
    argv: Vec<OsString>,
    env_overrides: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
    detached: bool,
}

impl SpawnParams {
    /// Start parameters for `program` (argv element 0, used for lookup as-is).
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            argv: vec![program.into()],
            env_overrides: Vec::new(),
            cwd: None,
            detached: false,
        }
    }

    /// Append one verbatim argument.
    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.argv.push(arg.as_ref().to_os_string());
        self
    }

    /// Append several verbatim arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.argv.push(arg.as_ref().to_os_string());
        }
        self
    }

    /// Override (or add) one child-only environment variable.
    ///
    /// Overrides win over inherited entries with the same key. Neither this
    /// call nor the spawn touches the parent environment.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env_overrides
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Run the child in `dir` instead of inheriting the parent cwd.
    #[must_use]
    pub fn current_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Detach the child into a new session (piped spawn; see [`spawn_piped`]).
    ///
    /// Detached children get `pgid == pid` and `sid == pid` and survive the
    /// parent. PTY spawn implies this unconditionally.
    #[must_use]
    pub fn detached(mut self, detached: bool) -> Self {
        self.detached = detached;
        self
    }

    /// Verbatim argv, element 0 first.
    #[must_use]
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }

    /// Child-only environment overrides, in insertion order.
    #[must_use]
    pub fn env_overrides(&self) -> &[(OsString, OsString)] {
        &self.env_overrides
    }

    /// Working directory override, if any.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Whether the child should start detached in a new session.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        self.detached
    }
}

// ---------------------------------------------------------------------------
// Piped stdio
// ---------------------------------------------------------------------------

/// What a piped child sees on one standard stream.
#[derive(Debug, Default)]
pub enum Stdio {
    /// `/dev/null`: reads see EOF, writes are discarded.
    #[default]
    Null,
    /// Inherit this process's stream.
    Inherit,
    /// A fresh pipe; the parent end is available via the take methods on
    /// [`PipedChild`] ([`PipedChild::take_stdin`], [`PipedChild::take_stdout`],
    /// [`PipedChild::take_stderr`]).
    Pipe,
    /// Duplicate this open file onto the stream. The caller keeps ownership;
    /// the file only needs to stay alive for the [`spawn_piped`] call.
    File(std::fs::File),
}

/// Standard-stream wiring for [`spawn_piped`].
///
/// Default: all three streams are [`Stdio::Null`] (no inherited fds, no hangs
/// on unread pipes). Override per stream with the builder methods.
#[derive(Debug, Default)]
pub struct PipedStdio {
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
}

impl PipedStdio {
    /// All streams [`Stdio::Null`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the child's stdin.
    #[must_use]
    pub fn stdin(mut self, stdin: Stdio) -> Self {
        self.stdin = stdin;
        self
    }

    /// Wire the child's stdout.
    #[must_use]
    pub fn stdout(mut self, stdout: Stdio) -> Self {
        self.stdout = stdout;
        self
    }

    /// Wire the child's stderr.
    #[must_use]
    pub fn stderr(mut self, stderr: Stdio) -> Self {
        self.stderr = stderr;
        self
    }
}

// ---------------------------------------------------------------------------
// Exit status
// ---------------------------------------------------------------------------

/// How a child terminated: an exit code, or death by signal.
///
/// Signal death follows the `portable-pty` convention: `code` is 1 and
/// `signal` carries a human-readable description. The description text is
/// spawn-path dependent (PTY statuses pass through `portable-pty`'s
/// `strsignal` text such as `"Terminated"`; piped statuses use `"SIGTERM"`
/// style names), so consumers must treat it as opaque text and branch only on
/// presence, never on content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    code: u32,
    signal: Option<String>,
}

impl ExitStatus {
    /// Exit code; 1 for signal death (which carries no code).
    #[must_use]
    pub fn exit_code(&self) -> u32 {
        self.code
    }

    /// Human-readable signal description when killed by a signal.
    ///
    /// Opaque text: compare only against `None`, never match content.
    #[must_use]
    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    /// True only for a clean `exit(0)`: signal death is never success.
    #[must_use]
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == 0
    }

    /// Assemble from PTY-backend parts (crate-internal; see `pty` module).
    #[cfg(feature = "pty")]
    pub(crate) fn from_pty(code: u32, signal: Option<&str>) -> Self {
        Self {
            code,
            signal: signal.map(str::to_owned),
        }
    }
}

impl std::fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.signal {
            Some(name) => write!(f, "terminated by {name}"),
            None => write!(f, "exited with code {}", self.code),
        }
    }
}

/// Signal number to display name: `"SIGTERM"` style, with a stable fallback.
///
/// `nix` covers the standard signals; anything else (e.g. realtime signals
/// outside the enum) renders as `"Signal <n>"`, mirroring `portable-pty`'s
/// fallback spelling.
fn signame(signo: i32) -> String {
    nix::sys::signal::Signal::try_from(signo)
        .map_or_else(|_| format!("Signal {signo}"), |sig| sig.as_str().to_owned())
}

/// Convert a `std` exit status, preserving the signal-death convention.
fn convert_std(status: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(signo) = status.signal() {
        let code = status
            .code()
            .and_then(|code| u32::try_from(code).ok())
            .unwrap_or(1);
        ExitStatus {
            code,
            signal: Some(signame(signo)),
        }
    } else {
        let code = status
            .code()
            .and_then(|code| u32::try_from(code).ok())
            .unwrap_or_else(|| u32::from(!status.success()));
        ExitStatus { code, signal: None }
    }
}

/// Convert a `waitpid` status; `None` means still running.
fn convert_wait(status: nix::sys::wait::WaitStatus) -> Option<ExitStatus> {
    use nix::sys::wait::WaitStatus as W;
    match status {
        W::Exited(_, code) => Some(ExitStatus {
            code: u32::try_from(code).unwrap_or(1),
            signal: None,
        }),
        W::Signaled(_, sig, _) => Some(ExitStatus {
            code: 1,
            signal: Some(sig.as_str().to_owned()),
        }),
        // Stopped/continued/ptrace states cannot occur without the matching
        // wait flags; treat any of them as "still running".
        W::StillAlive | W::Stopped(..) | W::Continued(..) => None,
        #[cfg(target_os = "linux")]
        W::PtraceEvent(..) | W::PtraceSyscall(..) => None,
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Failure to deliver a signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    /// No such process (`ESRCH`), already reaped, out of pid range — or pid
    /// 0, which is refused outright (it would address a process group).
    NotFound {
        /// The pid that could not be signalled.
        pid: u32,
    },
    /// The child handle has no known pid, so there is nothing to signal.
    UnknownPid,
    /// Not a deliverable signal number on this platform.
    InvalidSignal(i32),
    /// The `kill` failed with another errno (e.g. `EPERM`).
    Failed {
        /// The pid that could not be signalled.
        pid: u32,
        /// What went wrong, with the errno description.
        message: String,
    },
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { pid } => write!(f, "no such process: {pid}"),
            Self::UnknownPid => write!(f, "child pid unknown"),
            Self::InvalidSignal(signo) => write!(f, "invalid signal number: {signo}"),
            Self::Failed { pid, message } => {
                write!(f, "failed to signal process {pid}: {message}")
            }
        }
    }
}

impl std::error::Error for SignalError {}

/// `u32` pid to `nix` pid; out-of-range pids cannot exist.
fn pid_from_u32(pid: u32) -> Option<nix::unistd::Pid> {
    i32::try_from(pid).ok().map(nix::unistd::Pid::from_raw)
}

/// Raw pid to `u32`; kernel pids are positive, so 0 is an unreachable-in
/// practice sentinel for out-of-range values.
fn pid_to_u32(raw: i32) -> u32 {
    u32::try_from(raw).unwrap_or(0)
}

/// Deliver `signo` to `pid` (`kill`).
///
/// `ESRCH` surfaces distinctly as [`SignalError::NotFound`]; pid 0 is refused
/// with the same variant (addressing a group by accident is never what a
/// caller wants). Unknown signal numbers yield
/// [`SignalError::InvalidSignal`] before any syscall runs.
///
/// # Errors
///
/// Returns [`SignalError`] when the pid does not exist, the signal number is
/// unknown, or the `kill` fails (e.g. permission denied).
pub fn signal(pid: u32, signo: i32) -> Result<(), SignalError> {
    if pid == 0 {
        return Err(SignalError::NotFound { pid });
    }
    let Some(target) = pid_from_u32(pid) else {
        return Err(SignalError::NotFound { pid });
    };
    let Ok(sig) = nix::sys::signal::Signal::try_from(signo) else {
        return Err(SignalError::InvalidSignal(signo));
    };
    match nix::sys::signal::kill(target, Some(sig)) {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::ESRCH) => Err(SignalError::NotFound { pid }),
        Err(errno) => Err(SignalError::Failed {
            pid,
            message: format!("failed to deliver signal {signo}: {errno}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Liveness and identity probes
// ---------------------------------------------------------------------------

/// True when a process with `pid` exists: `kill(pid, 0)`.
///
/// Permission errors (`EPERM`) count as alive — the process exists, we just
/// cannot signal it. Unreaped zombies count as alive (their pid is still
/// taken); pid 0 and out-of-range pids report false.
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    let Some(target) = pid_from_u32(pid).filter(|_| pid != 0) else {
        return false;
    };
    match nix::sys::signal::kill(target, None) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Process-group and session ids of one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIds {
    /// Process-group id (`getpgid`).
    pub pgid: i32,
    /// Session id (`getsid`).
    pub sid: i32,
}

/// Process-group and session ids of `pid` (`getpgid`/`getsid`).
///
/// A thin wrapper: `None` on any failure (no such process, bad pid); all
/// policy (minimum pgid, group-membership guards) stays with the caller.
#[must_use]
pub fn process_ids(pid: u32) -> Option<ProcessIds> {
    let target = pid_from_u32(pid)?;
    let pgid = nix::unistd::getpgid(Some(target)).ok()?;
    let sid = nix::unistd::getsid(Some(target)).ok()?;
    Some(ProcessIds {
        pgid: pgid.as_raw(),
        sid: sid.as_raw(),
    })
}

/// Session id of `pid` (`getsid`), for re-verifying identity before a signal.
///
/// `None` on any failure. Prefer this over parsing `ps` output: it cannot
/// disagree with the kernel the way a text snapshot can.
#[must_use]
pub fn session_id_of(pid: u32) -> Option<i32> {
    let target = pid_from_u32(pid)?;
    nix::unistd::getsid(Some(target))
        .ok()
        .map(nix::unistd::Pid::as_raw)
}

/// Our own pid.
#[must_use]
pub fn own_pid() -> u32 {
    std::process::id()
}

/// Our own process-group id (`getpgrp`, infallible).
#[must_use]
pub fn own_pgid() -> i32 {
    nix::unistd::getpgrp().as_raw()
}

/// Our own effective user id (`getuid`, infallible).
#[must_use]
pub fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

// ---------------------------------------------------------------------------
// Piped spawn
// ---------------------------------------------------------------------------

/// Spawn a piped child from `params` with `stdio` wiring.
///
/// - `detached == false`: plain `std` spawn. The child inherits our session
///   and process group.
/// - `detached == true`: the child starts a new session via `posix_spawn`
///   with `POSIX_SPAWN_SETSID` (`pgid == pid`, `sid == pid`), so it survives
///   the parent and terminal hangup. `posix_spawn` has no chdir action, so a
///   detached spawn *with* `cwd` runs through `/bin/sh -c 'cd ... &&
///   exec ...'` (same pid, exact post-exec argv; the cwd is pre-validated so a
///   missing directory fails the spawn instead of the shell).
///
/// Program lookup follows `execvp` rules in both paths: a name without a
/// slash is resolved via `PATH`, otherwise used as a path.
///
/// # Errors
///
/// Returns [`std::io::Error`] when argv is empty, argv/env carries a NUL byte,
/// the cwd is unusable, or the spawn syscalls fail (errno preserved).
pub fn spawn_piped(params: &SpawnParams, stdio: &PipedStdio) -> std::io::Result<PipedChild> {
    let _transport = lock_transport_spawn();
    if params.detached {
        spawn_detached(params, stdio)
    } else {
        spawn_std(params, stdio)
    }
}

/// A running or reaped piped child.
///
/// Dropping the handle does not kill or wait for the child (same as
/// `std::process::Child`): an unreaped child keeps running, and an exited one
/// stays a zombie until [`PipedChild::wait`] or [`PipedChild::try_wait`]
/// reaps it. Grace loops and detach policies stay with the caller.
pub struct PipedChild {
    pid: u32,
    inner: PipedInner,
    stdin: Option<Box<dyn std::io::Write + Send>>,
    stdout: Option<Box<dyn std::io::Read + Send>>,
    stderr: Option<Box<dyn std::io::Read + Send>>,
}

enum PipedInner {
    Std(std::process::Child),
    Detached {
        pid: nix::unistd::Pid,
        done: Option<ExitStatus>,
    },
}

impl std::fmt::Debug for PipedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipedChild")
            .field("pid", &self.pid)
            .field("stdin_open", &self.stdin.is_some())
            .field("stdout_open", &self.stdout.is_some())
            .field("stderr_open", &self.stderr.is_some())
            .finish_non_exhaustive()
    }
}

impl PipedChild {
    /// Process id of the child. Always `Some` for piped children (`None`
    /// exists for transport symmetry with PTY children).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    /// Poll for exit without blocking: `Ok(None)` means still running.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the underlying wait fails.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match &mut self.inner {
            PipedInner::Std(child) => Ok(child.try_wait()?.map(convert_std)),
            PipedInner::Detached { pid, done } => {
                if let Some(status) = done {
                    return Ok(Some(status.clone()));
                }
                let status = convert_wait(waitpid_retry(
                    *pid,
                    Some(nix::sys::wait::WaitPidFlag::WNOHANG),
                )?);
                if let Some(status) = &status {
                    *done = Some(status.clone());
                }
                Ok(status)
            }
        }
    }

    /// Block until the child exits and return its status.
    ///
    /// Repeatable: once reaped, the status is returned again without another
    /// syscall. Never call this on a PTY session's reader thread path — see
    /// the crate-level teardown notes in the consumer instead.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the underlying wait fails.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match &mut self.inner {
            PipedInner::Std(child) => child.wait().map(convert_std),
            PipedInner::Detached { pid, done } => {
                if let Some(status) = done {
                    return Ok(status.clone());
                }
                loop {
                    // A blocking wait without stop flags only resolves on
                    // exit or signal death; anything else is spurious.
                    if let Some(status) = convert_wait(waitpid_retry(*pid, None)?) {
                        *done = Some(status.clone());
                        return Ok(status);
                    }
                }
            }
        }
    }

    /// Kill the child (`SIGKILL`). Errors when the child already exited.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the kill fails.
    pub fn kill(&mut self) -> std::io::Result<()> {
        match &mut self.inner {
            PipedInner::Std(child) => child.kill(),
            PipedInner::Detached { pid, .. } => {
                nix::sys::signal::kill(*pid, nix::sys::signal::Signal::SIGKILL)?;
                Ok(())
            }
        }
    }

    /// Deliver `signo` to the child; see [`signal`].
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] exactly like [`signal`].
    pub fn signal(&self, signo: i32) -> Result<(), SignalError> {
        signal(self.pid, signo)
    }

    /// Take the stdin pipe write end, if [`Stdio::Pipe`] was requested.
    pub fn take_stdin(&mut self) -> Option<Box<dyn std::io::Write + Send>> {
        self.stdin.take()
    }

    /// Take the stdout pipe read end, if [`Stdio::Pipe`] was requested.
    pub fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stdout.take()
    }

    /// Take the stderr pipe read end, if [`Stdio::Pipe`] was requested.
    pub fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stderr.take()
    }
}

/// `waitpid`, retrying `EINTR` so a stray signal never fails a wait.
fn waitpid_retry(
    pid: nix::unistd::Pid,
    flags: Option<nix::sys::wait::WaitPidFlag>,
) -> nix::Result<nix::sys::wait::WaitStatus> {
    loop {
        match nix::sys::wait::waitpid(pid, flags) {
            Err(nix::errno::Errno::EINTR) => {}
            outcome => return outcome,
        }
    }
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn check_nul(what: &str, value: &OsStr) -> std::io::Result<()> {
    if value.as_bytes().contains(&0) {
        return Err(invalid_input(format!("spawn: NUL byte in {what}")));
    }
    Ok(())
}

/// Validated argv/env: non-empty argv, no NUL bytes anywhere (both `std` and
/// `exec` reject NUL; `std` would panic, so we check first and fail cleanly).
fn checked_argv(params: &SpawnParams) -> std::io::Result<(&OsStr, &[OsString])> {
    let (program, args) = params
        .argv()
        .split_first()
        .ok_or_else(|| invalid_input("spawn: empty argv"))?;
    check_nul("argv element", program)?;
    for arg in args {
        check_nul("argv element", arg)?;
    }
    for (key, value) in params.env_overrides() {
        check_nul("env key", key)?;
        check_nul("env value", value)?;
    }
    Ok((program, args))
}

fn map_stdio(stdio: &Stdio) -> std::io::Result<std::process::Stdio> {
    match stdio {
        Stdio::Null => Ok(std::process::Stdio::null()),
        Stdio::Inherit => Ok(std::process::Stdio::inherit()),
        Stdio::Pipe => Ok(std::process::Stdio::piped()),
        Stdio::File(file) => file.try_clone().map(std::process::Stdio::from),
    }
}

/// Plain `std` spawn for the non-detached path.
fn spawn_std(params: &SpawnParams, stdio: &PipedStdio) -> std::io::Result<PipedChild> {
    let (program, args) = checked_argv(params)?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    for (key, value) in params.env_overrides() {
        cmd.env(key, value);
    }
    if let Some(cwd) = params.cwd() {
        cmd.current_dir(cwd);
    }
    cmd.stdin(map_stdio(&stdio.stdin)?)
        .stdout(map_stdio(&stdio.stdout)?)
        .stderr(map_stdio(&stdio.stderr)?);
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let stdin = child
        .stdin
        .take()
        .map(|stdin| -> Box<dyn std::io::Write + Send> { Box::new(stdin) });
    let stdout = child
        .stdout
        .take()
        .map(|stdout| -> Box<dyn std::io::Read + Send> { Box::new(stdout) });
    let stderr = child
        .stderr
        .take()
        .map(|stderr| -> Box<dyn std::io::Read + Send> { Box::new(stderr) });
    Ok(PipedChild {
        pid,
        inner: PipedInner::Std(child),
        stdin,
        stdout,
        stderr,
    })
}

// `POSIX_SPAWN_SETSID` is not exposed by `nix`'s spawn flags, so the value is
// spelled out per target. macOS 0x0400 is from the local Apple SDK `spawn.h`;
// Linux 0x80 is glibc's value (libc crate `linux/mod.rs`). A wrong value
// fails loudly (`set_flags` returns `EINVAL`), never silently.
#[cfg(target_os = "macos")]
const POSIX_SPAWN_SETSID: i32 = 0x0400;
#[cfg(target_os = "linux")]
const POSIX_SPAWN_SETSID: i32 = 0x80;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("detached spawn: POSIX_SPAWN_SETSID value not verified for this target");

/// `/bin/sh` wrapper used only for detached spawn *with* cwd (`posix_spawn`
/// has no chdir action): `$0` is the directory, the rest is the real argv.
const DETACHED_CWD_SCRIPT: &str = "cd -- \"$0\" && shift && exec \"$@\"";

/// Full child environment: parent entries plus overrides (overrides win).
fn spawn_env(params: &SpawnParams) -> std::io::Result<Vec<CString>> {
    let mut merged: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in params.env_overrides() {
        merged.insert(key.clone(), value.clone());
    }
    merged
        .iter()
        .map(|(key, value)| {
            let mut bytes = key.as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            CString::new(bytes).map_err(|_| invalid_input("spawn: NUL byte in environment"))
        })
        .collect()
}

/// A pipe whose ends are close-on-exec (macOS has no atomic `pipe2`, hence
/// the `fcntl`; callers hold [`TRANSPORT_SPAWN_LOCK`] across the window).
fn cloexec_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::AsFd as _;
    let (read, write) = nix::unistd::pipe()?;
    for end in [read.as_fd(), write.as_fd()] {
        nix::fcntl::fcntl(
            end,
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
        )?;
    }
    Ok((read, write))
}

/// Bytes to `CString`, failing cleanly on interior NUL.
fn cstr(bytes: &[u8], what: &str) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| invalid_input(format!("spawn: NUL byte in {what}")))
}

/// Detached spawn: new session via `posix_spawn` + `POSIX_SPAWN_SETSID`.
fn spawn_detached(params: &SpawnParams, stdio: &PipedStdio) -> std::io::Result<PipedChild> {
    let (program, args) = checked_argv(params)?;
    if let Some(cwd) = params.cwd()
        && let Err(err) = std::fs::metadata(cwd)
    {
        return Err(std::io::Error::new(
            err.kind(),
            format!("detached spawn: bad cwd {}: {err}", cwd.display()),
        ));
    }
    // Spawn image and argv: direct, or via /bin/sh when a cwd override needs
    // a chdir that posix_spawn cannot express.
    let (path, argv): (CString, Vec<CString>) = match params.cwd() {
        None => {
            let mut argv = Vec::with_capacity(args.len() + 1);
            argv.push(cstr(program.as_bytes(), "argv element")?);
            for arg in args {
                argv.push(cstr(arg.as_bytes(), "argv element")?);
            }
            let path = argv[0].clone();
            (path, argv)
        }
        Some(cwd) => {
            let mut argv = Vec::with_capacity(args.len() + 5);
            argv.push(cstr(b"/bin/sh", "spawn wrapper")?);
            argv.push(cstr(b"-c", "spawn wrapper")?);
            argv.push(cstr(DETACHED_CWD_SCRIPT.as_bytes(), "spawn wrapper")?);
            argv.push(cstr(cwd.as_os_str().as_bytes(), "cwd")?);
            argv.push(cstr(program.as_bytes(), "argv element")?);
            for arg in args {
                argv.push(cstr(arg.as_bytes(), "argv element")?);
            }
            let path = cstr(b"/bin/sh", "spawn wrapper")?;
            (path, argv)
        }
    };
    let env = spawn_env(params)?;

    let actions = nix::spawn::PosixSpawnFileActions::init()?;
    let mut wired = WireStdio::new(actions);
    wired.wire(0, &stdio.stdin)?;
    wired.wire(1, &stdio.stdout)?;
    wired.wire(2, &stdio.stderr)?;

    let mut attr = nix::spawn::PosixSpawnAttr::init()?;
    attr.set_flags(nix::spawn::PosixSpawnFlags::from_bits_retain(
        POSIX_SPAWN_SETSID,
    ))?;

    let pid = nix::spawn::posix_spawnp(&path, wired.inner(), &attr, &argv, &env)?;
    let [stdin, stdout, stderr] = wired.take_parent_ends();
    Ok(PipedChild {
        pid: pid_to_u32(pid.as_raw()),
        inner: PipedInner::Detached { pid, done: None },
        stdin: stdin.map(|file| -> Box<dyn std::io::Write + Send> { Box::new(file) }),
        stdout: stdout.map(|file| -> Box<dyn std::io::Read + Send> { Box::new(file) }),
        stderr: stderr.map(|file| -> Box<dyn std::io::Read + Send> { Box::new(file) }),
    })
}

/// Helper that wires child stdio file actions while retaining every fd the
/// parent side must keep alive across the spawn.
struct WireStdio {
    actions: nix::spawn::PosixSpawnFileActions,
    /// Borrowed-or-owned fds dup2'd into the child, kept open until spawn.
    keep: Vec<std::os::fd::OwnedFd>,
    /// `/dev/null` handles for [`Stdio::Null`], kept open until spawn.
    nulls: Vec<std::fs::File>,
    /// Parent ends of [`Stdio::Pipe`]s, indexed by fd number.
    parents: [Option<std::fs::File>; 3],
}

impl WireStdio {
    fn new(actions: nix::spawn::PosixSpawnFileActions) -> Self {
        Self {
            actions,
            keep: Vec::new(),
            nulls: Vec::new(),
            parents: [None, None, None],
        }
    }

    fn inner(&self) -> &nix::spawn::PosixSpawnFileActions {
        &self.actions
    }

    fn take_parent_ends(self) -> [Option<std::fs::File>; 3] {
        self.parents
    }

    fn wire(&mut self, fd: u8, stdio: &Stdio) -> std::io::Result<()> {
        let target = std::os::fd::RawFd::from(fd);
        let slot = usize::from(fd);
        match stdio {
            Stdio::Null => {
                // Opened via `nix`, not `std::fs::File::open` (workspace
                // policy keeps blocking std opens out of library code).
                let oflag = if fd == 0 {
                    nix::fcntl::OFlag::O_RDONLY
                } else {
                    nix::fcntl::OFlag::O_WRONLY
                };
                let owned = nix::fcntl::open("/dev/null", oflag, nix::sys::stat::Mode::empty())?;
                let null = std::fs::File::from(owned);
                self.actions.add_dup2(null.as_raw_fd(), target)?;
                self.nulls.push(null);
            }
            Stdio::Inherit => {}
            Stdio::Pipe => {
                let (read, write) = cloexec_pipe()?;
                if fd == 0 {
                    self.actions.add_dup2(read.as_raw_fd(), target)?;
                    self.keep.push(read);
                    self.parents[slot] = Some(std::fs::File::from(write));
                } else {
                    self.actions.add_dup2(write.as_raw_fd(), target)?;
                    self.keep.push(write);
                    self.parents[slot] = Some(std::fs::File::from(read));
                }
            }
            Stdio::File(file) => {
                self.actions.add_dup2(file.as_raw_fd(), target)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_params_builder_carries_argv_env_cwd_and_detached() {
        let params = SpawnParams::new("echo")
            .arg("hi")
            .args(["a", "b"])
            .env("TP2_A", "1")
            .current_dir("/tmp")
            .detached(true);
        assert_eq!(
            params.argv(),
            &[
                OsString::from("echo"),
                OsString::from("hi"),
                OsString::from("a"),
                OsString::from("b"),
            ]
        );
        assert_eq!(
            params.env_overrides(),
            &[(OsString::from("TP2_A"), OsString::from("1"))]
        );
        assert_eq!(params.cwd(), Some(Path::new("/tmp")));
        assert!(params.is_detached());
    }

    #[test]
    fn spawn_params_default_is_empty_and_attached() {
        let params = SpawnParams::new("true");
        assert_eq!(params.argv(), &[OsString::from("true")]);
        assert!(params.env_overrides().is_empty());
        assert_eq!(params.cwd(), None);
        assert!(!params.is_detached());
    }

    #[test]
    fn exit_status_success_and_display() {
        let clean = ExitStatus {
            code: 0,
            signal: None,
        };
        assert!(clean.success());
        assert_eq!(clean.exit_code(), 0);
        assert_eq!(clean.signal(), None);
        assert_eq!(format!("{clean}"), "exited with code 0");

        let failed = ExitStatus {
            code: 1,
            signal: None,
        };
        assert!(!failed.success());
        assert_eq!(format!("{failed}"), "exited with code 1");

        let signaled = ExitStatus {
            code: 1,
            signal: Some(String::from("SIGTERM")),
        };
        assert!(!signaled.success());
        assert_eq!(signaled.signal(), Some("SIGTERM"));
        assert_eq!(format!("{signaled}"), "terminated by SIGTERM");
    }

    #[test]
    fn signal_names_use_sig_prefix_with_stable_fallback() {
        assert_eq!(signame(SIGTERM), "SIGTERM");
        assert_eq!(signame(SIGKILL), "SIGKILL");
        assert_eq!(signame(SIGHUP), "SIGHUP");
        assert_eq!(signame(SIGINT), "SIGINT");
        assert_eq!(signame(SIGQUIT), "SIGQUIT");
        assert_eq!(signame(999_999), "Signal 999999");
    }

    #[test]
    fn signal_constants_match_unix_numbers() {
        assert_eq!(
            (SIGHUP, SIGINT, SIGQUIT, SIGKILL, SIGTERM),
            (1, 2, 3, 9, 15)
        );
    }

    #[test]
    fn stdio_default_is_null() {
        let stdio = PipedStdio::new();
        assert!(matches!(stdio.stdin, Stdio::Null));
        assert!(matches!(stdio.stdout, Stdio::Null));
        assert!(matches!(stdio.stderr, Stdio::Null));
    }

    #[test]
    fn pid_conversions_reject_out_of_range() {
        assert!(pid_from_u32(1).is_some());
        assert!(pid_from_u32(u32::MAX).is_none());
        assert_eq!(pid_to_u32(42), 42);
        assert_eq!(pid_to_u32(-1), 0);
    }

    #[test]
    fn child_handles_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<PipedChild>();
        assert_send::<ExitStatus>();
        assert_send::<SignalError>();
    }
}
