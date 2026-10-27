//! SMART self-test scheduling.
//!
//! Phase 1 reads SMART health every interval; this module adds the one thing a
//! homelab should also do on a timer: run the vendor self-tests and remember
//! what they said. `smartctl -t short|long` starts a test (the disk does the
//! work in the background), and progress plus the result log are parsed from
//! the same text output the tool has printed for decades.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;

/// How many test results are remembered.
pub const HISTORY_CAP: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestKind {
    Short,
    Long,
}

impl TestKind {
    pub fn flag(self) -> &'static str {
        match self {
            TestKind::Short => "short",
            TestKind::Long => "long",
        }
    }
}

/// A device's schedule. `weekday` is 0 (Sunday) to 6 (Saturday); `day` is
/// 1-31 for the monthly long test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevicePlan {
    pub device: String,
    /// `smartctl -d` type for a disk that needs one (e.g. `sat` for an ATA disk
    /// behind a SAT bridge). Detected on first use and remembered here.
    #[serde(default)]
    pub device_type: Option<String>,
    pub short_weekday: Option<u8>,
    pub short_at: String,
    pub long_day: Option<u8>,
    pub long_at: String,
}

impl Default for DevicePlan {
    fn default() -> Self {
        Self {
            device: String::new(),
            device_type: None,
            short_weekday: Some(0),
            short_at: "02:30".to_string(),
            long_day: Some(1),
            long_at: "03:30".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartConfig {
    pub devices: Vec<DevicePlan>,
}

impl SmartConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for (index, plan) in self.devices.iter().enumerate() {
            let field = format!("devices[{index}]");
            let device = plan.device.trim();
            if !device.starts_with('/') {
                problems.push(Problem {
                    field: format!("{field}.device"),
                    message: "device must be an absolute path (e.g. /dev/sda)".to_string(),
                });
                continue;
            }
            if seen.contains(&device) {
                problems.push(Problem {
                    field: format!("{field}.device"),
                    message: format!("{device:?} is scheduled twice"),
                });
            }
            seen.push(device);
            if let Some(kind) = plan.device_type.as_deref()
                && !valid_device_type(kind)
            {
                problems.push(Problem {
                    field: format!("{field}.device_type"),
                    message: "device type may only contain letters, digits and + , - _".to_string(),
                });
            }
            if let Some(weekday) = plan.short_weekday
                && weekday > 6
            {
                problems.push(Problem {
                    field: format!("{field}.short_weekday"),
                    message: "weekday must be 0 (Sunday) to 6 (Saturday)".to_string(),
                });
            }
            if plan.short_weekday.is_some()
                && crate::services::backup::parse_hhmm(&plan.short_at).is_none()
            {
                problems.push(Problem {
                    field: format!("{field}.short_at"),
                    message: "time must be HH:MM (24h)".to_string(),
                });
            }
            if let Some(day) = plan.long_day
                && !(1..=28).contains(&day)
            {
                problems.push(Problem {
                    field: format!("{field}.long_day"),
                    message: "day must be 1-28 so every month has it".to_string(),
                });
            }
            if plan.long_day.is_some()
                && crate::services::backup::parse_hhmm(&plan.long_at).is_none()
            {
                problems.push(Problem {
                    field: format!("{field}.long_at"),
                    message: "time must be HH:MM (24h)".to_string(),
                });
            }
        }
        problems
    }

    pub fn plan_for(&self, device: &str) -> Option<&DevicePlan> {
        self.devices
            .iter()
            .find(|plan| plan.device.eq_ignore_ascii_case(device))
    }
}

pub fn load(path: &std::path::Path) -> (SmartConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<SmartConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                SmartConfig::default(),
                Some(format!("{} is not valid smart JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (SmartConfig::default(), None),
        Err(err) => (
            SmartConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &std::path::Path, config: &SmartConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise smart config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

/// Device paths from `smartctl --scan-open` output. Each line looks like
/// `/dev/sda -d scsi # /dev/sda, SCSI device`; only the first field matters, and
/// comments or blank lines carry nothing.
pub fn parse_scan(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let first = line.split_whitespace().next()?;
            first.starts_with("/dev/").then(|| first.to_string())
        })
        .collect()
}

/// A `-d` device type is a short token that goes straight into argv, so it is
/// restricted to the characters smartctl itself uses (`sat`, `ata`, `nvme`,
/// `sat,12`, `scsi`, ...).
pub fn valid_device_type(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | ',' | '-' | '_'))
}

/// True when smartctl is telling us the disk needs an explicit type argument,
/// e.g. "Probable ATA device behind a SAT layer. Try an additional '-d ata'".
pub fn is_sat_hint(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("behind a sat layer") || text.contains("try an additional '-d")
}

/// True when smartctl refused to start a test because one is already running.
pub fn is_running_hint(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("without aborting current test") || text.contains("in progress")
}

/// `[device]`, with a leading `-d <type>` when the disk needs one.
fn device_args(device: &str, device_type: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(kind) = device_type.map(str::trim).filter(|kind| !kind.is_empty()) {
        args.push("-d".to_string());
        args.push(kind.to_string());
    }
    args.push(device.to_string());
    args
}

/// Ask smartctl what kind of disk this is. Today it only answers the SAT case:
/// a disk behind a bridge that needs `-d sat` to be read at all.
pub fn probe_device_type(runner: &dyn Runner, device: &str, timeout: Duration) -> Option<String> {
    let output = runner
        .run_timeout("smartctl", &["-i".to_string(), device.to_string()], timeout)
        .ok()?;
    is_sat_hint(&output.combined()).then(|| "sat".to_string())
}

/// One remembered test result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestRecord {
    pub device: String,
    pub kind: TestKind,
    pub started_ms: u64,
    /// True when `smartctl` accepted the request; the disk reports its own
    /// verdict later, surfaced through `progress`.
    pub accepted: bool,
    pub detail: String,
}

