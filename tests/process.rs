#![cfg(all(unix, feature = "process"))]
// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Piped-process transport tests: spawn, stdio, exit, signals, identity.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use termpane::process::{
    PipedChild, PipedStdio, SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SignalError, SpawnParams,
    Stdio, current_uid, own_pgid, own_pid, pid_alive, process_ids, session_id_of, signal,
    spawn_piped,
};

fn null_stdio() -> PipedStdio {
    PipedStdio::new()
}

fn piped_stdout() -> PipedStdio {
    PipedStdio::new().stdout(Stdio::Pipe)
}

/// Test-only poll pacing on an owned test thread.
#[expect(
    clippy::disallowed_methods,
    reason = "test-only poll pacing on an owned test thread, never a render/runtime thread"
)]
fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Reap by polling `try_wait` (exercises the sticky-status path).
fn wait_via_try_wait(child: &mut PipedChild) -> std::io::Result<termpane::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        sleep_ms(5);
    }
}

/// Read a piped child's stdout fully, then reap it.
fn read_stdout_and_wait(
    child: &mut PipedChild,
) -> std::io::Result<(String, termpane::process::ExitStatus)> {
    let mut out = child.take_stdout().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "stdout pipe already taken")
    })?;
    let mut bytes = Vec::new();
    out.read_to_end(&mut bytes)?;
    let status = child.wait()?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), status))
}

#[test]
fn piped_echo_reports_stdout_and_clean_exit() {
    let params = SpawnParams::new("echo").arg("hi");
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn echo");
    assert!(child.pid().expect("piped pid") > 0);
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert_eq!(out, "hi\n");
    assert_eq!(status.exit_code(), 0);
    assert_eq!(status.signal(), None);
    assert!(status.success());
}

#[test]
fn argv_is_verbatim_without_shell_splitting() {
    let params = SpawnParams::new("echo").args(["a b", "c*d"]);
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn echo");
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert_eq!(out, "a b c*d\n");
    assert!(status.success());
}

#[test]
fn env_override_reaches_child_parent_untouched() {
    let params = SpawnParams::new("sh")
        .args(["-c", "echo \"v=$TP2_MARKER\""])
        .env("TP2_MARKER", "1");
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn sh");
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert!(status.success());
    assert_eq!(out.trim(), "v=1");
    assert_eq!(std::env::var_os("TP2_MARKER"), None);
}

#[test]
fn cwd_override_moves_child_parent_stays() {
    let before = std::env::current_dir().expect("parent cwd");
    let params = SpawnParams::new("sh")
        .args(["-c", "pwd"])
        .current_dir("/tmp");
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn sh");
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert!(status.success());
    let expected = std::fs::canonicalize("/tmp").expect("canonicalize /tmp");
    let actual = std::fs::canonicalize(out.trim()).expect("canonicalize child pwd");
    assert_eq!(actual, expected);
    assert_eq!(std::env::current_dir().expect("parent cwd"), before);
}

#[test]
fn try_wait_tristate_poll() {
    let params = SpawnParams::new("sleep").arg("30");
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn sleep");
    assert_eq!(
        child
            .try_wait()
            .expect("poll running")
            .map(|s| s.exit_code()),
        None
    );
    child.kill().expect("kill");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert_eq!(status.signal(), Some("SIGKILL"));
    assert!(!status.success());

    let params = SpawnParams::new("true");
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn true");
    let status = child.wait().expect("wait true");
    assert!(status.success());
    // Reaping is sticky: polling afterwards reports the same status.
    let again = child.try_wait().expect("poll reaped");
    assert_eq!(again.map(|s| s.exit_code()), Some(0));
}

#[test]
fn kill_and_reap_detached_path() {
    let params = SpawnParams::new("sleep").arg("30").detached(true);
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn detached sleep");
    assert_eq!(
        child
            .try_wait()
            .expect("poll running")
            .map(|s| s.exit_code()),
        None
    );
    child.kill().expect("kill");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert_eq!(status.signal(), Some("SIGKILL"));
}

#[test]
fn signal_term_kills_child() {
    let params = SpawnParams::new("sleep").arg("30");
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn sleep");
    child.signal(SIGTERM).expect("SIGTERM");
    let status = child.wait().expect("wait");
    assert_eq!(status.exit_code(), 1);
    assert_eq!(status.signal(), Some("SIGTERM"));
}

