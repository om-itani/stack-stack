//! Scheduled tasks: run a shell command on a cron-like schedule.
//!
//! The scheduler already exists for backups, SMART, DDNS, blocklists and image
//! checks; this adds a document-driven version of it. A task's command is run
//! with `sh -c`, so it is a shell one-liner or a script, exactly as the operator
//! would type it, and it runs with the service's privileges.
//!
//! Only a deliberately small cron subset is supported: `*`, `a`, `a-b`, `*/n`,
//! `a-b/n` and comma lists, per field, plus `@hourly`/`@daily`/`@weekly`. That
//! covers scheduling a maintenance command without pulling in a cron crate or
//! pretending to support `@reboot` and name ranges.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::runner::Runner;

/// How long one task may run before it is killed.
pub const TASK_TIMEOUT_S: u64 = 3600;
/// Output kept for the UI, from the end of the run.
const MAX_OUTPUT: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    /// `daily`, `weekly`, an `@alias`, or a five-field cron expression.
    pub schedule: String,
    pub command: String,
    #[serde(default)]
    pub last_run: Option<u64>,
    /// `ok`, `failed` or `running`.
    #[serde(default)]
    pub last_status: Option<String>,
    /// Tail of the last run's combined output.
    #[serde(default)]
    pub last_output: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksDoc {
    #[serde(default)]
    pub tasks: Vec<Task>,
}

impl TasksDoc {
    pub fn find(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }
}

#[derive(Clone)]
pub struct TaskContext {
    path: PathBuf,
    runner: Arc<dyn Runner>,
}

impl TaskContext {
    pub fn new(path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self { path, runner }
    }

    pub fn load(&self) -> (TasksDoc, Option<String>) {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<TasksDoc>(&raw) {
                Ok(doc) => (doc, None),
                Err(err) => (
                    TasksDoc::default(),
                    Some(format!("{} is not valid JSON: {err}", self.path.display())),
                ),
            },
            Err(_) => (TasksDoc::default(), None),
        }
    }

    pub fn save(&self, doc: &TasksDoc) -> Result<(), ApiError> {
        let mut body = serde_json::to_vec_pretty(doc)
            .map_err(|err| ApiError::internal(format!("cannot serialise tasks: {err}")))?;
        body.push(b'\n');
        crate::networking::store::write_atomic(&self.path, &body).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", self.path.display()))
        })
    }

    /// Create or replace a task, validated.
    pub fn upsert(
        &self,
        id: &str,
        name: &str,
        schedule: &str,
        command: &str,
    ) -> Result<TasksDoc, ApiError> {
        let id = sanitize_id(id)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request("a task needs a name"));
        }
        let schedule = schedule.trim();
        validate_schedule(schedule)?;
        let command = command.trim();
        if command.is_empty() {
            return Err(ApiError::bad_request("a task needs a command"));
        }
        if command.len() > 8192 {
            return Err(ApiError::bad_request("command is too long"));
        }

        let (mut doc, _) = self.load();
        match doc.find_mut(&id) {
            Some(existing) => {
                existing.name = name.chars().take(80).collect();
                existing.schedule = schedule.to_string();
                existing.command = command.to_string();
            }
            None => doc.tasks.push(Task {
                id,
                name: name.chars().take(80).collect(),
                schedule: schedule.to_string(),
                command: command.to_string(),
                last_run: None,
                last_status: None,
                last_output: None,
            }),
        }
        doc.tasks.sort_by(|a, b| a.name.cmp(&b.name));
        self.save(&doc)?;
        Ok(doc)
    }

    pub fn remove(&self, id: &str) -> Result<TasksDoc, ApiError> {
        let (mut doc, _) = self.load();
        let before = doc.tasks.len();
        doc.tasks.retain(|task| task.id != id);
        if doc.tasks.len() == before {
            return Err(ApiError::not_found(format!("no task {id}")));
        }
        self.save(&doc)?;
        Ok(doc)
    }

    /// Run one task now and record the outcome. Returns the updated document.
    pub fn run(&self, id: &str) -> Result<(TasksDoc, String), ApiError> {
        let (mut doc, _) = self.load();
        let task = doc
            .find(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("no task {id}")))?;

        let (status, output) = execute(self.runner.as_ref(), &task.command);
        let now = now_secs();
        if let Some(entry) = doc.find_mut(id) {
            entry.last_run = Some(now);
            entry.last_status = Some(status.clone());
            entry.last_output = Some(output.clone());
        }
        self.save(&doc)?;
        Ok((doc, format!("{status}: {output}")))
    }
}