#[derive(Clone)]
pub struct SmartContext {
    pub config_path: PathBuf,
    pub runner: Arc<dyn Runner>,
    pub timeout: Duration,
    lock: Arc<Mutex<()>>,
    history: Arc<Mutex<VecDeque<TestRecord>>>,
}

impl SmartContext {
    pub fn new(config_path: PathBuf, runner: Arc<dyn Runner>, timeout: Duration) -> Self {
        Self {
            config_path,
            runner,
            timeout,
            lock: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub async fn history(&self) -> Vec<TestRecord> {
        self.history.lock().await.iter().rev().cloned().collect()
    }

    pub async fn busy(&self) -> bool {
        self.lock.try_lock().is_err()
    }

    pub fn config(&self) -> SmartConfig {
        load(&self.config_path).0
    }

    /// True when `smartctl` is installed.
    pub fn available(&self) -> bool {
        self.runner.available("smartctl", &["--version"])
    }

    /// Every device `smartctl` can talk to, so a fresh install can schedule
    /// self-tests for all of them without the operator naming each disk.
    pub fn detect_devices(&self) -> Vec<String> {
        let output = match self.runner.call("smartctl", &["--scan-open"]) {
            Ok(output) if output.success() => output.stdout,
            _ => return Vec::new(),
        };
        parse_scan(&output)
    }

    /// Add every detected disk to the schedule when the document has no entries
    /// yet. Returns the devices that were added. The operator's own document is
    /// never rewritten: once a device list exists, discovery is skipped so an
    /// opt-out is not undone by the next boot.
    pub fn ensure_default_schedule(&self) -> Vec<DevicePlan> {
        let (config, _) = load(&self.config_path);
        if !config.devices.is_empty() {
            return Vec::new();
        }
        let devices = self.detect_devices();
        if devices.is_empty() {
            return Vec::new();
        }
        let plans: Vec<DevicePlan> = devices
            .into_iter()
            .map(|device| DevicePlan {
                device,
                ..DevicePlan::default()
            })
            .collect();
        let next = SmartConfig {
            devices: plans.clone(),
        };
        if next.validate().is_empty() {
            let _ = save(&self.config_path, &next);
        }
        plans
    }

    /// Start a self-test and remember the outcome.
    ///
    /// A disk behind a SAT bridge needs `-d sat`; the first attempt discovers
    /// that, records it in the schedule (so the next call reuses it), and retries.
    pub async fn run_test(&self, device: &str, kind: TestKind) -> TestRecord {
        let started_ms = now_ms();
        let _guard = self.lock.lock().await;

        let mut config = self.config();
        let device_type = config
            .plan_for(device)
            .and_then(|plan| plan.device_type.clone());

        let mut outcome = {
            let runner = Arc::clone(&self.runner);
            let timeout = self.timeout;
            let device_owned = device.to_string();
            let dtype = device_type.clone();
            tokio::task::spawn_blocking(move || {
                start_test(
                    runner.as_ref(),
                    &device_owned,
                    kind,
                    dtype.as_deref(),
                    timeout,
                )
            })
            .await
            .unwrap_or_else(|err| Err(format!("smart task failed: {err}")))
        };

        if let Err(err) = &outcome
            && device_type.is_none()
            && is_sat_hint(err)
        {
            let probed = {
                let runner = Arc::clone(&self.runner);
                let timeout = self.timeout;
                let device_owned = device.to_string();
                tokio::task::spawn_blocking(move || {
                    probe_device_type(runner.as_ref(), &device_owned, timeout)
                })
                .await
                .ok()
                .flatten()
            };
            if let Some(flag) = probed {
                // Persist so the scheduler and every later call reuse it.
                if let Some(plan) = config
                    .devices
                    .iter_mut()
                    .find(|plan| plan.device.eq_ignore_ascii_case(device))
                {
                    plan.device_type = Some(flag.clone());
                    let _ = save(&self.config_path, &config);
                }
                outcome = {
                    let runner = Arc::clone(&self.runner);
                    let timeout = self.timeout;
                    let device_owned = device.to_string();
                    tokio::task::spawn_blocking(move || {
                        start_test(runner.as_ref(), &device_owned, kind, Some(&flag), timeout)
                    })
                    .await
                    .unwrap_or_else(|err| Err(format!("smart task failed: {err}")))
                };
            }
        }

        let record = match outcome {
            Ok(detail) => TestRecord {
                device: device.to_string(),
                kind,
                started_ms,
                accepted: true,
                detail,
            },
            Err(detail) => TestRecord {
                device: device.to_string(),
                kind,
                started_ms,
                accepted: false,
                detail,
            },
        };
        let mut history = self.history.lock().await;
        history.push_back(record.clone());
        while history.len() > HISTORY_CAP {
            history.pop_front();
        }
        record
    }

    /// Live progress for one device.
    pub async fn progress(&self, device: &str) -> Result<TestProgress, ApiError> {
        let device_type = self
            .config()
            .plan_for(device)
            .and_then(|plan| plan.device_type.clone());
        let runner = Arc::clone(&self.runner);
        let device = device.to_string();
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || {
            read_progress(runner.as_ref(), &device, device_type.as_deref(), timeout)
        })
        .await
        .map_err(|err| ApiError::internal(format!("smart task failed: {err}")))?
    }

    /// Abort a running self-test (`smartctl -X`).
    pub async fn abort(&self, device: &str) -> Result<String, ApiError> {
        let device_type = self
            .config()
            .plan_for(device)
            .and_then(|plan| plan.device_type.clone());
        let runner = Arc::clone(&self.runner);
        let device = device.to_string();
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || {
            abort_test(runner.as_ref(), &device, device_type.as_deref(), timeout)
        })
        .await
        .map_err(|err| ApiError::internal(format!("smart task failed: {err}")))?
    }
}