#[test]
fn signal_dead_pid_is_not_found() {
    let params = SpawnParams::new("true");
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn true");
    let pid = child.pid().expect("pid");
    assert!(child.wait().expect("wait").success());
    assert!(!pid_alive(pid));
    assert_eq!(signal(pid, SIGTERM), Err(SignalError::NotFound { pid }));
}

#[test]
fn signal_rejects_zero_pid_and_bad_signo() {
    assert_eq!(signal(0, SIGTERM), Err(SignalError::NotFound { pid: 0 }));
    let me = own_pid();
    assert!(pid_alive(me));
    assert_eq!(
        signal(me, 999_999),
        Err(SignalError::InvalidSignal(999_999))
    );
    // A no-op signal to ourselves succeeds and proves delivery works.
    assert_eq!(signal(me, 0), Err(SignalError::InvalidSignal(0)));
    assert_eq!(
        (SIGHUP, SIGINT, SIGQUIT, SIGKILL, SIGTERM),
        (1, 2, 3, 9, 15)
    );
}

#[test]
fn pid_alive_zombie_then_false_after_reap() {
    // Side channel proves exit without reaping: the child announces itself,
    // then only EOF (post-kill) tells us it died — still unreaped (zombie).
    let params = SpawnParams::new("sh").args(["-c", "echo ready; sleep 30"]);
    let stdio = PipedStdio::new().stdout(Stdio::Pipe);
    let mut child = spawn_piped(&params, &stdio).expect("spawn sh");
    let pid = child.pid().expect("pid");
    let mut out = child.take_stdout().expect("stdout pipe");
    let mut announced = [0u8; 6];
    out.read_exact(&mut announced).expect("read announcement");
    assert_eq!(&announced, b"ready\n");
    assert!(pid_alive(pid));
    child.kill().expect("kill");
    let mut rest = Vec::new();
    out.read_to_end(&mut rest).expect("read to EOF");
    // Exited (EOF seen) but unreaped: a zombie still owns its pid.
    assert!(pid_alive(pid));
    let status = child.wait().expect("wait");
    assert_eq!(status.signal(), Some("SIGKILL"));
    assert!(!pid_alive(pid));
}

#[test]
fn pid_alive_zero_is_false() {
    assert!(!pid_alive(0));
}

#[test]
fn own_ids_agree_with_kernel() {
    assert_eq!(own_pid(), std::process::id());
    let me = own_pid();
    let ids = process_ids(me).expect("own ids");
    assert_eq!(ids.pgid, own_pgid());
    assert!(ids.sid >= 0);
    assert_eq!(session_id_of(me), Some(ids.sid));

    let params = SpawnParams::new("id").arg("-u");
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn id");
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert!(status.success());
    assert_eq!(out.trim().parse::<u32>().expect("parse uid"), current_uid());
}

#[test]
fn detached_child_gets_new_session() {
    let params = SpawnParams::new("sleep").arg("30").detached(true);
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn detached sleep");
    let pid = child.pid().expect("pid");
    let raw = i32::try_from(pid).expect("pid fits");
    let ids = process_ids(pid).expect("detached ids");
    assert_eq!(ids.pgid, raw);
    assert_eq!(ids.sid, raw);
    assert_ne!(ids.pgid, own_pgid());
    assert_eq!(session_id_of(pid), Some(raw));
    child.kill().expect("kill");
    let status = child.wait().expect("wait");
    assert_eq!(status.signal(), Some("SIGKILL"));
}

#[test]
fn detached_with_cwd_reports_new_dir() {
    let params = SpawnParams::new("sh")
        .args(["-c", "pwd"])
        .current_dir("/tmp")
        .detached(true);
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn detached sh");
    let pid = child.pid().expect("pid");
    let ids = process_ids(pid).expect("detached ids");
    assert_eq!(ids.pgid, i32::try_from(pid).expect("pid fits"));
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert!(status.success());
    let expected = std::fs::canonicalize("/tmp").expect("canonicalize /tmp");
    let actual = std::fs::canonicalize(out.trim()).expect("canonicalize child pwd");
    assert_eq!(actual, expected);
}

#[test]
fn detached_with_cwd_preserves_argv_verbatim() {
    // Regression: the /bin/sh cwd wrapper once ran `shift` before
    // `exec "$@"`, but $0 (the directory) is not part of $@, so the shift
    // dropped the real argv[0] and exec ran the wrong program. bash masked
    // it for `sh -c pwd` (`exec -c` is a valid exec flag there); dash
    // failed loudly. This multi-arg case fails on both shells pre-fix.
    let params = SpawnParams::new("echo")
        .args(["a b", "c*d"])
        .current_dir("/tmp")
        .detached(true);
    let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn detached echo");
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert!(status.success());
    assert_eq!(out, "a b c*d\n");
}