/// Run a command with `sh -c`, capturing combined output.
pub fn execute(runner: &dyn Runner, command: &str) -> (String, String) {
    let args = vec!["-c".to_string(), command.to_string()];
    match runner.run_timeout("sh", &args, Duration::from_secs(TASK_TIMEOUT_S)) {
        Ok(output) => {
            let status = if output.success() {
                "ok"
            } else if output.timed_out {
                "failed (timed out)"
            } else {
                "failed"
            };
            (status.to_string(), tail(&output.combined()))
        }
        Err(err) => ("failed".to_string(), format!("could not start sh: {err}")),
    }
}

/// Task ids become filenames and URL segments, so keep them boring.
pub fn sanitize_id(raw: &str) -> Result<String, ApiError> {
    let id = raw.trim().to_ascii_lowercase();
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(id)
    } else {
        Err(ApiError::bad_request(
            "task id must be 1-64 characters of letters, digits, '-' or '_'",
        ))
    }
}

/// Expand the friendly names into cron expressions.
pub fn normalize_schedule(schedule: &str) -> String {
    match schedule.trim().to_ascii_lowercase().as_str() {
        "hourly" | "@hourly" => "0 * * * *".to_string(),
        "daily" | "@daily" | "midnight" => "0 3 * * *".to_string(),
        "weekly" | "@weekly" => "0 3 * * 0".to_string(),
        other => other.to_string(),
    }
}

pub fn validate_schedule(schedule: &str) -> Result<(), ApiError> {
    let normalized = normalize_schedule(schedule);
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ApiError::bad_request(
            "schedule must be daily, weekly, hourly, or a five-field cron expression",
        ));
    }
    for (field, (min, max)) in fields
        .iter()
        .zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)])
    {
        if !field_is_valid(field, min, max) {
            return Err(ApiError::bad_request(format!(
                "invalid cron field {field:?}"
            )));
        }
    }
    Ok(())
}

/// Local wall-clock components, so a schedule means the operator's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    pub minute: u32,
    pub hour: u32,
    pub day: u32,
    pub month: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

#[cfg(unix)]
pub fn local_now() -> LocalNow {
    // SAFETY: valid pointers; `tm` is zeroed and only read after a non-null
    // return from localtime_r.
    unsafe {
        let epoch = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&epoch, &mut tm).is_null() {
            return LocalNow {
                minute: 0,
                hour: 0,
                day: 1,
                month: 1,
                weekday: 0,
            };
        }
        LocalNow {
            minute: tm.tm_min.max(0) as u32,
            hour: tm.tm_hour.max(0) as u32,
            day: tm.tm_mday.max(1) as u32,
            month: (tm.tm_mon + 1).max(1) as u32,
            weekday: tm.tm_wday.max(0) as u32,
        }
    }
}

#[cfg(not(unix))]
pub fn local_now() -> LocalNow {
    LocalNow {
        minute: 0,
        hour: 0,
        day: 1,
        month: 1,
        weekday: 0,
    }
}

/// Does the expression fire at this minute?
pub fn matches(expr: &str, now: LocalNow) -> bool {
    let normalized = normalize_schedule(expr);
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let checks = [
        (fields[0], now.minute, 0, 59),
        (fields[1], now.hour, 0, 23),
        (fields[2], now.day, 1, 31),
        (fields[3], now.month, 1, 12),
        // Cron accepts both 0 and 7 for Sunday.
        (fields[4], now.weekday, 0, 7),
    ];
    // Cron accepts both 0 and 7 for Sunday, so a weekday value of 0 must also be
    // tested against a literal 7.
    let weekday_ok = field_matches(fields[4], now.weekday, 0, 7)
        || (now.weekday == 0 && field_matches(fields[4], 7, 0, 7));

    checks[..4]
        .iter()
        .all(|(field, value, min, max)| field_matches(field, *value, *min, *max))
        && weekday_ok
}

/// One cron field matches one value.
pub fn field_matches(field: &str, value: u32, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    field.split(',').any(|part| {
        let part = part.trim();
        if part.is_empty() {
            return false;
        }
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => match step.trim().parse::<u32>() {
                Ok(step) if step > 0 => (range.trim(), step),
                _ => return false,
            },
            None => (part, 1),
        };
        let (low, high) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                (Ok(a), Ok(b)) if a <= b => (a, b),
                _ => return false,
            }
        } else {
            match range.parse::<u32>() {
                Ok(single) => (single, single),
                Err(_) => return false,
            }
        };
        value >= low && value <= high && (value - low).is_multiple_of(step)
    })
}

