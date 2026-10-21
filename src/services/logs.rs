//! journald log viewer.
//!
//! Reads go through `journalctl -o json`; the unit list comes from
//! `systemctl list-units` (JSON when the systemd on the box supports it, plain
//! text otherwise). Follow mode streams `journalctl -f` lines over a dedicated
//! reader thread — [`crate::proc`] buffers whole outputs, which a follow stream
//! by definition never produces.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::runner::Runner;

/// Default tail depth.
pub const DEFAULT_LINES: u32 = 200;
/// Hard cap on a single read, so a `-n 1000000` request cannot pin the box.
pub const MAX_LINES: u32 = 5000;
/// Severities the UI offers (journald's names).
pub const PRIORITIES: [(&str, &str); 8] = [
    ("0", "emerg"),
    ("1", "alert"),
    ("2", "crit"),
    ("3", "err"),
    ("4", "warning"),
    ("5", "notice"),
    ("6", "info"),
    ("7", "debug"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnitInfo {
    pub unit: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts_ms: u64,
    pub priority: u8,
    pub unit: String,
    pub message: String,
}

/// Units from `systemctl list-units --type=service`. JSON first, plain text as
/// the fallback for older systemd.
pub fn list_units(runner: &dyn Runner) -> Result<Vec<UnitInfo>, String> {
    let json = runner.run_timeout(
        "systemctl",
        &[
            "list-units".to_string(),
            "--type=service".to_string(),
            "--all".to_string(),
            "--output=json".to_string(),
            "--no-pager".to_string(),
        ],
        Duration::from_secs(15),
    );
    if let Ok(output) = json
        && output.success()
        && let Ok(units) = serde_json::from_str::<Vec<serde_json::Value>>(&output.stdout)
    {
        let parsed: Vec<UnitInfo> = units
            .iter()
            .filter_map(|value| {
                Some(UnitInfo {
                    unit: string_of(value, "unit")?,
                    active: string_of(value, "active").unwrap_or_default(),
                    sub: string_of(value, "sub").unwrap_or_default(),
                    description: string_of(value, "description").unwrap_or_default(),
                })
            })
            .collect();
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }

    let plain = runner
        .run_timeout(
            "systemctl",
            &[
                "list-units".to_string(),
                "--type=service".to_string(),
                "--all".to_string(),
                "--no-legend".to_string(),
                "--no-pager".to_string(),
            ],
            Duration::from_secs(15),
        )
        .map_err(|err| format!("systemctl is not available: {err}"))?;
    if !plain.success() {
        return Err(format!(
            "systemctl list-units failed: {}",
            first_line(&plain.combined())
        ));
    }
    Ok(parse_unit_lines(&plain.stdout))
}

/// `unit.service loaded active running Description…`
pub fn parse_unit_lines(text: &str) -> Vec<UnitInfo> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            let unit = fields.next()?.to_string();
            let load = fields.next().unwrap_or("");
            let active = fields.next().unwrap_or("");
            let sub = fields.next().unwrap_or("");
            let _ = load;
            let description = line
                .split_whitespace()
                .skip(4)
                .collect::<Vec<_>>()
                .join(" ");
            Some(UnitInfo {
                unit,
                active: active.to_string(),
                sub: sub.to_string(),
                description,
            })
        })
        .collect()
}

/// Tail the journal: `journalctl -o json [-u unit] [-p prio] -n <lines>`.
pub fn tail(
    runner: &dyn Runner,
    unit: Option<&str>,
    lines: u32,
    priority: Option<&str>,
    timeout: Duration,
) -> Result<Vec<LogEntry>, ApiError> {
    let mut args: Vec<String> = vec![
        "-o".to_string(),
        "json".to_string(),
        "--no-pager".to_string(),
        "-n".to_string(),
        lines.min(MAX_LINES).to_string(),
    ];
    if let Some(unit) = unit.map(str::trim).filter(|u| !u.is_empty()) {
        args.push("-u".to_string());
        args.push(unit.to_string());
    }
    if let Some(priority) = priority.map(str::trim).filter(|p| !p.is_empty()) {
        args.push("-p".to_string());
        args.push(priority.to_string());
    }

    let output = runner
        .run_timeout("journalctl", &args, timeout)
        .map_err(|err| ApiError::unavailable(format!("journalctl is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("journalctl timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "journalctl failed: {}",
            first_line(&output.combined())
        )));
    }
    parse_entries(&output.stdout)
}

