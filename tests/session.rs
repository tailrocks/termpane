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
use termpane::session::{
    Capabilities, ProcessError, PtySession, SessionOptions, Signal, StreamState,
};
use termpane::width::VirtualTerminalProfile;

fn deadline(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

/// Poll cadence between snapshot reads.
fn poll_sleep() {
    test_sleep_ms(25);
}

/// Bounded sleep for negative proofs (no-exit-until windows) and staging
/// pauses. Test threads are owned OS threads, which the disallowed-methods
/// policy explicitly permits to block.
#[expect(
    clippy::disallowed_methods,
    reason = "test staging sleep on an owned OS thread — the policy's named carve-out"
)]
fn test_sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
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

// ---------------------------------------------------------------------------
// F04: bounded I/O + responsive cancellation
// ---------------------------------------------------------------------------

#[test]
fn control_stays_responsive_during_output_flood() {
    // `yes` floods stdout without bound; the session must apply backpressure
    // (bounded queue) while control ops bypass the flood, and teardown must
    // complete without touching the queued data.
    let mut session = spawn_session(&["yes"], false).unwrap();
    let pid = session.process_id();
    // Wait until the pump has demonstrably ingested flood bytes.
    let stop = deadline(10);
    loop {
        let obs = session.observe().unwrap();
        assert!(
            obs.diagnostics.queued_batches <= 8,
            "data queue escaped its bound: {}",
            obs.diagnostics.queued_batches
        );
        if obs.diagnostics.bytes_pumped > 64 * 1024 {
            break;
        }
        assert!(Instant::now() < stop, "flood never reached the pump");
        poll_sleep();
    }
    // Control bypasses the flood: resize + observation answer promptly.
    let ctrl_start = Instant::now();
    session.resize(100, 30).unwrap();
    let obs = session.observe().unwrap();
    assert_eq!((obs.grid.cols, obs.grid.rows), (100, 30));
    assert!(
        ctrl_start.elapsed() < Duration::from_secs(5),
        "control starved by flood"
    );
    // Shutdown completes while the child still floods: kill-first, real joins.
    let close_start = Instant::now();
    session.close().unwrap();
    assert!(
        close_start.elapsed() < Duration::from_secs(10),
        "close stalled behind flood data"
    );
    if let Some(pid) = pid {
        assert!(!pid_alive(pid), "flooding child {pid} survived close");
    }
}

#[test]
fn kill_aborts_slow_write_to_non_reading_child() {
    // `sleep` never reads stdin while a 256 MiB write is in flight (seconds
    // of `write` on macOS, which never blocks master writes; a hard block on
    // Linux once the line discipline fills). A kill — the mechanism teardown
    // relies on — must abort the in-flight write promptly on both.
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    let big = vec![b'A'; 256 * 1024 * 1024];
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| session.write_stdin(&big));
        test_sleep_ms(500);
        let kill_at = Instant::now();
        session.signal(Signal::Kill).unwrap();
        let result = writer.join().expect("writer thread joined");
        assert!(
            kill_at.elapsed() < Duration::from_secs(5),
            "kill must abort the in-flight write promptly"
        );
        assert!(
            matches!(
                result,
                Err(ProcessError::Io(_) | ProcessError::ChildExited(_) | ProcessError::Closed(_))
            ),
            "in-flight write must fail after kill, got {result:?}"
        );
    });
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.signal().is_some(), "got {status}");
    session.finish(deadline(10)).unwrap();
}

#[test]
fn event_cap_bounds_and_reports_drops() {
    let params = SpawnParams::new("sh").args([
        "-c",
        "i=0; while [ $i -lt 200 ]; do printf '\\a'; i=$((i + 1)); done",
    ]);
    let options = SessionOptions {
        event_cap: 16,
        ..SessionOptions::default()
    };
    let session = PtySession::spawn(&params, options).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success(), "got {status}");
    let events = session.drain_events().unwrap();
    assert!(
        events.len() <= 16,
        "event stash escaped its cap: {}",
        events.len()
    );
    assert!(
        events
            .iter()
            .all(|e| matches!(e, termpane::PassthroughEvent::Bell)),
        "expected only BELs, got {events:?}"
    );
    let obs = session.observe().unwrap();
    assert!(
        obs.diagnostics.events_dropped > 0,
        "200 BELs into a 16-cap stash must drop honestly"
    );
    assert_eq!(
        u64::try_from(events.len()).unwrap() + obs.diagnostics.events_dropped,
        200,
        "kept + dropped must account for every event"
    );
    session.finish(deadline(10)).unwrap();
}