fn field_is_valid(field: &str, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    field.split(',').all(|part| {
        let part = part.trim();
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range.trim(), step.trim().parse::<u32>().ok()),
            None => (part, Some(1)),
        };
        let Some(step) = step else { return false };
        if step == 0 {
            return false;
        }
        if range == "*" {
            return true;
        }
        match range.split_once('-') {
            Some((a, b)) => match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                (Ok(a), Ok(b)) => a >= min && b <= max && a <= b,
                _ => false,
            },
            None => match range.parse::<u32>() {
                // 7 is a valid Sunday alongside 0.
                Ok(single) => single >= min && single <= max,
                Err(_) => false,
            },
        }
    })
}

/// Tasks that should run on this tick: the schedule matches and the last run was
/// not in the same minute.
pub fn due_ids(doc: &TasksDoc, now_epoch: u64, now: LocalNow) -> Vec<String> {
    doc.tasks
        .iter()
        .filter(|task| {
            if !matches(&task.schedule, now) {
                return false;
            }
            match task.last_run {
                Some(last) => now_epoch.saturating_sub(last) >= 60,
                None => true,
            }
        })
        .map(|task| task.id.clone())
        .collect()
}

fn tail(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= MAX_OUTPUT {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_OUTPUT;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32, hour: u32, day: u32, month: u32, weekday: u32) -> LocalNow {
        LocalNow {
            minute,
            hour,
            day,
            month,
            weekday,
        }
    }

    #[test]
    fn friendly_names_expand() {
        assert_eq!(normalize_schedule("daily"), "0 3 * * *");
        assert_eq!(normalize_schedule("@weekly"), "0 3 * * 0");
        assert_eq!(normalize_schedule("0 4 * * *"), "0 4 * * *");
    }

    #[test]
    fn validation_accepts_the_supported_subset() {
        for ok in [
            "daily",
            "0 3 * * *",
            "*/15 * * * *",
            "0 9-17 * * 1-5",
            "0 0 1,15 * *",
        ] {
            assert!(validate_schedule(ok).is_ok(), "{ok:?} should be accepted");
        }
        for bad in [
            "",
            "* * * *",
            "0 3 * * * *",
            "61 * * * *",
            "0 25 * * *",
            "a b c d e",
        ] {
            assert!(
                validate_schedule(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn matching_covers_steps_ranges_and_lists() {
        // Every 15 minutes.
        assert!(matches("*/15 * * * *", at(30, 5, 1, 1, 1)));
        assert!(!matches("*/15 * * * *", at(31, 5, 1, 1, 1)));
        // Weekday range.
        assert!(matches("0 9-17 * * 1-5", at(0, 12, 3, 6, 3)));
        assert!(!matches("0 9-17 * * 1-5", at(0, 12, 3, 6, 0)));
        // Comma list.
        assert!(matches("0 0 1,15 * *", at(0, 0, 15, 2, 2)));
        assert!(!matches("0 0 1,15 * *", at(0, 0, 14, 2, 2)));
        // Sunday as 7 as well as 0.
        assert!(matches("0 3 * * 0", at(0, 3, 1, 1, 0)));
        assert!(matches("0 3 * * 7", at(0, 3, 1, 1, 0)));
    }

    #[test]
    fn due_ids_skips_a_task_that_already_ran_this_minute() {
        let task = |id: &str, last: Option<u64>| Task {
            id: id.to_string(),
            name: id.to_string(),
            schedule: "0 3 * * *".to_string(),
            command: "true".to_string(),
            last_run: last,
            last_status: None,
            last_output: None,
        };
        let now = 1_000_000_000;
        let doc = TasksDoc {
            tasks: vec![
                task("fresh", None),
                task("just-ran", Some(now - 5)),
                task("old", Some(now - 3600)),
            ],
        };
        let due = due_ids(&doc, now, at(0, 3, 1, 1, 1));
        assert_eq!(due, vec!["fresh".to_string(), "old".to_string()]);
    }

    #[test]
    fn ids_are_boring_on_purpose() {
        assert_eq!(sanitize_id("Disk-Prune_2").unwrap(), "disk-prune_2");
        assert_eq!(sanitize_id("nightly").unwrap(), "nightly");
        // Spaces and path characters are rejected rather than silently stripped.
        for bad in ["", "../etc", "a b", "a/b", "x".repeat(65).as_str()] {
            assert!(sanitize_id(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn field_matching_rejects_bad_syntax() {
        assert!(field_matches("*", 5, 0, 59));
        assert!(field_matches("5", 5, 0, 59));
        assert!(!field_matches("5", 6, 0, 59));
        assert!(field_matches("10-20/5", 15, 0, 59));
        assert!(!field_matches("10-20/5", 14, 0, 59));
        assert!(!field_matches("*/0", 5, 0, 59));
        assert!(!field_matches("nonsense", 5, 0, 59));
        assert!(!field_matches("", 5, 0, 59));
    }
}
