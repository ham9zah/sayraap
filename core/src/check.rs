//! Checkers decide whether a change is accepted; the model's word never does.
//! This first checker runs a project command (a compiler, a test runner)
//! inside the project and passes only if it exits successfully in time.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 120;
/// How much of a failing check's output is kept as the rejection reason.
const DETAIL_LINES: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Program and arguments, run without a shell.
    pub command: Vec<String>,
    pub timeout: Duration,
}

impl Check {
    /// Splits a command line on whitespace. Returns `None` if it is empty.
    pub fn from_line(line: &str, timeout: Duration) -> Option<Self> {
        let command: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if command.is_empty() { None } else { Some(Self { command, timeout }) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Passed,
    /// The command reported a problem; `detail` is the start of its output.
    Failed { detail: String },
    TimedOut,
    /// The command could not be started at all.
    Unavailable,
}

pub fn run_check(project: &Path, check: &Check) -> CheckResult {
    // Output goes to a file so a chatty command can never block on a full pipe.
    let Ok(mut output) = tempfile::tempfile() else {
        return CheckResult::Unavailable;
    };
    let (Ok(out), Ok(err)) = (output.try_clone(), output.try_clone()) else {
        return CheckResult::Unavailable;
    };
    let spawned = Command::new(&check.command[0])
        .args(&check.command[1..])
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    let Ok(mut child) = spawned else {
        return CheckResult::Unavailable;
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > check.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return CheckResult::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return CheckResult::Unavailable,
        }
    };
    if status.success() {
        return CheckResult::Passed;
    }

    let mut bytes = Vec::new();
    let _ = output.seek(SeekFrom::Start(0));
    let _ = output.read_to_end(&mut bytes);
    let detail = String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(DETAIL_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    CheckResult::Failed { detail }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn check(line: &str, millis: u64) -> CheckResult {
        let dir = tempfile::TempDir::new().unwrap();
        run_check(dir.path(), &Check::from_line(line, Duration::from_millis(millis)).unwrap())
    }

    #[test]
    fn passes_fails_times_out_and_reports_missing_programs() {
        assert_eq!(check("true", 5000), CheckResult::Passed);
        assert_eq!(check("ls /definitely/not/here", 5000).detail_contains("not/here"), true);
        assert_eq!(check("sleep 5", 200), CheckResult::TimedOut);
        assert_eq!(check("no-such-program-sayraap", 5000), CheckResult::Unavailable);
    }

    #[test]
    fn empty_command_line_is_no_check() {
        assert_eq!(Check::from_line("   ", Duration::from_secs(1)), None);
    }

    impl CheckResult {
        fn detail_contains(&self, needle: &str) -> bool {
            matches!(self, CheckResult::Failed { detail } if detail.contains(needle))
        }
    }
}