/// Start a self-test on a disk.
pub fn start_test(
    runner: &dyn Runner,
    device: &str,
    kind: TestKind,
    device_type: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let mut args = vec!["-t".to_string(), kind.flag().to_string()];
    args.extend(device_args(device, device_type));
    let output = runner
        .run_timeout("smartctl", &args, timeout)
        .map_err(|err| format!("smartctl is not available: {err}"))?;
    if output.timed_out {
        return Err("smartctl timed out".to_string());
    }
    if !output.success() {
        return Err(format!(
            "smartctl -t {} failed: {}",
            kind.flag(),
            trimmed(&output.combined())
        ));
    }
    Ok(trimmed(&output.stdout))
}

/// Abort a running self-test (`smartctl -X`).
pub fn abort_test(
    runner: &dyn Runner,
    device: &str,
    device_type: Option<&str>,
    timeout: Duration,
) -> Result<String, ApiError> {
    let mut args = vec!["-X".to_string()];
    args.extend(device_args(device, device_type));
    let output = runner
        .run_timeout("smartctl", &args, timeout)
        .map_err(|err| ApiError::unavailable(format!("smartctl is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("smartctl timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "smartctl -X failed: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(trimmed(&output.stdout))
}

/// Live self-test state parsed from `smartctl -a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestProgress {
    pub running: bool,
    /// `short`, `long`, or the raw description, when a test is in flight.
    pub running_kind: Option<String>,
    /// Percent of the test still to run.
    pub percent_remaining: Option<u32>,
    /// Percent of the test already done (100 - remaining).
    pub percent_complete: Option<u32>,
    pub detail: String,
    /// The most recent completed entry from the self-test log.
    pub last_result: Option<String>,
    pub last_percent: Option<String>,
}

impl TestProgress {
    /// An idle result with no log, used when a device is unknown or unreadable.
    pub fn idle(detail: impl Into<String>) -> Self {
        Self {
            running: false,
            running_kind: None,
            percent_remaining: None,
            percent_complete: None,
            detail: detail.into(),
            last_result: None,
            last_percent: None,
        }
    }
}

pub fn read_progress(
    runner: &dyn Runner,
    device: &str,
    device_type: Option<&str>,
    timeout: Duration,
) -> Result<TestProgress, ApiError> {
    // `-a` carries both the execution-status line and the self-test log (which
    // is where the *type* of a running test lives), so one call answers
    // everything the panel needs.
    let mut args = vec!["-a".to_string()];
    args.extend(device_args(device, device_type));
    let output = runner
        .run_timeout("smartctl", &args, timeout)
        .map_err(|err| ApiError::unavailable(format!("smartctl is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("smartctl timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "smartctl -a failed: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(parse_progress(&output.stdout))
}

/// Parse self-test state from `smartctl -a` text output.
///
/// The running test's *type* is not in the execution-status line, so the
/// self-test log's in-progress row is the source of truth for it; the
/// execution-status line is the fallback (and the only signal in `-c` output,
/// where the percentage is on the line after it).
pub fn parse_progress(text: &str) -> TestProgress {
    let lines: Vec<&str> = text.lines().collect();
    let mut running = false;
    let mut running_kind: Option<String> = None;
    let mut remaining: Option<u32> = None;
    let mut last_result: Option<String> = None;
    let mut last_percent: Option<String> = None;

    let mut in_log = false;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();

        if let Some(rest) = line.strip_prefix("Self-test execution status") {
            let tail = rest
                .split_once(')')
                .map(|(_, tail)| tail.trim())
                .unwrap_or("")
                .to_ascii_lowercase();
            let looks_running =
                tail.contains("% of test remaining") || tail.contains("in progress");
            if looks_running {
                running = true;
                // The parenthesised number is only a percentage when it is
                // immediately followed by `%` — otherwise it is the status code
                // ("( 245) Self-test routine in progress"), and the real
                // percentage is on the next line in `-c` output.
                remaining = paren_percent(rest).or_else(|| {
                    lines
                        .get(index + 1)
                        .and_then(|next| percent_token(next.trim()))
                });
            }
            continue;
        }

        if line.starts_with("Num  Test_Description") || line.starts_with("Num Test_Description") {
            in_log = true;
            continue;
        }
        if !in_log {
            continue;
        }
        if line.is_empty() {
            in_log = false;
            continue;
        }
        if !line.starts_with('#') {
            continue;
        }

        let segments: Vec<&str> = line
            .trim_start_matches('#')
            .split("  ")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect();
        if segments.len() < 3 {
            continue;
        }
        let description = segments[1];
        let status = segments[2];

        if status.to_ascii_lowercase().contains("in progress") {
            running = true;
            // The remaining percent may sit inside the status segment
            // ("Self-test routine in progress 10%").
            if remaining.is_none() {
                remaining = status.split_whitespace().rev().find_map(percent_token);
            }
            if running_kind.is_none() {
                running_kind = Some(kind_word(description));
            }
        } else if last_result.is_none() {
            // The percentage is its own column ("00%"), so scan the whole line.
            let percent = line
                .split_whitespace()
                .find(|token| token.ends_with('%'))
                .map(str::to_string);
            last_result = Some(format!("{} — {}", kind_word(description), status));
            last_percent = percent;
        }
    }

    if last_result.is_none() {
        last_result = Some("no completed tests logged".to_string());
    }

    let percent_complete = remaining.map(|percent| 100u32.saturating_sub(percent));
    let detail = if !running {
        "idle".to_string()
    } else {
        match (running_kind.as_deref(), percent_complete) {
            (Some(kind), Some(done)) => format!("Self-test in progress: {kind}, {done}% complete"),
            (Some(kind), None) => format!("Self-test in progress: {kind}"),
            (None, Some(done)) => format!("Self-test in progress: {done}% complete"),
            (None, None) => "Self-test in progress".to_string(),
        }
    };

    TestProgress {
        running,
        running_kind,
        percent_remaining: remaining,
        percent_complete,
        detail,
        last_result,
        last_percent,
    }
}

/// Map a self-test log description to the short/long wording the API uses.
fn kind_word(description: &str) -> String {
    let lower = description.to_ascii_lowercase();
    if lower.contains("extended") || lower.contains("long") {
        "long".to_string()
    } else if lower.contains("short") {
        "short".to_string()
    } else {
        lower
    }
}

/// A whitespace token that is a percentage, e.g. `10%` → 10.
fn percent_token(token: &str) -> Option<u32> {
    token.trim().strip_suffix('%')?.trim().parse::<u32>().ok()
}

/// The number inside the first `( ... )`, but only when it is followed by `%`.
///
/// This distinguishes the percentage form `(  90)% of test remaining` from the
/// status code form `( 245) Self-test routine in progress`, where the number in
/// the parentheses is not a percentage at all.
fn paren_percent(line: &str) -> Option<u32> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    if !line[close + 1..].trim_start().starts_with('%') {
        return None;
    }
    line[open + 1..close].trim().parse::<u32>().ok()
}

/// Next due time (unix seconds) for a planned test, at or after `from`.
pub fn next_due(plan: &DevicePlan, kind: TestKind, from: u64) -> Option<u64> {
    const DAY: u64 = 86_400;
    // 1970-01-01 was a Thursday (weekday 4 with Sunday = 0).
    let weekday_of = |days: u64| ((days + 4) % 7) as u8;

    let (time, constraint): (u64, Box<dyn Fn(u64) -> bool>) = match kind {
        TestKind::Short => {
            let weekday = plan.short_weekday?;
            let (_, second) = parse_time(&plan.short_at)?;
            (
                second,
                Box::new(move |days: u64| weekday_of(days) == weekday % 7),
            )
        }
        TestKind::Long => {
            let day = u64::from(plan.long_day?);
            let (_, second) = parse_time(&plan.long_at)?;
            (
                second,
                Box::new(move |days: u64| day_of_month(days) == day as u8),
            )
        }
    };

    let mut days = from / DAY;
    let mut guard = 0;
    loop {
        let candidate = days * DAY + time;
        if candidate > from && constraint(days) {
            return Some(candidate);
        }
        days += 1;
        guard += 1;
        if guard > 366 * 2 {
            return None;
        }
    }
}

/// Civil day-of-month for a unix day index, assuming UTC.
fn day_of_month(days: u64) -> u8 {
    // Days since 1970-01-01; civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    day.clamp(1, 31) as u8
}

fn parse_time(value: &str) -> Option<(u32, u64)> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, u64::from(hour * 3600 + minute * 60)))
}

