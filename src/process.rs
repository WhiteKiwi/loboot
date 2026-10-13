//! Bounded subprocess capture for native manager helpers, never service processes.
//!
//! Each helper gets its own process group. Nonblocking reads, a fixed per-stream
//! budget, and a deadline prevent either a noisy helper or an inherited pipe in
//! a descendant from keeping loboot waiting indefinitely.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct CaptureLimits {
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub timeout: Duration,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 64 * 1024,
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Default)]
pub struct CapturedOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub termination_for_limit: bool,
}

// A bounded cleanup interval permits SIGKILL delivery and collecting its exact
// wait status. If a kernel-stuck child remains, a reaper owns only the Child,
// never its pipes, and cannot keep capture itself from returning.
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(100);
const POLL_INTERVAL: Duration = Duration::from_millis(2);

pub fn capture(command: &mut Command, limits: CaptureLimits) -> io::Result<CapturedOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let result = capture_child(&mut child, limits);
    if result.is_err() {
        terminate_group(&child);
    }
    // try_wait reaps a completed child. An exceptional kernel delay cannot turn
    // the helper's deadline into a blocking wait, or leave a future zombie.
    if !matches!(child.try_wait(), Ok(Some(_))) {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

fn capture_child(child: &mut Child, limits: CaptureLimits) -> io::Result<CapturedOutput> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if let Some(pipe) = &stdout {
        set_nonblocking(pipe)?;
    }
    if let Some(pipe) = &stderr {
        set_nonblocking(pipe)?;
    }
    let started = Instant::now();
    let mut cleanup_started = None;
    let mut status = None;
    let mut result = CapturedOutput::default();
    loop {
        // Do not reap while descendants may still hold capture pipes. Keeping
        // the helper's PID reserved prevents a later group-kill from targeting
        // an unrelated process group after PID reuse.
        // One small read per stream per round keeps both streams and the clock
        // moving, including when a producer never stops writing.
        let stdout_progress = drain_once(
            &mut stdout,
            &mut result.stdout,
            limits.stdout_bytes,
            &mut result.stdout_truncated,
        )?;
        let stderr_progress = drain_once(
            &mut stderr,
            &mut result.stderr,
            limits.stderr_bytes,
            &mut result.stderr_truncated,
        )?;
        if cleanup_started.is_none() {
            if result.stdout_truncated || result.stderr_truncated {
                result.termination_for_limit = true;
                terminate_group(child);
                cleanup_started = Some(Instant::now());
            } else if started.elapsed() >= limits.timeout {
                result.timed_out = true;
                terminate_group(child);
                cleanup_started = Some(Instant::now());
            }
        }
        if stdout.is_none() && stderr.is_none() {
            status = child.try_wait()?;
            if status.is_some() {
                break;
            }
        }
        if cleanup_started.is_some_and(|start: Instant| start.elapsed() >= CLEANUP_TIMEOUT) {
            // Streams still held by escaped descendants are deliberately closed.
            // Their completeness is unknown, not an empty successful capture.
            result.stdout_truncated |= stdout.is_some();
            result.stderr_truncated |= stderr.is_some();
            break;
        }
        if !stdout_progress && !stderr_progress {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
    if status.is_none() {
        status = child.try_wait()?;
    }
    if let Some(status) = status {
        record_status(&mut result, status);
    }
    Ok(result)
}

fn record_status(result: &mut CapturedOutput, status: ExitStatus) {
    result.success = status.success();
    result.code = status.code();
    result.signal = status.signal();
}

fn set_nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd belongs to the still-live pipe. F_GETFL has no extra argument;
    // F_SETFL changes only O_NONBLOCK while preserving every existing flag.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn terminate_group(child: &Child) {
    // SAFETY: process_group(0) gave this spawned helper a new group whose ID is
    // its PID. A negative PID targets that group, never loboot's caller group.
    // This never targets service PIDs managed separately by systemd/launchd.
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

fn drain_once<T: Read>(
    pipe: &mut Option<T>,
    captured: &mut Vec<u8>,
    limit: usize,
    truncated: &mut bool,
) -> io::Result<bool> {
    let Some(stream) = pipe.as_mut() else {
        return Ok(false);
    };
    let mut buffer = [0u8; 8192];
    match stream.read(&mut buffer) {
        Ok(0) => {
            *pipe = None;
            Ok(false)
        }
        Ok(count) => {
            let retained = count.min(limit.saturating_sub(captured.len()));
            captured.extend_from_slice(&buffer[..retained]);
            *truncated |= count > retained;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str, limits: CaptureLimits) -> CapturedOutput {
        capture(Command::new("/bin/sh").args(["-c", script]), limits).unwrap()
    }

    #[test]
    fn exact_exit_and_signal_are_retained() {
        let output = shell(
            "printf hello; printf error >&2; exit 42",
            CaptureLimits::default(),
        );
        assert_eq!(output.stdout, b"hello");
        assert_eq!(output.stderr, b"error");
        assert_eq!(output.code, Some(42));
        assert_eq!(output.signal, None);
        assert!(!output.success);
        let output = shell("kill -TERM $$", CaptureLimits::default());
        assert_eq!(output.code, None);
        assert_eq!(output.signal, Some(libc::SIGTERM));
    }

    #[test]
    fn simultaneous_unbounded_streams_are_stopped_and_bounded() {
        let started = Instant::now();
        let output = shell(
            "while :; do printf '0123456789abcdef'; printf 'fedcba9876543210' >&2; done",
            CaptureLimits {
                stdout_bytes: 1024,
                stderr_bytes: 1024,
                ..Default::default()
            },
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(output.stdout.len() <= 1024);
        assert!(output.stderr.len() <= 1024);
        assert!(output.termination_for_limit);
        assert!(output.stdout_truncated || output.stderr_truncated);
        assert!(!output.timed_out);
        assert_eq!(output.signal, Some(libc::SIGKILL));
    }

    #[test]
    fn exact_limit_and_invalid_bytes_are_not_altered() {
        let output = shell(
            "printf '\\377abc'; printf abcd >&2",
            CaptureLimits {
                stdout_bytes: 4,
                stderr_bytes: 4,
                ..Default::default()
            },
        );
        assert_eq!(output.stdout, [255, b'a', b'b', b'c']);
        assert_eq!(output.stderr, b"abcd");
        assert!(output.success);
        assert!(!output.stdout_truncated);
        assert!(!output.stderr_truncated);
        assert!(!output.termination_for_limit);
    }

    #[test]
    fn timeout_terminates_and_reaps_helper() {
        let started = Instant::now();
        let output = shell(
            "sleep 30",
            CaptureLimits {
                timeout: Duration::from_millis(50),
                ..Default::default()
            },
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(output.timed_out);
        assert!(!output.termination_for_limit);
        assert_eq!(output.signal, Some(libc::SIGKILL));
    }

    #[test]
    fn timeout_does_not_signal_unrelated_process_and_handles_closed_pipes() {
        let mut unrelated = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let output = shell(
            "exec 1>&- 2>&-; sleep 30",
            CaptureLimits {
                timeout: Duration::from_millis(50),
                ..Default::default()
            },
        );
        let survived = unrelated.try_wait().unwrap().is_none();
        terminate_group(&unrelated);
        let _ = unrelated.wait();
        assert!(survived);
        assert!(output.timed_out);
        assert_eq!(output.signal, Some(libc::SIGKILL));
    }
    #[test]
    fn exited_child_with_descendant_holding_pipes_cannot_hang() {
        let started = Instant::now();
        let output = shell(
            "sleep 30 & exit 7",
            CaptureLimits {
                timeout: Duration::from_millis(50),
                ..Default::default()
            },
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(output.timed_out);
        assert_eq!(output.code, Some(7));
        assert_eq!(output.signal, None);
    }
}
