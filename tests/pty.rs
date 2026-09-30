#![cfg(all(unix, feature = "pty"))]
// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! PTY transport tests: spawn, EOF discipline, resize, teardown.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use termpane::process::{SIGTERM, SignalError, SpawnParams, pid_alive};
use termpane::pty::{PtyError, PtyReader, openpty, spawn_pty};

/// Drain a PTY reader to end-of-output: `Ok(0)` (macOS) or `EIO` (Linux,
/// after child exit) both mean EOF; anything else is a real failure.
fn drain_to_eof(mut reader: PtyReader) -> std::io::Result<Vec<u8>> {
    let eio = nix::errno::Errno::EIO as i32;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(err) if err.raw_os_error() == Some(eio) => break,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

/// Test-only poll pacing on an owned test thread.
#[expect(
    clippy::disallowed_methods,
    reason = "test-only poll pacing on an owned test thread, never a render/runtime thread"
)]
fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn pty_echo_contains_output_and_clean_exit() {
    let params = SpawnParams::new("echo").arg("hi");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty echo");
    assert!(child.pid().expect("pty pid") > 0);
    let reader = master.try_clone_reader().expect("reader");
    let out = drain_to_eof(reader).expect("drain");
    assert!(contains(&out, "hi"), "expected hi in {out:?}");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 0);
    assert_eq!(status.signal(), None);
    assert!(status.success());
}

#[test]
fn pty_argv_is_verbatim() {
    let params = SpawnParams::new("echo").args(["a b", "c*d"]);
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty echo");
    let out = drain_to_eof(master.try_clone_reader().expect("reader")).expect("drain");
    assert!(
        contains(&out, "a b c*d"),
        "expected verbatim argv in {out:?}"
    );
    assert!(child.wait().expect("wait").success());
}

#[test]
fn pty_env_override_visible_to_child() {
    let params = SpawnParams::new("sh")
        .args(["-c", "printf '%s' \"$TP2_PTY_MARK\""])
        .env("TP2_PTY_MARK", "7");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sh");
    let out = drain_to_eof(master.try_clone_reader().expect("reader")).expect("drain");
    assert_eq!(out, b"7".to_vec());
    assert!(child.wait().expect("wait").success());
    assert_eq!(std::env::var_os("TP2_PTY_MARK"), None);
}

#[test]
fn pty_cwd_moves_child() {
    let params = SpawnParams::new("sh")
        .args(["-c", "pwd"])
        .current_dir("/tmp");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sh");
    let out = drain_to_eof(master.try_clone_reader().expect("reader")).expect("drain");
    let expected = std::fs::canonicalize("/tmp").expect("canonicalize /tmp");
    let expected = expected.to_string_lossy().into_owned();
    assert!(contains(&out, &expected), "expected {expected} in {out:?}");
    assert!(child.wait().expect("wait").success());
}

#[test]
fn eof_by_drop_finishes_cat() {
    let params = SpawnParams::new("cat");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty cat");
    let reader = master.try_clone_reader().expect("reader");
    let writer = master.take_writer().expect("writer");
    drop(writer);
    // The backend implements drop as newline + VEOF, so the drain may carry
    // line-discipline echo (`^D` artifacts); the contract is EOF delivery.
    let _echo = drain_to_eof(reader).expect("drain");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 0);
    assert!(status.success());
}

#[test]
fn writer_to_reader_roundtrip_then_graceful_finish() {
    let params = SpawnParams::new("cat");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty cat");
    let mut reader = master.try_clone_reader().expect("reader");
    let mut writer = master.take_writer().expect("writer");
    // A second take is invalid.
    master.take_writer().expect_err("second take must fail");
    writer.write_all(b"xyz\n").expect("write");
    writer.flush().expect("flush");
    // PTY echo plus cat's copy: read until the line has round-tripped.
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    let start = Instant::now();
    while !contains(&seen, "xyz") {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "roundtrip timed out in {seen:?}"
        );
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(err) => panic!("pty read failed: {err}"),
        }
    }
    drop(writer);
    let status = child.wait().expect("wait");
    assert!(status.success());
}

#[test]
fn resize_readback_roundtrips() {
    let params = SpawnParams::new("sleep").arg("30");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sleep");
    assert_eq!(master.size().expect("size"), (80, 24));
    master.resize(100, 30).expect("resize");
    assert_eq!(master.size().expect("size"), (100, 30));
    child.kill().expect("kill");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert!(status.signal().is_some());
}