fn trimmed(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "no output".to_string()
    } else if text.len() > 300 {
        text[..300].to_string()
    } else {
        text.to_string()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    const SAMPLE: &str = "=== START OF OFFLINE IMMEDIATE AND SELF-TEST SECTION ===\n\
Self-test execution status: (   0) The previous self-test routine completed without error or abort.\n\
Num  Test_Description    Status                  LifeTime  LBA_of_first_err\n\
# 1  Short offline       Completed without error       00%        0         -\n\
# 2  Extended offline    Completed without error       00%      432         -\n";

    #[test]
    fn default_config_validates_clean() {
        assert!(SmartConfig::default().validate().is_empty());
    }

    #[test]
    fn schedules_are_validated() {
        let config = SmartConfig {
            devices: vec![
                DevicePlan {
                    device: "/dev/sda".to_string(),
                    short_weekday: Some(9),
                    ..DevicePlan::default()
                },
                DevicePlan {
                    device: "/dev/sda".to_string(),
                    short_at: "99:99".to_string(),
                    long_day: Some(31),
                    ..DevicePlan::default()
                },
                DevicePlan {
                    device: "sdb".to_string(),
                    ..DevicePlan::default()
                },
            ],
        };
        let fields: Vec<String> = config.validate().iter().map(|p| p.field.clone()).collect();
        for expected in [
            "devices[0].short_weekday",
            "devices[1].short_at",
            "devices[1].long_day",
            "devices[2].device",
        ] {
            assert!(fields.iter().any(|field| field == expected), "{fields:?}");
        }
    }

    #[test]
    fn progress_is_parsed_from_smartctl_text() {
        let progress = parse_progress(SAMPLE);
        assert!(!progress.running);
        assert_eq!(progress.detail, "idle");
        assert_eq!(progress.percent_remaining, None);
        assert_eq!(
            progress.last_result.as_deref(),
            Some("short — Completed without error")
        );
        assert_eq!(progress.last_percent.as_deref(), Some("00%"));
    }

    #[test]
    fn a_running_test_is_reported_with_type_and_progress() {
        // A full `smartctl -a` while a long test runs: the type comes from the
        // self-test log, the percentage from the status column.
        let running = "Self-test execution status:      ( 245) Self-test routine in progress...\n\
                                        10% of test remaining.\n\
Num  Test_Description    Status                  Remaining  LifeTime(hours)\n\
# 1  Extended offline    Self-test routine in progress 10%        0   -\n\
# 2  Short offline       Completed without error       00%        0   -\n";
        let progress = parse_progress(running);
        assert!(progress.running);
        assert_eq!(progress.running_kind.as_deref(), Some("long"));
        assert_eq!(progress.percent_remaining, Some(10));
        assert_eq!(progress.percent_complete, Some(90));
        assert_eq!(progress.detail, "Self-test in progress: long, 90% complete");
        // The newest *completed* entry is still reported.
        assert_eq!(
            progress.last_result.as_deref(),
            Some("short — Completed without error")
        );
    }

    #[test]
    fn execution_status_alone_is_enough() {
        let running = "Self-test execution status: (  90)% of test remaining.\n";
        let progress = parse_progress(running);
        assert!(progress.running);
        assert_eq!(progress.percent_remaining, Some(90));
        assert_eq!(progress.percent_complete, Some(10));
        assert!(
            progress.detail.contains("10% complete"),
            "{}",
            progress.detail
        );
    }

    #[test]
    fn sat_hint_is_detected() {
        assert!(is_sat_hint(
            "Probable ATA device behind a SAT layer. Try an additional '-d ata' or '-d sat'"
        ));
        assert!(!is_sat_hint(
            "SMART overall-health self-assessment test result: PASSED"
        ));
        assert!(valid_device_type("sat"));
        assert!(valid_device_type("sat,12"));
        assert!(!valid_device_type("sat;rm -rf /"));
    }

    #[test]
    fn starting_a_test_requires_smartctl() {
        let runner = FakeRunner::new().missing("smartctl");
        assert!(
            start_test(
                &runner,
                "/dev/sda",
                TestKind::Short,
                None,
                Duration::from_secs(10)
            )
            .is_err()
        );
    }

    #[test]
    fn starting_a_test_returns_the_output() {
        let runner = FakeRunner::new().respond("smartctl", Some(0), "Testing has begun", "");
        let detail = start_test(
            &runner,
            "/dev/sda",
            TestKind::Long,
            None,
            Duration::from_secs(10),
        )
        .expect("started");
        assert_eq!(detail, "Testing has begun");
        assert!(runner.ran_with("smartctl", &["-t", "long", "/dev/sda"]));
    }

    #[test]
    fn a_remembered_device_type_is_passed_through() {
        let runner = FakeRunner::new().respond("smartctl", Some(0), "Testing has begun", "");
        start_test(
            &runner,
            "/dev/sda",
            TestKind::Short,
            Some("sat"),
            Duration::from_secs(10),
        )
        .expect("started");
        assert!(runner.ran_with("smartctl", &["-t", "short", "-d", "sat", "/dev/sda"]));
    }

    #[test]
    fn the_device_type_is_probed_and_persisted() {
        let dir = std::env::temp_dir().join(format!("ss-smart-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config_path = dir.join("smart.json");
        let config = SmartConfig {
            devices: vec![DevicePlan {
                device: "/dev/sda".to_string(),
                ..DevicePlan::default()
            }],
        };
        save(&config_path, &config).expect("save");

        // The first `-t` fails with the SAT hint, `-i` confirms it, the retry
        // succeeds — and the type is written back into the schedule.
        // The retry (which carries `-d sat`) succeeds and is matched first; the
        // `-i` probe answers with the SAT hint; everything else fails with it.
        let runner = Arc::new(
            FakeRunner::new()
                .respond_when("smartctl", "-d sat", Some(0), "Testing has begun", "")
                .respond_when(
                    "smartctl",
                    "-i",
                    Some(1),
                    "Probable ATA device behind a SAT layer. Try an additional '-d ata' or '-d sat'",
                    "",
                )
                .respond(
                    "smartctl",
                    Some(1),
                    "",
                    "Probable ATA device behind a SAT layer. Try an additional '-d ata' or '-d sat'",
                ),
        );
        let context = SmartContext::new(config_path.clone(), runner, Duration::from_secs(10));
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let record = runtime.block_on(context.run_test("/dev/sda", TestKind::Short));
        assert!(record.accepted, "{}", record.detail);
        let (saved, _) = load(&config_path);
        assert_eq!(
            saved
                .plan_for("/dev/sda")
                .and_then(|plan| plan.device_type.clone()),
            Some("sat".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_is_capped_and_newest_first() {
        let dir = std::env::temp_dir().join(format!("ss-smart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runner = Arc::new(FakeRunner::new().respond("smartctl", Some(0), "ok", ""));
        let context = SmartContext::new(dir.join("smart.json"), runner, Duration::from_secs(10));
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        runtime.block_on(async {
            for _ in 0..(HISTORY_CAP + 3) {
                context.run_test("/dev/sda", TestKind::Short).await;
            }
            let history = context.history().await;
            assert_eq!(history.len(), HISTORY_CAP);
        });
    }

    #[test]
    fn next_due_lands_on_the_planned_slot() {
        // 2026-01-01T00:00:00Z is a Thursday.
        let from = 1_767_225_600;
        let weekly = DevicePlan {
            device: "/dev/sda".to_string(),
            short_weekday: Some(0), // Sunday
            short_at: "02:30".to_string(),
            ..DevicePlan::default()
        };
        let due = next_due(&weekly, TestKind::Short, from).expect("due");
        assert!(due > from);
        assert!((due / 86_400) - (from / 86_400) <= 7);
        assert_eq!(((due / 86_400 + 4) % 7) as u8, 0);
        assert_eq!(due % 86_400, 2 * 3600 + 30 * 60);

        let monthly = DevicePlan {
            device: "/dev/sda".to_string(),
            long_day: Some(1),
            long_at: "03:30".to_string(),
            ..DevicePlan::default()
        };
        let due = next_due(&monthly, TestKind::Long, from).expect("due");
        assert!(due > from);
        assert_eq!(day_of_month(due / 86_400), 1);
        assert_eq!(due % 86_400, 3 * 3600 + 30 * 60);
    }

    #[test]
    fn a_plan_with_no_schedule_is_never_due() {
        let plan = DevicePlan {
            device: "/dev/sda".to_string(),
            short_weekday: None,
            long_day: None,
            ..DevicePlan::default()
        };
        assert_eq!(next_due(&plan, TestKind::Short, 0), None);
        assert_eq!(next_due(&plan, TestKind::Long, 0), None);
    }
}
