//! Best-effort audit trail for destructive operations.
//!
//! Every entry goes to the process log (so it lands in journald with a real
//! timestamp) *and* to an append-only file that survives log rotation and
//! restarts. Writing must never fail the operation being audited, so every error
//! here is a warning, not a returned error.

use std::io::Write;
use std::path::Path;

/// Append one entry. `actor` is `admin` in this single-admin build.
pub fn record(path: &str, actor: &str, action: &str, target: &str, detail: &str) {
    tracing::info!(target: "audit", actor, action, target, %detail, "audit");
    append(path, actor, action, target, detail);
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
    // Tab-separated: trivial to parse, and no field can contain a tab after the
    // callers sanitise their targets.
    let line = format!(
        "{epoch_s}\t{actor}\t{action}\t{}\t{}\n",
        sanitize(target),
        sanitize(detail)
    );

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut file) => {
            let _ = file.write_all(line.as_bytes());
        }
        Err(err) => tracing::warn!(path = %path.display(), %err, "cannot write audit log"),
    }
}

/// Replace tabs and newlines so one entry is always one line.
fn sanitize(field: &str) -> String {
    field.replace(['\t', '\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_entries_single_line() {
        assert_eq!(sanitize("a\tb\nc"), "a b c");
        assert_eq!(sanitize("plain"), "plain");
    }

    #[test]
    fn record_appends_a_parseable_line() {
        let dir = std::env::temp_dir().join(format!("ss-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.log");
        let path_str = path.to_string_lossy().into_owned();

        record(&path_str, "admin", "stack.deploy", "web", "compose up");
        record(&path_str, "admin", "container.remove", "redis\t1", "x");

        let body = std::fs::read_to_string(&path).expect("audit file");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[1], "admin");
        assert_eq!(fields[2], "stack.deploy");
        assert_eq!(fields[3], "web");
        // A tab in the target was flattened, so the line still has 5 fields.
        assert_eq!(lines[1].split('\t').count(), 5);
    }
}
