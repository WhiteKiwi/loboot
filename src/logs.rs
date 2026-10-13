//! Bounded log views. Application content remains potentially sensitive even
//! after best-effort token redaction; metadata never includes environment values.
use crate::backend::{Platform, Runner};
use crate::process::CaptureLimits;
use crate::{Error, Result};
use serde::Serialize;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

pub const DEFAULT_LINES: usize = 100;
pub const DEFAULT_BYTES: usize = 64 * 1024;
pub const MAX_LINES: usize = 10_000;
pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct LogLimits {
    pub lines: usize,
    pub bytes: usize,
}
impl LogLimits {
    pub fn new(lines: usize, bytes: usize) -> Result<Self> {
        if !(1..=MAX_LINES).contains(&lines) || !(1..=MAX_BYTES).contains(&bytes) {
            return Err(Error::new(
                "invalid_log_limit",
                "log limits must be 1..=10000 lines and 1..=1048576 bytes per source",
            ));
        }
        Ok(Self { lines, bytes })
    }
}

#[derive(Debug, Serialize)]
pub struct LogSource {
    pub source: &'static str,
    pub stream: &'static str,
    pub path: Option<PathBuf>,
    pub order: &'static str,
    pub missing: bool,
    pub empty: bool,
    pub truncated: bool,
    pub bytes_truncated: bool,
    pub lines_truncated: bool,
    pub invalid_utf8: bool,
    pub redacted: bool,
    pub bytes_returned: usize,
    pub lines_returned: usize,
    pub text: String,
}