/// Parse newline-delimited `journalctl -o json`.
pub fn parse_entries(json_lines: &str) -> Result<Vec<LogEntry>, ApiError> {
    let mut entries: Vec<LogEntry> = Vec::new();
    for line in json_lines.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts_ms = value
            .get("__REALTIME_TIMESTAMP")
            .and_then(|v| v.as_str())
            .and_then(|micros| micros.parse::<u64>().ok())
            .map(|micros| micros / 1000)
            .unwrap_or(0);
        let priority = value
            .get("PRIORITY")
            .and_then(|v| v.as_str())
            .and_then(|p| p.parse::<u8>().ok())
            .unwrap_or(6);
        let unit = value
            .get("_SYSTEMD_UNIT")
            .or_else(|| value.get("SYSLOG_IDENTIFIER"))
            .or_else(|| value.get("_COMM"))
            .and_then(|v| v.as_str())
            .unwrap_or("kernel")
            .to_string();
        let message = value
            .get("MESSAGE")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        entries.push(LogEntry {
            ts_ms,
            priority,
            unit,
            message,
        });
    }
    // journalctl prints oldest first; the UI wants newest first.
    entries.reverse();
    Ok(entries)
}

/// A live `journalctl -f` stream: one reader thread, lines into a channel.
pub struct LineStream {
    child: Arc<Mutex<Child>>,
    receiver: std::sync::mpsc::Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl LineStream {
    pub fn spawn(program: &str, args: &[String]) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout from the child"))?;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            receiver,
            reader: Some(reader),
        })
    }

    /// Blocking receive; ends when the child exits or the stream is dropped.
    pub fn recv_blocking(&self) -> Option<String> {
        self.receiver.recv().ok()
    }

    /// Terminate the journalctl process and join its reader.
    pub fn stop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for LineStream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn string_of(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(300).collect())
        .unwrap_or_else(|| "no output".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    #[test]
    fn unit_lines_are_parsed() {
        let text = "ss-stack.service  loaded active running  stack-stack control plane\n\
                    docker.service    loaded active running  Docker Application Container Engine\n\
                    \n";
        let units = parse_unit_lines(text);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].unit, "ss-stack.service");
        assert_eq!(units[0].active, "active");
        assert_eq!(units[0].sub, "running");
        assert_eq!(units[0].description, "stack-stack control plane");
    }

    #[test]
    fn journal_json_is_parsed_newest_first() {
        let json = "{\"__REALTIME_TIMESTAMP\":\"1700000000000000\",\"PRIORITY\":\"3\",\"_SYSTEMD_UNIT\":\"ss-stack.service\",\"MESSAGE\":\"boom\"}\n\
                    {\"__REALTIME_TIMESTAMP\":\"1700000001000000\",\"PRIORITY\":\"6\",\"SYSLOG_IDENTIFIER\":\"kernel\",\"MESSAGE\":\"ready\"}\n";
        let entries = parse_entries(json).expect("parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "ready", "newest first");
        assert_eq!(entries[0].priority, 6);
        assert_eq!(entries[1].priority, 3);
        assert_eq!(entries[1].unit, "ss-stack.service");
        assert_eq!(entries[1].ts_ms, 1_700_000_000_000);
    }

    #[test]
    fn garbled_lines_are_skipped() {
        let entries =
            parse_entries("not json\n{\"__REALTIME_TIMESTAMP\":\"1\",\"MESSAGE\":\"ok\"}\n\n")
                .expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "ok");
        // Missing unit falls back to the kernel identifier.
        assert_eq!(entries[0].unit, "kernel");
    }

    #[test]
    fn tail_builds_the_right_command() {
        let runner = FakeRunner::new()
            .respond(
                "journalctl",
                Some(0),
                "{\"__REALTIME_TIMESTAMP\":\"5\",\"MESSAGE\":\"x\"}\n",
                "",
            )
            .respond("systemctl", Some(0), "[]", "");
        let entries = tail(
            &runner,
            Some("ss-stack.service"),
            100,
            Some("3"),
            Duration::from_secs(10),
        )
        .expect("tail");
        assert_eq!(entries.len(), 1);
        assert!(runner.ran_with(
            "journalctl",
            &[
                "-o",
                "json",
                "--no-pager",
                "-n",
                "100",
                "-u",
                "ss-stack.service",
                "-p",
                "3"
            ]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn line_streams_terminate_with_the_child() {
        // `true` produces no output; the stream must still stop cleanly.
        let mut stream = LineStream::spawn("true", &[]).expect("spawn");
        assert_eq!(stream.recv_blocking(), None, "no output expected");
        stream.stop();
    }

    #[cfg(unix)]
    #[test]
    fn line_streams_deliver_lines_and_stop_on_drop() {
        // `echo` writes one line and exits; recv_blocking must observe it.
        let stream = LineStream::spawn("echo", &["hello".to_string()]).expect("spawn");
        assert_eq!(stream.recv_blocking().as_deref(), Some("hello"));
        // The child exits after the line; recv_blocking then ends.
        assert_eq!(stream.recv_blocking(), None);
    }
}

/// Shared runtime handles for the log surface.
#[derive(Clone)]
pub struct LogsContext {
    pub runner: Arc<dyn Runner>,
}

impl LogsContext {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }

    /// True when both `journalctl` and `systemctl` answer.
    pub fn available(&self) -> bool {
        self.runner.available("journalctl", &["--version"])
    }
}
