//! Best-effort audit trail for destructive operations.
//!
//! Every entry goes to the process log (so it lands in journald with a real
//! timestamp) *and* to an append-only JSON-lines file that survives log rotation
//! and restarts. The file is created at startup (mode 0600) so `tail -f` works
//! before the first destructive action.
//!
//! Writing must never fail the operation being audited, so every error here is a
//! warning, not a returned error.

use std::io::Write;
use std::path::Path;

use serde_json::json;

/// Create the audit file if needed, enforce 0600, and record the start.
pub fn init(path: &str) {
    ensure_file(path);
    record(path, "system", "service_start", "", "");
}

/// Append one entry. `actor` is `admin` for operator actions, `system` at boot.
pub fn record(path: &str, actor: &str, action: &str, target: &str, detail: &str) {
    tracing::info!(target: "audit", actor, action, target, %detail, "audit");
    append(path, actor, action, target, detail);
}

fn ensure_file(path: &str) {
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if !path.exists() {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Err(err) = options.open(path) {
            tracing::warn!(path = %path.display(), %err, "cannot create audit log");
        }
    }
    // A pre-existing file in a looser mode should not stay world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

fn append(path: &str, actor: &str, action: &str, target: &str, detail: &str) {
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let epoch_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let line = json!({
        "timestamp": rfc3339_utc(epoch_s),
        "unix": epoch_s,
        "user": actor,
        "action": action,
        "target": target,
        "detail": detail,
    })
    .to_string();

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut file) => {
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
        }
        Err(err) => tracing::warn!(path = %path.display(), %err, "cannot write audit log"),
    }
}

/// Seconds since the Unix epoch -> `YYYY-MM-DDTHH:MM:SSZ` (UTC).
///
/// Implemented directly (Howard Hinnant's civil-from-days) rather than pulling in
/// a date crate for one log field.
pub fn rfc3339_utc(epoch_s: u64) -> String {
    let days = (epoch_s / 86_400) as i64;
    let secs_of_day = epoch_s % 86_400;
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );

    // days since 1970-01-01 -> civil date
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2024-02-29 (leap day) at 12:34:56 UTC
        assert_eq!(rfc3339_utc(1_709_210_096), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn init_creates_the_file_and_records_the_start() {
        let dir = std::env::temp_dir().join(format!("ss-audit-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.log");
        let path_str = path.to_string_lossy().into_owned();

        init(&path_str);
        assert!(path.exists(), "init must create the audit file");
        let body = std::fs::read_to_string(&path).expect("read");
        let value: serde_json::Value =
            serde_json::from_str(body.lines().next().expect("one line")).expect("valid json");
        assert_eq!(value["action"], "service_start");
        assert_eq!(value["user"], "system");
        assert!(value["timestamp"].as_str().unwrap().ends_with('Z'));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "audit log must be 0600");
        }
    }

    #[test]
    fn record_appends_one_json_object_per_line() {
        let dir = std::env::temp_dir().join(format!("ss-audit-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.log");
        let path_str = path.to_string_lossy().into_owned();

        record(&path_str, "admin", "stack.deploy", "web", "compose up");
        record(&path_str, "admin", "container.remove", "redis", "x");

        let body = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            assert!(value["action"].is_string());
            assert!(value["timestamp"].is_string());
        }
    }
}