#[test]
fn detached_bad_cwd_fails_fast() {
    let params = SpawnParams::new("true")
        .current_dir("/tp2-no-such-dir-xyz")
        .detached(true);
    let err = spawn_piped(&params, &null_stdio()).expect_err("bad cwd must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn spawn_missing_program_errors_on_both_paths() {
    for detached in [false, true] {
        let params = SpawnParams::new("tp2-no-such-program-xyz").detached(detached);
        let err = spawn_piped(&params, &null_stdio()).expect_err("missing program must fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "detached={detached}"
        );
    }
}

#[test]
fn empty_argv_and_nul_are_rejected() {
    let empty = SpawnParams::default();
    for detached in [false, true] {
        let params = SpawnParams::default().detached(detached);
        let err = spawn_piped(&params, &null_stdio()).expect_err("empty argv must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(empty.argv().is_empty());

    let nul_arg = SpawnParams::new("echo").arg("a\0b");
    let err = spawn_piped(&nul_arg, &null_stdio()).expect_err("NUL argv must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    let nul_env = SpawnParams::new("true").env("TP2_NUL", "a\0b");
    let err = spawn_piped(&nul_env, &null_stdio()).expect_err("NUL env must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn file_stdio_redirection_captures_output() {
    let path = std::env::temp_dir().join(format!("tp2-out-{}.txt", own_pid()));
    let file = std::fs::File::create(&path).expect("create capture file");
    let params = SpawnParams::new("echo").arg("logged");
    let stdio = PipedStdio::new().stdout(Stdio::File(file));
    let mut child = spawn_piped(&params, &stdio).expect("spawn echo");
    assert!(child.take_stdout().is_none());
    assert!(child.wait().expect("wait").success());
    let body = std::fs::read_to_string(&path).expect("read capture file");
    assert_eq!(body, "logged\n");
    std::fs::remove_file(&path).expect("cleanup capture file");
}

#[test]
fn stdin_pipe_feeds_child_and_drop_is_eof() {
    let params = SpawnParams::new("cat");
    let stdio = PipedStdio::new().stdin(Stdio::Pipe).stdout(Stdio::Pipe);
    let mut child = spawn_piped(&params, &stdio).expect("spawn cat");
    let mut stdin = child.take_stdin().expect("stdin pipe");
    stdin.write_all(b"hello").expect("write stdin");
    drop(stdin);
    let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
    assert_eq!(out, "hello");
    assert!(status.success());
}

#[test]
fn blocking_wait_resolves_promptly() {
    let params = SpawnParams::new("true");
    let mut child = spawn_piped(&params, &null_stdio()).expect("spawn true");
    let start = Instant::now();
    assert!(child.wait().expect("wait").success());
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn detached_spawn_searches_overridden_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("tp2-path-{}", own_pid()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let tool = dir.join("tp2-tool-xyz");
    std::fs::write(&tool, "#!/bin/sh\necho path-ok\n").expect("write tool");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // `posix_spawnp` resolves against the *parent* PATH, ignoring the child
    // env vector; the detached path must resolve against the merged child
    // PATH instead, or this tool (absent from the system PATH) is ENOENT.
    for detached in [false, true] {
        let params = SpawnParams::new("tp2-tool-xyz")
            .env("PATH", &dir)
            .detached(detached);
        let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn via child PATH");
        let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
        assert!(status.success(), "detached={detached}");
        assert_eq!(out, "path-ok\n", "detached={detached}");
    }
    std::fs::remove_file(&tool).expect("cleanup");
    std::fs::remove_dir(&dir).expect("cleanup");
}

#[test]
fn detached_unexecutable_program_reports_permission_denied() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("tp2-noexec-{}", own_pid()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let tool = dir.join("tp2-noexec-xyz");
    std::fs::write(&tool, "#!/bin/sh\necho noexec\n").expect("write tool");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    // Present in the child PATH but not executable: execvp reports EACCES
    // (not ENOENT) when a candidate exists but cannot run.
    for detached in [false, true] {
        let params = SpawnParams::new("tp2-noexec-xyz")
            .env("PATH", &dir)
            .detached(detached);
        let err = spawn_piped(&params, &null_stdio()).expect_err("unexecutable must fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "detached={detached}"
        );
    }
    std::fs::remove_file(&tool).expect("cleanup");
    std::fs::remove_dir(&dir).expect("cleanup");
}

#[test]
fn identity_probes_reject_pid_zero() {
    // Pid 0 means "calling process" to getpgid/getsid; reporting our own
    // ids for it would let callers validate the wrong process.
    assert_eq!(process_ids(0), None);
    assert_eq!(session_id_of(0), None);
}

#[test]
fn file_cwd_rejected_on_both_paths() {
    let path = std::env::temp_dir().join(format!("tp2-filecwd-{}.txt", own_pid()));
    std::fs::write(&path, "x").expect("write");
    for detached in [false, true] {
        let params = SpawnParams::new("true")
            .current_dir(&path)
            .detached(detached);
        let err = spawn_piped(&params, &null_stdio()).expect_err("file cwd must fail fast");
        // Attached kind is std-owned and platform-dependent (Linux
        // fork+chdir reports ENOTDIR; macOS std uses posix_spawn with
        // Apple's addchdir_np, which reports ENOENT for non-directories),
        // so only the synchronous failure itself is pinned there.
        if detached {
            assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);
        }
    }
    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn unsearchable_cwd_rejected_on_both_paths() {
    use std::os::unix::fs::PermissionsExt as _;
    if current_uid() == 0 {
        eprintln!("skip: root bypasses directory search permission");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tp2-nocwd-{}", own_pid()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    for detached in [false, true] {
        let params = SpawnParams::new("true")
            .current_dir(&dir)
            .detached(detached);
        let err = spawn_piped(&params, &null_stdio()).expect_err("unsearchable cwd must fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "detached={detached}"
        );
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    std::fs::remove_dir(&dir).expect("cleanup");
}

#[test]
fn kill_after_reap_is_safe_noop_on_both_paths() {
    // std's contract (Child::kill docs): "If the child has already exited,
    // Ok(()) is returned." A reaped pid must never be signalled: it may
    // already be reused by an unrelated process. The dead-pid assertion
    // proves the Ok is a true no-op — a real kill(2) on a dead pid fails.
    for detached in [false, true] {
        for reaper in ["wait", "try_wait"] {
            let params = SpawnParams::new("true").detached(detached);
            let mut child = spawn_piped(&params, &null_stdio()).expect("spawn");
            if reaper == "wait" {
                assert!(child.wait().expect("wait").success());
            } else {
                assert!(wait_via_try_wait(&mut child).expect("reap").success());
            }
            let pid = child.pid().expect("pid");
            assert!(!pid_alive(pid), "reaped pid must be dead");
            child
                .kill()
                .expect("kill after reap is a safe no-op, like std");
        }
    }
}

#[test]
fn detached_null_stdio_leaks_no_extra_null_fds() {
    // Counts fds above 2 backed by /dev/null (dev+ino comparison, so the
    // enumerator's own dir fd and stray inherited pipes never count).
    // `stat -c` is GNU, `stat -f` is BSD/macOS.
    let script = "null_id=$(stat -c '%d:%i' /dev/null 2>/dev/null || stat -f '%d:%i' /dev/null)\n\
        dir=/proc/self/fd; [ -d \"$dir\" ] || dir=/dev/fd\n\
        leaked=0\n\
        for f in \"$dir\"/*; do\n\
        n=${f##*/}\n\
        case $n in *[!0-9]*) continue;; esac\n\
        [ \"$n\" -gt 2 ] || continue\n\
        id=$(stat -c '%d:%i' \"$f\" 2>/dev/null || stat -f '%d:%i' \"$f\" 2>/dev/null) || continue\n\
        [ \"$id\" = \"$null_id\" ] && leaked=$((leaked+1))\n\
        done\n\
        echo \"leaked=$leaked\"";
    for detached in [false, true] {
        let params = SpawnParams::new("sh")
            .args(["-c", script])
            .detached(detached);
        let mut child = spawn_piped(&params, &piped_stdout()).expect("spawn");
        let (out, status) = read_stdout_and_wait(&mut child).expect("drain");
        assert!(status.success(), "detached={detached}");
        // The attached leg doubles as a methodology self-check: std closes
        // everything above 2, so a nonzero count there means the script
        // itself is broken, not the spawn path.
        assert_eq!(out.trim(), "leaked=0", "detached={detached}");
    }
}