#[test]
fn sigwinch_reaches_child() {
    let path = std::env::temp_dir().join(format!("tp2-winch-{}.txt", std::process::id()));
    // The shell idles in the `read` builtin (not waiting on a child: shells
    // defer trapped signals until a foreground child completes) and announces
    // trap installation, so the test resizes only once the trap is live.
    let script = format!(
        "trap \"echo WINCH >> '{}'\" WINCH; echo READY; read dummy",
        path.to_string_lossy()
    );
    let params = SpawnParams::new("sh").args(["-c", &script]);
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sh");
    let mut reader = master.try_clone_reader().expect("reader");
    let mut announced = Vec::new();
    let mut buf = [0u8; 256];
    while !contains(&announced, "READY") {
        match reader.read(&mut buf).expect("announcement read") {
            0 => break,
            n => announced.extend_from_slice(&buf[..n]),
        }
    }
    assert!(contains(&announced, "READY"), "no READY in {announced:?}");
    // Back-to-back resizes may coalesce into one pending SIGWINCH (standard
    // signals do not queue), so one trap fire already proves delivery.
    master.resize(90, 26).expect("resize 1");
    master.resize(100, 30).expect("resize 2");
    let start = Instant::now();
    loop {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        if body.lines().next().is_some() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "SIGWINCH trap did not fire: {body:?}"
        );
        sleep_ms(50);
    }
    child.kill().expect("kill");
    drop(child.wait());
    std::fs::remove_file(&path).expect("cleanup trap file");
}

#[test]
fn try_wait_poll_then_kill_reap() {
    let params = SpawnParams::new("sleep").arg("30");
    let (master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sleep");
    let _reader = master.try_clone_reader().expect("reader");
    assert_eq!(child.try_wait().expect("poll").map(|s| s.exit_code()), None);
    let pid = child.pid().expect("pty pid");
    assert!(pid_alive(pid));
    child.kill().expect("kill");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert!(status.signal().is_some());
}

#[test]
fn signal_term_via_child_kills_pty_child() {
    let params = SpawnParams::new("sleep").arg("30");
    let (_master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty sleep");
    child.signal(SIGTERM).expect("SIGTERM");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert!(status.signal().is_some());
}

#[test]
fn signal_after_reap_is_refused_without_syscall() {
    // F03 at the PTY layer: once reaped (via either `wait` or `try_wait`),
    // the pid is dead to the handle — `signal` reports NotFound without a
    // syscall, so pid reuse can never redirect it, and `kill` is a no-op.
    for reaper in ["wait", "try_wait"] {
        let params = SpawnParams::new("true");
        let (_master, mut child) = spawn_pty(&params, 80, 24).expect("spawn pty true");
        let pid = child.pid().expect("pty pid");
        assert!(!child.is_reaped());
        if reaper == "wait" {
            assert!(child.wait().expect("wait").success());
        } else {
            let start = Instant::now();
            loop {
                if let Some(status) = child.try_wait().expect("poll") {
                    assert!(status.success());
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "true never exited"
                );
                sleep_ms(5);
            }
        }
        assert!(child.is_reaped());
        assert_eq!(
            child.signal(SIGTERM),
            Err(SignalError::NotFound { pid }),
            "{reaper}: signal after reap must not touch the pid"
        );
        child.kill().expect("kill after reap is a safe no-op");
    }
}

#[test]
fn openpty_split_api_needs_slave_drop_for_eof() {
    let params = SpawnParams::new("echo").arg("split");
    let (master, slave) = openpty(80, 24).expect("openpty");
    let mut child = slave.spawn(&params).expect("slave spawn");
    drop(slave);
    let out = drain_to_eof(master.try_clone_reader().expect("reader")).expect("drain");
    assert!(contains(&out, "split"), "expected split in {out:?}");
    assert!(child.wait().expect("wait").success());
}

#[test]
fn invalid_size_rejected_on_both_entries() {
    assert_eq!(
        openpty(0, 24).expect_err("zero cols must fail"),
        PtyError::InvalidSize { cols: 0, rows: 24 }
    );
    let params = SpawnParams::new("true");
    assert_eq!(
        spawn_pty(&params, 80, 0).expect_err("zero rows must fail"),
        PtyError::InvalidSize { cols: 80, rows: 0 }
    );
}

#[test]
fn empty_argv_rejected() {
    let empty = SpawnParams::default();
    assert_eq!(
        spawn_pty(&empty, 80, 24).expect_err("empty argv must fail"),
        PtyError::EmptyArgv
    );
    let (_master, slave) = openpty(80, 24).expect("openpty");
    assert_eq!(
        slave.spawn(&empty).expect_err("empty argv must fail"),
        PtyError::EmptyArgv
    );
}
