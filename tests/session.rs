#![cfg(all(unix, feature = "pty"))]
// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Live-session conformance tests for the `pty` feature.
//!
//! Hermetic: only POSIX utilities (`true`, `false`, `sleep`, `stty`, `sh`,
//! `cat`, `printf`) that exist on every Unix CI image. No network, no
//! sleeps-as-synchronization (all waits poll with deadlines).

use std::time::{Duration, Instant};

use termpane::DamageGrid;
use termpane::process::{SpawnParams, pid_alive};
use termpane::session::{ProcessError, PtySession, SessionOptions, Signal};

fn deadline(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

/// Poll cadence between snapshot reads. Test threads are owned OS threads,
/// which the disallowed-methods policy explicitly permits to block.
#[expect(
    clippy::disallowed_methods,
    reason = "test poll loop on an owned OS thread — the policy's named carve-out"
)]
fn poll_sleep() {
    std::thread::sleep(Duration::from_millis(25));
}

fn spawn_session(argv: &[&str], record_output: bool) -> Result<PtySession, ProcessError> {
    let mut params = SpawnParams::new(argv[0]);
    for arg in argv.iter().skip(1) {
        params = params.arg(arg);
    }
    let options = SessionOptions {
        record_output,
        ..SessionOptions::default()
    };
    PtySession::spawn(&params, options)
}

#[test]
fn options_default_term_from_profile() {
    let opts = SessionOptions::default();
    assert_eq!(opts.term, "xterm-256color");
    assert_eq!(opts.colorterm, "truecolor");
    assert_eq!((opts.cols, opts.rows), (80, 24));
}

#[test]
fn spawn_rejects_empty_argv() {
    let params = SpawnParams::default();
    let err = PtySession::spawn(&params, SessionOptions::default()).unwrap_err();
    assert!(matches!(err, ProcessError::Spawn(_)), "got {err}");
}

#[test]
fn spawn_rejects_bad_geometry() {
    for (cols, rows) in [(0, 24), (80, 0), (1001, 24), (80, 1001)] {
        let params = SpawnParams::new("true");
        let options = SessionOptions {
            cols,
            rows,
            ..SessionOptions::default()
        };
        let err = PtySession::spawn(&params, options).unwrap_err();
        assert!(matches!(err, ProcessError::Spawn(_)), "got {err}");
    }
}

#[test]
fn true_exits_successfully() {
    let session = spawn_session(&["true"], false).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success());
    assert_eq!(status.exit_code(), 0);
    assert_eq!(status.signal(), None);
    assert!(session.poll_exit().is_some());
    session.finish(deadline(10)).unwrap();
}

#[test]
fn false_exits_with_code_1() {
    let session = spawn_session(&["false"], false).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(!status.success());
    assert_eq!(status.exit_code(), 1);
    session.finish(deadline(10)).unwrap();
}

#[test]
fn signal_kill_terminates_sleep() {
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    assert!(session.poll_exit().is_none());
    session.signal(Signal::Kill).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(!status.success());
    assert!(status.signal().is_some(), "expected a signal, got {status}");
    session.finish(deadline(10)).unwrap();
}

#[test]
fn signal_dead_child_errors() {
    let session = spawn_session(&["true"], false).unwrap();
    session.wait_exit(deadline(10)).unwrap();
    let err = session.signal(Signal::Term).unwrap_err();
    assert!(matches!(err, ProcessError::ChildExited(_)), "got {err}");
    session.finish(deadline(10)).unwrap();
}

#[test]
fn stty_size_reports_spawn_geometry() {
    let mut params = SpawnParams::new("sh");
    params = params.args(["-c", "stty size"]);
    let options = SessionOptions {
        cols: 100,
        rows: 40,
        ..SessionOptions::default()
    };
    let session = PtySession::spawn(&params, options).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success(), "got {status}");
    let text = session.snapshot().unwrap().to_text();
    assert!(
        text.contains("40 100"),
        "stty size should report rows cols; grid text:\n{text}"
    );
    session.finish(deadline(10)).unwrap();
}

#[test]
fn resize_updates_pty_and_grid() {
    let session = spawn_session(&["sh"], false).unwrap();
    session.resize(100, 30).unwrap();
    assert_eq!(session.size().unwrap(), (100, 30));
    // The PTY ioctl took effect when a fresh `stty size` reads the new rows.
    session.write_stdin(b"stty size\n").unwrap();
    let stop = deadline(10);
    loop {
        let text = session.snapshot().unwrap().to_text();
        if text.contains("30 100") {
            break;
        }
        assert!(
            Instant::now() < stop,
            "resized stty size never appeared; grid text:\n{text}"
        );
        poll_sleep();
    }
    session.finish(deadline(10)).unwrap();
}

#[test]
fn resize_rejects_bad_geometry() {
    let mut session = spawn_session(&["sleep", "60"], false).unwrap();
    let err = session.resize(0, 24).unwrap_err();
    assert!(matches!(err, ProcessError::InvalidInput(_)), "got {err}");
    assert_eq!(session.size().unwrap(), (80, 24));
    session.close().unwrap();
}