#[test]
fn output_cap_freezes_log_and_reports_truncation() {
    let params = SpawnParams::new("sh").args(["-c", "head -c 65536 /dev/zero | tr '\\0' 'x'"]);
    let options = SessionOptions {
        record_output: true,
        output_cap_bytes: 1024,
        ..SessionOptions::default()
    };
    let session = PtySession::spawn(&params, options).unwrap();
    let status = session.wait_exit(deadline(10)).unwrap();
    assert!(status.success(), "got {status}");
    let log = session.output_log().unwrap();
    assert_eq!(log.len(), 1024, "log must freeze exactly at the cap");
    assert!(
        log.iter().all(|b| *b == b'x'),
        "log must hold the oldest prefix"
    );
    let obs = session.observe().unwrap();
    assert!(obs.diagnostics.output_truncated);
    assert!(obs.diagnostics.output_bytes_total >= 65_536);
    assert_eq!(obs.diagnostics.output_bytes_kept, 1024);
    session.finish(deadline(10)).unwrap();
}

#[test]
fn reply_after_close_input_is_dropped_and_counted() {
    // Two DSR queries with a sleep between: close stdin after the first reply
    // routes, and the second reply must drop (counted) instead of failing.
    let params = SpawnParams::new("sh").args([
        "-c",
        "printf '\\033[6n'; sleep 3; printf '\\033[6n'; exec sleep 60",
    ]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    let stop = deadline(10);
    loop {
        if session.observe().unwrap().diagnostics.replies_routed >= 1 {
            break;
        }
        assert!(Instant::now() < stop, "first reply never routed");
        poll_sleep();
    }
    session.close_input().unwrap();
    let stop = deadline(15);
    loop {
        let obs = session.observe().unwrap();
        if obs.diagnostics.replies_dropped_no_writer >= 1 {
            assert_eq!(obs.diagnostics.reply_write_errors, 0);
            break;
        }
        assert!(Instant::now() < stop, "second reply never dropped");
        poll_sleep();
    }
    // The session stays fully functional after the dropped reply.
    session.observe().unwrap();
    let mut session = session;
    session.close().unwrap();
}

// ---------------------------------------------------------------------------
// F05: truthful capture/lifecycle
// ---------------------------------------------------------------------------

#[test]
fn clean_exit_reports_clean_eof_outcome() {
    let session = spawn_session(&["true"], false).unwrap();
    let outcome = session.wait_outcome(deadline(10)).unwrap();
    assert!(outcome.exit.success());
    assert_eq!(outcome.stream, StreamState::CleanEof);
    // Exit and outcome agree; the outcome carries the same status.
    assert_eq!(session.poll_exit().as_ref(), Some(&outcome.exit));
    session.finish(deadline(10)).unwrap();
}

#[test]
fn final_output_immutable_after_drain() {
    let script = "printf 'stable-bytes-0123456789\\n'";
    let session = spawn_session(&["sh", "-c", script], true).unwrap();
    session.wait_exit(deadline(10)).unwrap();
    let log_before = session.output_log().unwrap();
    assert!(!log_before.is_empty());
    let text_before = session.snapshot().unwrap().to_text();
    let rev_before = session.observe().unwrap().revision;
    // Past the drain grace: nothing may change the declared-drained output.
    test_sleep_ms(700);
    assert_eq!(session.output_log().unwrap(), log_before);
    assert_eq!(session.snapshot().unwrap().to_text(), text_before);
    assert_eq!(session.observe().unwrap().revision, rev_before);
    session.finish(deadline(10)).unwrap();
}

#[test]
fn close_input_raw_mode_is_not_universal_eof() {
    // `cat` in raw mode reads the injected VEOF bytes as data: close_input
    // reports Ok (request accepted) but the child must NOT exit. The sleeps
    // are a negative proof (no-exit-until), not synchronization.
    let params = SpawnParams::new("sh").args(["-c", "stty raw -echo; exec cat"]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    test_sleep_ms(500);
    session.close_input().unwrap();
    test_sleep_ms(500);
    assert!(
        session.poll_exit().is_none(),
        "raw-mode child must survive close_input: no universal half-close"
    );
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn grandchild_holding_slave_still_terminates_truthfully() {
    // Direct child exits at ~0.1s while a SIGHUP-immune grandchild holds the
    // slave for 3s. The terminal stream state is platform-defined here (Linux
    // withholds EOF until the last slave fd closes, so the grace expires;
    // macOS delivers EOF at the session-leader hangup), but both are truthful
    // terminal states — never Streaming, never Aborted — and the reaped pid
    // refuses signals on every platform. (The deterministic DrainExpired proof
    // lives in the worker unit tests, which hold the data channel themselves.)
    let params = SpawnParams::new("sh").args(["-c", "trap '' HUP; sleep 3 & exec sleep 0.1"]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    test_sleep_ms(400);
    let err = session.signal(Signal::Term).unwrap_err();
    assert!(
        matches!(err, ProcessError::ChildExited(_)),
        "reaped pid must refuse signals, got {err}"
    );
    let outcome = session.wait_outcome(deadline(10)).unwrap();
    assert!(outcome.exit.success(), "got {}", outcome.exit);
    assert!(
        matches!(
            outcome.stream,
            StreamState::CleanEof | StreamState::DrainExpired
        ),
        "terminal truth required, got {:?}",
        outcome.stream
    );
    session.finish(deadline(15)).unwrap();
}

// ---------------------------------------------------------------------------
// F06: atomic observation
// ---------------------------------------------------------------------------

#[test]
fn observe_carries_complete_single_revision_state() {
    let session = spawn_session(&["sh"], false).unwrap();
    session.write_stdin(b"echo hi\n").unwrap();
    let stop = deadline(10);
    let obs = loop {
        let obs = session.observe().unwrap();
        if obs.grid.to_text().contains("hi") {
            break obs;
        }
        assert!(Instant::now() < stop, "echo never appeared");
        poll_sleep();
    };
    // Grid + cursor agree internally.
    assert!(obs.cursor.position.0 < obs.grid.rows);
    assert!(obs.cursor.position.1 <= obs.grid.cols);
    assert!(obs.cursor.visible);
    // Power-on color/mode facts from the model defaults.
    let profile = VirtualTerminalProfile::default();
    assert_eq!(obs.colors.reported_fg, profile.default_reported_fg);
    assert_eq!(obs.colors.reported_bg, profile.default_reported_bg);
    assert!(!obs.modes.alternate_screen);
    assert!(obs.modes.autowrap);
    assert!(!obs.modes.bracketed_paste);
    // Capabilities are the deterministic model-only defaults, always.
    assert_eq!(obs.capabilities, Capabilities::model_defaults());
    assert_eq!(
        obs.capabilities,
        Capabilities::from(VirtualTerminalProfile::default())
    );
    // Still running: no exit, streaming, unfinalized.
    assert_eq!(obs.completeness.exit, None);
    assert_eq!(obs.completeness.stream, StreamState::Streaming);
    assert!(!obs.completeness.finalized);
    assert!(obs.diagnostics.bytes_pumped > 0);
    assert!(obs.diagnostics.stdin_open);
    session.finish(deadline(10)).unwrap();
}

#[test]
fn mode_only_change_bumps_revision_without_touching_text() {
    let params = SpawnParams::new("sh").args([
        "-c",
        "printf '\\033[?2004h'; sleep 2; printf '\\033[?2004l'; exec sleep 60",
    ]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    let stop = deadline(10);
    let first = loop {
        let obs = session.observe().unwrap();
        if obs.modes.bracketed_paste {
            break obs;
        }
        assert!(Instant::now() < stop, "mode never engaged");
        poll_sleep();
    };
    let stop = deadline(15);
    let second = loop {
        let obs = session.observe().unwrap();
        if !obs.modes.bracketed_paste && obs.revision > first.revision {
            break obs;
        }
        assert!(Instant::now() < stop, "mode never released");
        poll_sleep();
    };
    assert_eq!(first.grid.to_text(), second.grid.to_text());
    assert_eq!(first.cursor.position, second.cursor.position);
    assert!(second.revision > first.revision);
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn cursor_only_change_bumps_revision_without_touching_text() {
    let params = SpawnParams::new("sh").args([
        "-c",
        "printf '\\033[?25l'; sleep 2; printf '\\033[?25h'; exec sleep 60",
    ]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    let stop = deadline(10);
    let first = loop {
        let obs = session.observe().unwrap();
        if !obs.cursor.visible {
            break obs;
        }
        assert!(Instant::now() < stop, "cursor never hid");
        poll_sleep();
    };
    let stop = deadline(15);
    let second = loop {
        let obs = session.observe().unwrap();
        if obs.cursor.visible && obs.revision > first.revision {
            break obs;
        }
        assert!(Instant::now() < stop, "cursor never returned");
        poll_sleep();
    };
    assert_eq!(first.grid.to_text(), second.grid.to_text());
    assert!(second.revision > first.revision);
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn palette_only_change_bumps_revision_without_touching_text_or_modes() {
    // A silent child: the only state changes are the capsule-driven color
    // updates, so text and modes must be bit-identical across them.
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    let before = session.observe().unwrap();
    session
        .set_reported_colors(Some((1, 2, 3)), Some((4, 5, 6)))
        .unwrap();
    let after = session.observe().unwrap();
    assert_eq!(after.colors.reported_fg, (1, 2, 3));
    assert_eq!(after.colors.reported_bg, (4, 5, 6));
    assert_eq!(before.grid.to_text(), after.grid.to_text());
    assert_eq!(before.modes, after.modes);
    assert_eq!(before.cursor, after.cursor);
    assert!(after.revision > before.revision);
    // None keeps the current value.
    session.set_reported_colors(None, None).unwrap();
    let kept = session.observe().unwrap();
    assert_eq!(kept.colors.reported_fg, (1, 2, 3));
    assert_eq!(kept.colors.reported_bg, (4, 5, 6));
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn resize_repaint_race_resolves_via_revision() {
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    let mut revision = session.observe().unwrap().revision;
    for (cols, rows) in [(90, 26), (100, 30), (80, 24)] {
        session.resize(cols, rows).unwrap();
        revision = session.wait_revision(revision + 1, deadline(10)).unwrap();
        let obs = session.observe().unwrap();
        assert_eq!((obs.grid.cols, obs.grid.rows), (cols, rows));
        assert!(obs.revision >= revision);
    }
    let obs = session.observe().unwrap();
    assert_eq!(obs.diagnostics.resizes, 3);
    // Racing observers always see internally consistent revisions.
    std::thread::scope(|scope| {
        let resizer = scope.spawn(|| {
            for _ in 0..10 {
                session.resize(90, 26).unwrap();
                session.resize(80, 24).unwrap();
            }
        });
        for _ in 0..50 {
            let obs = session.observe().unwrap();
            let dims = (obs.grid.cols, obs.grid.rows);
            assert!(
                dims == (80, 24) || dims == (90, 26),
                "torn observation: {dims:?} at rev {}",
                obs.revision
            );
            assert_eq!(obs.grid.cells.len(), usize::from(obs.grid.rows));
        }
        resizer.join().unwrap();
    });
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn wait_frame_observes_complete_sync_frame() {
    // Two frames with sleeps around: the first may complete before the wait
    // starts (baseline absorbs it), the second provably completes after.
    let params = SpawnParams::new("sh").args([
        "-c",
        "printf 'A'; sleep 2; printf '\\033[?2026hB\\033[?2026l'; sleep 2; printf '\\033[?2026hC\\033[?2026l'; exec sleep 60",
    ]);
    let session = PtySession::spawn(&params, SessionOptions::default()).unwrap();
    let stop = deadline(10);
    loop {
        if session.observe().unwrap().grid.to_text().contains('A') {
            break;
        }
        assert!(Instant::now() < stop, "pump never started");
        poll_sleep();
    }
    let frames = session.wait_frame(deadline(10)).unwrap();
    assert!(frames >= 1);
    let obs = session.observe().unwrap();
    assert!(obs.diagnostics.sync_frames_completed >= 1);
    assert!(!obs.modes.in_synchronized_update);
    let mut session = session;
    session.close().unwrap();
}

#[test]
fn wait_frame_times_out_when_mode_never_engages() {
    // Mode off the whole time: must time out, never report a phantom frame.
    let session = spawn_session(&["sleep", "60"], false).unwrap();
    let err = session
        .wait_frame(Instant::now() + Duration::from_secs(1))
        .unwrap_err();
    assert!(matches!(err, ProcessError::Timeout(_)), "got {err}");
    assert_eq!(
        session.observe().unwrap().diagnostics.sync_frames_completed,
        0
    );
    let mut session = session;
    session.close().unwrap();
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
