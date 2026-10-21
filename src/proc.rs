//! Process execution helper shared by every subsystem that shells out.
//!
//! Two properties matter here and they are the reason this is not just
//! `Command::output()`:
//!
//! * **No pipe deadlock.** stdout and stderr are drained on their own threads,
//!   so a chatty child cannot fill a pipe buffer and wedge forever.
//! * **A wall-clock deadline.** A wedged `docker compose` or `nft` is killed
//!   rather than hanging a request until the process is restarted.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{io, thread};

/// How often the child is polled while waiting for it to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Exit code, or `None` when the child was killed or died on a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout then stderr, for error messages. Empty lines are not trimmed so
    /// caller-side formatting stays predictable.
    pub fn combined(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout.trim_end(), self.stderr),
        }
    }
}

/// Run `program` to completion, killing it past `timeout`.
///
/// Returns `Err` only when the process could not be spawned at all (not found,
/// permission denied); a non-zero exit or a timeout is an `Ok(Output)`.
pub fn run(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
) -> io::Result<Output> {
    run_inner(program, args, cwd, &[], timeout, None)
}

/// Like [`run`], but with extra environment variables set on the child.
///
/// This is how restic receives `RESTIC_PASSWORD` and backend credentials: an
/// environment variable is not visible in `ps` the way an argument is.
pub fn run_env(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> io::Result<Output> {
    run_inner(program, args, None, env, timeout, None)
}

/// Like [`run`], but writes `input` to the child's stdin before closing it.
///
/// This is how a secret is handed to `chpasswd` without ever appearing in the
/// process argument list.
pub fn run_with_input(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
    input: &[u8],
) -> io::Result<Output> {
    run_inner(program, args, cwd, &[], timeout, Some(input))
}

fn run_inner(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &[(String, String)],
    timeout: Duration,
    input: Option<&[u8]>,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }

    let mut child = command.spawn()?;

    // Write stdin first, then close it, so a child reading to EOF is unblocked.
    if let Some(bytes) = input
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(bytes);
        // Dropping `stdin` closes the pipe.
    }

    // Take the pipes and drain them concurrently.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || drain(stdout));
    let stderr_reader = thread::spawn(move || drain(stderr));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                // Reap the readers so the threads do not outlive this call.
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(err);
            }
        }
    };

    // Killing the child closes the pipes, so both readers terminate.
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    Ok(Output {
        code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        timed_out,
    })
}

fn drain(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buffer = Vec::new();
    // A read error here means the child died mid-write; whatever was read so
    // far is still the most useful thing to report.
    let _ = pipe.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_command() -> (&'static str, Vec<String>) {
        #[cfg(windows)]
        {
            ("cmd", vec!["/C".to_string(), "echo hi".to_string()])
        }
        #[cfg(not(windows))]
        {
            ("echo", vec!["hi".to_string()])
        }
    }

    #[test]
    fn captures_stdout_and_reports_success() {
        let (program, args) = echo_command();
        let output = run(program, &args, None, Duration::from_secs(10)).expect("spawn");
        assert!(output.success(), "{output:?}");
        assert!(output.stdout.contains("hi"), "{output:?}");
        assert!(!output.timed_out);
    }

    #[test]
    fn missing_program_is_a_spawn_error() {
        let err = run(
            "stack-stack-definitely-not-a-real-binary",
            &[],
            None,
            Duration::from_secs(2),
        );
        assert!(err.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hangs_are_killed_at_the_deadline() {
        let output = run(
            "sleep",
            &["30".to_string()],
            None,
            Duration::from_millis(300),
        )
        .expect("spawn");
        assert!(output.timed_out, "{output:?}");
        assert!(!output.success());
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_exit_is_reported_not_returned_as_err() {
        let output = run("false", &[], None, Duration::from_secs(5)).expect("spawn");
        assert!(!output.success());
        assert!(!output.timed_out);
        assert_eq!(output.code, Some(1));
    }

    #[test]
    fn combined_prefers_both_streams() {
        let output = Output {
            code: Some(0),
            stdout: "out\n".to_string(),
            stderr: "err\n".to_string(),
            timed_out: false,
        };
        assert_eq!(output.combined(), "out\nerr\n");
    }

    #[cfg(unix)]
    #[test]
    fn input_is_written_to_stdin() {
        let output =
            run_with_input("cat", &[], None, Duration::from_secs(5), b"secret\n").expect("spawn");
        assert!(output.success(), "{output:?}");
        assert_eq!(output.stdout, "secret\n");
    }
}