#[test]
fn close_input_eofs_cat() {
    let session = spawn_session(&["cat"], false).unwrap();
    session.write_stdin(b"hello\n").unwrap();
    let stop = deadline(10);
    loop {
        let text = session.snapshot().unwrap().to_text();
        if text.contains("hello") {
            break;
        }
        assert!(
            Instant::now() < stop,
            "cat echo never appeared; grid text:\n{text}"
        );
        poll_sleep();
    }
    session.close_input().unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success(), "got {status}");
    // Second close reports stdin already closed.
    let err = session.close_input().unwrap_err();
    assert!(
        matches!(err, ProcessError::ChildExited(_) | ProcessError::Closed(_)),
        "got {err}"
    );
    session.finish(deadline(10)).unwrap();
}

#[test]
fn dsr_reply_is_routed_to_pty_stdin() {
    // `cat` echoes whatever the worker writes to PTY stdin. A DSR 6n
    // (cursor position request) must produce a CPR `ESC[{row};{col}R`
    // answer routed back through the child. The trailing newline pushes
    // both request and reply through canonical mode to `cat`.
    let session = spawn_session(&["cat"], true).unwrap();
    session.write_stdin(b"\x1b[6n\n").unwrap();
    let stop = deadline(10);
    loop {
        let log = session.output_log().unwrap();
        // Row-agnostic: echo trim (ECHOCTL) and echo timing vary by
        // platform, but the CPR shape `ESC digits ; digits R` is fixed.
        // (The request itself ends in `n`, so it cannot match.)
        if contains_cpr(&log) {
            break;
        }
        assert!(
            Instant::now() < stop,
            "CPR reply never routed; log: {log:?}"
        );
        // Nudge canonical mode: flush reply bytes queued behind the DSR
        // line through `cat` so its raw echo lands in the log.
        session.write_stdin(b"\n").unwrap();
        poll_sleep();
    }
    session.finish(deadline(10)).unwrap();
}

/// True when `log` holds a cursor-position report `ESC [ {n} ; {n} R`.
fn contains_cpr(log: &[u8]) -> bool {
    let mut i = 0;
    while i + 5 <= log.len() {
        if log[i] == b'\x1b' && log[i + 1] == b'[' {
            let mut j = i + 2;
            while j < log.len() && log[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 && j < log.len() && log[j] == b';' {
                let mut k = j + 1;
                while k < log.len() && log[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 && k < log.len() && log[k] == b'R' {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

#[test]
fn live_state_equals_byte_replay_state() {
    // Styled output + cursor motion + mode flips. The live grid processed
    // exactly these cooked PTY bytes; replaying the recorded log into a
    // fresh grid must reproduce the live state exactly.
    // Octal escapes: portable across dash/bash `printf` alike.
    let script = "printf '\\033[1;31mred-bold\\033[0m\\n\\033[2;10Hhere\\033[?25lhidden\\033[?25h\\033]0;title\\a\\n'";
    let session = spawn_session(&["sh", "-c", script], true).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success(), "got {status}");

    let log = session.output_log().unwrap();
    assert!(!log.is_empty());
    let live_state = session.state_formatted().unwrap();

    let mut replayed = DamageGrid::new(24, 80, 1000);
    replayed.process(&log);

    // The live state, re-serialized and replayed, must equal the byte
    // replay: both encode the same grid the worker pumped.
    let mut from_live = DamageGrid::new(24, 80, 1000);
    from_live.process(&live_state);
    assert!(
        replayed.state_eq(&from_live),
        "byte-replay state != live state.\nlog: {log:?}"
    );

    // Title events surface through drain_events, not the grid.
    let events = session.drain_events().unwrap();
    assert!(
        events.iter().any(|e| format!("{e:?}").contains("title")),
        "expected a title event, got {events:?}"
    );
    session.finish(deadline(10)).unwrap();
}

#[test]
fn finish_kills_lingering_child_on_timeout() {
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    let pid = session.process_id();
    let err = session.finish(deadline(1)).unwrap_err();
    assert!(matches!(err, ProcessError::Timeout(_)), "got {err}");
    if let Some(pid) = pid {
        assert!(!pid_alive(pid), "child {pid} survived finish timeout");
    }
}

#[test]
fn close_is_idempotent() {
    let mut session = spawn_session(&["sleep", "60"], false).unwrap();
    session.close().unwrap();
    assert!(session.is_closed());
    session.close().unwrap();
    // Ops after close fail closed.
    let err = session.resize(80, 24).unwrap_err();
    assert!(matches!(err, ProcessError::Closed(_)), "got {err}");
}

#[test]
fn wait_exit_times_out_on_live_child() {
    let mut session = spawn_session(&["sleep", "60"], false).unwrap();
    let err = session.wait_exit(deadline(0)).unwrap_err();
    // deadline(0) is already past: immediate timeout, child untouched.
    assert!(matches!(err, ProcessError::Timeout(_)), "got {err}");
    assert!(session.poll_exit().is_none());
    session.close().unwrap();
}