fn bounded_text(bytes: &[u8], limits: LogLimits, tail: bool, already_truncated: bool) -> LogSource {
    let mut bytes_truncated = already_truncated || bytes.len() > limits.bytes;
    let bounded = if bytes.len() > limits.bytes {
        if tail {
            &bytes[bytes.len() - limits.bytes..]
        } else {
            &bytes[..limits.bytes]
        }
    } else {
        bytes
    };
    let invalid_utf8 = std::str::from_utf8(bounded).is_err();
    let text = String::from_utf8_lossy(bounded);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let lines_truncated = lines.len() > limits.lines;
    let selected = if lines_truncated {
        if tail {
            &lines[lines.len() - limits.lines..]
        } else {
            &lines[..limits.lines]
        }
    } else {
        &lines[..]
    };
    let (mut text, redacted) = redact(&selected.concat());
    // Lossy decoding and replacement markers can grow; the public text remains
    // bounded by bytes after both transformations, on a UTF-8 boundary.
    if text.len() > limits.bytes {
        bytes_truncated = true;
        if tail {
            let mut begin = text.len() - limits.bytes;
            while !text.is_char_boundary(begin) {
                begin += 1;
            }
            text = text[begin..].to_owned();
        } else {
            let mut end = limits.bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
    }
    LogSource {
        source: "file",
        stream: "stdout",
        path: None,
        order: if tail {
            "oldest-first"
        } else {
            "newest-first-entries"
        },
        missing: false,
        empty: bytes.is_empty(),
        truncated: bytes_truncated || lines_truncated,
        bytes_truncated,
        lines_truncated,
        invalid_utf8,
        redacted,
        bytes_returned: text.len(),
        lines_returned: text.split_inclusive('\n').count(),
        text,
    }
}

pub fn read_file(
    stream: &'static str,
    path: PathBuf,
    file: Option<File>,
    limits: LogLimits,
) -> Result<LogSource> {
    let Some(mut file) = file else {
        let mut result = bounded_text(b"", limits, true, false);
        result.stream = stream;
        result.path = Some(path);
        result.missing = true;
        return Ok(result);
    };
    // Snapshot length plus a bounded read. Concurrent growth is left for a later
    // invocation; a shrink cannot turn this into an unbounded read.
    let length = file.metadata()?.len();
    let amount = (limits.bytes + 1) as u64;
    file.seek(SeekFrom::Start(length.saturating_sub(amount)))?;
    let mut bytes = Vec::with_capacity(amount.min(length) as usize);
    file.take(amount).read_to_end(&mut bytes)?;
    let mut result = bounded_text(&bytes, limits, true, length > limits.bytes as u64);
    result.stream = stream;
    result.path = Some(path);
    Ok(result)
}

pub fn read_journal(id: &str, limits: LogLimits, runner: &mut dyn Runner) -> Result<LogSource> {
    let command = Platform::Linux.journal_plan(id, limits.lines)?;
    let output = runner.run_with_limits(
        &command,
        CaptureLimits {
            stdout_bytes: limits.bytes + 1,
            ..Default::default()
        },
    )?;
    let bounded_stop = output.termination_for_limit
        && output.stdout_truncated
        && !output.stderr_truncated
        && !output.timed_out;
    if output.timed_out || output.stderr_truncated || (!output.success && !bounded_stop) {
        return Err(Error::new("logs_unavailable", output.failure_summary()));
    }
    let mut result = bounded_text(
        output.stdout.as_bytes(),
        limits,
        false,
        output.stdout_truncated,
    );
    result.source = "journal";
    result.stream = "combined";
    result.invalid_utf8 |= output.stdout_invalid_utf8;
    Ok(result)
}

/// Common assignment/header forms only. Arbitrary application formats, split
/// tokens, encoded values and secrets without these names cannot be identified.
fn redact(input: &str) -> (String, bool) {
    let lower = input.to_ascii_lowercase();
    let mut replacements = Vec::new();
    for key in [
        "authorization",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "token",
        "secret",
    ] {
        let mut offset = 0;
        while let Some(found) = lower[offset..].find(key) {
            let start = offset + found;
            offset = start + key.len();
            if start > 0
                && (lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                    || lower.as_bytes()[start - 1] == b'_')
            {
                continue;
            }
            let mut begin = offset;
            if lower.as_bytes().get(begin) == Some(&b'"')
                || lower.as_bytes().get(begin) == Some(&b'\'')
            {
                begin += 1;
            }
            while lower
                .as_bytes()
                .get(begin)
                .is_some_and(|b| matches!(b, b' ' | b'\t'))
            {
                begin += 1;
            }
            if !lower
                .as_bytes()
                .get(begin)
                .is_some_and(|b| matches!(b, b'=' | b':'))
            {
                continue;
            }
            begin += 1;
            while lower
                .as_bytes()
                .get(begin)
                .is_some_and(|b| matches!(b, b' ' | b'\t'))
            {
                begin += 1;
            }
            let quote = lower
                .as_bytes()
                .get(begin)
                .copied()
                .filter(|b| matches!(b, b'"' | b'\''));
            if quote.is_some() {
                begin += 1;
            }
            let end = if key == "authorization" {
                input[begin..]
                    .find('\n')
                    .map(|n| begin + n)
                    .unwrap_or(input.len())
            } else {
                input[begin..]
                    .char_indices()
                    .find(|(_, ch)| {
                        if let Some(quote) = quote {
                            *ch == quote as char || *ch == '\n'
                        } else {
                            ch.is_whitespace() || matches!(ch, ',' | ';' | '}' | ']')
                        }
                    })
                    .map(|(n, _)| begin + n)
                    .unwrap_or(input.len())
            };
            if end > begin {
                replacements.push((begin, end));
            }
        }
    }
    replacements.sort_unstable();
    let mut output = String::new();
    let mut end = 0;
    for (begin, next) in replacements {
        if begin < end {
            continue;
        }
        output.push_str(&input[end..begin]);
        output.push_str("[REDACTED]");
        end = next;
    }
    if end == 0 {
        return (input.into(), false);
    }
    output.push_str(&input[end..]);
    (output, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CommandOutput, CommandSpec};
    use std::io::Write;
    #[test]
    fn invalid_limits_are_rejected() {
        for (lines, bytes) in [(0, 1), (1, 0), (MAX_LINES + 1, 1), (1, MAX_BYTES + 1)] {
            assert_eq!(
                LogLimits::new(lines, bytes).unwrap_err().code,
                "invalid_log_limit"
            );
        }
    }
    #[test]
    fn tails_bound_huge_empty_missing_and_invalid_utf8() {
        let limits = LogLimits::new(2, 16).unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![b'x'; MAX_BYTES * 4]).unwrap();
        file.write_all(b"\nold\nnew\nlast\n").unwrap();
        let result = read_file("stderr", "/owned/log".into(), Some(file), limits).unwrap();
        assert_eq!(result.text, "new\nlast\n");
        assert!(result.truncated && result.lines_truncated && result.bytes_truncated);
        let missing = read_file("stdout", "/missing".into(), None, limits).unwrap();
        assert!(missing.missing && missing.empty);
        let empty = read_file(
            "stdout",
            "/empty".into(),
            Some(tempfile::tempfile().unwrap()),
            limits,
        )
        .unwrap();
        assert!(empty.empty && !empty.missing && !empty.truncated);
        let invalid = bounded_text(&[0xff; 20], limits, true, false);
        assert!(invalid.invalid_utf8 && invalid.bytes_returned <= 16);
    }
    #[test]
    fn utf8_boundaries_and_line_limits_are_honest() {
        for bytes in 1..16 {
            let limits = LogLimits::new(1, bytes).unwrap();
            for tail in [true, false] {
                let result = bounded_text("한글😀\nsecond\n".as_bytes(), limits, tail, false);
                assert!(result.bytes_returned <= bytes);
                assert!(result.lines_returned <= 1);
                assert!(result.truncated);
            }
        }
        let exact = bounded_text(b"one\ntwo\n", LogLimits::new(2, 8).unwrap(), true, false);
        assert!(!exact.truncated);
    }
    #[test]
    fn common_tokens_are_redacted_with_explicit_limits() {
        let (text, changed) = redact(
            "password=hidden token = 'another secret'\n{\"api_key\":\"abc123\"}\nAuthorization: Bearer private\nordinary content\n",
        );
        assert!(changed);
        for secret in ["hidden", "another secret", "abc123", "private"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("ordinary content"));
        assert_eq!(
            redact("application wrote an unlabelled secret"),
            ("application wrote an unlabelled secret".into(), false)
        );
    }
    struct Fake {
        output: Option<CommandOutput>,
        calls: Vec<CommandSpec>,
    }
    impl Runner for Fake {
        fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
            self.calls.push(command.clone());
            Ok(self.output.take().unwrap())
        }
    }
    #[test]
    fn journal_uses_argv_preserves_multiline_order_and_bounds_output() {
        let mut fake = Fake {
            output: Some(CommandOutput {
                success: true,
                code: Some(0),
                stdout: "new entry\ncontinuation\nold entry\n".into(),
                ..Default::default()
            }),
            calls: vec![],
        };
        let result = read_journal("worker", LogLimits::new(2, 64).unwrap(), &mut fake).unwrap();
        assert_eq!(result.text, "new entry\ncontinuation\n");
        assert!(result.lines_truncated);
        assert_eq!(fake.calls.len(), 1);
        assert!(fake.calls[0].args.iter().any(|arg| arg == "--reverse"));
        assert!(
            fake.calls[0]
                .args
                .iter()
                .any(|arg| arg.contains("loboot-worker.service"))
        );
    }
    #[test]
    fn journal_failures_do_not_leak_stderr_and_partial_caps_are_explicit() {
        let mut fake = Fake {
            output: Some(CommandOutput {
                code: Some(1),
                stderr: "secret=private-token".into(),
                ..Default::default()
            }),
            calls: vec![],
        };
        assert!(
            !read_journal("worker", LogLimits::new(2, 64).unwrap(), &mut fake)
                .unwrap_err()
                .message
                .contains("private-token")
        );
        fake.output = Some(CommandOutput {
            signal: Some(9),
            stdout: "recent\n".into(),
            stdout_truncated: true,
            termination_for_limit: true,
            ..Default::default()
        });
        assert!(
            read_journal("worker", LogLimits::new(2, 64).unwrap(), &mut fake)
                .unwrap()
                .truncated
        );
        fake.output = Some(CommandOutput {
            success: true,
            timed_out: true,
            ..Default::default()
        });
        assert!(read_journal("worker", LogLimits::new(2, 64).unwrap(), &mut fake).is_err());
    }
}
